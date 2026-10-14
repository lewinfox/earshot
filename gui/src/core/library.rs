//! Library folders and the audio files in them.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use earshot_engine::decode;
use earshot_engine::media::Location;

use super::analysis::Analyser;
use super::hash;
use super::store::{Reader, ScannedFile, Store, StoreCmd, StoreError, path_bytes};
use super::{Event, Sink};

pub struct Library {
    pub store: Store,
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
    p.file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with('.'))
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

#[derive(Clone)]
pub struct Subdir {
    pub name: String,
    pub path: String,
    /// False when the folder has no subfolders, so the tree needn't show an arrow.
    pub has_children: bool,
}

/// Immediate subfolders of `path`, for the folder tree.
/// Show a file in the system's file manager: selected in its folder where the
/// platform can, else the folder opened. Doesn't wait for it.
pub fn reveal(path: &Path) -> Result<(), String> {
    let mut cmd;
    #[cfg(target_os = "macos")]
    {
        cmd = std::process::Command::new("open");
        cmd.arg("-R").arg(path);
    }
    #[cfg(target_os = "windows")]
    {
        cmd = std::process::Command::new("explorer");
        cmd.arg(format!("/select,{}", path.display()));
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        // The freedesktop file manager interface selects the file; xdg-open on its
        // folder works everywhere else.
        let uri = format!("file://{}", path.display());
        let shown = std::process::Command::new("dbus-send")
            .args([
                "--session",
                "--dest=org.freedesktop.FileManager1",
                "--type=method_call",
                "/org/freedesktop/FileManager1",
                "org.freedesktop.FileManager1.ShowItems",
            ])
            .arg(format!("array:string:{uri}"))
            .arg("string:")
            .status()
            .is_ok_and(|s| s.success());
        if shown {
            return Ok(());
        }
        cmd = std::process::Command::new("xdg-open");
        cmd.arg(path.parent().unwrap_or(path));
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("opening the file manager: {e}"))?;
    // Reaped on a thread, so it doesn't linger as a zombie.
    std::thread::spawn(move || child.wait());
    Ok(())
}

pub fn list_subdirs(path: &str) -> Result<Vec<Subdir>, String> {
    let entries = std::fs::read_dir(path).map_err(|e| format!("{path}: {e}"))?;
    let mut out: Vec<Subdir> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && !is_hidden(p))
        .map(|p| Subdir {
            name: p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            has_children: std::fs::read_dir(&p)
                .map(|mut r| r.any(|e| e.is_ok_and(|e| e.path().is_dir() && !is_hidden(&e.path()))))
                .unwrap_or(false),
            path: p.to_string_lossy().into_owned(),
        })
        .collect();
    out.sort_by_key(|s| s.name.to_lowercase());
    Ok(out)
}

/// Find every audio file under the library folders and record them in the Store,
/// which then sends the library as a `StoreEvent::Library`. Tags (and a stamped
/// audio hash) are only read for files that are new or whose size or modified time
/// changed, so a rescan is quick; files without a stamped hash are hashed later by
/// [`start_hasher`]. Blocking: call from a background thread.
pub fn scan_library(store: &Store) -> Result<(), StoreError> {
    // Through the writer, so a folder added just before is included.
    let roots = store.call(|w| w.folders())?;
    let known = store.reader.known_files()?;
    let mut files = Vec::new();
    for r in &roots {
        walk(r, &mut files);
    }
    files.sort();
    files.dedup();

    let mut changed = Vec::new();
    for p in &files {
        let Ok(meta) = std::fs::metadata(p) else {
            continue;
        };
        let (size, mtime_ns) = stamp(&meta);
        if known.get(&path_bytes(p)) != Some(&(size, mtime_ns)) {
            changed.push((p.clone(), size, mtime_ns));
        }
    }

    let rows = read_tags(&changed);
    store.call(move |w| w.apply_scan(&roots, &files, &rows))
}

/// The files among `paths` that are new to the library or changed since it last
/// looked, with their tags, ready for the Store. Files that can't be read are left
/// out. Blocking: call from a background thread.
pub fn changed_files(store: &Store, paths: &[PathBuf]) -> Result<Vec<ScannedFile>, StoreError> {
    let known = store.reader.known_files()?;
    let changed: Vec<_> = paths
        .iter()
        .filter_map(|p| {
            let (size, mtime_ns) = stamp(&std::fs::metadata(p).ok()?);
            (known.get(&path_bytes(p)) != Some(&(size, mtime_ns)))
                .then(|| (p.clone(), size, mtime_ns))
        })
        .collect();
    Ok(read_tags(&changed))
}

/// Read the tags of (path, size, modified time) files.
fn read_tags(changed: &[(PathBuf, i64, i64)]) -> Vec<ScannedFile> {
    // Reading tags is mostly file I/O; spread it over a few threads.
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let chunk = changed.len().div_ceil(threads).max(1);
    std::thread::scope(|s| {
        let handles: Vec<_> = changed
            .chunks(chunk)
            .map(|c| {
                s.spawn(move || {
                    c.iter()
                        .map(|(p, size, mtime_ns)| scanned(p, *size, *mtime_ns))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    })
}

/// Record one file, new or changed, without walking the library folders: for a file
/// just put in one (the tempo lock tool's "Save to library"). The Store then sends
/// the library as a `StoreEvent::Library`. Blocking: call from a background thread.
pub fn scan_file(store: &Store, path: &Path) -> Result<(), StoreError> {
    let meta = std::fs::metadata(path).map_err(|source| StoreError::Io {
        path: path.into(),
        source,
    })?;
    let (size, mtime_ns) = stamp(&meta);
    let row = scanned(path, size, mtime_ns);
    let found = vec![path.to_path_buf()];
    // No roots: nothing else is looked at, so nothing is marked missing.
    store.call(move |w| w.apply_scan(&[], &found, &[row]))
}

/// (size, modified time in ns): how a scan tells a file has changed.
fn stamp(meta: &std::fs::Metadata) -> (i64, i64) {
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as i64);
    (meta.len() as i64, mtime_ns)
}

fn scanned(path: &Path, size: i64, mtime_ns: i64) -> ScannedFile {
    ScannedFile {
        tags: decode::read_tags(&path.into()).unwrap_or_default(),
        tag_hash: hash::tag_hash(&path.into()),
        path: path.to_path_buf(),
        size,
        mtime_ns,
    }
}

/// A background thread that works out the audio hash of files that don't have one
/// stamped in their tags, one at a time at idle priority, and gives each to the
/// Store (which matches moved files up with their tracks). Nudge it after a scan.
/// Its queue is just "files with no hash", so it picks up where it left off.
pub fn start_hasher(store: Store) -> std::sync::mpsc::Sender<()> {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    std::thread::Builder::new()
        .name("earshot-hash".into())
        .spawn(move || {
            idle_priority();
            while rx.recv().is_ok() {
                while rx.try_recv().is_ok() {} // one pass covers every nudge so far
                let files = match store.reader.files_without_hash() {
                    Ok(f) => f,
                    Err(e) => {
                        eprintln!("hashing: {e}");
                        continue;
                    }
                };
                for (file, path) in files {
                    match hash::content_hash(&path.as_path().into()) {
                        Ok(hash) => store.send(StoreCmd::SetContentHash { file, hash }),
                        Err(e) => eprintln!("hashing {}: {e}", path.display()),
                    }
                }
            }
        })
        .expect("hash thread");
    tx
}

/// Linux's idle CPU and disk classes for the calling thread: hashing reads whole
/// files, and mustn't get in the way of playback.
fn idle_priority() {
    #[cfg(target_os = "linux")]
    // SAFETY: plain syscalls on the calling thread (pid/who 0) with valid args.
    unsafe {
        let param = libc::sched_param { sched_priority: 0 };
        if libc::sched_setscheduler(0, libc::SCHED_IDLE, &param) != 0 {
            libc::setpriority(libc::PRIO_PROCESS, 0, 19);
        }
        // ioprio_set(IOPRIO_WHO_PROCESS, 0, IOPRIO_CLASS_IDLE << 13)
        libc::syscall(libc::SYS_ioprio_set, 1, 0, 3 << 13);
    }
}

/// Cover art is scaled to this many pixels square.
pub const ART_SIZE: u32 = 96;

/// A background thread that reads tracks' embedded covers, one at a time, and sends
/// each as an `Event::Art` (`None` when the file has none). Send it track ids.
pub fn start_art_loader(reader: Reader, sink: Sink) -> std::sync::mpsc::Sender<i64> {
    let (tx, rx) = std::sync::mpsc::channel::<i64>();
    std::thread::Builder::new()
        .name("art".into())
        .spawn(move || {
            for id in rx {
                let art = reader.track_location(id).ok().and_then(|at| cover(&at));
                sink(Event::Art(id, art));
            }
        })
        .expect("art thread");
    tx
}

/// The file's cover, cropped to a square and scaled down.
fn cover(at: &Location) -> Option<Vec<u8>> {
    let bytes = decode::read_cover(at).ok()??;
    let img = image::load_from_memory(&bytes).ok()?;
    let side = img.width().min(img.height());
    let img = img.crop_imm(
        (img.width() - side) / 2,
        (img.height() - side) / 2,
        side,
        side,
    );
    Some(
        img.thumbnail_exact(ART_SIZE, ART_SIZE)
            .to_rgba8()
            .into_raw(),
    )
}
