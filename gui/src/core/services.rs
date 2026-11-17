//! What the app starts with: where its database is, which audio output and analysis
//! worker to use, and whether to open MIDI. The real app uses [`Services::real`];
//! tests start the whole app on a temporary database and a stand-in worker, from a
//! thread that steps its background work (`core::background`): the Store then writes
//! where it's asked, analysis and loads wait to be run, and audio is rendered only
//! when the test advances it, so nothing in a test depends on timing.

use std::path::PathBuf;

use super::store;
use super::worker_client::{Role, WorkerConfig};

pub struct Services {
    /// The library database file (created if it isn't there).
    pub db: PathBuf,
    /// Always play to the null output (no sound card needed).
    pub null_audio: bool,
    /// How to start an analysis worker process for a role, with this many threads.
    pub worker: fn(Role, usize) -> WorkerConfig,
    /// Open every MIDI input at startup.
    pub midi: bool,
}

impl Services {
    pub fn real() -> Services {
        Services {
            db: data_dir().join(store::FILE_NAME),
            null_audio: false,
            worker: WorkerConfig::python,
            midi: true,
        }
    }
}

/// Where earshot keeps its data (the name is the app id it was first released under).
/// `EARSHOT_DATA_DIR` overrides it (e.g. to try things on a copy).
pub fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("EARSHOT_DATA_DIR") {
        return PathBuf::from(d);
    }
    directories::BaseDirs::new()
        .map(|d| d.data_dir().join("io.github.lewinfox.earshot"))
        .unwrap_or_else(|| PathBuf::from("."))
}
