//! Sources (`core::sources`): adding, changing and removing them.

use rusqlite::{OptionalExtension, params};

use super::{ScannedFile, ScannedObject, Writer, last_track, new_tracks, record_at, reindex};
use crate::core::sources::{NewSource, Source, SourceId, SourceKind};
use crate::core::store::{Result, StoreError, StoreEvent, now, path_bytes, read};

impl Writer {
    pub(super) fn emit_sources(&mut self) -> Result<()> {
        let s = read::sources(&self.conn)?;
        self.emit(StoreEvent::Sources(s));
        Ok(())
    }

    /// The enabled music folders, for a scan.
    pub fn folders(&self) -> Result<Vec<std::path::PathBuf>> {
        Ok(read::sources(&self.conn)?
            .into_iter()
            .filter(|s| s.enabled && s.kind.is_folder())
            .filter_map(|s| s.location)
            .collect())
    }

    /// Add a source, or (the same folder or library again, or a program's library
    /// when it already has one) set the one there and switch it on. Returns its id.
    pub fn add_source(&mut self, s: &NewSource) -> Result<SourceId> {
        if s.kind.is_folder()
            && let Some(p) = &s.location
            && !p.is_dir()
        {
            return Err(StoreError::Refused("that is not a folder"));
        }
        let location = s.location.as_deref().map(|p| {
            let mut b = path_bytes(p);
            while b.len() > 1 && b.ends_with(b"/") {
                b.pop();
            }
            b
        });
        if s.kind.is_bucket() {
            self.check_bucket(s)?;
        }
        let config = serde_json::to_string(&s.config).expect("config serialises");
        // A program has one library: the one it had, set again.
        let existing: Option<SourceId> = if s.kind.is_program() {
            self.conn
                .query_row(
                    "SELECT id FROM sources WHERE kind = ?1 ORDER BY id LIMIT 1",
                    [s.kind.as_str()],
                    |r| r.get(0),
                )
                .optional()?
        } else {
            self.conn
                .query_row(
                    "SELECT id FROM sources WHERE kind = ?1 AND location IS ?2",
                    params![s.kind.as_str(), location],
                    |r| r.get(0),
                )
                .optional()?
        };
        let id = match existing {
            Some(id) => {
                self.conn.execute(
                    "UPDATE sources SET name = ?2, location = ?3, config = ?4, enabled = 1
                     WHERE id = ?1",
                    params![id, s.name, location, config],
                )?;
                id
            }
            None => {
                self.conn.execute(
                    "INSERT INTO sources(kind, name, location, config, enabled, added_at)
                     VALUES (?1, ?2, ?3, ?4, 1, ?5)",
                    params![s.kind.as_str(), s.name, location, config, now()],
                )?;
                self.conn.last_insert_rowid()
            }
        };
        self.emit_sources()?;
        self.emit_library()?;
        Ok(id)
    }

    /// A bucket's objects are filed by `s3://<bucket>/<key>`, so a bucket of the same
    /// name at another service can't be a source too: its objects would be taken for
    /// the first one's.
    fn check_bucket(&self, s: &NewSource) -> Result<()> {
        let bucket_of = |location: &std::path::Path| {
            let url = location.to_string_lossy().into_owned();
            let rest = url.strip_prefix("s3://")?;
            Some(rest.split('/').next().unwrap_or(rest).to_string())
        };
        let Some(bucket) = s.location.as_deref().and_then(bucket_of) else {
            return Err(StoreError::Refused(
                "a bucket's location is s3://bucket/prefix",
            ));
        };
        let endpoint =
            |c: &crate::core::sources::SourceConfig| c.s3.as_ref().and_then(|c| c.endpoint.clone());
        let clash = read::sources(&self.conn)?.into_iter().any(|o| {
            o.kind == SourceKind::S3
                && o.location.as_deref().and_then(bucket_of).as_ref() == Some(&bucket)
                && endpoint(&o.config) != endpoint(&s.config)
        });
        if clash {
            return Err(StoreError::Refused(
                "a bucket of that name at another service is already a source",
            ));
        }
        Ok(())
    }

    // ---- buckets (`core::bucket`) --------------------------------------------------------

    /// Record objects a bucket scan read: new ones, and ones whose ETag or size
    /// changed. Each is a file whose path is its URL, linked to the bucket's source by
    /// its key. As with a folder's files, only tag-sourced metadata is written, and a
    /// new object whose stamped hash matches a track whose files have all gone becomes
    /// that track's.
    pub fn record_objects(&mut self, source: SourceId, objects: &[ScannedObject]) -> Result<()> {
        if objects.is_empty() {
            return Ok(());
        }
        let t = now();
        let tx = self.conn.transaction()?;
        let before = last_track(&tx)?;
        for o in objects {
            let as_file = ScannedFile {
                path: std::path::PathBuf::from(&o.url),
                size: o.size,
                mtime_ns: o.modified_ns,
                tags: o.tags.clone(),
                tag_hash: o.tag_hash,
            };
            let track = record_at(&tx, o.url.as_bytes(), &as_file, t)?;
            tx.execute(
                "UPDATE files SET etag = ?2 WHERE path = ?1",
                params![o.url.as_bytes(), o.etag],
            )?;
            tx.execute(
                "INSERT INTO track_sources(source_id, external_id, track_id, synced_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (source_id, external_id)
                 DO UPDATE SET track_id = excluded.track_id, synced_at = excluded.synced_at",
                params![source, o.key, track, t],
            )?;
        }
        let new = new_tracks(&tx, before)?;
        tx.commit()?;
        self.emit_library()?;
        self.emit_new_tracks(new);
        Ok(())
    }

    /// A bucket source was listed: `found` is every audio object under `root` (its
    /// URL, `s3://bucket/prefix`), as (URL, key). The ones known that weren't found
    /// are marked missing (not deleted); ones found again aren't any more; and every
    /// one found is linked to the source.
    pub fn bucket_listed(
        &mut self,
        source: SourceId,
        root: &str,
        found: &[(String, String)],
    ) -> Result<()> {
        let t = now();
        let tx = self.conn.transaction()?;
        tx.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS seen(path BLOB PRIMARY KEY); DELETE FROM seen;",
        )?;
        {
            let mut st = tx.prepare("INSERT OR IGNORE INTO seen(path) VALUES (?1)")?;
            let mut link = tx.prepare(
                "INSERT OR IGNORE INTO track_sources(source_id, external_id, track_id, synced_at)
                 SELECT ?1, ?2, track_id, ?3 FROM files WHERE path = ?4",
            )?;
            for (url, key) in found {
                st.execute([url.as_bytes()])?;
                link.execute(params![source, key, t, url.as_bytes()])?;
            }
        }
        let prefix = format!("{}/", root.trim_end_matches('/')).into_bytes();
        tx.execute(
            "UPDATE files SET missing_since = NULL
             WHERE missing_since IS NOT NULL AND path IN (SELECT path FROM seen)",
            [],
        )?;
        tx.execute(
            "UPDATE files SET missing_since = ?1
             WHERE missing_since IS NULL AND substr(path, 1, ?2) = ?3
               AND path NOT IN (SELECT path FROM seen)",
            params![t, prefix.len() as i64, prefix],
        )?;
        tx.commit()?;
        self.emit_library()
    }

    pub fn update_source(&mut self, s: &Source) -> Result<()> {
        let config = serde_json::to_string(&s.config).expect("config serialises");
        let n = self.conn.execute(
            "UPDATE sources SET name = ?2, location = ?3, config = ?4, enabled = ?5
             WHERE id = ?1",
            params![
                s.id,
                s.name,
                s.location.as_deref().map(path_bytes),
                config,
                s.enabled
            ],
        )?;
        if n == 0 {
            return Err(StoreError::Refused("no such source"));
        }
        self.emit_sources()?;
        self.emit_library()
    }

    /// A folder or added library goes, with its links and what it said about tracks'
    /// metadata; the tracks stay in the database (keeping their analysis), unlisted
    /// unless another source has them. A program's library is switched off instead.
    pub fn remove_source(&mut self, id: SourceId) -> Result<()> {
        let Some(s) = read::sources(&self.conn)?.into_iter().find(|s| s.id == id) else {
            return Ok(());
        };
        let tx = self.conn.transaction()?;
        if s.kind.is_program() {
            tx.execute("UPDATE sources SET enabled = 0 WHERE id = ?1", [id])?;
        } else {
            let meta = format!("source:{id}");
            let touched: Vec<i64> = tx
                .prepare("SELECT DISTINCT track_id FROM track_meta WHERE source = ?1")?
                .query_map([&meta], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            tx.execute("DELETE FROM track_meta WHERE source = ?1", [&meta])?;
            for t in touched {
                reindex(&tx, t)?;
            }
            tx.execute("DELETE FROM sources WHERE id = ?1", [id])?;
        }
        tx.commit()?;
        self.emit_sources()?;
        self.emit_library()
    }
}
