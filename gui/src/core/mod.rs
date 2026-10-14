//! Everything that isn't drawing: the database (the Store), the library in memory
//! (tracks, queries, collections, play history), library scanning, the analysis
//! queue, the decks (player and their sessions), commands, the control registry,
//! the keymap and MIDI, settings, and what's on screen as plain data ([`view`]). The UI talks to it by dispatching commands, calling
//! functions and sending Store commands, and hears back through [`Event`]s.

pub mod analysis;
pub mod bus;
pub mod collections;
pub mod command;
pub mod edit;
#[allow(dead_code)] // searched by the Vibe suggestion feature to come; tests use it
pub mod embeddings;
pub mod external;
pub mod hash;
pub mod history;
pub mod index;
pub mod keymap;
pub mod keys;
pub mod library;
pub mod log;
pub mod midi;
pub mod player;
pub mod query;
pub mod selection;
pub mod services;
pub mod session;
pub mod settings;
pub mod sources;
pub mod store;
pub mod suggest;
pub mod tempo_lock;
pub mod track;
pub mod view;
pub mod worker_client;

use std::sync::Arc;

use analysis::AnalysisEvent;

/// Something that happened off the UI thread.
#[derive(Debug)]
pub enum Event {
    Analysis(AnalysisEvent),
    Midi {
        port: String,
        bytes: Vec<u8>,
        mapped: Option<String>,
    },
    /// A command for the UI to dispatch (one that needs app state, e.g. from MIDI).
    Command(command::Command),
    Loaded {
        deck: earshot_engine::DeckId,
        /// The request's ticket (`player::request_load`).
        ticket: u64,
        result: Result<(), String>,
    },
    /// A library scan finished (the tracks themselves arrive as a
    /// [`store::StoreEvent::Library`] just before).
    Scanned(Result<(), store::StoreError>),
    Store(store::StoreEvent),
    /// A track's cover art (RGBA, `library::ART_SIZE` square), or `None` if it has none.
    Art(i64, Option<Vec<u8>>),
    /// Groups of tracks with the same audio (`Reader::duplicates`).
    Duplicates(Result<Vec<Vec<i64>>, String>),
    /// Tracks with stale analysis: counted, or queued again (`queued`).
    Stale {
        queued: bool,
        result: Result<usize, String>,
    },
    /// Other DJ software's libraries: found, read, imported.
    External(external::ExternalEvent),
    /// News from the tempo lock tool.
    #[allow(dead_code)] // read by the tempo lock tab (step 7, B4)
    TempoLock(tempo_lock::TempoLockEvent),
}

/// Where background threads send [`Event`]s (the UI's channel, plus a repaint).
pub type Sink = Arc<dyn Fn(Event) + Send + Sync>;
