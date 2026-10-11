//! Everything that isn't drawing: the database, library scanning, the analysis queue,
//! the decks (player), the control bus and MIDI. The UI talks to it by calling
//! functions and hears back through [`Event`]s.

pub mod analysis;
pub mod controls;
pub mod db;
pub mod keys;
pub mod library;
pub mod midi;
pub mod player;
pub mod suggest;

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
    /// A control the UI owns (library navigation, load selected), e.g. from MIDI.
    UiControl {
        group: String,
        key: String,
        value: f64,
    },
    Loaded {
        deck: usize,
        track_id: i64,
        result: Result<(), String>,
    },
    Scanned(Result<Vec<db::LibTrack>, String>),
}

/// Where background threads send [`Event`]s (the UI's channel, plus a repaint).
pub type Sink = Arc<dyn Fn(Event) + Send + Sync>;
