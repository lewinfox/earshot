//! Sources (`core::sources`): adding, changing and removing them.

use rusqlite::{OptionalExtension, params};

use super::{Writer, reindex};
use crate::core::sources::{NewSource, Source, SourceId};
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
            .filter(|s| s.enabled && !s.kind.is_library())
            .filter_map(|s| s.location)
            .collect())
    }

    /// Add a source, or (the same folder or library again, or a program's library
    /// when it already has one) set the one there and switch it on. Returns its id.
    pub fn add_source(&mut self, s: &NewSource) -> Result<SourceId> {
        if !s.kind.is_library()
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
