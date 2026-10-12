//! The library database (`library.db`, SQLite) and the one thread that writes to it.
//!
//! - [`Store`] owns the only writing connection, on its own thread. Others send it
//!   [`StoreCmd`]s (the UI: never waits) or [`Store::call`] it (background threads
//!   that need the answer). What changed comes back as [`StoreEvent`]s.
//! - [`Reader`] is a small pool of read-only connections for loaders and other
//!   background threads. WAL mode lets them read while the writer writes.
//! - The UI thread never runs SQL.
//!
//! The schema is `gui/migrations/001_baseline.sql`.

mod read;
mod write;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use earshot_engine::HOTCUES;
use rusqlite::{Connection, OpenFlags};

pub use read::{JobRow, LibTrack, Memory, Preview, ResultState};
pub use write::{AnalysisSave, GridSave, ScannedFile, Writer};

use super::flows::{Card, FlowEntry, Folder};

/// Bump when `engine/src/waveform.rs` changes its output, so cached waveforms are redone.
pub const WAVEFORM_VERSION: i64 = 1;

/// The file name in the data folder. Not `earshot.db`: the schema was rebuilt from
/// scratch, and an old database is left alone rather than migrated.
pub const FILE_NAME: &str = "library.db";

const MIGRATIONS: &[&str] = &[
    include_str!("../../../migrations/001_baseline.sql"),
    include_str!("../../../migrations/002_analysis_results.sql"),
];

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("migration {step}: {source}")]
    Migration {
        step: usize,
        source: rusqlite::Error,
    },
    #[error("the database is from a newer earshot (schema {found}, this one knows {known})")]
    TooNew { found: i64, known: usize },
    #[error("no track {0}")]
    NoTrack(i64),
    #[error("no Flow or folder {0}")]
    NoCollection(i64),
    #[error("track {track}'s beatgrid is unreadable: {reason}")]
    BadGrid { track: i64, reason: String },
    #[error("{0}")]
    Refused(&'static str),
    #[error("the database thread has stopped")]
    Closed,
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Where a beatgrid came from. Stored as TEXT (with a CHECK in the schema).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridSource {
    /// Static (one tempo) analysis.
    Analysis,
    /// Dynamic analysis: follows the drift.
    AnalysisDynamic,
    /// Fixed by hand in the grid editor.
    User,
    /// tempo-lock's `<file>.beats.json`.
    Sidecar,
    /// From another library (cratelib, Rekordbox …).
    Import,
}

impl GridSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            GridSource::Analysis => "analysis",
            GridSource::AnalysisDynamic => "analysis-dynamic",
            GridSource::User => "user",
            GridSource::Sidecar => "sidecar",
            GridSource::Import => "import",
        }
    }

    pub fn parse(s: &str) -> Option<GridSource> {
        Some(match s {
            "analysis" => GridSource::Analysis,
            "analysis-dynamic" => GridSource::AnalysisDynamic,
            "user" => GridSource::User,
            "sidecar" => GridSource::Sidecar,
            "import" => GridSource::Import,
            _ => return None,
        })
    }

    /// How the deck names it.
    pub const fn label(self) -> &'static str {
        match self {
            GridSource::Analysis => "analysis",
            GridSource::AnalysisDynamic => "analysis (dynamic)",
            GridSource::User => "user",
            GridSource::Sidecar => "tempo-lock sidecar",
            GridSource::Import => "import",
        }
    }
}

/// What a row in `cues` is. Stored as INTEGER.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CueKind {
    /// A hot cue: `slot` is the pad (0-7).
    Hot,
    /// A memory cue: a point, no slot.
    Memory,
    /// A memory loop: `end_s` is set, no slot.
    Loop,
}

impl CueKind {
    pub const fn as_i64(self) -> i64 {
        match self {
            CueKind::Hot => 0,
            CueKind::Memory => 1,
            CueKind::Loop => 2,
        }
    }
}

/// What the UI (or anything that doesn't need an answer) asks the Store to do.
/// Results and failures come back as [`StoreEvent`]s.
#[derive(Debug)]
pub enum StoreCmd {
    ListRoots,
    AddRoots(Vec<PathBuf>),
    RemoveRoot(String),
    /// A hand-edited grid: saved as a new grid row and made the active one.
    SaveUserGrid {
        track: i64,
        grid: Arc<earshot_engine::beatgrid::Beatgrid>,
    },
    /// A waveform worked out on load (the track list shows its thumbnail).
    SaveWaveform {
        track: i64,
        data: Vec<u8>,
    },
    /// Numbers measured outside analysis (loudness on load).
    SaveFeatures {
        track: i64,
        features: Vec<(&'static str, f64)>,
    },
    /// Replace what's in the analysis queue for a track (it survives a restart).
    SaveJobs {
        track: i64,
        jobs: Vec<JobRow>,
    },
    #[allow(dead_code)] // no settings screen yet (tests use it)
    SetSetting {
        key: String,
        value: String,
    },
    /// A field the user typed (`None` clears it, so the tag's value shows again).
    #[allow(dead_code)] // library management API, not wired to the UI yet (tests use it)
    SetMeta {
        track: i64,
        field: String,
        value: Option<String>,
    },
    /// The audio hash of a file, worked out in the background.
    SetContentHash {
        file: i64,
        hash: [u8; 32],
    },
    /// Replace a track's hot cues (pad → seconds).
    SaveHotcues {
        track: i64,
        cues: [Option<f64>; HOTCUES],
    },
    /// Save a memory cue (or loop, with `end`), unless the same one is already saved.
    /// Answered with [`StoreEvent::Memories`].
    AddMemory {
        track: i64,
        start: f64,
        end: Option<f64>,
    },
    /// Answered with [`StoreEvent::Memories`].
    DeleteMemory {
        track: i64,
        id: i64,
    },
    ListFlows,
    /// Answered with [`StoreEvent::FlowLoaded`].
    LoadFlow(i64),
    /// Ids come from [`Store::new_collection_id`], so the caller knows them at once.
    CreateFolder {
        id: i64,
        parent: Option<i64>,
        name: String,
    },
    CreateFlow {
        id: i64,
        folder: Option<i64>,
        name: String,
    },
    Rename {
        id: i64,
        name: String,
    },
    /// Move a Flow or folder into a folder (`None`: top level).
    Move {
        id: i64,
        parent: Option<i64>,
    },
    /// A Flow or folder, with everything in it.
    Delete(i64),
    DuplicateFlow {
        from: i64,
        to: i64,
    },
    SaveFlow {
        id: i64,
        cards: Vec<Card>,
    },
    /// Save one path through a Flow (card ids in order) as a new playlist.
    #[allow(dead_code)] // library management API, not wired to the UI yet (tests use it)
    ExportPath {
        flow: i64,
        nodes: Vec<u64>,
        playlist: i64,
        name: String,
    },
}

/// What changed. The UI patches its copy of the library from these.
#[derive(Debug)]
pub enum StoreEvent {
    Roots(Vec<String>),
    /// Every track in the library (after a scan, or tracks merging).
    Library(Vec<LibTrack>),
    /// These tracks changed (analysis, a grid edit, metadata).
    TracksChanged(Vec<LibTrack>),
    /// Waveform thumbnails for the track list.
    Previews(Vec<Preview>),
    /// A track's memory cues and loops, in order through the track.
    Memories {
        track: i64,
        memories: Vec<Memory>,
    },
    Flows(Vec<Folder>, Vec<FlowEntry>),
    /// (id, name, cards), or `None` if there's no such Flow.
    FlowLoaded(Option<(i64, String, Vec<Card>)>),
    /// Flows and folders that no longer exist.
    Deleted(Vec<i64>),
    Error(StoreError),
}

/// Where the Store sends its events.
pub type EventSink = Arc<dyn Fn(StoreEvent) + Send + Sync>;

type Job = Box<dyn FnOnce(&mut Writer) + Send>;

/// The handle to the writer thread, plus a read pool. Cheap to clone.
#[derive(Clone)]
pub struct Store {
    tx: Sender<Job>,
    pub reader: Reader,
    next_collection: Arc<AtomicI64>,
}

impl Store {
    /// Open (creating and migrating if needed) and start the writer thread.
    pub fn open(path: &Path, sink: EventSink) -> Result<Store> {
        let conn = open_writer(path)?;
        let next = conn.query_row(
            "SELECT coalesce(max(id), 0) + 1 FROM collections",
            [],
            |r| r.get(0),
        )?;
        let (tx, rx) = channel::<Job>();
        let mut writer = Writer::new(conn);
        std::thread::Builder::new()
            .name("earshot-store".into())
            .spawn(move || {
                for job in rx {
                    job(&mut writer);
                    for e in writer.take_events() {
                        sink(e);
                    }
                }
            })
            .map_err(|e| StoreError::Io {
                path: path.into(),
                source: e,
            })?;
        Ok(Store {
            tx,
            reader: Reader::new(path),
            next_collection: Arc::new(AtomicI64::new(next)),
        })
    }

    /// Queue a command. Never blocks; errors come back as [`StoreEvent::Error`].
    pub fn send(&self, cmd: StoreCmd) {
        let _ = self.tx.send(Box::new(move |w: &mut Writer| {
            if let Err(e) = w.apply(cmd) {
                w.emit(StoreEvent::Error(e));
            }
        }));
    }

    /// Run `f` on the writer thread and wait for its answer. For background threads
    /// only: it blocks until everything queued before it has been written.
    pub fn call<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Writer) -> Result<R> + Send + 'static,
    ) -> Result<R> {
        let (tx, rx) = channel();
        self.tx
            .send(Box::new(move |w: &mut Writer| {
                let _ = tx.send(f(w));
            }))
            .map_err(|_| StoreError::Closed)?;
        rx.recv().map_err(|_| StoreError::Closed)?
    }

    /// An id for a new Flow, folder or playlist. Only the writer inserts collections,
    /// so handing ids out here can't clash, and the caller can use the id straight away.
    pub fn new_collection_id(&self) -> i64 {
        self.next_collection.fetch_add(1, Ordering::Relaxed)
    }

    /// Load a track's memory cues on a reader thread and send them as an event.
    pub fn request_memories(&self, track: i64, sink: EventSink) {
        let reader = self.reader.clone();
        std::thread::spawn(move || match reader.with(|c| read::memories(c, track)) {
            Ok(memories) => sink(StoreEvent::Memories { track, memories }),
            Err(e) => sink(StoreEvent::Error(e)),
        });
    }

    /// Load waveform thumbnails on a reader thread and send them as an event.
    pub fn request_previews(&self, ids: Option<Vec<i64>>, sink: EventSink) {
        let reader = self.reader.clone();
        std::thread::spawn(
            move || match reader.with(|c| read::previews(c, ids.as_deref())) {
                Ok(p) => sink(StoreEvent::Previews(p)),
                Err(e) => sink(StoreEvent::Error(e)),
            },
        );
    }
}

fn open_writer(path: &Path) -> Result<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| StoreError::Io {
            path: dir.into(),
            source: e,
        })?;
    }
    let mut conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    migrate(&mut conn)?;
    Ok(conn)
}

/// Apply migrations in order; `user_version` records how many have run.
fn migrate(conn: &mut Connection) -> Result<()> {
    let done: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if done > MIGRATIONS.len() as i64 {
        return Err(StoreError::TooNew {
            found: done,
            known: MIGRATIONS.len(),
        });
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(done as usize) {
        let step = |source| StoreError::Migration {
            step: i + 1,
            source,
        };
        let tx = conn.transaction().map_err(step)?;
        tx.execute_batch(sql).map_err(step)?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)
            .map_err(step)?;
        tx.commit().map_err(step)?;
    }
    Ok(())
}

/// Read-only connections, opened as needed and kept for reuse. The mutex is only
/// held to take or return a connection, never while a query runs.
#[derive(Clone)]
pub struct Reader {
    path: PathBuf,
    pool: Arc<Mutex<Vec<Connection>>>,
}

/// Connections kept in the pool; more are opened (and closed after) when busy.
const POOL: usize = 4;

impl Reader {
    fn new(path: &Path) -> Reader {
        Reader {
            path: path.into(),
            pool: Arc::default(),
        }
    }

    pub fn with<R>(&self, f: impl FnOnce(&Connection) -> Result<R>) -> Result<R> {
        let taken = self.pool.lock().unwrap().pop();
        let conn = match taken {
            Some(c) => c,
            None => {
                let c = Connection::open_with_flags(
                    &self.path,
                    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )?;
                c.busy_timeout(std::time::Duration::from_secs(5))?;
                c
            }
        };
        let out = f(&conn);
        let mut pool = self.pool.lock().unwrap();
        if pool.len() < POOL {
            pool.push(conn);
        }
        out
    }

    pub fn track(&self, id: i64) -> Result<LibTrack> {
        self.with(|c| {
            read::tracks(c, Some(&[id]))?
                .pop()
                .ok_or(StoreError::NoTrack(id))
        })
    }

    pub fn track_path(&self, id: i64) -> Result<PathBuf> {
        self.with(|c| read::track_path(c, id))
    }

    /// The active grid and where it came from.
    pub fn grid(
        &self,
        id: i64,
    ) -> Result<Option<(earshot_engine::beatgrid::Beatgrid, GridSource)>> {
        self.with(|c| read::grid(c, id))
    }

    /// The active grid's row id and source, without decoding it.
    pub fn active_grid(&self, id: i64) -> Result<Option<(i64, GridSource)>> {
        self.with(|c| read::active_grid(c, id))
    }

    pub fn waveform(&self, id: i64, bins_per_s: f64) -> Result<Option<Vec<u8>>> {
        self.with(|c| read::waveform(c, id, bins_per_s))
    }

    pub fn loudness(&self, id: i64) -> Result<Option<earshot_engine::loudness::Loudness>> {
        self.with(|c| read::loudness(c, id))
    }

    /// A track's saved hot cues, by pad.
    pub fn hotcues(&self, id: i64) -> Result<[Option<f64>; HOTCUES]> {
        self.with(|c| read::hotcues(c, id))
    }

    /// A track's saved analyser results.
    pub fn analysis_results(&self, id: i64) -> Result<Vec<ResultState>> {
        self.with(|c| read::analysis_results(c, id))
    }

    /// Tracks needing `analyser` (at `version`) run again: no result, a failed one,
    /// an older version, or other audio.
    pub fn stale_tracks(&self, analyser: &str, version: u32) -> Result<Vec<i64>> {
        self.with(|c| read::stale_tracks(c, analyser, version))
    }

    /// The analysis queue as saved, oldest first.
    pub fn queued_jobs(&self) -> Result<Vec<JobRow>> {
        self.with(read::queued_jobs)
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        self.with(|c| read::setting(c, key))
    }

    /// Files whose audio hash hasn't been worked out: (file id, path).
    pub fn files_without_hash(&self) -> Result<Vec<(i64, PathBuf)>> {
        self.with(read::files_without_hash)
    }

    /// Every file the library knows: path bytes → (size, mtime in ns).
    pub fn known_files(&self) -> Result<std::collections::HashMap<Vec<u8>, (i64, i64)>> {
        self.with(read::known_files)
    }

    /// Tracks whose title, artist, album, genre or comment match (FTS5 syntax).
    #[allow(dead_code)] // library management API, not wired to the UI yet (tests use it)
    pub fn search(&self, query: &str) -> Result<Vec<i64>> {
        self.with(|c| read::search(c, query))
    }

    /// Groups of tracks with the same audio, each sorted, for "find duplicates".
    #[allow(dead_code)] // library management API, not wired to the UI yet (tests use it)
    pub fn duplicates(&self) -> Result<Vec<Vec<i64>>> {
        self.with(read::duplicates)
    }
}

// ---- paths as bytes -------------------------------------------------------------------

/// A path as it is stored: the OS's bytes, so names that aren't UTF-8 survive.
pub fn path_bytes(p: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        p.as_os_str().as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        p.to_string_lossy().into_owned().into_bytes()
    }
}

pub fn bytes_path(b: Vec<u8>) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_vec(b))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(&b).into_owned())
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests;
