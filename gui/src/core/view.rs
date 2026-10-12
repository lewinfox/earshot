//! What's on screen, as plain data (no egui): the app's mode, the library's sources,
//! the side panes, Prepare's tool tabs and the windows over the app, and the commands
//! that change them. The UI keeps these in its `UiPrefs` and draws from them.

use earshot_engine::DeckId;
use serde::{Deserialize, Serialize};

use super::collections::Section;

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
    /// Every track, with quick filters by analysis status.
    #[serde(alias = "Browser")]
    AllTracks,
    /// Playlists and smart playlists, in folders.
    Playlists,
    /// Crates, in folders.
    Crates,
    /// Flows, in folders.
    Flows,
    /// The library folders on disk.
    Explorer,
    /// Other DJ software's libraries and drives, through cratelib (to come).
    External,
    /// Streaming services (to come: greyed out).
    SoundCloud,
    Beatport,
    Spotify,
}

impl Source {
    /// The collections tree it shows, if it shows one.
    pub const fn section(self) -> Option<Section> {
        match self {
            Source::Playlists => Some(Section::Playlists),
            Source::Crates => Some(Section::Crates),
            Source::Flows => Some(Section::Flows),
            _ => None,
        }
    }
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
    /// The Flow editor: plan a set as a graph of cards, grown from suggestions.
    Flow,
}

/// The windows over the app. They stack: the last one opened is on top.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Modal {
    Audio,
    Keys,
    /// The metadata editor (opened on tracks by `EditCmd::Editor`).
    Metadata,
}

/// Which list has focus: the keyboard and the controller's browse knob move through
/// it, "load selected" loads from it, and the suggestions' *Selected* reference is
/// its selected track. Each keeps its own selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pane {
    /// The library's track table.
    #[default]
    Library,
    Suggest,
    /// The Flow canvas (its selected card).
    Flow,
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
    /// Move the selection in the focused list (negative is up) and load the track it
    /// lands on onto this deck: the previous or next track.
    LoadAdjacent(DeckId, i32),
    /// Four decks in Perform (false: two).
    FourDecks(bool),
    SetMode(Mode),
    /// Show a source in the library's left pane (`None` closes the pane).
    Source(Option<Source>),
    /// Open a pane on the library's right (`None` closes it).
    Aside(Option<Aside>),
    Tool(Tool),
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
    /// The library beside the Flow canvas (its width).
    FlowLibrary,
}
