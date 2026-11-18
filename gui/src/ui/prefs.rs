//! The UI's preferences, kept between runs under one eframe key: the mode, each
//! mode's layout, zoom and panel sizes. Deck and audio settings are engine state and
//! live in the database instead (`core::settings`).

use earshot_engine::DeckId;
use serde::{Deserialize, Serialize};

use super::panels::library::table::Tables;
use crate::core::bus::Control;
use crate::core::view::{Aside, Mode, Source, StemMode, Tool};

/// 1, 2 or 4 (anything else saved becomes 2).
pub fn deck_count(n: usize) -> usize {
    match n {
        1 | 2 | 4 => n,
        _ => 2,
    }
}

/// Where they're kept in eframe's storage.
pub const KEY: &str = "earshot.ui";

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct UiPrefs {
    pub mode: Mode,
    pub perform: Layout,
    pub prepare: Layout,
    /// Controller mode's layout.
    pub controller_view: Layout,
    /// Debug mode's layout.
    pub debug_view: Layout,
    /// Real seconds of waveform either side of the playhead, shared by every deck.
    pub wave_zoom: f64,
    /// The UI's size, as a multiple of normal: egui's zoom (see [`ui_scale`] and
    /// `ui::window`), so text, controls and spacing all grow together.
    pub ui_scale: f32,
    /// How many decks show: 1, 2 or 4. The engine always runs four. Read
    /// [`UiPrefs::deck_total`]: one deck is for Prepare only.
    pub deck_count: usize,
    /// Prepare's target: the deck (by index) whose track its tools act on. Read
    /// [`UiPrefs::target`], which keeps it to a deck that's showing.
    pub target: usize,
    /// Each kind of track list's columns, widths and sort.
    pub tables: Tables,
    /// Perform's mixer, between the decks (the top bar's Mixer switch), while no
    /// controller hides it.
    pub show_mixer: bool,
    /// Hide the mixer while a controller is connected (it has its own).
    pub auto_hide_mixer: bool,
    /// The scrolling waveforms hidden for a bigger library (`ui_library_maximize_toggle`); not kept
    /// between runs.
    #[serde(skip)]
    pub library_maximized: bool,
    /// What the decks' stem buttons do.
    pub stem_mode: StemMode,
    /// In four-deck mode, which deck each hand plays: the left on deck 1 or 3
    /// (`false`, `true`), the right on 2 or 4 (not kept: each run starts on 1 and 2).
    #[serde(skip)]
    pub layers: [bool; 2],
    /// A MIDI controller is connected (not kept). Read [`UiPrefs::controller`], which
    /// counts a pretend one too.
    #[serde(skip)]
    pub controller: bool,
    /// Act as though a controller is connected (Menu > Debug), to see and build the
    /// controller layout without one.
    pub pretend_controller: bool,
    /// The mixer, while a controller hides it: shown again by hand. Not kept: each
    /// time a controller connects it starts hidden.
    #[serde(skip)]
    pub mixer_with_controller: bool,
    /// The mapping chosen for each MIDI device (the MIDI pill): a
    /// `controllers::device_key` to a `controllers::MappingId::key`.
    pub controller_mappings: std::collections::BTreeMap<String, String>,
}

/// The UI size's range (Settings > Display, Ctrl +/-): everything is drawn this many
/// times its normal size, text and controls alike.
pub const UI_SCALES: std::ops::RangeInclusive<f32> = 0.8..=1.5;
/// What Ctrl +/- changes it by.
pub const UI_SCALE_STEP: f32 = 0.1;
/// Menu > View > UI size's choices.
pub const UI_SCALE_CHOICES: [f32; 6] = [0.8, 0.9, 1.0, 1.1, 1.25, 1.5];

/// A UI size within [`UI_SCALES`] (a saved one may be from an older version, or not a
/// number at all).
pub fn ui_scale(k: f32) -> f32 {
    if k.is_finite() {
        k.clamp(*UI_SCALES.start(), *UI_SCALES.end())
    } else {
        1.0
    }
}

impl Default for UiPrefs {
    fn default() -> UiPrefs {
        UiPrefs {
            mode: Mode::default(),
            perform: Layout::default(),
            prepare: Layout::default(),
            controller_view: Layout::default(),
            debug_view: Layout::default(),
            wave_zoom: 4.0,
            ui_scale: 1.0,
            deck_count: 2,
            target: 0,
            tables: Tables::default(),
            show_mixer: true,
            auto_hide_mixer: true,
            library_maximized: false,
            stem_mode: StemMode::default(),
            controller: false,
            pretend_controller: false,
            layers: [false; 2],
            mixer_with_controller: false,
            controller_mappings: Default::default(),
        }
    }
}

impl UiPrefs {
    /// How many decks show: 1, 2 or 4, but never 1 outside Prepare (Perform and
    /// Controller mode show two instead, and Prepare goes back to one).
    pub fn deck_total(&self) -> usize {
        match deck_count(self.deck_count) {
            1 if self.mode != Mode::Prepare => 2,
            n => n,
        }
    }

    /// The decks showing: 1, 2 or 4.
    pub fn decks(&self) -> Vec<DeckId> {
        DeckId::first(self.deck_total()).collect()
    }

    /// Four decks show: each hand switches between two.
    pub fn four_decks(&self) -> bool {
        self.deck_total() == 4
    }

    /// Prepare's target deck: the one its tools act on (deck 1 if the chosen one is
    /// hidden).
    pub fn target(&self) -> DeckId {
        let i = if self.target < self.deck_total() {
            self.target
        } else {
            0
        };
        DeckId::new(i).expect("a deck")
    }

    /// The deck the left (`side` 0) or right (1) hand plays: 1 or 3, 2 or 4 in
    /// four-deck mode, else 1 or 2.
    pub fn active_deck(&self, side: usize) -> DeckId {
        let layer = self.four_decks() && self.layers[side.min(1)];
        DeckId::new(side.min(1) + if layer { 2 } else { 0 }).expect("a deck")
    }

    /// The decks the hands play.
    pub fn active_decks(&self) -> [DeckId; 2] {
        [self.active_deck(0), self.active_deck(1)]
    }

    /// A control for deck 1 or 2 (the keymap's left and right hands), sent to the deck
    /// that hand plays now. Anything else as it is.
    pub fn layered(&self, control: Control) -> Control {
        match control {
            Control::Deck(d, c) if d.index() < 2 => Control::Deck(self.active_deck(d.index()), c),
            other => other,
        }
    }

    /// Perform shows the mixer.
    pub fn mixer_shown(&self) -> bool {
        if self.mixer_hidden_by_controller() {
            self.mixer_with_controller
        } else {
            self.show_mixer
        }
    }

    /// A controller is connected, or one is pretended: the whole app lays itself out
    /// for one (the decks' pads, the mixer).
    pub fn controller(&self) -> bool {
        self.controller || self.pretend_controller
    }

    /// The connected controller decides whether the mixer shows (Settings > Decks).
    fn mixer_hidden_by_controller(&self) -> bool {
        self.auto_hide_mixer && self.controller()
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
        let was = self.controller();
        self.controller = connected;
        self.controller_changed(was);
    }

    /// Pretend a controller is connected, or stop.
    pub fn set_pretend_controller(&mut self, on: bool) {
        let was = self.controller();
        self.pretend_controller = on;
        self.controller_changed(was);
    }

    /// Each time one (real or pretend) comes, the mixer starts hidden.
    fn controller_changed(&mut self, was: bool) {
        if self.controller() && !was {
            self.mixer_with_controller = false;
        }
    }

    /// Bring what was saved by an older version up to date.
    pub fn migrate(&mut self) {
        self.ui_scale = ui_scale(self.ui_scale);
        for l in [
            &mut self.perform,
            &mut self.prepare,
            &mut self.controller_view,
            &mut self.debug_view,
        ] {
            l.sizes = l.sizes.as_shares();
        }
    }

    pub fn layout(&self) -> &Layout {
        match self.mode {
            Mode::Perform => &self.perform,
            Mode::Prepare => &self.prepare,
            Mode::Controller => &self.controller_view,
            Mode::Debug => &self.debug_view,
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
            Mode::Controller => &mut self.controller_view,
            Mode::Debug => &mut self.debug_view,
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

/// Panel sizes as last dragged, each as a share (0 to 1) of the row it's in, so they
/// keep their proportions in any window: `ui::layout` turns them into points, within
/// what there's room for.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(default)]
pub struct Sizes {
    /// The waveforms' row, of the window's height.
    pub waves: f32,
    /// The library's left pane (the source), of the library's width.
    pub source: f32,
    /// The library's right pane, of the library's width.
    pub aside: f32,
    /// The library beside a Prepare tool, of the two's width.
    pub flow_library: f32,
}

impl Default for Sizes {
    fn default() -> Sizes {
        Sizes {
            waves: 0.16,
            source: 0.17,
            aside: 0.24,
            flow_library: 0.4,
        }
    }
}

impl Sizes {
    /// Sizes saved by an older version were in points: those go back to the defaults.
    pub fn as_shares(self) -> Sizes {
        let d = Sizes::default();
        let share = |v: f32, d: f32| {
            if v.is_finite() && (0.0..=1.0).contains(&v) {
                v
            } else {
                d
            }
        };
        Sizes {
            waves: share(self.waves, d.waves),
            source: share(self.source, d.source),
            aside: share(self.aside, d.aside),
            flow_library: share(self.flow_library, d.flow_library),
        }
    }
}
