//! What the app starts with: where its database is, which audio output and analysis
//! worker to use, and whether to open MIDI. The real app uses [`Services::real`];
//! tests start the whole app on a temporary database and a stand-in worker, from a
//! thread that steps its background work (`core::background`): the Store then writes
//! where it's asked, analysis and loads wait to be run, and audio is rendered only
//! when the test advances it, so nothing in a test depends on timing.

use std::path::PathBuf;

use super::folders::Folders;
use super::store;
use super::worker_client::{Role, WorkerConfig};

pub struct Services {
    /// Where it keeps its files (`core::folders`). The library database is in `data`
    /// (created if it isn't there).
    pub folders: Folders,
    /// Always play to the null output (no sound card needed).
    pub null_audio: bool,
    /// How to start an analysis worker process for a role, with this many threads.
    pub worker: fn(Role, usize) -> WorkerConfig,
    /// Open every MIDI input at startup.
    pub midi: bool,
    /// Where bucket sources' access keys are kept (`core::bucket`): the OS keychain.
    pub secrets: std::sync::Arc<dyn super::bucket::Secrets>,
}

impl Services {
    pub fn real(folders: Folders) -> Services {
        Services {
            folders,
            null_audio: false,
            worker: WorkerConfig::python,
            midi: true,
            secrets: std::sync::Arc::new(super::bucket::Keychain),
        }
    }

    /// The library database file.
    pub fn db(&self) -> PathBuf {
        self.folders.data.join(store::FILE_NAME)
    }
}
