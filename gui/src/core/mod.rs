//! Everything that isn't drawing: the database (the Store), Flows, library scanning,
//! the analysis queue, the decks (player and their sessions), commands, the control
//! registry, the keymap and MIDI. The UI talks to it by dispatching commands, calling
//! functions and sending Store commands, and hears back through [`Event`]s.

pub mod analysis;
pub mod bus;
pub mod command;
pub mod flows;
pub mod hash;
pub mod keymap;
pub mod keys;
pub mod library;
pub mod midi;
pub mod player;
pub mod session;
pub mod store;
pub mod suggest;
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
        track_id: i64,
        result: Result<(), String>,
    },
    /// A library scan finished (the tracks themselves arrive as a
    /// [`store::StoreEvent::Library`] just before).
    Scanned(Result<(), store::StoreError>),
    Store(store::StoreEvent),
    /// A track's cover art (RGBA, `library::ART_SIZE` square), or `None` if it has none.
    Art(i64, Option<Vec<u8>>),
}

/// Where background threads send [`Event`]s (the UI's channel, plus a repaint).
pub type Sink = Arc<dyn Fn(Event) + Send + Sync>;
