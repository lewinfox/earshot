//! What's on screen, as plain data (no egui): the app's mode, the library's sources,
//! the side panes, Prepare's tool tabs and the windows over the app, and the commands
//! that change them. The UI keeps these in its `UiPrefs` and draws from them.

use earshot_engine::DeckId;
use serde::{Deserialize, Serialize};

/// The two halves of the app, like Rekordbox's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Mode {
    /// The decks and the mixer, for playing a set.
    #[default]
    Perform,
    /// One player across the top for setting cues, fixing grids and building playlists.
    Prepare,
}

/// What the library's left icon stack shows: what we're operating on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Source {
    /// All tracks, the collections and the Explorer folders, in one tree.
    Browser,
}

/// The panes the library's right icon stack opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Aside {
    Suggestions,
}

/// Prepare's tool tabs: what we're doing to the track.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tool {
    /// The player's cues, loops, beat jumps and the grid editor.
    #[default]
    CuesGrid,
    /// Analyse a track's tempo, straighten it and save the result (to come: a
    /// placeholder for now).
    TempoLock,
}

/// The windows over the app. They stack: the last one opened is on top.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Modal {
    Audio,
    Keys,
}

/// Which list the keyboard and the controller's browse knob move through.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pane {
    #[default]
    Library,
    Suggest,
}

/// Things only the UI can do: they're about what's on screen.
#[derive(Debug, Clone, Copy)]
pub enum ViewCmd {
    /// Move the selection in the focused list (negative is up).
    Browse(i32),
    TogglePane,
    /// A list was clicked: it takes the keyboard.
    FocusPane(Pane),
    FocusSearch,
    /// Load the selected track onto this deck.
    LoadSelected(DeckId),
    SetMode(Mode),
    /// Show a source in the library's left pane (`None` closes the pane).
    Source(Option<Source>),
    /// Open a pane on the library's right (`None` closes it).
    Aside(Option<Aside>),
    Tool(Tool),
    /// The track list (false) or the Flow canvas (true) in the library's middle.
    ShowFlow(bool),
    /// Open a window, or close it if it's open.
    ToggleModal(Modal),
    CloseModal(Modal),
    /// Real seconds of waveform either side of the playhead.
    WaveZoom(f64),
    /// The size of everything (1.0 is 100 %).
    UiScale(f32),
    /// A panel was dragged to this size (points), in the current mode.
    Resize(Region, f32),
}

/// The parts of a view that can be resized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    /// The waveforms' row (its height).
    Waves,
    /// The library's left pane (its width).
    Source,
    /// The library's right pane (its width).
    Aside,
}
