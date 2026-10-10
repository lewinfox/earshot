//! Library folders and the audio files in them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use earshot_engine::decode;
use tauri::State;

use crate::analysis::{Analyser, Mode};
use crate::db::{self, Db, LibTrack, TrackRow};

pub struct Library {
    pub db: Db,
    pub analyser: Analyser,
}

const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "wav", "aif", "aiff", "m4a", "mp4", "aac", "ogg",
];

fn is_audio(p: &Path) -> bool {
    p.extension()
        .and_then(|x| x.to_str())
        .is_some_and(|x| AUDIO_EXTENSIONS.contains(&x.to_lowercase().as_str()))
}

fn is_hidden(p: &Path) -> bool {
    p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'))
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if is_hidden(&p) {
            continue;
        }
        if p.is_dir() {
            walk(&p, out);
        } else if is_audio(&p) {
            out.push(p);
        }
    }
}

#[tauri::command]
pub fn library_folders(lib: State<'_, Library>) -> Result<Vec<String>, String> {
    db::folders(&lib.db.lock().unwrap())
}

#[tauri::command]
pub fn add_library_folder(lib: State<'_, Library>, path: String) -> Result<Vec<String>, String> {
    if !Path::new(&path).is_dir() {
        return Err(format!("{path} is not a folder"));
    }
    let conn = lib.db.lock().unwrap();
    db::add_folder(&conn, path.trim_end_matches('/'))?;
    db::folders(&conn)
}

#[tauri::command]
pub fn remove_library_folder(lib: State<'_, Library>, path: String) -> Result<Vec<String>, String> {
    let conn = lib.db.lock().unwrap();
    db::remove_folder(&conn, &path)?;
    db::folders(&conn)
}

#[derive(serde::Serialize)]
pub struct Subdir {
    name: String,
    path: String,
    /// False when the folder has no subfolders, so the tree needn't show an arrow.
    has_children: bool,
}

/// Immediate subfolders of `path`, for the folder tree.
#[tauri::command]
pub fn list_subdirs(path: String) -> Result<Vec<Subdir>, String> {
    let entries = std::fs::read_dir(&path).map_err(|e| format!("{path}: {e}"))?;
    let mut out: Vec<Subdir> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && !is_hidden(p))
        .map(|p| Subdir {
            name: p.file_name().unwrap_or_default().to_string_lossy().into_owned(),
            has_children: std::fs::read_dir(&p)
                .map(|mut r| r.any(|e| e.is_ok_and(|e| e.path().is_dir() && !is_hidden(&e.path()))))
                .unwrap_or(false),
            path: p.to_string_lossy().into_owned(),
        })
        .collect();
    out.sort_by_key(|s| s.name.to_lowercase());
    Ok(out)
}

/// Find every audio file under the library folders. Tags are only re-read for files
/// that are new or whose size or modified time changed, so a rescan is quick.
#[tauri::command]
pub async fn scan_library(lib: State<'_, Library>) -> Result<Vec<LibTrack>, String> {
    let db = lib.db.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (folders, known) = {
            let conn = db.lock().unwrap();
            (db::folders(&conn)?, db::known_tracks(&conn)?)
        };
        let known: HashMap<String, db::Known> = known.into_iter().collect();
        let mut files = Vec::new();
        for f in &folders {
            walk(Path::new(f), &mut files);
        }
        files.sort();
        files.dedup();

        let mut found = std::collections::HashSet::new();
        let mut changed = Vec::new();
        for p in files {
            let Ok(meta) = std::fs::metadata(&p) else { continue };
            let size = meta.len() as i64;
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs() as i64);
            let path = p.to_string_lossy().into_owned();
            found.insert(path.clone());
            if !known.get(&path).is_some_and(|k| k.size == size && k.mtime == mtime) {
                changed.push((p, size, mtime));
            }
        }

        // Reading tags is mostly file I/O; spread it over a few threads.
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
        let chunk = changed.len().div_ceil(threads).max(1);
        let rows: Vec<TrackRow> = std::thread::scope(|s| {
            let handles: Vec<_> = changed
                .chunks(chunk)
                .map(|c| {
                    s.spawn(move || {
                        c.iter()
                            .map(|(p, size, mtime)| {
                                let tags = decode::read_tags(p).unwrap_or_default();
                                TrackRow {
                                    path: p.to_string_lossy().into_owned(),
                                    size: *size,
                                    mtime: *mtime,
                                    duration: tags.duration,
                                    title: tags.title,
                                    artist: tags.artist,
                                    bpm: tags.bpm,
                                    key: tags.key,
                                }
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles.into_iter().flat_map(|h| h.join().unwrap()).collect()
        });

        let mut conn = db.lock().unwrap();
        db::upsert_tracks(&mut conn, &rows)?;
        // Tracks stay in the database when their folder is removed (keeping their
        // analysis), but only files under a current library folder are listed.
        Ok(db::all_tracks(&conn)?
            .into_iter()
            .filter(|t| found.contains(&t.path))
            .collect())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Waveform thumbnails, packed for the UI: per track, its id (i64, little-endian) then
/// `PREVIEW_BINS` × 4 bytes (peak, low, mid, high).
#[tauri::command]
pub async fn waveform_previews(
    lib: State<'_, Library>,
    ids: Option<Vec<i64>>,
) -> Result<tauri::ipc::Response, String> {
    let db = lib.db.clone();
    let rows = tauri::async_runtime::spawn_blocking(move || db::previews(&db.lock().unwrap(), ids.as_deref()))
        .await
        .map_err(|e| e.to_string())??;
    let mut out = Vec::with_capacity(rows.len() * (8 + earshot_engine::waveform::PREVIEW_BINS * 4));
    for (id, p) in rows {
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&p);
    }
    Ok(tauri::ipc::Response::new(out))
}

/// Queue tracks for analysis (right-click → Analyse). Re-analyses even if a grid exists.
/// `mode`: "static" (default) or "dynamic".
#[tauri::command]
pub fn analyse_tracks(lib: State<'_, Library>, ids: Vec<i64>, mode: Option<Mode>) {
    for id in ids {
        lib.analyser.queue(id, None, true, mode.unwrap_or(Mode::Static));
    }
}
