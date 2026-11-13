//! What's on screen, as plain data (no egui): the app's mode, the library's sources,
//! the side panes, Prepare's tool tabs and the windows over the app, and the commands
//! that change them. The UI keeps these in its `UiPrefs` and draws from them.

use earshot_engine::DeckId;
use serde::{Deserialize, Serialize};

use super::collections::Section;
use super::sources::{SourceId, SourceKind};
use super::track::TrackId;

/// What a deck's stem button does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum StemMode {
    /// Turns its stem off or on (a controller's stem buttons: VOX takes the vocals out).
    #[default]
    Toggle,
    /// With all on, plays just that stem; otherwise turns it off or on, and turning off
    /// the last one puts them all back on.
    Solo,
}

/// The two halves of the app, like Rekordbox's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Mode {
    /// The decks and the mixer, for playing a set.
    #[default]
    Perform,
    /// The same decks, over tools for setting cues, fixing grids and building playlists.
    Prepare,
    /// Perform's decks over the library and a controller's mapping, for setting up and
    /// testing a MIDI controller.
    Controller,
}

impl Mode {
    /// As the top bar names it.
    pub fn name(self) -> &'static str {
        match self {
            Mode::Perform => "Perform",
            Mode::Prepare => "Prepare",
            Mode::Controller => "Controller",
        }
    }

    /// The mode whose keymap bindings it uses: Controller uses Perform's.
    pub fn keys(self) -> Mode {
        match self {
            Mode::Controller => Mode::Perform,
            m => m,
        }
    }
}

/// What a controller's performance pads do, picked with its pad mode buttons. The
/// deck's pads page shows what each pad does in it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PadMode {
    #[default]
    HotCue,
    BeatLoop,
    BeatJump,
    Sampler,
    Keyboard,
    PadFx1,
    PadFx2,
    KeyShift,
}

impl PadMode {
    /// As the controller prints it.
    pub fn label(self) -> &'static str {
        match self {
            PadMode::HotCue => "Hot cue",
            PadMode::BeatLoop => "Beat loop",
            PadMode::BeatJump => "Beat jump",
            PadMode::Sampler => "Sampler",
            PadMode::Keyboard => "Keyboard",
            PadMode::PadFx1 => "Pad FX 1",
            PadMode::PadFx2 => "Pad FX 2",
            PadMode::KeyShift => "Key shift",
        }
    }
}

/// What the library's left icon stack shows: where to look for music. The first
/// four are views of earshot's own library; the rest are other places tracks come
/// from (`core::sources`), each with an icon of its own.
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
    /// A DJ program's library (Rekordbox, Serato, Mixxx): always listed, set up or not.
    Program(SourceKind),
    /// Another library added by hand (a Rekordbox XML, a copy of a stick).
    Library(SourceId),
    /// A USB stick, while it's plugged in, by [`Drive::id`](crate::core::drives::Drive::id).
    Stick(u64),
    /// Managing where music comes from: the music folders, adding libraries, and
    /// (to come) streaming services.
    #[serde(alias = "Sources", alias = "Explorer", alias = "External")]
    #[serde(alias = "SoundCloud", alias = "Beatport", alias = "Spotify")]
    Manage,
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

/// Prepare's tool tabs, under the player: what we're doing to the track.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tool {
    /// Analyse a track's tempo, straighten it and save the result.
    TempoLock,
    /// The Flow editor: plan a set as a graph of cards, grown from suggestions.
    /// (Saved layouts from when the player's controls were a tab of their own,
    /// "Cues & Grid", open here.)
    #[default]
    #[serde(alias = "CuesGrid")]
    Flow,
    /// Write playlists to another DJ program's library or a USB stick, or convert a stick.
    Export,
    /// Split the player's track into stems, and play them with a level and mute each.
    Stems,
    /// Cut the target track down to the part between two points; save it as a new file.
    Trim,
    /// The target track's memory cues and loops, in order through it.
    Cues,
}

/// The windows over the app. They stack: the last one opened is on top.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Modal {
    /// Audio, analysis and the keymap, a tab each ([`SettingsTab`]).
    Settings,
    Keys,
    /// The metadata editor (opened on tracks by `EditCmd::Editor`).
    Metadata,
}

/// The settings window's tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SettingsTab {
    /// The output device, buffer and routing.
    #[default]
    Audio,
    /// How the decks behave (hot cues).
    Decks,
    /// How many background workers analyse at once.
    Analysis,
    /// The keymap file.
    Keymap,
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
    /// Browse: into what's highlighted (1), or back out (-1).
    PaneStep(i32),
    /// Open the suggestions with the browse knob on their top track, or close them.
    RelatedTracks,
    /// Hide the decks for a bigger library, or show them again.
    ToggleLibraryMaximized,
    /// A list was clicked: it takes the keyboard.
    FocusPane(Pane),
    FocusSearch,
    /// Load the selected track onto this deck.
    LoadSelected(DeckId),
    /// Show the deck's track in the library, in the list it was loaded from.
    ShowInLibrary(DeckId),
    /// Move the selection in the focused list (negative is up) and load the track it
    /// lands on onto this deck: the previous or next track.
    LoadAdjacent(DeckId, i32),
    /// Show 1, 2 or 4 decks.
    DeckCount(usize),
    /// Prepare's target: the deck its tools act on.
    Target(DeckId),
    /// The controller's pads on this deck are in this mode now.
    PadMode(DeckId, PadMode),
    /// The controller's SHIFT on this deck held (true) or let go: the pads page shows
    /// what the pads do with it.
    Shift(DeckId, bool),
    /// A pad pressed (`down`) or let go in keyboard or key shift mode: see
    /// `core::pads`.
    ModePad {
        deck: DeckId,
        mode: PadMode,
        pad: u8,
        shift: bool,
        down: bool,
    },
    /// Four decks: the left hand (0) between decks 1 and 3, or the right (1) between
    /// 2 and 4.
    SwitchLayer(usize),
    SetMode(Mode),
    /// Open the track in Prepare's Tempo lock tab and chart it (the track menu's
    /// "Normalise tempo"). Nothing happens if the switch to Prepare is refused.
    NormaliseTempo(TrackId),
    /// Show a source in the library's left pane (`None` closes the pane).
    Source(Option<Source>),
    /// Open a pane on the library's right (`None` closes it).
    Aside(Option<Aside>),
    Tool(Tool),
    /// Open a window, or close it if it's open.
    ToggleModal(Modal),
    /// Open the settings window on this tab (or bring it to this tab).
    Settings(SettingsTab),
    CloseModal(Modal),
    /// Real seconds of waveform either side of the playhead.
    WaveZoom(f64),
    /// The size of everything (1.0 is 100 %).
    UiScale(f32),
    /// A panel was dragged to this size (points), in the current mode.
    Resize(Region, f32),
    /// Show Perform's mixer (the top bar's Mixer switch).
    ShowMixer(bool),
    /// Hide the mixer while a controller is connected.
    AutoHideMixer(bool),
    /// What the decks' stem buttons do.
    StemMode(StemMode),
    /// Act as though a controller is connected.
    PretendController(bool),
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
