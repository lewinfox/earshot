//! The UI's preferences, kept between runs under one eframe key: the mode, each
//! mode's layout, zoom and panel sizes. Deck and audio settings are engine state and
//! live in the database instead (`core::settings`).

use earshot_engine::DeckId;
use serde::{Deserialize, Serialize};

use super::panels::library::table::Tables;
use crate::core::view::{Aside, Mode, Source, Tool};

/// Where they're kept in eframe's storage.
pub const KEY: &str = "earshot.ui";

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct UiPrefs {
    pub mode: Mode,
    pub perform: Layout,
    pub prepare: Layout,
    /// Real seconds of waveform either side of the playhead, shared by every deck.
    pub wave_zoom: f64,
    /// The text size, as a multiple of normal (see [`crate::ui::theme::fs`]).
    pub ui_scale: f32,
    /// Four decks in Perform instead of two. The engine always runs four.
    pub four_decks: bool,
    /// Each kind of track list's columns, widths and sort.
    pub tables: Tables,
    /// Perform's mixer, between the decks (the top bar's Mixer switch), while no
    /// controller hides it.
    pub show_mixer: bool,
    /// Perform's effect units, beside the mixer (the top bar's FX switch).
    pub show_fx: bool,
    /// Hide the mixer while a controller is connected (it has its own).
    pub auto_hide_mixer: bool,
    /// A MIDI controller is connected (not kept).
    #[serde(skip)]
    pub controller: bool,
    /// The mixer, while a controller hides it: shown again by hand. Not kept: each
    /// time a controller connects it starts hidden.
    #[serde(skip)]
    pub mixer_with_controller: bool,
}

/// View > Text size's sizes, which Ctrl +/- step through, with their names.
pub const TEXT_SIZES: [(f32, &str); 3] = [(0.9, "Small"), (1.0, "Normal"), (1.2, "Large")];

/// The nearest of [`TEXT_SIZES`] to `k` (a size saved before there were three).
pub fn text_size(k: f32) -> f32 {
    TEXT_SIZES
        .iter()
        .map(|&(z, _)| z)
        .min_by(|a, b| (a - k).abs().total_cmp(&(b - k).abs()))
        .unwrap_or(1.0)
}

impl Default for UiPrefs {
    fn default() -> UiPrefs {
        UiPrefs {
            mode: Mode::default(),
            perform: Layout::default(),
            prepare: Layout::default(),
            wave_zoom: 4.0,
            ui_scale: 1.0,
            four_decks: false,
            tables: Tables::default(),
            show_mixer: true,
            show_fx: true,
            auto_hide_mixer: true,
            controller: false,
            mixer_with_controller: false,
        }
    }
}

impl UiPrefs {
    /// The decks Perform shows: two or four.
    pub fn decks(&self) -> Vec<DeckId> {
        DeckId::first(if self.four_decks { 4 } else { 2 }).collect()
    }

    /// Perform shows the mixer.
    pub fn mixer_shown(&self) -> bool {
        if self.mixer_hidden_by_controller() {
            self.mixer_with_controller
        } else {
            self.show_mixer
        }
    }

    /// The connected controller decides whether the mixer shows (Settings > Decks).
    fn mixer_hidden_by_controller(&self) -> bool {
        self.auto_hide_mixer && self.controller
    }

    /// Show or hide the mixer: for now, while a controller hides it; else from now on.
    pub fn set_mixer(&mut self, show: bool) {
        if self.mixer_hidden_by_controller() {
            self.mixer_with_controller = show;
        } else {
            self.show_mixer = show;
        }
    }

    /// A controller was connected or disconnected (`true`: one is there now).
    pub fn set_controller(&mut self, connected: bool) {
        if connected && !self.controller {
            self.mixer_with_controller = false;
        }
        self.controller = connected;
    }

    pub fn layout(&self) -> &Layout {
        match self.mode {
            Mode::Perform => &self.perform,
            Mode::Prepare => &self.prepare,
        }
    }

    /// Prepare's Flow editor is on screen.
    pub fn flow_tab(&self) -> bool {
        self.mode == Mode::Prepare && self.prepare.tool == Tool::Flow
    }

    pub fn layout_mut(&mut self) -> &mut Layout {
        match self.mode {
            Mode::Perform => &mut self.perform,
            Mode::Prepare => &mut self.prepare,
        }
    }
}

/// One mode's layout.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Layout {
    /// The source shown left of the track list (`None`: its pane is closed).
    pub source: Option<Source>,
    /// The pane open right of the track list.
    pub aside: Option<Aside>,
    /// Prepare's tool tab.
    pub tool: Tool,
    pub sizes: Sizes,
}

impl Default for Layout {
    fn default() -> Layout {
        Layout {
            source: Some(Source::AllTracks),
            aside: Some(Aside::Suggestions),
            tool: Tool::default(),
            sizes: Sizes::default(),
        }
    }
}

/// Panel sizes in points, as last dragged.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(default)]
pub struct Sizes {
    /// The waveforms' row.
    pub waves: f32,
    /// The library's left pane (the source).
    pub source: f32,
    /// The library's right pane.
    pub aside: f32,
    /// The library beside the Flow canvas.
    pub flow_library: f32,
}

impl Default for Sizes {
    fn default() -> Sizes {
        Sizes {
            waves: 150.0,
            source: 250.0,
            aside: 380.0,
            flow_library: 640.0,
        }
    }
}
