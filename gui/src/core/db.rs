//! `earshot.db`: library folders, tracks and analysis results (SQLite).
//!
//! One connection behind a mutex, so the app is the only writer. The audio thread
//! never touches it.

use std::path::Path;
use std::sync::{Arc, Mutex};

use earshot_engine::beatgrid::Beatgrid;
use earshot_engine::loudness::Loudness;
use rusqlite::{Connection, OptionalExtension, params};

pub type Db = Arc<Mutex<Connection>>;

/// Bump when `engine/src/waveform.rs` changes its output, so cached waveforms are redone.
pub const WAVEFORM_VERSION: i64 = 1;

const MIGRATIONS: &[&str] = &[
    include_str!("../../migrations/001_library.sql"),
    include_str!("../../migrations/002_energy.sql"),
    include_str!("../../migrations/003_waveform_preview.sql"),
    include_str!("../../migrations/004_analysis_mode.sql"),
    include_str!("../../migrations/005_flows.sql"),
    include_str!("../../migrations/006_loudness.sql"),
];

pub fn open(path: &Path) -> Result<Db, String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut conn = Connection::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| e.to_string())?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|e| e.to_string())?;
    migrate(&mut conn)?;
    Ok(Arc::new(Mutex::new(conn)))
}

/// Apply migrations in order; `user_version` records how many have run.
fn migrate(conn: &mut Connection) -> Result<(), String> {
    let done: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(|e| e.to_string())?;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(done as usize) {
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        tx.execute_batch(sql)
            .map_err(|e| format!("migration {}: {e}", i + 1))?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn err(e: rusqlite::Error) -> String {
    e.to_string()
}

// ---- folders -----------------------------------------------------------------------

pub fn folders(conn: &Connection) -> Result<Vec<String>, String> {
    let mut st = conn
        .prepare("SELECT path FROM library_folders ORDER BY path")
        .map_err(err)?;
    let rows = st.query_map([], |r| r.get(0)).map_err(err)?;
    rows.collect::<Result<_, _>>().map_err(err)
}

pub fn add_folder(conn: &Connection, path: &str) -> Result<(), String> {
    conn.execute(
        "INSERT OR IGNORE INTO library_folders(path) VALUES (?1)",
        [path],
    )
    .map_err(err)?;
    Ok(())
}

pub fn remove_folder(conn: &Connection, path: &str) -> Result<(), String> {
    conn.execute("DELETE FROM library_folders WHERE path = ?1", [path])
        .map_err(err)?;
    Ok(())
}

// ---- tracks ------------------------------------------------------------------------

pub struct Known {
    pub size: i64,
    pub mtime: i64,
}

pub fn known_tracks(conn: &Connection) -> Result<Vec<(String, Known)>, String> {
    let mut st = conn
        .prepare("SELECT path, size, mtime FROM tracks")
        .map_err(err)?;
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                Known {
                    size: r.get(1)?,
                    mtime: r.get(2)?,
                },
            ))
        })
        .map_err(err)?;
    rows.collect::<Result<_, _>>().map_err(err)
}

pub struct TrackRow {
    pub path: String,
    pub size: i64,
    pub mtime: i64,
    pub duration: Option<f64>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub bpm: Option<f64>,
    pub key: Option<String>,
}

/// Insert new tracks or refresh the tags of changed ones, in one transaction.
pub fn upsert_tracks(conn: &mut Connection, rows: &[TrackRow]) -> Result<(), String> {
    let tx = conn.transaction().map_err(err)?;
    {
        let mut st = tx
            .prepare(
                "INSERT INTO tracks(path, size, mtime, duration_s, title, artist, tag_bpm, tag_key)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(path) DO UPDATE SET size = excluded.size, mtime = excluded.mtime,
                   duration_s = excluded.duration_s, title = excluded.title, artist = excluded.artist,
                   tag_bpm = excluded.tag_bpm, tag_key = excluded.tag_key",
            )
            .map_err(err)?;
        for r in rows {
            st.execute(params![
                r.path, r.size, r.mtime, r.duration, r.title, r.artist, r.bpm, r.key
            ])
            .map_err(err)?;
        }
    }
    tx.commit().map_err(err)
}

/// A library track as the UI sees it.
#[derive(Clone, Debug)]
pub struct LibTrack {
    pub id: i64,
    pub path: String,
    pub title: String,
    pub artist: String,
    pub duration: Option<f64>,
    /// BPM from analysis, or `None` if not analysed yet.
    pub bpm: Option<f64>,
    pub tag_bpm: Option<f64>,
    pub key: Option<String>,
    pub grid_source: Option<String>,
    /// Stand-in energy (engine/src/energy.rs); only its order means anything.
    pub energy: Option<f64>,
    /// A static analysis found the tempo seems to drift.
    pub drifts: bool,
}

pub fn all_tracks(conn: &Connection) -> Result<Vec<LibTrack>, String> {
    let mut st = conn
        .prepare(
            "SELECT t.id, t.path, t.title, t.artist, t.duration_s, a.bpm, t.tag_bpm, t.tag_key, g.source, t.energy, a.drifts
             FROM tracks t
             LEFT JOIN analysis a ON a.track_id = t.id
             LEFT JOIN beatgrids g ON g.track_id = t.id
             ORDER BY t.path",
        )
        .map_err(err)?;
    let rows = st
        .query_map([], |r| {
            let path: String = r.get(1)?;
            let stem = Path::new(&path)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            Ok(LibTrack {
                id: r.get(0)?,
                title: r
                    .get::<_, Option<String>>(2)?
                    .filter(|s| !s.is_empty())
                    .unwrap_or(stem),
                artist: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                duration: r.get(4)?,
                bpm: r.get(5)?,
                tag_bpm: r.get(6)?,
                key: r.get(7)?,
                grid_source: r.get(8)?,
                energy: r.get(9)?,
                drifts: r.get::<_, Option<bool>>(10)?.unwrap_or(false),
                path,
            })
        })
        .map_err(err)?;
    rows.collect::<Result<_, _>>().map_err(err)
}

pub fn track_path(conn: &Connection, id: i64) -> Result<String, String> {
    conn.query_row("SELECT path FROM tracks WHERE id = ?1", [id], |r| r.get(0))
        .optional()
        .map_err(err)?
        .ok_or_else(|| format!("no track {id}"))
}

// ---- analysis results ------------------------------------------------------------------

pub fn grid(conn: &Connection, id: i64) -> Result<Option<(Beatgrid, String)>, String> {
    let row: Option<(Vec<u8>, String)> = conn
        .query_row(
            "SELECT beats, source FROM beatgrids WHERE track_id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(err)?;
    match row {
        Some((bytes, source)) => Ok(Some((
            Beatgrid::from_bytes(&bytes).map_err(|e| e.to_string())?,
            source,
        ))),
        None => Ok(None),
    }
}

pub fn save_grid(conn: &Connection, id: i64, grid: &Beatgrid, source: &str) -> Result<(), String> {
    conn.execute(
        "INSERT INTO beatgrids(track_id, beats, source) VALUES (?1, ?2, ?3)
         ON CONFLICT(track_id) DO UPDATE SET beats = excluded.beats, source = excluded.source,
           updated_at = CURRENT_TIMESTAMP",
        params![id, grid.to_bytes(), source],
    )
    .map_err(err)?;
    Ok(())
}

pub fn save_analysis(
    conn: &Connection,
    id: i64,
    bpm: f64,
    min: f64,
    max: f64,
    detector: &str,
) -> Result<(), String> {
    conn.execute(
        "INSERT INTO analysis(track_id, bpm, bpm_min, bpm_max, detector) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(track_id) DO UPDATE SET bpm = excluded.bpm, bpm_min = excluded.bpm_min,
           bpm_max = excluded.bpm_max, detector = excluded.detector, analysed_at = CURRENT_TIMESTAMP",
        params![id, bpm, min, max, detector],
    )
    .map_err(err)?;
    Ok(())
}

pub fn set_mode(conn: &Connection, id: i64, dynamic: bool, drifts: bool) -> Result<(), String> {
    conn.execute(
        "UPDATE analysis SET mode = ?2, drifts = ?3 WHERE track_id = ?1",
        params![id, if dynamic { "dynamic" } else { "static" }, drifts],
    )
    .map_err(err)?;
    Ok(())
}

pub fn set_loudness(conn: &Connection, id: i64, l: &Loudness) -> Result<(), String> {
    if l.lufs.is_finite() {
        conn.execute(
            "UPDATE tracks SET loudness = ?2, peak = ?3 WHERE id = ?1",
            params![id, l.lufs, l.peak],
        )
        .map_err(err)?;
    }
    Ok(())
}

/// The track's measured loudness, if it has been measured.
pub fn loudness(conn: &Connection, id: i64) -> Result<Option<Loudness>, String> {
    let row: Option<(Option<f64>, Option<f64>)> = conn
        .query_row(
            "SELECT loudness, peak FROM tracks WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(err)?;
    Ok(match row {
        Some((Some(lufs), Some(peak))) => Some(Loudness { lufs, peak }),
        _ => None,
    })
}

pub fn set_energy(conn: &Connection, id: i64, energy: f64) -> Result<(), String> {
    if energy.is_finite() {
        conn.execute(
            "UPDATE tracks SET energy = ?2 WHERE id = ?1",
            params![id, energy],
        )
        .map_err(err)?;
    }
    Ok(())
}

pub fn set_bpm(conn: &Connection, id: i64, bpm: f64) -> Result<(), String> {
    conn.execute(
        "UPDATE analysis SET bpm = ?2 WHERE track_id = ?1",
        params![id, bpm],
    )
    .map_err(err)?;
    Ok(())
}

pub fn waveform(conn: &Connection, id: i64, bins_per_s: f64) -> Result<Option<Vec<u8>>, String> {
    conn.query_row(
        "SELECT data FROM waveforms WHERE track_id = ?1 AND version = ?2 AND bins_per_s = ?3",
        params![id, WAVEFORM_VERSION, bins_per_s],
        |r| r.get(0),
    )
    .optional()
    .map_err(err)
}

pub fn save_waveform(
    conn: &Connection,
    id: i64,
    data: &[u8],
    bins_per_s: f64,
) -> Result<(), String> {
    conn.execute(
        "INSERT INTO waveforms(track_id, data, bins_per_s, version, preview) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(track_id) DO UPDATE SET data = excluded.data, bins_per_s = excluded.bins_per_s,
           version = excluded.version, preview = excluded.preview",
        params![id, data, bins_per_s, WAVEFORM_VERSION, earshot_engine::waveform::preview(data)],
    )
    .map_err(err)?;
    Ok(())
}

/// Waveform thumbnails for the track list, for the given tracks or all of them.
/// Waveforms saved before thumbnails existed get one made (and saved) here, once.
pub fn previews(conn: &Connection, ids: Option<&[i64]>) -> Result<Vec<(i64, Vec<u8>)>, String> {
    let mut st = conn
        .prepare("SELECT track_id, preview FROM waveforms")
        .map_err(err)?;
    let rows: Vec<(i64, Option<Vec<u8>>)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(err)?
        .collect::<Result<_, _>>()
        .map_err(err)?;
    let wanted: Option<std::collections::HashSet<i64>> = ids.map(|i| i.iter().copied().collect());
    let mut out = Vec::new();
    for (id, preview) in rows {
        if wanted.as_ref().is_some_and(|w| !w.contains(&id)) {
            continue;
        }
        let preview = match preview {
            Some(p) => p,
            None => {
                let data: Vec<u8> = conn
                    .query_row(
                        "SELECT data FROM waveforms WHERE track_id = ?1",
                        [id],
                        |r| r.get(0),
                    )
                    .map_err(err)?;
                let p = earshot_engine::waveform::preview(&data);
                conn.execute(
                    "UPDATE waveforms SET preview = ?2 WHERE track_id = ?1",
                    params![id, p],
                )
                .map_err(err)?;
                p
            }
        };
        out.push((id, preview));
    }
    Ok(out)
}
