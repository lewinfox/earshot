//! Queries. Plain functions on a connection, so the writer and the read pool share them.

use std::collections::HashMap;
use std::path::PathBuf;

use earshot_engine::HOTCUES;
use earshot_engine::beatgrid::Beatgrid;
use earshot_engine::loudness::Loudness;
use rusqlite::{Connection, OptionalExtension, params};

use super::{CueKind, GridSource, Result, StoreError, WAVEFORM_VERSION, bytes_path};
use crate::core::collections::{Collection, CollectionKind, FlowGraph, FlowNode, NodeId};
use crate::core::query;

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
    pub album: String,
    pub genre: String,
    pub comment: String,
    pub grid_source: Option<GridSource>,
    /// Stand-in energy (engine/src/energy.rs); only its order means anything.
    pub energy: Option<f64>,
    /// A static analysis found the tempo seems to drift.
    pub drifts: bool,
    /// 0-5 stars.
    pub rating: Option<u8>,
    pub colour: Option<u32>,
    /// Tag names, sorted.
    pub tags: Vec<String>,
}

/// (track id, waveform thumbnail)
pub type Preview = (i64, Vec<u8>);

/// Metadata fields the track list needs.
const LIST_FIELDS: &str = "('title', 'artist', 'album', 'genre', 'comment', 'bpm', 'key')";

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
    let base = "SELECT t.id, f.path, t.duration_s, t.bpm, g.source, t.rating, t.color
         FROM tracks t
         JOIN files f ON f.id = (SELECT id FROM files
                                 WHERE track_id = t.id AND missing_since IS NULL
                                 ORDER BY id LIMIT 1)
         LEFT JOIN beatgrids g ON g.id = t.active_grid_id
         WHERE t.deleted_at IS NULL";
    type Row = (
        i64,
        Vec<u8>,
        Option<f64>,
        Option<f64>,
        Option<String>,
        Option<i64>,
        Option<i64>,
    );
    let map = |r: &rusqlite::Row| -> rusqlite::Result<Row> {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
            r.get(6)?,
        ))
    };
    let mut rows: Vec<Row> = vec![];
    // Metadata and features: (track, name) → (rank, value)
    let mut meta: HashMap<(i64, String), (u8, String)> = HashMap::new();
    let mut feats: HashMap<(i64, String), f64> = HashMap::new();
    let mut tags: HashMap<i64, Vec<String>> = HashMap::new();
    let tag_sql = "SELECT tt.track_id, g.name FROM track_tags tt JOIN tags g ON g.id = tt.tag_id";
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
            let mut st = c.prepare(tag_sql)?;
            let mut q = st.query([])?;
            while let Some(r) = q.next()? {
                tags.entry(r.get(0)?).or_default().push(r.get(1)?);
            }
        }
        Some(ids) => {
            let mut st = c.prepare_cached(&format!("{base} AND t.id = ?1"))?;
            let mut st_meta = c.prepare_cached(&format!("{meta_sql} AND track_id = ?1"))?;
            let mut st_feat = c.prepare_cached(&format!("{feat_sql} AND track_id = ?1"))?;
            let mut st_tag = c.prepare_cached(&format!("{tag_sql} WHERE tt.track_id = ?1"))?;
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
                let mut q = st_tag.query([id])?;
                while let Some(r) = q.next()? {
                    tags.entry(r.get(0)?).or_default().push(r.get(1)?);
                }
            }
        }
    }
    Ok(rows
        .into_iter()
        .map(|(id, path, duration, bpm, source, rating, colour)| {
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
                album: take("album").unwrap_or_default(),
                genre: take("genre").unwrap_or_default(),
                comment: take("comment").unwrap_or_default(),
                rating: rating.and_then(|r| u8::try_from(r).ok()),
                colour: colour.and_then(|c| u32::try_from(c).ok()),
                tags: {
                    let mut t = tags.remove(&id).unwrap_or_default();
                    t.sort();
                    t
                },
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

/// Where analysis found the track's beats start (s), if it has (grid analyser 3 on).
pub fn first_beat(c: &Connection, id: i64) -> Result<Option<f64>> {
    feature(c, id, "first_beat")
}

pub fn loudness(c: &Connection, id: i64) -> Result<Option<Loudness>> {
    Ok(match (feature(c, id, "lufs")?, feature(c, id, "peak")?) {
        (Some(lufs), Some(peak)) => Some(Loudness { lufs, peak }),
        _ => None,
    })
}

// ---- cues --------------------------------------------------------------------------

/// A memory cue (a point) or memory loop (with an end), as Rekordbox has them.
#[derive(Clone, Debug, PartialEq)]
pub struct Memory {
    pub id: i64,
    pub start: f64,
    pub end: Option<f64>,
    pub label: Option<String>,
    /// 0xRRGGBB; `None` shows the default for a cue or a loop.
    pub color: Option<u32>,
}

/// What the UI shows of a track's cues besides the hot cue times (those go to the
/// engine on load): the hot cue labels by pad, and the memory cues and loops.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrackCues {
    pub hot_labels: [Option<String>; HOTCUES],
    pub memories: Vec<Memory>,
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

/// A track's hot cue labels, by pad.
pub fn hot_labels(c: &Connection, track: i64) -> Result<[Option<String>; HOTCUES]> {
    let mut out: [Option<String>; HOTCUES] = Default::default();
    let mut st = c.prepare_cached(
        "SELECT slot, label FROM cues WHERE track_id = ?1 AND kind = ?2 AND label IS NOT NULL",
    )?;
    let rows = st.query_map(params![track, CueKind::Hot.as_i64()], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (slot, label) = row?;
        if let Some(h) = usize::try_from(slot).ok().and_then(|s| out.get_mut(s)) {
            *h = Some(label);
        }
    }
    Ok(out)
}

/// A track's memory cues and loops, in order through the track.
pub fn memories(c: &Connection, track: i64) -> Result<Vec<Memory>> {
    let mut st = c.prepare_cached(
        "SELECT id, start_s, end_s, label, color FROM cues
         WHERE track_id = ?1 AND kind IN (?2, ?3)
         ORDER BY start_s, end_s",
    )?;
    let rows = st.query_map(
        params![track, CueKind::Memory.as_i64(), CueKind::Loop.as_i64()],
        |r| {
            Ok(Memory {
                id: r.get(0)?,
                start: r.get(1)?,
                end: r.get(2)?,
                label: r.get(3)?,
                color: r.get(4)?,
            })
        },
    )?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn cues(c: &Connection, track: i64) -> Result<TrackCues> {
    Ok(TrackCues {
        hot_labels: hot_labels(c, track)?,
        memories: memories(c, track)?,
    })
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

// ---- collections ----------------------------------------------------------------------

/// Every collection, sorted by name, with its tracks (see [`Collection::tracks`]).
pub fn collections(c: &Connection) -> Result<Vec<Collection>> {
    // All members at once, rather than a query per collection.
    let mut tracks: HashMap<i64, Vec<i64>> = HashMap::new();
    for sql in [
        "SELECT collection_id, track_id FROM crate_tracks ORDER BY collection_id, added_at, track_id",
        "SELECT collection_id, track_id FROM playlist_entries ORDER BY collection_id, position",
        "SELECT collection_id, track_id FROM flow_nodes ORDER BY collection_id, id",
    ] {
        let mut st = c.prepare(sql)?;
        let mut q = st.query([])?;
        while let Some(r) = q.next()? {
            tracks.entry(r.get(0)?).or_default().push(r.get(1)?);
        }
    }
    let mut graphs: HashMap<i64, (Vec<FlowNode>, Vec<(NodeId, NodeId)>)> = HashMap::new();
    let mut st = c.prepare("SELECT collection_id, id, track_id FROM flow_nodes")?;
    let mut q = st.query([])?;
    while let Some(r) = q.next()? {
        graphs.entry(r.get(0)?).or_default().0.push(FlowNode {
            id: r.get::<_, i64>(1)? as NodeId,
            track: r.get(2)?,
            x: 0.0,
            y: 0.0,
        });
    }
    let mut st = c.prepare("SELECT collection_id, from_node, to_node FROM flow_edges")?;
    let mut q = st.query([])?;
    while let Some(r) = q.next()? {
        graphs
            .entry(r.get(0)?)
            .or_default()
            .1
            .push((r.get::<_, i64>(1)? as NodeId, r.get::<_, i64>(2)? as NodeId));
    }
    let mut st = c.prepare(
        "SELECT id, parent_id, kind, name, query, source_flow_id FROM collections
         ORDER BY name COLLATE NOCASE, id",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get(1)?,
                r.get::<_, String>(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, parent, kind, name, query, source_flow)| {
            let kind = CollectionKind::parse(&kind)?;
            let linear = kind == CollectionKind::Flow
                && graphs
                    .remove(&id)
                    .is_none_or(|(n, e)| FlowGraph::new(n, e).linear_order().is_some());
            Some(Collection {
                id,
                parent,
                kind,
                name,
                tracks: tracks.remove(&id).unwrap_or_default(),
                query,
                source_flow,
                linear,
            })
        })
        .collect())
}

pub fn kind(c: &Connection, id: i64) -> Result<Option<CollectionKind>> {
    let k: Option<String> = c
        .query_row("SELECT kind FROM collections WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(k.as_deref().and_then(CollectionKind::parse))
}

pub fn flow_graph(c: &Connection, id: i64) -> Result<FlowGraph> {
    let mut st = c.prepare_cached(
        "SELECT id, track_id, x, y FROM flow_nodes WHERE collection_id = ?1 ORDER BY id",
    )?;
    let nodes = st
        .query_map([id], |r| {
            Ok(FlowNode {
                id: r.get::<_, i64>(0)? as NodeId,
                track: r.get(1)?,
                x: r.get::<_, f64>(2)? as f32,
                y: r.get::<_, f64>(3)? as f32,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut st = c.prepare_cached(
        "SELECT from_node, to_node FROM flow_edges WHERE collection_id = ?1
         ORDER BY from_node, to_node",
    )?;
    let edges = st
        .query_map([id], |r| {
            Ok((r.get::<_, i64>(0)? as NodeId, r.get::<_, i64>(1)? as NodeId))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(FlowGraph::new(nodes, edges))
}

/// A Flow's name and graph, or `None` if there's no such Flow.
pub fn flow(c: &Connection, id: i64) -> Result<Option<(i64, String, FlowGraph)>> {
    let name: Option<String> = c
        .query_row(
            "SELECT name FROM collections WHERE id = ?1 AND kind = 'flow'",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(name) = name else { return Ok(None) };
    Ok(Some((id, name, flow_graph(c, id)?)))
}

/// The tracks of any collection: a crate's, a playlist's in order, a Flow's cards'
/// (in card order), or what a smart playlist's query finds (in the database, by
/// `query::to_sql`).
pub fn collection_tracks(c: &Connection, id: i64) -> Result<Vec<i64>> {
    let row: Option<(String, Option<String>)> = c
        .query_row(
            "SELECT kind, query FROM collections WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((kind, q)) = row else {
        return Err(StoreError::NoCollection(id));
    };
    let sql = match CollectionKind::parse(&kind) {
        Some(CollectionKind::Smart) => {
            let q = query::parse(q.as_deref().unwrap_or(""))
                .map_err(|e| StoreError::BadQuery(e.to_string()))?;
            let (sql, args) = query::to_sql(&q);
            let mut st = c.prepare(&sql)?;
            return Ok(st
                .query_map(rusqlite::params_from_iter(args), |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?);
        }
        Some(CollectionKind::Crate) => {
            "SELECT track_id FROM crate_tracks WHERE collection_id = ?1 ORDER BY added_at, track_id"
        }
        Some(CollectionKind::Playlist) => {
            "SELECT track_id FROM playlist_entries WHERE collection_id = ?1 ORDER BY position"
        }
        Some(CollectionKind::Flow) => {
            "SELECT track_id FROM flow_nodes WHERE collection_id = ?1 ORDER BY id"
        }
        _ => return Ok(vec![]),
    };
    let mut st = c.prepare_cached(sql)?;
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

// ---- analysis results and jobs ---------------------------------------------------------

/// A saved analyser result, as far as staleness goes.
#[derive(Clone, Debug, PartialEq)]
pub struct ResultState {
    pub analyser: String,
    pub version: u32,
    pub failed: bool,
    /// Made from the audio the track has now (or one side's hash isn't known yet).
    pub same_audio: bool,
}

/// `r` (an `analysis_results` row) was made from the audio its track has now, as far
/// as anyone knows: a hash missing on either side counts as the same.
const SAME_AUDIO: &str = "(r.content_hash IS NULL
   OR NOT EXISTS (SELECT 1 FROM files f WHERE f.track_id = r.track_id
                  AND f.missing_since IS NULL AND f.content_hash IS NOT NULL)
   OR EXISTS (SELECT 1 FROM files f WHERE f.track_id = r.track_id
              AND f.missing_since IS NULL AND f.content_hash = r.content_hash))";

pub fn analysis_results(c: &Connection, track: i64) -> Result<Vec<ResultState>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT r.analyser, r.version, r.error IS NOT NULL, {SAME_AUDIO}
         FROM analysis_results r WHERE r.track_id = ?1"
    ))?;
    Ok(st
        .query_map([track], |r| {
            Ok(ResultState {
                analyser: r.get(0)?,
                version: r.get(1)?,
                failed: r.get(2)?,
                same_audio: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// Present tracks whose result from `analyser` is missing, failed, older than
/// `version`, or from other audio.
pub fn stale_tracks(c: &Connection, analyser: &str, version: u32) -> Result<Vec<i64>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT t.id FROM tracks t
         WHERE t.deleted_at IS NULL
           AND EXISTS (SELECT 1 FROM files f WHERE f.track_id = t.id AND f.missing_since IS NULL)
           AND NOT EXISTS (SELECT 1 FROM analysis_results r
                           WHERE r.track_id = t.id AND r.analyser = ?1 AND r.version >= ?2
                             AND r.error IS NULL AND {SAME_AUDIO})
         ORDER BY t.id"
    ))?;
    Ok(st
        .query_map(params![analyser, version], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// One queued analysis job (a `jobs` row).
#[derive(Clone, Debug, PartialEq)]
pub struct JobRow {
    pub track: i64,
    /// The analyser's name (the row's kind is [`JOB_PREFIX`] + it).
    pub analyser: String,
    /// missing | stale | force
    pub want: String,
    /// The grid mode.
    pub params: Option<String>,
}

pub const JOB_PREFIX: &str = "analyse:";

/// The analysis queue as it was saved, oldest first.
pub fn queued_jobs(c: &Connection) -> Result<Vec<JobRow>> {
    let mut st = c.prepare(
        "SELECT track_id, substr(kind, length(?1) + 1), coalesce(want, 'missing'), params
         FROM jobs WHERE kind LIKE ?1 || '%' AND track_id IS NOT NULL ORDER BY id",
    )?;
    Ok(st
        .query_map([JOB_PREFIX], |r| {
            Ok(JobRow {
                track: r.get(0)?,
                analyser: r.get(1)?,
                want: r.get(2)?,
                params: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn setting(c: &Connection, key: &str) -> Result<Option<String>> {
    Ok(c
        .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
            r.get(0)
        })
        .optional()?)
}
