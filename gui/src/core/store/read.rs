//! Queries. Plain functions on a connection, so the writer and the read pool share them.

use std::collections::HashMap;
use std::path::PathBuf;

use earshot_engine::HOTCUES;
use earshot_engine::beatgrid::Beatgrid;
use earshot_engine::loudness::Loudness;
use rusqlite::{Connection, OptionalExtension, params};

use super::{CueKind, GridSource, Result, StoreError, WAVEFORM_VERSION, bytes_path};
use crate::core::flows::{self, Card, FlowEntry, Folder};

/// A library track as the UI sees it: metadata resolved (user > import > tag), and
/// the effective BPM (from the active grid).
#[derive(Clone, Debug, PartialEq)]
pub struct LibTrack {
    pub id: i64,
    /// Where the track is now, for showing (lossy if the name isn't UTF-8).
    pub path: String,
    /// The file name when it has no title.
    pub title: String,
    pub artist: String,
    pub duration: Option<f64>,
    /// The active grid's BPM, or `None` with no grid.
    pub bpm: Option<f64>,
    /// The BPM in the metadata (tag, or what the user typed).
    pub tag_bpm: Option<f64>,
    pub key: Option<String>,
    pub grid_source: Option<GridSource>,
    /// Stand-in energy (engine/src/energy.rs); only its order means anything.
    pub energy: Option<f64>,
    /// A static analysis found the tempo seems to drift.
    pub drifts: bool,
}

/// (track id, waveform thumbnail)
pub type Preview = (i64, Vec<u8>);

/// Metadata fields the track list needs.
const LIST_FIELDS: &str = "('title', 'artist', 'bpm', 'key')";

/// Lower is stronger: user > import > tag.
pub(super) fn source_rank(source: &str) -> u8 {
    match source {
        "user" => 0,
        "tag" => 2,
        _ => 1,
    }
}

/// Tracks with a file that's present: all of them under the library roots (`None`),
/// or just these ids (wherever they are).
pub fn tracks(c: &Connection, ids: Option<&[i64]>) -> Result<Vec<LibTrack>> {
    let base = "SELECT t.id, f.path, t.duration_s, t.bpm, g.source
         FROM tracks t
         JOIN files f ON f.id = (SELECT id FROM files
                                 WHERE track_id = t.id AND missing_since IS NULL
                                 ORDER BY id LIMIT 1)
         LEFT JOIN beatgrids g ON g.id = t.active_grid_id
         WHERE t.deleted_at IS NULL";
    type Row = (i64, Vec<u8>, Option<f64>, Option<f64>, Option<String>);
    let map = |r: &rusqlite::Row| -> rusqlite::Result<Row> {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
    };
    let mut rows: Vec<Row> = vec![];
    // Metadata and features: (track, name) → (rank, value)
    let mut meta: HashMap<(i64, String), (u8, String)> = HashMap::new();
    let mut feats: HashMap<(i64, String), f64> = HashMap::new();
    let mut add_meta = |id: i64, field: String, source: String, value: String| {
        let rank = source_rank(&source);
        let e = meta.entry((id, field)).or_insert((u8::MAX, String::new()));
        if rank < e.0 {
            *e = (rank, value);
        }
    };
    let meta_sql = format!(
        "SELECT track_id, field, source, value FROM track_meta WHERE field IN {LIST_FIELDS}"
    );
    let feat_sql = "SELECT track_id, name, value FROM track_features
                    WHERE name IN ('energy', 'drifts')";
    match ids {
        None => {
            let roots = roots(c)?;
            let mut st = c.prepare(&format!("{base} ORDER BY f.path"))?;
            rows = st
                .query_map([], map)?
                .collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter()
                .filter(|r| roots.iter().any(|root| under(&r.1, root)))
                .collect();
            let mut st = c.prepare(&meta_sql)?;
            let mut q = st.query([])?;
            while let Some(r) = q.next()? {
                add_meta(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
            }
            let mut st = c.prepare(feat_sql)?;
            let mut q = st.query([])?;
            while let Some(r) = q.next()? {
                feats.insert((r.get(0)?, r.get(1)?), r.get(2)?);
            }
        }
        Some(ids) => {
            let mut st = c.prepare_cached(&format!("{base} AND t.id = ?1"))?;
            let mut st_meta = c.prepare_cached(&format!("{meta_sql} AND track_id = ?1"))?;
            let mut st_feat = c.prepare_cached(&format!("{feat_sql} AND track_id = ?1"))?;
            for &id in ids {
                rows.extend(st.query_row([id], map).optional()?);
                let mut q = st_meta.query([id])?;
                while let Some(r) = q.next()? {
                    add_meta(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
                }
                let mut q = st_feat.query([id])?;
                while let Some(r) = q.next()? {
                    feats.insert((r.get(0)?, r.get(1)?), r.get(2)?);
                }
            }
        }
    }
    Ok(rows
        .into_iter()
        .map(|(id, path, duration, bpm, source)| {
            let path = bytes_path(path);
            let mut take = |f: &str| {
                meta.remove(&(id, f.to_string()))
                    .map(|(_, v)| v)
                    .filter(|v| !v.is_empty())
            };
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            LibTrack {
                id,
                title: take("title").unwrap_or(stem),
                artist: take("artist").unwrap_or_default(),
                tag_bpm: take("bpm").and_then(|b| b.trim().parse().ok()),
                key: take("key"),
                duration,
                bpm,
                grid_source: source.as_deref().and_then(GridSource::parse),
                energy: feats.get(&(id, "energy".into())).copied(),
                drifts: feats.get(&(id, "drifts".into())).is_some_and(|d| *d != 0.0),
                path: path.to_string_lossy().into_owned(),
            }
        })
        .collect())
}

/// `path` is `root` or inside it.
pub(super) fn under(path: &[u8], root: &[u8]) -> bool {
    let root = root.strip_suffix(b"/").unwrap_or(root);
    path.starts_with(root) && (path.len() == root.len() || path[root.len()] == b'/')
}

pub fn roots(c: &Connection) -> Result<Vec<Vec<u8>>> {
    let mut st = c.prepare_cached("SELECT path FROM library_roots ORDER BY path")?;
    Ok(st
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn track_path(c: &Connection, id: i64) -> Result<PathBuf> {
    c.query_row(
        "SELECT path FROM files WHERE track_id = ?1 AND missing_since IS NULL
         ORDER BY id LIMIT 1",
        [id],
        |r| r.get::<_, Vec<u8>>(0),
    )
    .optional()?
    .map(bytes_path)
    .ok_or(StoreError::NoTrack(id))
}

pub fn active_grid(c: &Connection, id: i64) -> Result<Option<(i64, GridSource)>> {
    let row: Option<(i64, String)> = c
        .query_row(
            "SELECT g.id, g.source FROM tracks t JOIN beatgrids g ON g.id = t.active_grid_id
             WHERE t.id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row.and_then(|(g, s)| Some((g, GridSource::parse(&s)?))))
}

pub fn grid(c: &Connection, id: i64) -> Result<Option<(Beatgrid, GridSource)>> {
    let row: Option<(Vec<u8>, String)> = c
        .query_row(
            "SELECT g.beats, g.source FROM tracks t JOIN beatgrids g ON g.id = t.active_grid_id
             WHERE t.id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((bytes, source)) = row else {
        return Ok(None);
    };
    let bad = |reason: String| StoreError::BadGrid { track: id, reason };
    let grid = Beatgrid::from_bytes(&bytes).map_err(|e| bad(e.to_string()))?;
    let source = GridSource::parse(&source).ok_or_else(|| bad(format!("source {source}")))?;
    Ok(Some((grid, source)))
}

pub(super) fn waveform_params(bins_per_s: f64) -> String {
    format!("bins_per_s={bins_per_s}")
}

pub fn waveform(c: &Connection, id: i64, bins_per_s: f64) -> Result<Option<Vec<u8>>> {
    Ok(c.query_row(
        "SELECT data FROM blobs WHERE track_id = ?1 AND kind = 'waveform'
           AND format_version = ?2 AND params = ?3",
        params![id, WAVEFORM_VERSION, waveform_params(bins_per_s)],
        |r| r.get(0),
    )
    .optional()?)
}

/// Thumbnails for these tracks, or every track.
pub fn previews(c: &Connection, ids: Option<&[i64]>) -> Result<Vec<Preview>> {
    let sql = "SELECT track_id, data FROM blobs
               WHERE kind = 'waveform_preview' AND format_version = ?1";
    let map = |r: &rusqlite::Row| Ok((r.get(0)?, r.get(1)?));
    Ok(match ids {
        None => c
            .prepare(sql)?
            .query_map([WAVEFORM_VERSION], map)?
            .collect::<rusqlite::Result<_>>()?,
        Some(ids) => {
            let mut st = c.prepare_cached(&format!("{sql} AND track_id = ?2"))?;
            let mut out = vec![];
            for &id in ids {
                out.extend(
                    st.query_row(params![WAVEFORM_VERSION, id], map)
                        .optional()?,
                );
            }
            out
        }
    })
}

pub fn feature(c: &Connection, id: i64, name: &str) -> Result<Option<f64>> {
    Ok(c.query_row(
        "SELECT value FROM track_features WHERE track_id = ?1 AND name = ?2",
        params![id, name],
        |r| r.get(0),
    )
    .optional()?)
}

pub fn loudness(c: &Connection, id: i64) -> Result<Option<Loudness>> {
    Ok(match (feature(c, id, "lufs")?, feature(c, id, "peak")?) {
        (Some(lufs), Some(peak)) => Some(Loudness { lufs, peak }),
        _ => None,
    })
}

// ---- cues --------------------------------------------------------------------------

/// A memory cue (a point) or memory loop (with an end), as Rekordbox has them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Memory {
    pub id: i64,
    pub start: f64,
    pub end: Option<f64>,
}

pub fn hotcues(c: &Connection, track: i64) -> Result<[Option<f64>; HOTCUES]> {
    let mut out = [None; HOTCUES];
    let mut st =
        c.prepare_cached("SELECT slot, start_s FROM cues WHERE track_id = ?1 AND kind = ?2")?;
    let rows = st.query_map(params![track, CueKind::Hot.as_i64()], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?))
    })?;
    for row in rows {
        let (slot, t) = row?;
        if let Some(h) = usize::try_from(slot).ok().and_then(|s| out.get_mut(s)) {
            *h = Some(t);
        }
    }
    Ok(out)
}

/// A track's memory cues and loops, in order through the track.
pub fn memories(c: &Connection, track: i64) -> Result<Vec<Memory>> {
    let mut st = c.prepare_cached(
        "SELECT id, start_s, end_s FROM cues WHERE track_id = ?1 AND kind IN (?2, ?3)
         ORDER BY start_s, end_s",
    )?;
    let rows = st.query_map(
        params![track, CueKind::Memory.as_i64(), CueKind::Loop.as_i64()],
        |r| {
            Ok(Memory {
                id: r.get(0)?,
                start: r.get(1)?,
                end: r.get(2)?,
            })
        },
    )?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn files_without_hash(c: &Connection) -> Result<Vec<(i64, PathBuf)>> {
    let mut st = c.prepare(
        "SELECT id, path FROM files WHERE content_hash IS NULL AND missing_since IS NULL
         ORDER BY id",
    )?;
    Ok(st
        .query_map([], |r| Ok((r.get(0)?, bytes_path(r.get(1)?))))?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn known_files(c: &Connection) -> Result<HashMap<Vec<u8>, (i64, i64)>> {
    let mut st = c.prepare("SELECT path, size, mtime_ns FROM files")?;
    Ok(st
        .query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
        .collect::<rusqlite::Result<_>>()?)
}

#[allow(dead_code)] // library management API, not wired to the UI yet (tests use it)
pub fn search(c: &Connection, query: &str) -> Result<Vec<i64>> {
    let mut st = c.prepare_cached(
        "SELECT rowid FROM track_search WHERE track_search MATCH ?1 ORDER BY rank",
    )?;
    Ok(st
        .query_map([query], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

#[allow(dead_code)] // library management API, not wired to the UI yet (tests use it)
pub fn duplicates(c: &Connection) -> Result<Vec<Vec<i64>>> {
    let mut st = c.prepare(
        "SELECT group_concat(DISTINCT track_id) FROM files
         WHERE content_hash IS NOT NULL AND missing_since IS NULL
         GROUP BY content_hash HAVING count(DISTINCT track_id) > 1",
    )?;
    let groups = st
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(groups
        .into_iter()
        .map(|g| {
            let mut ids: Vec<i64> = g.split(',').filter_map(|s| s.parse().ok()).collect();
            ids.sort();
            ids
        })
        .collect())
}

// ---- Flows ----------------------------------------------------------------------------

/// Every folder and Flow, sorted by name.
pub fn flows(c: &Connection) -> Result<(Vec<Folder>, Vec<FlowEntry>)> {
    let mut st = c.prepare(
        "SELECT id, parent_id, name FROM collections WHERE kind = 'folder'
         ORDER BY name COLLATE NOCASE",
    )?;
    let folders = st
        .query_map([], |r| {
            Ok(Folder {
                id: r.get(0)?,
                parent: r.get(1)?,
                name: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // All nodes and edges at once, rather than a query per Flow.
    let mut nodes: HashMap<i64, Vec<(u64, i64, f32, f32)>> = HashMap::new();
    let mut st = c.prepare("SELECT collection_id, id, track_id FROM flow_nodes")?;
    let mut q = st.query([])?;
    while let Some(r) = q.next()? {
        nodes
            .entry(r.get(0)?)
            .or_default()
            .push((r.get::<_, i64>(1)? as u64, r.get(2)?, 0.0, 0.0));
    }
    let mut edges: HashMap<i64, Vec<(u64, u64)>> = HashMap::new();
    let mut st = c.prepare("SELECT collection_id, from_node, to_node FROM flow_edges")?;
    let mut q = st.query([])?;
    while let Some(r) = q.next()? {
        edges
            .entry(r.get(0)?)
            .or_default()
            .push((r.get::<_, i64>(1)? as u64, r.get::<_, i64>(2)? as u64));
    }
    let mut st = c.prepare(
        "SELECT id, parent_id, name FROM collections WHERE kind = 'flow'
         ORDER BY name COLLATE NOCASE",
    )?;
    let rows = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<(i64, Option<i64>, String)>>>()?;
    Ok((
        folders,
        rows.into_iter()
            .map(|(id, folder, name)| {
                let n = nodes.remove(&id).unwrap_or_default();
                let e = edges.remove(&id).unwrap_or_default();
                let count = n.len();
                let linear = e.len() + 1 == count.max(1)
                    && flows::playlist_order(&flows::cards_from(n, &e)).is_some();
                FlowEntry {
                    id,
                    folder,
                    name,
                    cards: count,
                    linear,
                }
            })
            .collect(),
    ))
}

pub fn flow_graph(c: &Connection, id: i64) -> Result<(Vec<(u64, i64, f32, f32)>, Vec<(u64, u64)>)> {
    let mut st = c.prepare_cached(
        "SELECT id, track_id, x, y FROM flow_nodes WHERE collection_id = ?1 ORDER BY id",
    )?;
    let nodes = st
        .query_map([id], |r| {
            Ok((
                r.get::<_, i64>(0)? as u64,
                r.get(1)?,
                r.get::<_, f64>(2)? as f32,
                r.get::<_, f64>(3)? as f32,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut st = c.prepare_cached(
        "SELECT from_node, to_node FROM flow_edges WHERE collection_id = ?1
         ORDER BY from_node, to_node",
    )?;
    let edges = st
        .query_map([id], |r| {
            Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((nodes, edges))
}

/// A Flow's name and cards, or `None` if there's no such Flow.
pub fn flow(c: &Connection, id: i64) -> Result<Option<(i64, String, Vec<Card>)>> {
    let name: Option<String> = c
        .query_row(
            "SELECT name FROM collections WHERE id = ?1 AND kind = 'flow'",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(name) = name else { return Ok(None) };
    let (nodes, edges) = flow_graph(c, id)?;
    Ok(Some((id, name, flows::cards_from(nodes, &edges))))
}

/// A playlist's tracks in order.
#[allow(dead_code)] // library management API, not wired to the UI yet (tests use it)
pub fn playlist(c: &Connection, id: i64) -> Result<Vec<i64>> {
    let mut st = c.prepare_cached(
        "SELECT track_id FROM playlist_entries WHERE collection_id = ?1 ORDER BY position",
    )?;
    Ok(st
        .query_map([id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// For tests and debugging: the path of a file row.
#[allow(dead_code)]
pub fn file_path(c: &Connection, file: i64) -> Result<PathBuf> {
    let b: Vec<u8> = c.query_row("SELECT path FROM files WHERE id = ?1", [file], |r| r.get(0))?;
    Ok(bytes_path(b))
}
