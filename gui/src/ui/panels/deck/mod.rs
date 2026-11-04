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

pub use self::cue_pane::{CUE_PANE_W, CuePane};
pub use self::waveform::WavePanel;

use self::wave::{DeckWave, Grid, Marks};
use crate::core::bus::{Control, DeckControl};
use crate::core::command::{Command, DeckCmd, ViewCmd};
use crate::core::store::TrackCues;
use crate::core::track::Track;
use crate::ui::panels::library::drag;
use crate::ui::panels::{Cx, Panel};
use crate::ui::theme::THEME;
use crate::ui::util::{self, Camelot};
use crate::ui::widgets::{HwButton, held, key_chip_sized};

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
    pub grid_source: String,
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
            grid_source: String::new(),
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
/// SYNC and ★ over Q and KEY, in the header (without a tempo fader).
const BUTTONS_W: f32 = TEMPO_W + 6.0;

/// The controls row's scale: every size in it is [`design`]'s times `k`, so the row
/// grows and shrinks as one piece.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Dims {
    k: f32,
}

impl Dims {
    const FULL: Dims = Dims { k: 1.0 };

    /// While Perform shows four decks: the row this tall at least.
    fn compact() -> Dims {
        Dims::for_row(COMPACT_ROW)
    }

    /// The scale for a controls row `h` tall.
    fn for_row(h: f32) -> Dims {
        Dims { k: h / row_h() }
    }

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
        self.px(row_h())
    }

    fn jog(self) -> f32 {
        self.px(design::JOG_D)
    }
}

/// The controls row's height at its design size.
fn row_h() -> f32 {
    design::JOG_D.max(2.0 * design::BTN_H + design::GAP_Y)
}

/// The controls row's least height while Perform shows four decks.
const COMPACT_ROW: f32 = 60.0;

/// A deck panel's least height, full size or while Perform shows four decks. Given
/// more, its controls grow to fill it (the overview keeps its height).
pub fn min_height(compact: bool) -> f32 {
    if compact { 150.0 } else { 198.0 }
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
    /// What the controls between the tabs and CUE / play show.
    page: Page,
    /// Each page's width (player, grid) at `page_dims`, measured once, so the box is
    /// as wide as the wider page and switching doesn't move anything.
    page_widths: [Option<f32>; 3],
    page_dims: Dims,
    /// Width of the page and CUE / play last frame, to keep a left deck's against
    /// the mixer.
    controls_w: f32,
    /// Where it was drawn last (see [`DeckUi::show`]).
    side: Side,
    /// Its buttons' size, as last drawn.
    dims: Dims,
    /// The largest scale that fits the deck's width (the width it was found for, and
    /// the scale), once its controls turned out too wide for it.
    fit: Option<(f32, f32)>,
    bpm_text: String,
    /// The engine's state for this frame.
    state: DeckSnap,
    /// A load in progress; see [`PendingLoad`].
    pending: Option<PendingLoad>,
    /// A memory cue or hot cue label being typed.
    edit: Option<LabelEdit>,
}

impl DeckUi {
    pub fn new(id: DeckId) -> DeckUi {
        DeckUi {
            id,
            shown: 0.0,
            seen_generation: u64::MAX,
            scrub: None,
            wave: None,
            page: Page::Player,
            page_widths: [None; 3],
            page_dims: Dims::FULL,
            controls_w: 0.0,
            side: Side::Left,
            dims: Dims::FULL,
            fit: None,
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

    fn marks<'a>(&self, info: &'a DeckInfo, hotcues: &'a [Option<f64>]) -> Marks<'a> {
        Marks {
            grid: info.grid.as_deref().map_or(Grid::None, Grid::Beatgrid),
            cue: Some(self.state.cue_point),
            hotcues,
            hot_labels: &info.cues.hot_labels,
            memories: &info.cues.memories,
            loop_range: self.state.loop_in.zip(self.state.loop_out),
            loop_active: self.state.loop_active,
            ..Marks::default()
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

    /// Draw the deck as `layout` says.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        layout: DeckLayout,
        info: &DeckInfo,
        out: &mut Vec<Command>,
    ) {
        let DeckLayout {
            width,
            height,
            side,
            compact,
            tempo_fader,
        } = layout;
        self.side = side;
        // The controls as big as the panel's height allows, but no bigger than fits
        // its width: the frame, the tempo fader, the controls, the tabs and the least
        // gap between.
        let fader_w = if tempo_fader { TEMPO_W + 8.0 } else { 0.0 };
        let least = if compact { Dims::compact() } else { Dims::FULL };
        let extra = (height - min_height(compact)).max(0.0);
        let mut k = Dims::for_row(least.controls_h() + extra).k;
        if self.fit.is_some_and(|(w, _)| w != width) {
            self.fit = None;
        }
        let floor = Dims::compact().k;
        if let Some((_, cap)) = self.fit {
            k = k.min(cap).max(floor);
        }
        self.dims = Dims { k };
        self.panel(
            ui,
            width,
            height,
            info,
            out,
            |deck, ui, info, out, inner_h| {
                let left = if tempo_fader {
                    ui.available_width() - TEMPO_W - 8.0
                } else {
                    ui.available_width()
                };
                // The tempo fader on the outside edge (its buttons in the header without it).
                ui.horizontal_top(|ui| {
                    if tempo_fader && side == Side::Left {
                        deck.tempo(ui, info, out, inner_h);
                        ui.add_space(4.0);
                    }
                    ui.vertical(|ui| {
                        ui.set_width(left);
                        let top = ui.cursor().min.y;
                        deck.header(ui, info, out, !tempo_fader);
                        // The overview takes what the header and controls leave, so the
                        // controls sit on the bottom edge.
                        let used = ui.cursor().min.y - top;
                        let overview_h =
                            (inner_h - used - deck.dims.controls_h() - ui.spacing().item_spacing.y)
                                .max(24.0);
                        deck.overview(ui, info, out, overview_h);
                        deck.controls(ui, info, out);
                    });
                    if tempo_fader && side == Side::Right {
                        ui.add_space(4.0);
                        deck.tempo(ui, info, out, inner_h);
                    }
                });
            },
        );
        // Controls that turn out too wide: draw this frame again at the scale that
        // fits (everything in the row scales with it), so nothing is pushed off the
        // edge.
        let fixed = 18.0 + fader_w + 6.0;
        let tabs = TABS_W + GAP_X;
        if self.controls_w + k * tabs > width - fixed && k > floor {
            let cap = ((width - fixed) / (self.controls_w / k + tabs) * 0.995).max(floor);
            self.fit = Some((width, cap));
            ui.ctx().request_discard("a deck's controls didn't fit");
        }
    }

    /// The deck's panel, `width` × `height`, around a layout (`body`, given the
    /// track's details, or the one loading's, and the height inside). A track
    /// dropped anywhere on it loads, in every layout.
    fn panel(
        &mut self,
        ui: &mut Ui,
        width: f32,
        height: f32,
        info: &DeckInfo,
        out: &mut Vec<Command>,
        body: impl FnOnce(&mut DeckUi, &mut Ui, &DeckInfo, &mut Vec<Command>, f32),
    ) {
        let loading = self.pending.as_ref().map(PendingLoad::info);
        let info = loading.as_ref().unwrap_or(info);
        let resp = THEME
            .panel_frame()
            .show(ui, |ui| {
                ui.set_width(width - 18.0);
                let inner_h = height - 12.0;
                ui.set_min_height(inner_h);
                ui.spacing_mut().item_spacing = vec2(4.0, 5.0);
                body(self, ui, info, out, inner_h);
            })
            .response;
        if drag::hovering_tracks(&resp) {
            ui.painter().rect_stroke(
                resp.rect,
                8.0,
                Stroke::new(2.0_f32, THEME.accent),
                StrokeKind::Inside,
            );
        }
        if let Some(id) = drag::take_track(&resp) {
            self.send(out, DeckCmd::Load(id));
        }
    }

    /// Prepare mode's player: the title and overview, then CUE / play and the BPM
    /// ring, Q, and the page the tabs pick (hot cues, loops and beat jumps, or the
    /// grid editor), in one row, `width` ×
    /// `height`. Its memory cues are in the cue pane beside the waveform
    /// ([`DeckUi::cue_pane`]).
    pub fn show_prepare(
        &mut self,
        ui: &mut Ui,
        width: f32,
        height: f32,
        info: &DeckInfo,
        out: &mut Vec<Command>,
    ) {
        let extra = (height - min_height(false)).max(0.0);
        self.dims = Dims::for_row(row_h() + extra);
        self.panel(
            ui,
            width,
            height,
            info,
            out,
            |deck, ui, info, out, inner_h| {
                let top = ui.cursor().min.y;
                deck.header(ui, info, out, false);
                let used = ui.cursor().min.y - top;
                let overview_h =
                    (inner_h - used - deck.dims.controls_h() - ui.spacing().item_spacing.y)
                        .max(24.0);
                deck.overview(ui, info, out, overview_h);
                let d = deck.dims;
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing = vec2(d.px(GAP_X), d.px(GAP_Y));
                    deck.transport(ui, out);
                    // Quantise beside the transport, on either page (Rekordbox has it by
                    // the jog).
                    let s = deck.state;
                    let r = ui
                        .add(
                            HwButton::new("Q", THEME.accent, vec2(d.px(Q_W), d.px(Q_H)))
                                .font(d.px(FONT_Q))
                                .lit(s.quantize),
                        )
                        .on_hover_text("Quantise: snap cues, loops and hot cues to the beat");
                    if held(ui, &r) == Some(true) {
                        deck.control(out, DeckControl::Quantize, 1.0);
                    }
                    ui.add_space(d.px(GROUP_GAP));
                    deck.page(ui, info, out, false);
                    // The tabs at the row's outer edge, as on the decks.
                    let tabs = d.px(TABS_W) + ui.spacing().item_spacing.x;
                    ui.add_space((ui.available_width() - tabs).max(d.px(GROUP_GAP)));
                    deck.page_tabs(ui);
                });
            },
        );
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
            // Deck number (Prepare's single player has none).
            if !self.id.is_prepare() {
                ui.label(
                    RichText::new((self.id.index() + 1).to_string())
                        .strong()
                        .size(16.0)
                        .color(self.colour()),
                );
            }
            // Title and details on the left, as wide as the times (and buttons) allow.
            let right = 90.0 + if buttons { BUTTONS_W } else { 0.0 };
            let left = (ui.available_width() - right).max(60.0);
            ui.allocate_ui_with_layout(vec2(left, 36.0), Layout::top_down(Align::Min), |ui| {
                ui.set_width(left);
                let title = if info.title.is_empty() {
                    "Drop a track here"
                } else {
                    &info.title
                };
                let title = if info.artist.is_empty() || info.title.is_empty() {
                    title.to_string()
                } else {
                    format!("{title} - {}", info.artist)
                };
                ui.horizontal(|ui| {
                    if let Some(k) = info.key {
                        key_chip_sized(ui, k, 15.0);
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
                    ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
                    self.sync_buttons(ui, info, out);
                    self.mode_buttons(ui, out);
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
    /// (hot cue pads, loops and beat jumps, or the grid editor) and its tabs on the
    /// outer edge. A left deck's row is right-aligned and mirrored.
    fn controls(&mut self, ui: &mut Ui, info: &DeckInfo, out: &mut Vec<Command>) {
        let mirrored = self.side == Side::Left;
        let d = self.dims;
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing = vec2(d.px(GAP_X), d.px(GAP_Y));
            // The tabs at the deck's outer edge (by the tempo fader); the page and CUE /
            // play against the mixer. The space between takes what's left (the group's
            // width is from last frame).
            let gap = |ui: &mut Ui, group: f32| {
                let tabs = d.px(TABS_W) + ui.spacing().item_spacing.x;
                ui.add_space((ui.available_width() - group - tabs).max(d.px(GROUP_GAP)));
            };
            if mirrored {
                self.page_tabs(ui);
                gap(ui, self.controls_w);
                let start = ui.cursor().min.x;
                self.page(ui, info, out, true);
                ui.add_space(d.px(GROUP_GAP));
                self.transport(ui, out);
                self.controls_w = ui.cursor().min.x - start;
            } else {
                let start = ui.cursor().min.x;
                self.transport(ui, out);
                ui.add_space(d.px(GROUP_GAP));
                self.page(ui, info, out, false);
                self.controls_w = ui.cursor().min.x - start;
                gap(ui, 0.0);
                self.page_tabs(ui);
            }
        });
    }

    /// The page the tabs picked, in a fixed box (as wide as the widest page, as tall
    /// as the controls) so switching doesn't move or resize anything. `mirrored`: the
    /// player page's groups run the other way (a left deck).
    fn page(&mut self, ui: &mut Ui, info: &DeckInfo, out: &mut Vec<Command>, mirrored: bool) {
        if self.page_dims != self.dims {
            self.page_dims = self.dims;
            self.page_widths = [None; 3];
        }
        let h = self.dims.double();
        // The other pages, each measured once without being shown (nothing on them can
        // be clicked).
        for other in Page::ALL {
            if other == self.page || self.page_widths[other as usize].is_some() {
                continue;
            }
            // Room to spare, so nothing in it wraps or squeezes.
            let rect = Rect::from_min_size(ui.cursor().min, vec2(4000.0, h));
            let mut hidden = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(rect)
                    .layout(Layout::left_to_right(Align::Min))
                    .invisible(),
            );
            let mut ignored = vec![];
            let w = self.page_contents(&mut hidden, info, &mut ignored, mirrored, other);
            self.page_widths[other as usize] = Some(w);
        }
        let width = self
            .page_widths
            .iter()
            .flatten()
            .fold(0.0, |a: f32, b| a.max(*b));
        let size = vec2(width, h);
        let page = self.page;
        let used = ui
            .allocate_ui_with_layout(size, Layout::left_to_right(Align::Min), |ui| {
                ui.set_min_size(size);
                ui.set_max_height(h);
                self.page_contents(ui, info, out, mirrored, page)
            })
            .inner;
        // Never narrower than measured before (a hidden measure can come out a little
        // wider than the page drawn): the box mustn't shrink under the tabs.
        let known = self.page_widths[page as usize].unwrap_or(0.0);
        self.page_widths[page as usize] = Some(known.max(used));
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
        ui.spacing_mut().item_spacing = vec2(d.px(GAP_X), d.px(GAP_Y));
        let start = ui.cursor().min.x;
        match page {
            Page::Player => {
                if mirrored {
                    self.jumps(ui, info, out);
                    ui.add_space(d.px(GROUP_GAP));
                    self.loops(ui, info, out);
                    ui.add_space(d.px(GROUP_GAP));
                    self.pads(ui, info, out);
                } else {
                    self.pads(ui, info, out);
                    ui.add_space(d.px(GROUP_GAP));
                    self.loops(ui, info, out);
                    ui.add_space(d.px(GROUP_GAP));
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
    /// Its width, and at least its height.
    pub width: f32,
    pub height: f32,
    /// Which side of the mixer it's on.
    pub side: Side,
    /// Smaller buttons (while Perform shows four decks).
    pub compact: bool,
    /// The tempo fader on its outer edge (off while the mixer's controls are hidden:
    /// a controller has its own); without it SYNC, Q and KEY go in the header.
    pub tempo_fader: bool,
}

/// A Perform deck.
pub struct DeckPanel<'a> {
    pub deck: &'a mut DeckUi,
    pub layout: DeckLayout,
}

impl Panel for DeckPanel<'_> {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        crate::ui::theme::normal_text(ui);
        let info = cx.engine.deck(self.deck.id);
        self.deck.show(ui, self.layout, info, cx.out);
    }
}

/// Prepare's player controls, under its waveform in every tool, `width` × `height`.
pub struct PreparePlayer<'a> {
    pub deck: &'a mut DeckUi,
    pub width: f32,
    pub height: f32,
}

impl Panel for PreparePlayer<'_> {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        crate::ui::theme::normal_text(ui);
        let info = cx.engine.deck(self.deck.id);
        self.deck
            .show_prepare(ui, self.width, self.height, info, cx.out);
    }
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
            grid_source: String::new(),
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
                (DeckUi::new(DeckId::PREPARE), vec![]),
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

    #[test]
    fn loading_deck_shows_the_new_track_at_once() {
        use egui_kittest::kittest::Queryable;
        let mut deck = DeckUi::new(DeckId::new(0).unwrap());
        deck.start_load(pending(7, "Glide"));
        let info = info("Old track", TrackCues::default());
        let mut h = egui_kittest::Harness::builder()
            .with_size(vec2(700.0, 260.0))
            .build_ui_state(
                move |ui, deck: &mut DeckUi| {
                    let mut out = vec![];
                    let layout = DeckLayout {
                        width: 680.0,
                        height: 200.0,
                        side: Side::Left,
                        compact: false,
                        tempo_fader: true,
                    };
                    deck.show(ui, layout, &info, &mut out);
                },
                deck,
            );
        h.run_steps(2);
        assert!(h.query_by_label("Glide - Someone").is_some());
        assert!(h.query_by_label("Old track").is_none());
        h.state_mut().finish_load(7);
        h.run_steps(2);
        assert!(h.query_by_label("Old track").is_some());
    }

    #[test]
    fn size_labels() {
        assert_eq!(size_label(0.25), "1/4");
        assert_eq!(size_label(8.0), "8");
    }
}
