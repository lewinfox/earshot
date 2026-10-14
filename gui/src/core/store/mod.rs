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
use std::sync::{Arc, Mutex, RwLock};

use earshot_engine::HOTCUES;
use earshot_engine::media::Location;
use rusqlite::{Connection, OpenFlags};

pub use read::{JobRow, LibTrack, Memory, PlaySession, Preview, ResultState, Sources, TrackCues};
pub use write::{AnalysisSave, GridSave, ScannedFile, Writer, grid_bpm};

use super::collections::{Collection, CollectionKind, FlowGraph, NodeId, Section};
use super::edit::{Edit, EditCmd};
use super::embeddings::Embeddings;
use super::sources::{NewSource, Source, SourceId};

/// Bump when `engine/src/waveform.rs` changes its output, so cached waveforms are redone.
pub const WAVEFORM_VERSION: i64 = 1;

/// The file name in the data folder. Not `earshot.db`: the schema was rebuilt from
/// scratch, and an old database is left alone rather than migrated.
pub const FILE_NAME: &str = "library.db";

/// The whole schema. There are no migrations yet: development databases from before
/// it are refused (see [`SCHEMA_VERSION`]).
const BASELINE: &str = include_str!("../../../migrations/001_baseline.sql");

/// `user_version` of a database made from [`BASELINE`]. Development builds used 1 and
/// 2 (before beatgrids stored their model), 3 (before collections had sections), 4
/// (before imports from other libraries) and 5 (before sources); those are refused,
/// not converted.
pub const SCHEMA_VERSION: i64 = 6;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("creating the database: {0}")]
    Create(rusqlite::Error),
    #[error("the database is from a newer earshot (schema {found}, this one knows {known})")]
    TooNew { found: i64, known: i64 },
    #[error(
        "the database is from an older development build (schema {found}); \
         delete {FILE_NAME} in the data folder to start a new library"
    )]
    TooOld { found: i64 },
    #[error("no track {0}")]
    NoTrack(i64),
    #[error("{0}: not a file on this computer")]
    NotLocal(String),
    #[error("no Flow or folder {0}")]
    NoCollection(i64),
    #[error("track {track}'s beatgrid is unreadable: {reason}")]
    BadGrid { track: i64, reason: String },
    #[error("{0}")]
    Refused(&'static str),
    #[error("smart playlist: {0}")]
    BadQuery(String),
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

/// Where a Flow's canvas looks: `[left, top, right, bottom]` in canvas points. Kept
/// in the Flow's `collections.view_state` (as JSON), so it goes when the Flow does.
pub type FlowCamera = [f32; 4];

/// What the UI (or anything that doesn't need an answer) asks the Store to do.
/// Results and failures come back as [`StoreEvent`]s.
#[derive(Debug)]
pub enum StoreCmd {
    /// Answered with [`StoreEvent::Sources`].
    ListSources,
    /// A music folder, another library … Adding one that's there (a program's, of
    /// the same kind) sets it again and switches it on.
    AddSource(NewSource),
    /// Its name, location, settings, or switched on or off.
    UpdateSource(Source),
    /// A folder or added library goes (its tracks stay in the database, unlisted
    /// unless another source has them); a program's library is switched off, so
    /// looking in the usual places doesn't bring it back.
    RemoveSource(SourceId),
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
    /// A track's embedding from `model` (e.g. `embeddings::CLAP_MODEL`). Kept in the in-memory
    /// block too ([`Store::embeddings`]) when it's that model's.
    #[allow(dead_code)] // for the CLAP analyser to come; tests use it
    SaveEmbedding {
        track: i64,
        model: String,
        vector: Vec<f32>,
    },
    /// The audio hash of a file, worked out in the background.
    SetContentHash {
        file: i64,
        hash: [u8; 32],
    },
    /// A track's hot cue times (pad → seconds). A pad that stays set keeps its label;
    /// a cleared pad loses it. Answered with [`StoreEvent::Cues`].
    SaveHotcues {
        track: i64,
        cues: [Option<f64>; HOTCUES],
    },
    /// Name a hot cue (`None` clears the name). Does nothing to a pad that isn't set.
    /// Answered with [`StoreEvent::Cues`].
    SetHotcueLabel {
        track: i64,
        slot: u8,
        label: Option<String>,
    },
    /// Save a memory cue (or loop, with `end`), unless the same one is already saved.
    /// Answered with [`StoreEvent::Cues`].
    AddMemory {
        track: i64,
        start: f64,
        end: Option<f64>,
    },
    /// Answered with [`StoreEvent::Cues`].
    DeleteMemory {
        track: i64,
        id: i64,
    },
    /// Name a memory cue or loop (`None` clears the name). Answered with
    /// [`StoreEvent::Cues`].
    SetMemoryLabel {
        track: i64,
        id: i64,
        label: Option<String>,
    },
    /// Colour a memory cue or loop (0xRRGGBB; `None`: the default). Answered with
    /// [`StoreEvent::Cues`].
    SetMemoryColor {
        track: i64,
        id: i64,
        color: Option<u32>,
    },
    /// A new play session (one run of the app): the plays saved from now on are
    /// its. An earlier session left open (the app didn't get to end it) is closed at
    /// its last play.
    StartSession,
    /// A track counted as played (`history::PLAYED_AFTER`) on deck `deck` (0-based),
    /// playing since `started_at` (unix seconds). Starts a session if none is open.
    SavePlay {
        track: i64,
        deck: usize,
        started_at: i64,
    },
    /// That deck's play ended (it stopped, or another track went on it).
    EndPlay {
        deck: usize,
    },
    /// Answered with [`StoreEvent::Sessions`]. On the writer thread, so it sees every
    /// play saved before it.
    ListSessions,
    ListCollections,
    /// Answered with [`StoreEvent::FlowLoaded`].
    LoadFlow(i64),
    /// A folder, crate, playlist, smart playlist or Flow. Ids come from
    /// [`Store::new_collection_id`], so the caller knows them at once.
    Create {
        id: i64,
        parent: Option<i64>,
        kind: CollectionKind,
        name: String,
        /// A smart playlist's query.
        query: Option<String>,
    },
    /// A folder in one section's tree. The UI sends the undoable form,
    /// `EditCmd::Create`.
    #[allow(dead_code)]
    CreateFolder {
        id: i64,
        parent: Option<i64>,
        section: Section,
        name: String,
    },
    /// The UI sends the undoable form, `EditCmd::Rename`.
    #[allow(dead_code)]
    Rename {
        id: i64,
        name: String,
    },
    /// Move a collection into a folder (`None`: top level). The UI sends the
    /// undoable form, `EditCmd::Move`.
    #[allow(dead_code)]
    Move {
        id: i64,
        parent: Option<i64>,
    },
    /// A collection, with everything in it. The UI sends the undoable form,
    /// `EditCmd::Delete`.
    #[allow(dead_code)]
    Delete(i64),
    /// A copy beside it, named "<name> copy". The UI sends the undoable form,
    /// `EditCmd::Duplicate`.
    #[allow(dead_code)]
    Duplicate {
        from: i64,
        to: i64,
    },
    SaveFlow {
        id: i64,
        graph: FlowGraph,
    },
    /// Where a Flow's canvas is looking (its `view_state`), so it opens there again.
    SaveFlowCamera {
        id: i64,
        camera: FlowCamera,
    },
    /// Add tracks to a crate (each once), the end of a playlist, or a Flow (as
    /// new starting cards). The UI sends the undoable form, `EditCmd::AddTracks`.
    #[allow(dead_code)]
    AddTracks {
        id: i64,
        tracks: Vec<i64>,
    },
    /// Take these tracks out of a crate, playlist or Flow, wherever they are in it.
    /// The UI sends the undoable form, `EditCmd::RemoveTracks`.
    #[allow(dead_code)]
    RemoveTracks {
        id: i64,
        tracks: Vec<i64>,
    },
    /// A smart playlist's query.
    SetQuery {
        id: i64,
        query: String,
    },
    /// An edit the user asked for: made, and reported (with what it replaced) as
    /// [`StoreEvent::Edited`] for the undo history.
    Edit(EditCmd),
    /// Undo or redo: make an edit exactly as it is. Not reported as `Edited`.
    ApplyEdit(Edit),
    /// Save one path through a Flow (card ids in order) as a new playlist.
    ExportPath {
        flow: i64,
        nodes: Vec<NodeId>,
        playlist: i64,
        name: String,
    },
}

/// What changed. The UI patches its copy of the library from these.
#[derive(Debug)]
pub enum StoreEvent {
    /// Every source (after any of them changes).
    Sources(Vec<Source>),
    /// Every track in the library (after a scan, or tracks merging).
    Library(Vec<LibTrack>),
    /// These tracks changed (analysis, a grid edit, metadata).
    TracksChanged(Vec<LibTrack>),
    /// Waveform thumbnails for the track list.
    Previews(Vec<Preview>),
    /// A track's hot cue labels, and its memory cues and loops in order through it.
    Cues {
        track: i64,
        cues: TrackCues,
    },
    /// Every collection (after any of them changes).
    Collections(Vec<Collection>),
    /// The play sessions with plays, newest first.
    Sessions(Vec<PlaySession>),
    /// What each metadata source says for these tracks (track → field → sources,
    /// strongest first): from [`Store::request_meta`].
    MetaSources(std::collections::HashMap<i64, std::collections::HashMap<String, Sources>>),
    /// (id, name, graph), or `None` if there's no such Flow; and where its canvas was
    /// looking, if that was saved.
    FlowLoaded(Option<(i64, String, FlowGraph)>, Option<FlowCamera>),
    /// Collections that no longer exist.
    Deleted(Vec<i64>),
    /// An edit was made (from [`StoreCmd::Edit`], or a hand-fixed grid): one step
    /// for the undo history.
    Edited(Edit),
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
    embeddings: Arc<RwLock<Embeddings>>,
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
        let embeddings = writer.embeddings();
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
            embeddings,
        })
    }

    /// Every library track's `embeddings::CLAP_MODEL` embedding, in one block in memory, for
    /// sound-alike search (`core::embeddings`). Loaded when the Store opened and kept
    /// up to date by the writer; read-lock it only for a search.
    #[allow(dead_code)] // for the Vibe suggestion feature to come; tests use it
    pub fn embeddings(&self) -> &Arc<RwLock<Embeddings>> {
        &self.embeddings
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

    /// Bring in what an import found (`core::external`), handing out ids for the
    /// collections it may make. Blocking: call from a background thread.
    pub fn import(&self, save: super::external::ImportSave) -> Result<super::external::Imported> {
        let n = save.nodes() as i64;
        let first = self.next_collection.fetch_add(n, Ordering::Relaxed);
        self.call(move |w| w.import_external(save, first..first + n))
    }

    /// An id for a new collection. Only the writer inserts collections,
    /// so handing ids out here can't clash, and the caller can use the id straight away.
    pub fn new_collection_id(&self) -> i64 {
        self.next_collection.fetch_add(1, Ordering::Relaxed)
    }

    /// Load a track's hot cue labels and memory cues on a reader thread and send them
    /// as an event.
    pub fn request_cues(&self, track: i64, sink: EventSink) {
        let reader = self.reader.clone();
        std::thread::spawn(move || match reader.with(|c| read::cues(c, track)) {
            Ok(cues) => sink(StoreEvent::Cues { track, cues }),
            Err(e) => sink(StoreEvent::Error(e)),
        });
    }

    /// Load what each metadata source says for these tracks on a reader thread, and
    /// send it as [`StoreEvent::MetaSources`].
    pub fn request_meta(&self, ids: Vec<i64>, sink: EventSink) {
        let reader = self.reader.clone();
        std::thread::spawn(move || match reader.with(|c| read::meta_sources(c, &ids)) {
            Ok(m) => sink(StoreEvent::MetaSources(m)),
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
    write::fill_keys(&conn)?;
    Ok(conn)
}

/// Create the schema in a new database (`user_version` 0), and refuse any other
/// version than [`SCHEMA_VERSION`].
fn migrate(conn: &mut Connection) -> Result<()> {
    let found: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    match found {
        0 => {
            let tx = conn.transaction().map_err(StoreError::Create)?;
            tx.execute_batch(BASELINE).map_err(StoreError::Create)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(StoreError::Create)?;
            tx.commit().map_err(StoreError::Create)
        }
        SCHEMA_VERSION => Ok(()),
        found if found > SCHEMA_VERSION => Err(StoreError::TooNew {
            found,
            known: SCHEMA_VERSION,
        }),
        found => Err(StoreError::TooOld { found }),
    }
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

    /// Where a track's audio is now.
    pub fn track_location(&self, id: i64) -> Result<Location> {
        self.with(|c| read::track_location(c, id))
    }

    /// A track's local file, for what needs one (the tempo lock tool, revealing it
    /// in the file manager); an error for audio that's elsewhere.
    pub fn track_path(&self, id: i64) -> Result<PathBuf> {
        match self.track_location(id)? {
            Location::File(p) => Ok(p),
            Location::Url(u) => Err(StoreError::NotLocal(u)),
        }
    }

    /// The active grid's model and where it came from.
    pub fn grid(
        &self,
        id: i64,
    ) -> Result<Option<(earshot_engine::beatgrid::GridModel, GridSource)>> {
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

    /// Where analysis found the track's beats start (s): the initial cue goes on the
    /// grid line nearest it. `None` if it hasn't (or analysed it before grid 3).
    pub fn first_beat(&self, id: i64) -> Result<Option<f64>> {
        self.with(|c| read::first_beat(c, id))
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

    /// Every source (`core::sources`).
    pub fn sources(&self) -> Result<Vec<Source>> {
        self.with(read::sources)
    }

    /// Where a Flow's canvas was looking, as saved. (The app gets it with the Flow:
    /// `StoreEvent::FlowLoaded`.)
    #[cfg(test)]
    pub fn flow_camera(&self, id: i64) -> Result<Option<FlowCamera>> {
        self.with(|c| read::flow_camera(c, id))
    }

    /// Every saved play, oldest first.
    #[cfg(test)]
    pub fn plays(&self) -> Result<Vec<read::Play>> {
        self.with(read::plays)
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

    /// A collection's tracks (a smart playlist's found by its query).
    #[allow(dead_code)] // for export and sync; the UI evaluates queries in memory
    pub fn collection_tracks(&self, id: i64) -> Result<Vec<i64>> {
        self.with(|c| read::collection_tracks(c, id))
    }

    /// Groups of tracks with the same audio, each sorted, for the Duplicates view.
    pub fn duplicates(&self) -> Result<Vec<Vec<i64>>> {
        self.with(read::duplicates)
    }
}

// ---- paths as bytes -------------------------------------------------------------------

/// A path as it is stored: the OS's bytes, so names that aren't UTF-8 survive.
pub fn path_bytes(p: &Path) -> Vec<u8> {
    earshot_engine::media::path_to_bytes(p)
}

pub fn bytes_path(b: Vec<u8>) -> PathBuf {
    earshot_engine::media::path_from_bytes(b)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests;
