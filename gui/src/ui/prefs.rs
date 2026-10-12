//! The UI's preferences, kept between runs under one eframe key: the mode, each
//! mode's layout, zoom and panel sizes. Deck and audio settings are engine state and
//! live in the database instead (`core::settings`).

use serde::{Deserialize, Serialize};

use super::panels::flow;
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
    /// The size of everything (egui's zoom factor).
    pub ui_scale: f32,
    /// Four decks in Perform instead of two. Not used yet: the engine runs two.
    pub four_decks: bool,
    /// The Flow canvas: the Flow that was open and where it was looking.
    pub flow: flow::Saved,
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
            flow: flow::Saved::default(),
        }
    }
}

impl UiPrefs {
    pub fn layout(&self) -> &Layout {
        match self.mode {
            Mode::Perform => &self.perform,
            Mode::Prepare => &self.prepare,
        }
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
            source: Some(Source::Browser),
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
}

impl Default for Sizes {
    fn default() -> Sizes {
        Sizes {
            waves: 150.0,
            source: 250.0,
            aside: 380.0,
        }
    }
}
