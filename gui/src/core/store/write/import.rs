//! Importing from another library (`core::external`): its tracks, their metadata,
//! grids and cues, and its playlists and crates, in one transaction.

use std::collections::HashMap;
use std::ops::Range;

use rusqlite::{Connection, OptionalExtension, params};

use super::{Writer, activate_grid, grid_bpm, insert_grid, put_meta, record_file, reindex};
use crate::core::collections::{CollectionKind, Section};
use crate::core::external::{self, ExtCue, ImportNode, ImportSave, Imported, NodeKind};
use crate::core::store::{CueKind, GridSource, Result, now, path_bytes, read};

/// The fields an import can set.
const FIELDS: [&str; 7] = ["title", "artist", "album", "genre", "comment", "bpm", "key"];

impl Writer {
    /// Bring in what `save` says (see `core::external`). New collections take their
    /// ids from `ids`, handed out by the Store beforehand.
    pub fn import_external(&mut self, save: ImportSave, ids: Range<i64>) -> Result<Imported> {
        let t = now();
        let source = format!("source:{}", save.source);
        let opts = save.options;
        let mut done = Imported::default();
        let tx = self.conn.transaction()?;
        for f in &save.files {
            record_file(&tx, f, t)?;
        }
        // External id → our track.
        let mut ours: HashMap<&str, i64> = HashMap::new();
        for it in &save.tracks {
            let track: Option<i64> = tx
                .query_row(
                    "SELECT track_id FROM files WHERE path = ?1",
                    [path_bytes(&it.file)],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(track) = track else {
                continue; // unreadable when the files were looked at
            };
            ours.insert(&it.external_id, track);
            done.tracks += 1;
            tx.execute(
                "INSERT INTO track_sources(source_id, external_id, track_id, synced_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(source_id, external_id) DO UPDATE SET
                   track_id = excluded.track_id, synced_at = excluded.synced_at",
                params![save.source, it.external_id, track, t],
            )?;
            for field in FIELDS {
                let value = it.meta.iter().find(|m| m.0 == field).map(|m| m.1.as_str());
                import_field(&tx, track, field, &source, value, opts.overwrite, t)?;
            }
            reindex(&tx, track)?;
            let sql = if opts.overwrite {
                "UPDATE tracks SET rating = coalesce(?2, rating), color = coalesce(?3, color)
                 WHERE id = ?1"
            } else {
                "UPDATE tracks SET rating = coalesce(rating, ?2), color = coalesce(color, ?3)
                 WHERE id = ?1"
            };
            tx.execute(sql, params![track, it.rating, it.colour])?;
            if !it.grid.is_empty() {
                let duration: Option<f64> = tx.query_row(
                    "SELECT duration_s FROM tracks WHERE id = ?1",
                    [track],
                    |r| r.get(0),
                )?;
                let end = duration.unwrap_or_else(|| it.grid.last().unwrap().start_s + 600.0);
                if let Some(grid) = external::grid_of(&it.grid, end) {
                    let id = insert_grid(&tx, track, &grid, GridSource::Import, None)?;
                    activate_grid(&tx, track, id, grid_bpm(&grid))?;
                    done.grids += 1;
                }
            }
            done.cues += hot_cues(&tx, track, &it.hot_cues, &source, opts.overwrite)?;
            done.cues += memory_cues(&tx, track, &it.memory_cues, &source)?;
        }
        let mut ids = ids;
        for (section, root) in &save.roots {
            put_node(&tx, root, None, *section, &mut ids, &ours, t, &mut done)?;
        }
        tx.commit()?;
        self.emit_library()?;
        self.emit_collections()?;
        Ok(done)
    }
}

/// One field's imported value (`None`: the library has none). Without
/// `overwrite` it only fills a gap: it's written when no other source has a value
/// (an earlier import's own value is kept up to date). With `overwrite` it replaces
/// what the user typed and other imports said; the file's tag stays, below it.
fn import_field(
    c: &Connection,
    track: i64,
    field: &str,
    source: &str,
    value: Option<&str>,
    overwrite: bool,
    t: i64,
) -> Result<()> {
    if overwrite {
        c.prepare_cached(
            "DELETE FROM track_meta WHERE track_id = ?1 AND field = ?2
               AND source NOT IN ('tag', ?3)",
        )?
        .execute(params![track, field, source])?;
        return put_meta(c, track, field, source, value, t);
    }
    let (others, own): (i64, i64) = c
        .prepare_cached(
            "SELECT count(*) FILTER (WHERE source != ?3 AND value != ''),
                    count(*) FILTER (WHERE source = ?3)
             FROM track_meta WHERE track_id = ?1 AND field = ?2",
        )?
        .query_row(params![track, field, source], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    if others == 0 || own > 0 {
        put_meta(c, track, field, source, value, t)?;
    }
    Ok(())
}

/// Hot cues into their pads: empty ones, or (`overwrite`) any. Returns how many.
fn hot_cues(
    c: &Connection,
    track: i64,
    cues: &[ExtCue],
    source: &str,
    overwrite: bool,
) -> Result<usize> {
    let sql = if overwrite {
        "INSERT INTO cues(track_id, kind, slot, start_s, label, color, source)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT (track_id, kind, slot) DO UPDATE SET start_s = excluded.start_s,
           end_s = NULL, label = excluded.label, color = excluded.color,
           source = excluded.source"
    } else {
        "INSERT OR IGNORE INTO cues(track_id, kind, slot, start_s, label, color, source)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
    };
    let mut st = c.prepare_cached(sql)?;
    let mut n = 0;
    for cue in cues {
        let Some(slot) = cue.slot.filter(|&s| (s as usize) < earshot_engine::HOTCUES) else {
            continue;
        };
        n += st.execute(params![
            track,
            CueKind::Hot.as_i64(),
            slot,
            cue.start_s,
            label(cue),
            cue.colour,
            source
        ])?;
    }
    Ok(n)
}

/// Memory cues and loops, except ones the track already has (within 5 ms).
/// Returns how many were added.
fn memory_cues(c: &Connection, track: i64, cues: &[ExtCue], source: &str) -> Result<usize> {
    let mut have: Vec<(f64, Option<f64>)> = read::memories(c, track)?
        .iter()
        .map(|m| (m.start, m.end))
        .collect();
    let close = |a: f64, b: f64| (a - b).abs() < 0.005;
    let mut n = 0;
    for cue in cues {
        let end = cue.end_s.filter(|&e| e > cue.start_s);
        let same = have.iter().any(|&(s, e)| {
            close(s, cue.start_s)
                && match (e, end) {
                    (None, None) => true,
                    (Some(a), Some(b)) => close(a, b),
                    _ => false,
                }
        });
        if same {
            continue;
        }
        let kind = if end.is_some() {
            CueKind::Loop
        } else {
            CueKind::Memory
        };
        c.prepare_cached(
            "INSERT INTO cues(track_id, kind, start_s, end_s, label, color, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?
        .execute(params![
            track,
            kind.as_i64(),
            cue.start_s,
            end,
            label(cue),
            cue.colour,
            source
        ])?;
        have.push((cue.start_s, end));
        n += 1;
    }
    Ok(n)
}

fn label(cue: &ExtCue) -> Option<&str> {
    Some(cue.name.trim()).filter(|s| !s.is_empty())
}

/// Make the collection a node describes, or find the one an earlier import made
/// (by `external`; the user may have renamed or moved it since: it stays so), and
/// give a playlist or crate the library's contents. Returns its id.
#[allow(clippy::too_many_arguments)]
fn put_node(
    c: &Connection,
    node: &ImportNode,
    parent: Option<i64>,
    section: Section,
    ids: &mut Range<i64>,
    ours: &HashMap<&str, i64>,
    t: i64,
    done: &mut Imported,
) -> Result<i64> {
    let kind = match node.kind {
        NodeKind::Folder => CollectionKind::Folder,
        NodeKind::Playlist => CollectionKind::Playlist,
        NodeKind::Crate => CollectionKind::Crate,
    };
    let found: Option<i64> = c
        .query_row(
            "SELECT id FROM collections WHERE external = ?1",
            [&node.external],
            |r| r.get(0),
        )
        .optional()?;
    let id = match found {
        Some(id) => {
            c.execute(
                "UPDATE collections SET updated_at = ?2 WHERE id = ?1",
                params![id, t],
            )?;
            id
        }
        None => {
            let id = ids.next().expect("an id for every node");
            // An earlier import's folder may have been deleted by the user, taking
            // this one's parent with it: then it goes in at the top level.
            c.execute(
                "INSERT INTO collections(id, parent_id, kind, section, name, external,
                                         created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                params![
                    id,
                    parent,
                    kind.as_str(),
                    section.as_str(),
                    node.name,
                    node.external,
                    t
                ],
            )?;
            id
        }
    };
    let tracks = node.tracks.iter().filter_map(|e| ours.get(e.as_str()));
    match kind {
        CollectionKind::Folder => {
            for child in &node.children {
                put_node(c, child, Some(id), section, ids, ours, t, done)?;
            }
        }
        CollectionKind::Crate => {
            c.execute("DELETE FROM crate_tracks WHERE collection_id = ?1", [id])?;
            let mut st = c.prepare_cached(
                "INSERT OR IGNORE INTO crate_tracks(collection_id, track_id, added_at)
                 VALUES (?1, ?2, ?3)",
            )?;
            for track in tracks {
                st.execute(params![id, track, t])?;
            }
            done.collections += 1;
        }
        _ => {
            c.execute(
                "DELETE FROM playlist_entries WHERE collection_id = ?1",
                [id],
            )?;
            let mut st = c.prepare_cached(
                "INSERT INTO playlist_entries(collection_id, position, track_id)
                 VALUES (?1, ?2, ?3)",
            )?;
            for (i, track) in tracks.enumerate() {
                st.execute(params![id, i as i64, track])?;
            }
            done.collections += 1;
        }
    }
    Ok(id)
}
