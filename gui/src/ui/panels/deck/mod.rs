//! One deck, laid out like Mixxx's: header and overview with a spinning platter, then
//! CUE and play, hot cue pads (or the beat grid editor), loop and beat jump buttons,
//! and on the right the BPM, SYNC and a tempo fader with nudge buttons. Its scrolling
//! waveform is drawn separately, in the stacked row above the decks.
//!
//! Drawing reads the engine's snapshot; every button becomes a [`Command`] (from the
//! control registry, where there is a control for it) that the app dispatches after
//! the frame, the same way as keyboard and MIDI.
//!
//! [`DeckUi`] keeps one player's view state; its parts are drawn by the modules
//! below. The panels draw them where a view puts them: [`DeckPanel`] (a Perform
//! deck), [`WavePanel`] (its scrolling waveform), [`PreparePlayer`] (Prepare's player
//! row) and [`CuePane`] (Prepare's memory cue list).

use std::sync::Arc;

use earshot_engine::beatgrid::Beatgrid;
use earshot_engine::{DeckCmd as EngineCmd, DeckId, DeckSnap};

use egui::{Align, Color32, FontId, Layout, Rect, RichText, Sense, Stroke, StrokeKind, Ui, vec2};

mod controller_pads;
mod cue_pane;
mod design;
mod grid_editor;
mod loops;
mod pads;
mod stems;
mod tempo;
mod transport;
pub mod wave;
mod waveform;

use design::*;

pub use self::cue_pane::CuePane;
pub use self::waveform::{WavePanel, zoomed};

use self::wave::{DeckWave, Grid, Layer, Marks, Overlay};
use crate::core::bus::{Control, DeckControl};
use crate::core::command::{Command, DeckCmd, ViewCmd};
use crate::core::pads::DeckPads;
use crate::core::store::TrackCues;
use crate::core::track::Track;
use crate::core::view::PadMode;
use crate::ui::flex::{self, Flex, Tui, TuiBuilderLogic, col, leaf, row};
use crate::ui::panels::library::drag;
use crate::ui::theme::THEME;
use crate::ui::util::{self, Camelot};
use crate::ui::widgets::key_chip_sized;

/// Which side of the mixer a deck sits on. Its tempo fader goes on the outside edge
/// and its controls run outwards from the mixer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Left,
    Right,
}

/// What a deck shows this frame, gathered by the app.
pub struct DeckInfo {
    pub title: String,
    pub artist: String,
    pub grid: Option<Arc<Beatgrid>>,
    pub generation: u64,
    pub waveform: Arc<Vec<u8>>,
    pub detail: Arc<Vec<u8>>,
    /// Each stem's waveform, once the track's stems are on the player.
    pub stem_waves: Option<Arc<crate::core::stems::StemWaves>>,
    /// The job working out this track's stems, while it has one.
    pub stems_job: Option<crate::core::stems::Status>,
    pub tag_bpm: Option<f64>,
    pub key: Option<Camelot>,
    /// (progress, stage) while this track is being analysed.
    pub analysing: Option<(f64, String)>,
    /// Beats a beat jump moves, and the loop button loops (from the deck's session).
    pub jump_size: f64,
    pub loop_size: f64,
    /// Synced decks follow this one (the mixer's chosen sync leader).
    pub leader: bool,
    /// The track's hot cue labels and memory cues, from the Store.
    pub cues: Arc<TrackCues>,
}

/// A sampler slot on a deck's pads page (`core::sampler`), as the app has it this
/// frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SamplePad {
    /// The slot, 0-based.
    pub slot: usize,
    /// Its sound's name (`None`: the slot is empty).
    pub name: Option<String>,
    /// The engine has its sound, and is playing it.
    pub loaded: bool,
    pub playing: bool,
    pub looping: bool,
}

/// A track asked for but still decoding. The deck shows its details from the library
/// straight away, and a placeholder where the waveforms go.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingLoad {
    /// The load's ticket (`player::request_load`): which request this is.
    pub ticket: u64,
    pub track_id: i64,
    pub title: String,
    pub artist: String,
    pub bpm: Option<f64>,
    pub key: Option<Camelot>,
    pub duration: Option<f64>,
}

impl PendingLoad {
    /// From the library's row for the track, if it has one.
    pub fn new(ticket: u64, track_id: i64, t: Option<&Track>) -> PendingLoad {
        PendingLoad {
            ticket,
            track_id,
            title: t.map(|t| t.title.clone()).unwrap_or_default(),
            artist: t.map(|t| t.artist.clone()).unwrap_or_default(),
            bpm: t.and_then(|t| t.bpm),
            key: t.and_then(|t| t.key),
            duration: t.and_then(|t| t.duration),
        }
    }

    fn info(&self) -> DeckInfo {
        DeckInfo {
            title: if self.title.is_empty() {
                "Loading…".into()
            } else {
                self.title.clone()
            },
            artist: self.artist.clone(),
            grid: None,
            generation: 0,
            waveform: Arc::default(),
            detail: Arc::default(),
            stem_waves: None,
            stems_job: None,
            tag_bpm: self.bpm,
            key: self.key,
            analysing: None,
            jump_size: 4.0,
            loop_size: 4.0,
            leader: false,
            cues: Arc::default(),
        }
    }
}

// Layout.
const TEMPO_W: f32 = 76.0;
/// Between the tempo fader and the rest of the deck.
const FADER_GAP: f32 = 8.0;
/// SYNC, ★, Q and KEYLOCK over the key shift, in the header (without a tempo fader):
/// two lines, no taller than the title and details beside them.
const BUTTONS_W: f32 = 2.0 * TEMPO_W + 4.0 + 6.0;

/// The controls row's scale: every size in it is [`design`]'s times `k`, so the row
/// grows and shrinks as one piece. The deck picks it from the box the layout gave it
/// ([`scale_to_fit`]): the biggest at which its controls fit.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Dims {
    k: f32,
}

impl Dims {
    const FULL: Dims = Dims { k: 1.0 };

    /// A design size, scaled.
    fn px(self, v: f32) -> f32 {
        v * self.k
    }

    /// The height of one row of buttons, or both (with the gap between them).
    fn single(self) -> f32 {
        self.px(design::BTN_H)
    }

    fn double(self) -> f32 {
        self.px(2.0 * design::BTN_H + design::GAP_Y)
    }

    /// The controls row: the two rows of buttons, or the jog ring if it's taller.
    fn controls_h(self) -> f32 {
        self.px(ROW_H)
    }

    fn jog(self) -> f32 {
        self.ring().px(design::JOG_D)
    }

    /// The jog ring's own scale: it and all in it are drawn bigger than the rest.
    fn ring(self) -> Dims {
        Dims {
            k: self.k * design::JOG_SCALE,
        }
    }
}

// ---- the deck's size, for the layout ----
//
// A deck is a frame round a header, the overview and the controls row, with the tempo
// fader beside them. Only the controls row scales; the rest is fixed. The layout gives
// a deck its design shape ([`aspect`]) at whatever width its row has, and the deck
// draws its controls at the biggest scale that fits that box ([`scale_to_fit`]).

/// The controls row's height at its design size: the jog ring's, which two rows of
/// buttons match.
const ROW_H: f32 = design::JOG_D * design::JOG_SCALE;
/// The on-screen player page's width (hot cues, loops and beat jumps) at its design
/// size: drawn wider, it's stretched to fill its box. Change it when the page gains
/// or loses a button (`a_deck_fits_the_box_it_is_given` fails if it's too small).
const PLAYER_PAGE_W: f32 = 351.0;
/// The other pages' (the grid editor's, a controller's pads) at their design size.
const PAGE_W: f32 = 361.0;
/// CUE / play and the jog ring beside them, at the design size.
const TRANSPORT_W: f32 = CUE_D + TRANSPORT_GAP + (JOG_D + JOG_LEFT) * JOG_SCALE;
/// The controls row's width at its design size: CUE / play, the widest page and the
/// tabs, with the gaps between them.
const CONTROLS_W: f32 = TRANSPORT_W + GROUP_GAP + PAGE_W + GROUP_GAP + TABS_W;
/// The most a page is stretched across its box, times its own width.
const PAGE_STRETCH_MAX: f32 = 1.3;
/// The frame's margins and border, and the gap kept inside it, across.
const FRAME_W: f32 = 18.0 + 6.0;
/// The frame's margins and border, the header and the gaps between the three, down.
const FIXED_H: f32 = 14.0 + HEADER_H + 2.0 * 5.0;
/// The header: the title over the BPM, or two lines of buttons.
const HEADER_H: f32 = 39.0;
/// The least a deck's panel is tall, whatever the scale: its tempo fader's height.
const MIN_H: f32 = 153.0;
/// The overview's least height, and its most: it grows with the scale between them.
const OVERVIEW_MIN_H: f32 = 24.0;
const OVERVIEW_MAX_H: f32 = 56.0;

/// The biggest the controls are drawn (a controls row 150 points tall), and the
/// smallest: under it some labels stop shrinking, and the row is wider than [`width`]
/// says (never reached by choice: the layout drops the mixer first).
pub const SCALE_MAX: f32 = 150.0 / ROW_H;
pub const SCALE_MIN: f32 = 0.45;

/// The smallest scale for a deck with or without its tempo fader: without, its header
/// holds the fader's buttons too, and needs the width.
pub fn scale_min(tempo_fader: bool) -> f32 {
    if tempo_fader { SCALE_MIN } else { 0.55 }
}

/// The overview's height at scale `k`.
fn overview_h(k: f32) -> f32 {
    (0.4 * ROW_H * k - 4.0).clamp(OVERVIEW_MIN_H, OVERVIEW_MAX_H)
}

/// A deck's width at scale `k`, with or without its tempo fader.
pub fn width(k: f32, tempo_fader: bool) -> f32 {
    let fader = if tempo_fader {
        TEMPO_W + FADER_GAP
    } else {
        0.0
    };
    FRAME_W + fader + k * CONTROLS_W
}

/// The biggest scale at which a deck is no wider than `w`.
pub fn scale_for_width(w: f32, tempo_fader: bool) -> f32 {
    // A hair under, so rounding never pushes the row a pixel past its place.
    (w - width(0.0, tempo_fader)) / CONTROLS_W * 0.995
}

/// A deck's height at scale `k`.
pub fn height(k: f32) -> f32 {
    (FIXED_H + overview_h(k) + ROW_H * k).max(MIN_H)
}

/// The shape a deck keeps as its row gives it more or less room: its width over its
/// height at its design size.
pub fn aspect(tempo_fader: bool) -> f32 {
    width(1.0, tempo_fader) / height(1.0)
}

/// The size of each deck when four show in a decks' row `row` big (inside its
/// margins; its height the most it may take): two to a side, `stack` apart, either
/// side of the mixer `mixer` wide (0 if none), `gap` between the columns and the
/// mixer. They keep their design shape ([`aspect`]), as wide as their column, or as
/// tall as the row lets two of them be.
pub fn four_deck_size(
    row: egui::Vec2,
    mixer: f32,
    gap: f32,
    stack: f32,
    tempo_fader: bool,
) -> egui::Vec2 {
    let gaps = if mixer > 0.0 { 2.0 } else { 1.0 } * gap;
    let w = ((row.x - mixer - gaps) / 2.0).max(0.0);
    let h = (w / aspect(tempo_fader)).min(((row.y - stack) / 2.0).max(0.0));
    vec2(w, h)
}

/// The biggest scale at which a deck's controls fit a deck `size` big (its frame
/// included): as wide as it is, under the header and the overview at its least.
pub fn scale_to_fit(size: egui::Vec2, tempo_fader: bool) -> f32 {
    let by_height = (size.y - FIXED_H - OVERVIEW_MIN_H) / ROW_H;
    scale_for_width(size.x, tempo_fader)
        .min(by_height)
        .clamp(scale_min(tempo_fader), SCALE_MAX)
}

/// The pages of a player's controls, picked with the tabs on their outer edge (as in
/// Rekordbox): playing (hot cues, loops, beat jumps), editing the grid, or its stems.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Page {
    Player,
    Grid,
    Stems,
}

impl Page {
    const ALL: [Page; 3] = [Page::Player, Page::Grid, Page::Stems];
}

/// What a label being typed belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LabelTarget {
    Memory(i64),
    Hotcue(u8),
}

/// A cue label being typed.
struct LabelEdit {
    target: LabelTarget,
    text: String,
    /// Focus still to be given to the field (the frame it opens).
    focus: bool,
}

impl LabelEdit {
    fn new(target: LabelTarget, label: Option<&str>) -> LabelEdit {
        LabelEdit {
            target,
            text: label.unwrap_or_default().to_owned(),
            focus: true,
        }
    }

    fn field(&mut self) -> egui::TextEdit<'_> {
        egui::TextEdit::singleline(&mut self.text)
            .font(FontId::proportional(11.0))
            .char_limit(64)
            .hint_text("Name")
    }

    /// After this frame's field: `Some(save)` once editing is over. Enter, or clicking
    /// elsewhere, saves; Esc cancels. While the field has focus the app's keymap stands
    /// aside (`wants_keyboard_input`), so Space, C or M type rather than play or cue.
    fn end(&mut self, ui: &Ui, r: &egui::Response) -> Option<bool> {
        if std::mem::take(&mut self.focus) {
            r.request_focus();
        }
        r.lost_focus()
            .then(|| !ui.input(|i| i.key_pressed(egui::Key::Escape)))
    }

    /// The command that saves it.
    fn save(self) -> DeckCmd {
        let label = Some(self.text);
        match self.target {
            LabelTarget::Memory(id) => DeckCmd::SetMemoryLabel(id, label),
            LabelTarget::Hotcue(slot) => DeckCmd::SetHotcueLabel(slot, label),
        }
    }
}

/// A drag on the scrolling waveform.
#[derive(Clone, Copy)]
struct Scrub {
    /// Where it started: the pointer's x and the time under it.
    x0: f32,
    t0: f64,
    /// The pointer's x as last sent.
    last: f32,
    /// In vinyl mode: a hand on the record (scratching). Otherwise the deck is held and
    /// the playhead moved silently, and `resume` says whether it plays on after.
    hand: bool,
    resume: bool,
}

pub struct DeckUi {
    pub id: DeckId,
    /// Smoothed playhead used for drawing.
    shown: f64,
    seen_generation: u64,
    /// While dragging the waveform.
    scrub: Option<Scrub>,
    pub wave: Option<DeckWave>,
    /// The stems' waveforms on the stems page, for the stems they were made from.
    stem_waves: Option<(Arc<crate::core::stems::StemWaves>, Vec<DeckWave>)>,
    /// Four decks, and the hands play this one (each frame): outlined.
    pub active: bool,
    /// What its stem buttons do (the setting, each frame).
    pub stem_mode: crate::core::view::StemMode,
    /// A controller is connected (or pretended), each frame: the player page shows
    /// the controller's pads instead of the on-screen hot cues, loops and jumps.
    pub controller: bool,
    /// What the controls between the tabs and CUE / play show.
    page: Page,
    /// The page box's width while the page shown is drawn: what a page that fills it
    /// fills.
    page_box: Option<f32>,
    /// The page box's width as last drawn.
    page_drawn: Option<f32>,
    /// How much wider than its own width the player page is drawn, to fill the box:
    /// its widths and the gaps across are times this (see [`DeckUi::wx`]); heights
    /// aren't.
    page_sx: f32,
    /// Where it was drawn last (see [`DeckUi::show`]).
    side: Side,
    /// Its buttons' size, as last drawn.
    dims: Dims,
    bpm_text: String,
    /// The engine's state for this frame.
    state: DeckSnap,
    /// A load in progress; see [`PendingLoad`].
    pending: Option<PendingLoad>,
    /// A memory cue or hot cue label being typed.
    edit: Option<LabelEdit>,
    /// Layers tools add over its waveform ([`DeckUi::set_overlay`]).
    overlays: Vec<Overlay>,
    /// The controller's pad mode on this deck: what its pads page shows.
    pub pad_mode: PadMode,
    /// Keyboard and key shift mode's pages and hot cue.
    pub pads: DeckPads,
    /// The controller's SHIFT is held: the pads page shows the pads' SHIFT actions.
    pub shift_held: bool,
    /// The sampler slots on this deck's pads (kept up to date while they show).
    pub sample_pads: [SamplePad; crate::core::sampler::PADS],
}

impl DeckUi {
    pub fn new(id: DeckId) -> DeckUi {
        DeckUi {
            overlays: Vec::new(),
            pad_mode: PadMode::default(),
            pads: DeckPads::default(),
            shift_held: false,
            sample_pads: Default::default(),
            id,
            shown: 0.0,
            seen_generation: u64::MAX,
            scrub: None,
            wave: None,
            stem_waves: None,
            stem_mode: Default::default(),
            active: false,
            controller: false,
            page: Page::Player,
            page_box: None,
            page_drawn: None,
            page_sx: 1.0,
            side: Side::Left,
            dims: Dims::FULL,
            bpm_text: String::new(),
            state: DeckSnap::default(),
            pending: None,
            edit: None,
        }
    }

    pub fn playing(&self) -> bool {
        self.state.playing
    }

    /// A load was asked for: show the track's details until it arrives. A newer load
    /// replaces an older one.
    pub fn start_load(&mut self, p: PendingLoad) {
        self.pending = Some(p);
    }

    /// The load with `ticket` finished (or failed). Returns false, and keeps waiting,
    /// unless it is the request shown.
    pub fn finish_load(&mut self, ticket: u64) -> bool {
        match &self.pending {
            Some(p) if p.ticket == ticket => {
                self.pending = None;
                true
            }
            _ => false,
        }
    }

    /// Right-click `r` for "Show in library", once a track is on the player.
    pub(super) fn library_menu(&self, r: &egui::Response, out: &mut Vec<Command>) {
        if self.pending.is_some() || self.state.duration <= 0.0 {
            return;
        }
        r.context_menu(|ui| {
            if ui
                .button("Show in library")
                .on_hover_text("Select it in the list it was loaded from")
                .clicked()
            {
                out.push(ViewCmd::ShowInLibrary(self.id).into());
                ui.close();
            }
        });
    }

    /// Moving by itself: a track loading, or the waveform being scrubbed.
    pub fn busy(&self) -> bool {
        self.pending.is_some() || self.scrub.is_some()
    }

    #[cfg(test)]
    pub fn pending(&self) -> Option<&PendingLoad> {
        self.pending.as_ref()
    }

    fn colour(&self) -> Color32 {
        THEME.deck_colour(self.id)
    }

    /// Send what one of this deck's controls sends for `value`.
    fn control(&self, out: &mut Vec<Command>, c: DeckControl, value: f64) {
        out.extend(Control::Deck(self.id, c).command(value));
    }

    /// A pad pressed or let go in keyboard, key shift or pad FX mode: what the deck is
    /// told.
    pub fn mode_pad(&mut self, mode: PadMode, pad: usize, shift: bool, down: bool) -> Vec<Command> {
        let state = self.state;
        self.pads
            .press(mode, pad, shift, down, &state)
            .into_iter()
            .map(|c| Command::Deck(self.id, DeckCmd::Engine(c)))
            .collect()
    }

    /// The pads change to `mode`: a pad FX pad held in the old one is let go.
    pub fn set_pad_mode(&mut self, mode: PadMode) -> Vec<Command> {
        self.pad_mode = mode;
        self.pads
            .release_fx()
            .into_iter()
            .map(|c| Command::Deck(self.id, DeckCmd::Engine(c)))
            .collect()
    }

    /// A deck command that isn't a registry control.
    fn send(&self, out: &mut Vec<Command>, c: DeckCmd) {
        out.push(Command::Deck(self.id, c));
    }

    /// A deck command straight for the engine.
    fn engine(&self, out: &mut Vec<Command>, c: EngineCmd) {
        self.send(out, DeckCmd::Engine(c));
    }

    /// Read the engine and advance the smoothed playhead. Once per frame, before drawing.
    pub fn tick(&mut self, ctx: &egui::Context, sh: Option<&DeckSnap>, info: &DeckInfo) {
        let Some(&s) = sh else { return };
        let dt = ctx.input(|i| i.stable_dt).min(0.1) as f64;
        if self.scrub.is_none() {
            let next = if s.playing {
                self.shown + dt * s.speed
            } else {
                s.position
            };
            // Follow the engine smoothly, but jump on seeks and loads.
            self.shown =
                if (s.position - next).abs() > 0.05 || s.generation != self.state.generation {
                    s.position
                } else {
                    next + (s.position - next) * 0.15
                };
        }
        self.state = s;
        if info.generation != self.seen_generation {
            self.seen_generation = info.generation;
            self.edit = None;
            self.wave = (!info.detail.is_empty()).then(|| {
                DeckWave::new(
                    ctx,
                    &format!("overview-{}", self.id.index()),
                    info.waveform.clone(),
                    info.detail.clone(),
                )
            });
        }
    }

    fn marks<'a>(&'a self, info: &'a DeckInfo, hotcues: &'a [Option<f64>]) -> Marks<'a> {
        Marks {
            grid: info.grid.as_deref().map_or(Grid::None, Grid::Beatgrid),
            cue: Some(self.state.cue_point),
            hotcues,
            hot_labels: &info.cues.hot_labels,
            memories: &info.cues.memories,
            loop_range: self.state.loop_in.zip(self.state.loop_out),
            loop_active: self.state.loop_active,
            overlays: &self.overlays,
            ..Marks::default()
        }
    }

    /// Put `layer` over the waveform as `key` (replacing the one there by that name),
    /// or take `key`'s away.
    pub fn set_overlay(&mut self, key: &'static str, layer: Option<Arc<dyn Layer>>) {
        self.overlays.retain(|o| o.key != key);
        if let Some(layer) = layer {
            self.overlays.push(Overlay { key, layer });
        }
    }

    /// The quiet stand-in for a waveform while a track decodes.
    fn loading(&self, ui: &Ui, rect: Rect) {
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 4.0, THEME.wave_bg);
        p.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Loading…",
            FontId::proportional(12.0),
            THEME.muted,
        );
    }

    // ---- the deck panel -----------------------------------------------------------------

    /// Draw the deck in its side's column of the decks' row: as tall as its width
    /// makes it (it keeps its design shape), or taller if the row is. Its controls'
    /// scale is the biggest at which they fit the box the layout gave it.
    pub fn show(
        &mut self,
        t: &mut Tui,
        layout: DeckLayout,
        info: &DeckInfo,
        out: &mut Vec<Command>,
    ) {
        let DeckLayout {
            side,
            tempo_fader,
            four,
        } = layout;
        self.side = side;
        let loading = self.pending.as_ref().map(PendingLoad::info);
        let info = loading.as_ref().unwrap_or(info);
        // As tall as a deck of four (`flex: 1 1 <four.y>`), and taller if the column is
        // (the overview takes the room).
        let mut style = leaf().grow();
        style.flex_basis = flex::taffy::style_helpers::length(four.y);
        let rect = flex::framed(t, style, THEME.panel_frame(), |t, rect| {
            // Never bigger than a deck of four, so every deck is drawn the same.
            self.dims = Dims {
                k: scale_to_fit(rect.size(), tempo_fader).min(scale_to_fit(four, tempo_fader)),
            };
            t.egui_style_mut().spacing.item_spacing = vec2(4.0, 5.0);
            // The tempo fader on the outside edge (its buttons in the header without it).
            t.style(row().grow().gap(FADER_GAP).reverse(side == Side::Right))
                .add(|t| {
                    if tempo_fader {
                        flex::pane(t, leaf().w(TEMPO_W), |ui| {
                            let h = ui.max_rect().height();
                            self.tempo(ui, info, out, h)
                        });
                    }
                    // The header, the overview taking what's spare, and the controls on
                    // the bottom edge.
                    t.style(col().grow().gap(5.0)).add(|t| {
                        flex::fit(t, leaf(), |ui| self.header(ui, info, out, !tempo_fader));
                        flex::pane(t, leaf().grow().min_h(OVERVIEW_MIN_H), |ui| {
                            let h = ui.max_rect().height();
                            self.overview(ui, info, out, h)
                        });
                        self.controls(t, info, out);
                    });
                });
            rect
        });
        // On top of what's in it: a track dropped anywhere on the deck loads.
        let ui = t.egui_ui_mut();
        let resp = ui.interact(
            rect,
            egui::Id::new(("deck-drop", self.id.index())),
            Sense::hover(),
        );
        if self.active {
            ui.painter().rect_stroke(
                rect,
                8.0,
                Stroke::new(1.5_f32, self.colour()),
                StrokeKind::Inside,
            );
        }
        if drag::hovering_tracks(&resp) {
            ui.painter().rect_stroke(
                rect,
                8.0,
                Stroke::new(2.0_f32, THEME.accent),
                StrokeKind::Inside,
            );
        }
        if let Some(id) = drag::take_track(&resp) {
            self.send(out, DeckCmd::Load(id));
        }
    }

    /// The deck number, title and times; with `buttons`, also SYNC, ★, Q and KEY (when
    /// there's no tempo fader to sit over). Right-click the title: "Show in library".
    fn header(&mut self, ui: &mut Ui, info: &DeckInfo, out: &mut Vec<Command>, buttons: bool) {
        let s = self.state;
        // While loading: the new track's length, from the top.
        let (position, duration) = match &self.pending {
            Some(p) => (0.0, p.duration.unwrap_or(0.0)),
            None => (s.position, s.duration),
        };
        ui.horizontal(|ui| {
            // Title and details on the left, as wide as the times (and buttons) allow.
            let right = 90.0 + if buttons { BUTTONS_W } else { 0.0 };
            let left = (ui.available_width() - right).max(60.0);
            ui.allocate_ui_with_layout(vec2(left, HEADER_H), Layout::top_down(Align::Min), |ui| {
                ui.set_width(left);
                let title = if info.artist.is_empty() || info.title.is_empty() {
                    info.title.clone()
                } else {
                    format!("{} - {}", info.title, info.artist)
                };
                ui.horizontal(|ui| {
                    // The key as it sounds: moved by the key shift, which shows beside it.
                    if let Some(k) = info.key {
                        let n = s.key_shift.round() as i32;
                        key_chip_sized(ui, crate::core::keys::transpose(k, n), 15.0);
                        if n != 0 {
                            ui.label(
                                RichText::new(format!("{n:+}"))
                                    .size(11.0)
                                    .color(THEME.muted),
                            )
                            .on_hover_text(format!(
                                "Key shifted {n:+} semitones from the track's {}",
                                util::key_name(k)
                            ));
                        }
                    }
                    let r = ui.add(
                        egui::Label::new(RichText::new(title).strong().size(15.0))
                            .truncate()
                            .sense(Sense::click()),
                    );
                    self.library_menu(&r, out);
                });
                ui.horizontal(|ui| {
                    if let Some(b) = info.tag_bpm {
                        ui.label(RichText::new(format!("{b:.2} BPM")).color(THEME.muted));
                    }
                    match &info.analysing {
                        Some((p, stage)) => {
                            ui.label(
                                RichText::new(format!("{stage}… {:.0}%", p * 100.0))
                                    .size(11.0)
                                    .color(THEME.warn),
                            );
                        }
                        None => {}
                    }
                });
            });
            if buttons {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing = vec2(4.0, 3.0);
                    ui.horizontal(|ui| {
                        self.sync_buttons(ui, info, out, 16.0);
                        self.mode_buttons(ui, out, 16.0);
                    });
                    self.key_shift_buttons(ui, out, 2.0 * TEMPO_W + 4.0, 16.0);
                });
            }
            ui.with_layout(Layout::top_down(Align::Max), |ui| {
                let ending = s.playing && self.pending.is_none() && duration - position < 30.0;
                let remain = if duration > 0.0 {
                    util::fmt_tenths(position - duration)
                } else {
                    "--:--.-".into()
                };
                ui.label(
                    RichText::new(remain)
                        .size(17.0)
                        .monospace()
                        .color(if ending { THEME.bad } else { THEME.text }),
                );
                ui.label(
                    RichText::new(util::fmt_tenths(position))
                        .size(11.0)
                        .monospace()
                        .color(THEME.muted),
                );
            });
        });
    }

    /// CUE / play and the BPM ring nearest the mixer (like Rekordbox), then the page
    /// (hot cue pads, loops and beat jumps, or the grid editor), taking the room to
    /// spare, and its tabs on the outer edge. A left deck's row is mirrored.
    fn controls(&mut self, t: &mut Tui, info: &DeckInfo, out: &mut Vec<Command>) {
        let mirrored = self.side == Side::Left;
        let d = self.dims;
        let style = row()
            .h(d.controls_h())
            .gap(d.px(GROUP_GAP))
            .reverse(mirrored);
        t.style(style).add(|t| {
            flex::pane(t, leaf().w(d.px(TRANSPORT_W)), |ui| {
                ui.spacing_mut().item_spacing = vec2(d.px(GAP_X), d.px(GAP_Y));
                ui.horizontal_top(|ui| self.transport(ui, out));
            });
            flex::pane(t, leaf().grow(), |ui| self.page(ui, info, out, mirrored));
            flex::pane(t, leaf().w(d.px(TABS_W)), |ui| self.page_tabs(ui));
        });
    }

    /// The page the tabs picked, filling the box between CUE / play and the tabs (the
    /// same box whichever page shows, so switching doesn't move anything). `mirrored`:
    /// the player page's groups run the other way (a left deck).
    fn page(&mut self, ui: &mut Ui, info: &DeckInfo, out: &mut Vec<Command>, mirrored: bool) {
        let page = self.page;
        let on_screen_player = page == Page::Player && !self.controller;
        // Its own width, and the most it's stretched to: past that, the room to spare
        // is left beside it, on the outer side.
        let own = self.dims.px(if on_screen_player {
            PLAYER_PAGE_W
        } else {
            PAGE_W
        });
        let room = ui.max_rect().width();
        let size = vec2(room.min(own * PAGE_STRETCH_MAX), self.dims.double());
        // The pads are an egui grid, which sizes its columns from the last pass: when
        // the box changes size, draw it again before it's shown.
        if self.page_drawn != Some(size.x) {
            self.page_drawn = Some(size.x);
            ui.ctx().request_discard("a deck's page changed size");
        }
        self.page_box = Some(size.x);
        // The on-screen player page is stretched from its own width to the box's; the
        // other pages fill the box themselves.
        self.page_sx = if on_screen_player {
            (size.x / own).max(1.0)
        } else {
            1.0
        };
        let x = if mirrored { room - size.x } else { 0.0 };
        let rect = Rect::from_min_size(ui.max_rect().min + vec2(x, 0.0), size);
        let builder = egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Min));
        ui.scope_builder(builder, |ui| {
            self.page_contents(ui, info, out, mirrored, page)
        });
        self.page_box = None;
        self.page_sx = 1.0;
    }

    /// A width on the page shown: a design size, scaled, and stretched to fill the
    /// page box (see `page_sx`).
    fn wx(&self, v: f32) -> f32 {
        self.dims.px(v) * self.page_sx
    }

    /// One page's controls; returns how wide they are.
    fn page_contents(
        &mut self,
        ui: &mut Ui,
        info: &DeckInfo,
        out: &mut Vec<Command>,
        mirrored: bool,
        page: Page,
    ) -> f32 {
        let d = self.dims;
        ui.spacing_mut().item_spacing = vec2(self.wx(GAP_X), d.px(GAP_Y));
        let start = ui.cursor().min.x;
        let group = self.wx(GROUP_GAP);
        match page {
            Page::Player if self.controller => self.controller_pads(ui, info, out),
            Page::Player => {
                if mirrored {
                    self.jumps(ui, info, out);
                    ui.add_space(group);
                    self.loops(ui, info, out);
                    ui.add_space(group);
                    self.pads(ui, info, out);
                } else {
                    self.pads(ui, info, out);
                    ui.add_space(group);
                    self.loops(ui, info, out);
                    ui.add_space(group);
                    self.jumps(ui, info, out);
                }
            }
            Page::Grid => self.grid_editor(ui, info, out),
            Page::Stems => self.stems(ui, info, out),
        }
        ui.cursor().min.x - start - ui.spacing().item_spacing.x
    }

    /// The tabs on the controls' outer edge, as tall as the row together: the player's
    /// controls, the grid editor, or the stems.
    fn page_tabs(&mut self, ui: &mut Ui) {
        let n = Page::ALL.len() as f32;
        let d = self.dims;
        let h = (d.controls_h() - d.px(TAB_GAP) * (n - 1.0)) / n;
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = d.px(TAB_GAP);
            for (page, tip) in [
                (Page::Player, "Hot cues, loops and beat jumps"),
                (Page::Grid, "Edit the beat grid"),
                (Page::Stems, "Stems: drums, bass, vocals and the rest"),
            ] {
                let (rect, r) = ui.allocate_exact_size(vec2(d.px(TABS_W), h), Sense::click());
                let on = self.page == page;
                let hovered = r.hovered();
                let p = ui.painter();
                let fill = if on { THEME.panel_2 } else { THEME.panel };
                p.rect_filled(rect, 3.0, fill);
                if on {
                    p.rect_stroke(
                        rect,
                        3.0,
                        Stroke::new(1.0_f32, THEME.line),
                        StrokeKind::Inside,
                    );
                }
                let ink = match (on, hovered) {
                    (true, _) => THEME.text,
                    (false, true) => THEME.text.gamma_multiply(0.8),
                    (false, false) => THEME.muted,
                };
                page_icon(p, rect.center(), page, ink);
                let r = r
                    .on_hover_text(tip)
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                r.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::WidgetType::Button,
                        true,
                        on,
                        match page {
                            Page::Player => "Player controls",
                            Page::Grid => "Grid controls",
                            Page::Stems => "Stem controls",
                        },
                    )
                });
                if r.clicked() {
                    self.page = page;
                }
            }
        });
    }
}

/// A tab's icon: four pads for the player, bars for the grid, layers for the stems.
fn page_icon(p: &egui::Painter, c: egui::Pos2, page: Page, ink: Color32) {
    match page {
        Page::Player => {
            for (dx, dy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                let at = c + vec2(dx * 3.5, dy * 3.5);
                p.rect_filled(egui::Rect::from_center_size(at, vec2(5.0, 5.0)), 1.0, ink);
            }
        }
        Page::Grid => {
            for (i, h) in [8.0, 12.0, 8.0, 12.0].into_iter().enumerate() {
                let x = c.x - 4.5 + i as f32 * 3.0;
                p.line_segment(
                    [egui::pos2(x, c.y - h / 2.0), egui::pos2(x, c.y + h / 2.0)],
                    Stroke::new(1.5_f32, ink),
                );
            }
        }
        Page::Stems => {
            for (i, w) in [10.0, 7.0, 11.0, 8.0].into_iter().enumerate() {
                let y = c.y - 4.5 + i as f32 * 3.0;
                p.line_segment(
                    [egui::pos2(c.x - w / 2.0, y), egui::pos2(c.x + w / 2.0, y)],
                    Stroke::new(1.5_f32, ink),
                );
            }
        }
    }
}

// ---- panels --------------------------------------------------------------------------

/// How a Perform deck is drawn.
#[derive(Clone, Copy, Debug)]
pub struct DeckLayout {
    /// Which side of the mixer it's on.
    pub side: Side,
    /// The tempo fader on its outer edge (off while the mixer's controls are hidden:
    /// a controller has its own); without it SYNC, Q and KEY go in the header.
    pub tempo_fader: bool,
    /// A deck's size when four are showing ([`four_deck_size`]): every deck is drawn
    /// at that size, whether one, two or four show.
    pub four: egui::Vec2,
}

#[cfg(test)]
mod tests {
    use super::loops::size_label;
    use super::*;
    use crate::core::store::Memory;

    fn pending(id: i64, title: &str) -> PendingLoad {
        PendingLoad {
            ticket: id as u64,
            track_id: id,
            title: title.into(),
            artist: "Someone".into(),
            bpm: Some(124.0),
            key: None,
            duration: Some(300.0),
        }
    }

    fn info(title: &str, cues: TrackCues) -> DeckInfo {
        DeckInfo {
            title: title.into(),
            artist: String::new(),
            grid: None,
            generation: 0,
            waveform: Arc::default(),
            detail: Arc::default(),
            stem_waves: None,
            stems_job: None,
            tag_bpm: None,
            key: None,
            analysing: None,
            jump_size: 4.0,
            loop_size: 4.0,
            leader: false,
            cues: Arc::new(cues),
        }
    }

    #[test]
    fn typing_a_cue_name_goes_to_the_field_not_the_keys() {
        use egui_kittest::kittest::Queryable;
        let memory = Memory {
            id: 3,
            start: 10.0,
            end: None,
            label: Some("Old".into()),
            color: None,
            active: false,
        };
        let info = info(
            "Track",
            TrackCues {
                memories: vec![memory],
                ..TrackCues::default()
            },
        );
        let mut h = egui_kittest::Harness::builder()
            .with_size(vec2(320.0, 160.0))
            .with_step_dt(1.0 / 60.0)
            .build_ui_state(
                move |ui, (deck, out): &mut (DeckUi, Vec<Command>)| deck.cue_pane(ui, &info, out),
                (DeckUi::new(DeckId::new(0).unwrap()), vec![]),
            );
        h.run();
        let recalls = |out: &[Command]| {
            out.iter()
                .filter(|c| matches!(c, Command::Deck(_, DeckCmd::RecallMemory(_))))
                .count()
        };
        // Double-click the row to name it (the first click goes there, as any click does).
        let name_it = |h: &mut egui_kittest::Harness<(DeckUi, Vec<Command>)>| {
            // One frame apart, well inside the double-click time.
            for _ in 0..2 {
                h.get_by_label("Memory cue at 00:10.0").click();
                h.step();
            }
            h.run();
        };
        name_it(&mut h);
        assert_eq!(recalls(&h.state().1), 1);
        assert!(
            h.ctx.wants_keyboard_input(),
            "the app's keymap stands aside while the field has focus"
        );

        // Space, C and M are letters in the name, not play, cue or save-to-memory.
        h.state_mut().1.clear();
        h.get_by_role(egui::accesskit::Role::TextInput)
            .type_text(" CM");
        h.run();
        assert!(h.ctx.wants_keyboard_input());
        assert!(h.state().1.is_empty(), "nothing sent while typing");
        h.key_press(egui::Key::Enter);
        h.run();
        assert!(!h.ctx.wants_keyboard_input());
        assert!(matches!(
            &h.state().1[..],
            [Command::Deck(_, DeckCmd::SetMemoryLabel(3, Some(l)))] if l == "Old CM"
        ));

        // Esc gives up on an edit. (A second apart, so it isn't a triple click.)
        h.run_steps(60);
        name_it(&mut h);
        h.state_mut().1.clear();
        h.get_by_role(egui::accesskit::Role::TextInput)
            .type_text("x");
        h.run();
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(!h.ctx.wants_keyboard_input());
        assert!(h.state().1.is_empty(), "nothing saved");
    }

    #[test]
    fn newest_load_wins() {
        let mut d = DeckUi::new(DeckId::new(0).unwrap());
        d.start_load(pending(1, "First"));
        d.start_load(pending(2, "Second"));
        assert!(!d.finish_load(1), "a stale load is ignored");
        assert_eq!(d.pending().map(|p| p.track_id), Some(2));
        assert!(d.finish_load(2));
        assert!(d.pending().is_none());
    }

    /// A harness drawing `deck` as the layout would, filling a window `size` big.
    fn deck_harness(
        deck: DeckUi,
        info: DeckInfo,
        size: egui::Vec2,
        layout: DeckLayout,
    ) -> egui_kittest::Harness<'static, DeckUi> {
        let h = egui_kittest::Harness::builder()
            .with_size(size)
            .build_ui_state(
                move |ui, deck: &mut DeckUi| {
                    flex::root(ui, "deck-test", col(), |t| {
                        deck.show(t, layout, &info, &mut vec![])
                    });
                },
                deck,
            );
        THEME.install(&h.ctx);
        h
    }

    #[test]
    fn loading_deck_shows_the_new_track_at_once() {
        use egui_kittest::kittest::Queryable;
        let mut deck = DeckUi::new(DeckId::new(0).unwrap());
        deck.start_load(pending(7, "Glide"));
        let info = info("Old track", TrackCues::default());
        let size = vec2(680.0, 200.0);
        let layout = DeckLayout {
            side: Side::Left,
            tempo_fader: true,
            four: size,
        };
        let mut h = deck_harness(deck, info, size, layout);
        h.run_steps(4);
        assert!(h.query_by_label("Glide - Someone").is_some());
        assert!(h.query_by_label("Old track").is_none());
        h.state_mut().finish_load(7);
        h.run_steps(4);
        assert!(h.query_by_label("Old track").is_some());
    }

    /// A deck given its size at a scale is drawn at that scale, and everything in it
    /// is inside it: on both sides, with and without the fader.
    #[test]
    fn a_deck_fits_the_box_it_is_given() {
        use egui_kittest::kittest::{NodeT, Queryable, by};
        for k in [0.6, 0.75, 1.0, SCALE_MAX] {
            for side in [Side::Left, Side::Right] {
                for tempo_fader in [false, true] {
                    let size = vec2(width(k, tempo_fader), height(k)).ceil();
                    let what = format!("scale {k}, {side:?}, fader {tempo_fader}");
                    assert!((scale_to_fit(size, tempo_fader) - k).abs() < 0.01, "{what}");
                    let deck = DeckUi::new(DeckId::new(0).unwrap());
                    let info = info("A track", TrackCues::default());
                    let layout = DeckLayout {
                        side,
                        tempo_fader,
                        four: size,
                    };
                    let mut h = deck_harness(deck, info, size, layout);
                    h.run_steps(6);
                    let window = egui::Rect::from_min_size(egui::Pos2::ZERO, size).expand(0.5);
                    for n in h.query_all(by()) {
                        let node = n.accesskit_node();
                        if node.role() == egui::accesskit::Role::TextRun {
                            continue;
                        }
                        let Some(b) = node.bounding_box() else {
                            continue;
                        };
                        let r = egui::Rect::from_min_max(
                            egui::pos2(b.x0 as f32, b.y0 as f32),
                            egui::pos2(b.x1 as f32, b.y1 as f32),
                        );
                        assert!(window.contains_rect(r), "{what}: {n:?} at {r:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn size_labels() {
        assert_eq!(size_label(0.25), "1/4");
        assert_eq!(size_label(8.0), "8");
    }
}
