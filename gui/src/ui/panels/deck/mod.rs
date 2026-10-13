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
mod grid_editor;
mod loops;
mod pads;
mod tempo;
mod transport;
pub mod wave;
mod waveform;

pub use self::cue_pane::{CUE_PANE_W, CuePane};
pub use self::waveform::WavePanel;

use self::wave::{DeckWave, Grid, Marks};
use crate::core::bus::{Control, DeckControl};
use crate::core::command::{Command, DeckCmd};
use crate::core::store::TrackCues;
use crate::core::track::Track;
use crate::ui::panels::library::drag;
use crate::ui::panels::{Cx, Panel};
use crate::ui::theme::THEME;
use crate::ui::util::{self, Camelot};
use crate::ui::widgets::{HwButton, held, key_chip};

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

/// The size of a deck's buttons: full size, or smaller while Perform shows four decks.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Dims {
    btn_w: f32,
    btn_h: f32,
}

impl Dims {
    const FULL: Dims = Dims {
        btn_w: 34.0,
        btn_h: 40.0,
    };
    const COMPACT: Dims = Dims {
        btn_w: 30.0,
        btn_h: 28.0,
    };

    /// CUE and play: circles this wide, one above the other.
    fn transport(self) -> f32 {
        self.btn_h
    }

    /// Two rows of buttons.
    fn controls_h(self) -> f32 {
        2.0 * self.btn_h + 4.0
    }

    /// The jog ring (with the BPM in it) fills the height of the controls.
    fn jog(self) -> f32 {
        self.controls_h()
    }
}

/// The two pages of a player's controls, picked with the tabs on their outer edge
/// (as in Rekordbox): playing (hot cues, loops, beat jumps) or editing the grid.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Page {
    Player,
    Grid,
}

/// The tab strip's width.
const TABS_W: f32 = 22.0;

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

pub struct DeckUi {
    pub id: DeckId,
    /// Smoothed playhead used for drawing.
    shown: f64,
    seen_generation: u64,
    /// While dragging the waveform: (start x, start time, was playing).
    scrub: Option<(f32, f64, bool)>,
    pub wave: Option<DeckWave>,
    /// What the controls between the tabs and CUE / play show.
    page: Page,
    /// Each page's width (player, grid) at `page_dims`, measured once, so the box is
    /// as wide as the wider page and switching doesn't move anything.
    page_widths: [Option<f32>; 2],
    page_dims: Dims,
    /// Width of the controls row last frame, to right-align a left deck's.
    controls_w: f32,
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
            page_widths: [None; 2],
            page_dims: Dims::FULL,
            controls_w: 0.0,
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

    /// Draw the deck `width` points wide and at least `height` tall; `compact` with
    /// smaller buttons (while Perform shows four decks).
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut Ui,
        width: f32,
        height: f32,
        side: Side,
        compact: bool,
        info: &DeckInfo,
        out: &mut Vec<Command>,
    ) {
        self.side = side;
        self.dims = if compact { Dims::COMPACT } else { Dims::FULL };
        let loading = self.pending.as_ref().map(PendingLoad::info);
        let info = loading.as_ref().unwrap_or(info);
        let resp = THEME
            .panel_frame()
            .show(ui, |ui| {
                let inner = width - 18.0;
                ui.set_width(inner);
                ui.set_min_height(height - 12.0);
                ui.spacing_mut().item_spacing = vec2(4.0, 5.0);
                let left = inner - TEMPO_W - 8.0;
                let inner_h = height - 12.0;
                // The tempo fader on the outside edge.
                ui.horizontal_top(|ui| {
                    if side == Side::Left {
                        self.tempo(ui, info, out, inner_h);
                        ui.add_space(4.0);
                    }
                    ui.vertical(|ui| {
                        ui.set_width(left);
                        let top = ui.cursor().min.y;
                        self.header(ui, info);
                        // The overview takes what the header and controls leave, so the
                        // controls sit on the bottom edge.
                        let used = ui.cursor().min.y - top;
                        let overview_h =
                            (inner_h - used - self.dims.controls_h() - ui.spacing().item_spacing.y)
                                .max(24.0);
                        self.overview(ui, info, out, overview_h);
                        self.controls(ui, info, out);
                    });
                    if side == Side::Right {
                        ui.add_space(4.0);
                        self.tempo(ui, info, out, inner_h);
                    }
                });
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
        self.dims = Dims::FULL;
        let loading = self.pending.as_ref().map(PendingLoad::info);
        let info = loading.as_ref().unwrap_or(info);
        THEME.panel_frame().show(ui, |ui| {
            ui.set_width(width - 18.0);
            let inner_h = height - 12.0;
            ui.set_min_height(inner_h);
            ui.spacing_mut().item_spacing = vec2(4.0, 5.0);
            let top = ui.cursor().min.y;
            self.header(ui, info);
            let used = ui.cursor().min.y - top;
            let overview_h =
                (inner_h - used - self.dims.controls_h() - ui.spacing().item_spacing.y).max(24.0);
            self.overview(ui, info, out, overview_h);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing = vec2(3.0, 4.0);
                self.transport(ui, out);
                // Quantise beside the transport, on either page (Rekordbox has it by
                // the jog).
                let s = self.state;
                let r = ui
                    .add(
                        HwButton::new("Q", THEME.accent, vec2(24.0, 18.0))
                            .font(10.0)
                            .lit(s.quantize),
                    )
                    .on_hover_text("Quantise: snap cues, loops and hot cues to the beat");
                if held(ui, &r) == Some(true) {
                    self.control(out, DeckControl::Quantize, 1.0);
                }
                ui.add_space(6.0);
                self.page(ui, info, out, false);
                ui.add_space(6.0);
                self.page_tabs(ui);
            });
        });
    }

    fn header(&mut self, ui: &mut Ui, info: &DeckInfo) {
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
            // Title and details on the left, as wide as the times allow.
            let right = 90.0;
            let left = (ui.available_width() - right).max(60.0);
            ui.allocate_ui_with_layout(vec2(left, 36.0), Layout::top_down(Align::Min), |ui| {
                ui.set_width(left);
                let title = if info.title.is_empty() {
                    "Drop a track here"
                } else {
                    &info.title
                };
                ui.add(egui::Label::new(RichText::new(title).strong().size(15.0)).truncate());
                ui.horizontal(|ui| {
                    if let Some(k) = info.key {
                        key_chip(ui, k);
                    }
                    if let Some(b) = info.tag_bpm {
                        ui.label(RichText::new(format!("{b:.2}")).color(THEME.muted));
                    }
                    match &info.analysing {
                        Some((p, stage)) => {
                            ui.label(
                                RichText::new(format!("{stage}… {:.0}%", p * 100.0))
                                    .size(11.0)
                                    .color(THEME.warn),
                            );
                        }
                        None if !info.grid_source.is_empty() => {
                            ui.label(
                                RichText::new(format!("grid: {}", info.grid_source))
                                    .size(11.0)
                                    .color(THEME.muted),
                            );
                        }
                        None => {}
                    }
                    ui.add(
                        egui::Label::new(RichText::new(&info.artist).color(THEME.muted)).truncate(),
                    );
                });
            });
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
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing = vec2(3.0, 4.0);
            if mirrored {
                // Push the row against the mixer side (its width is from last frame).
                ui.add_space((ui.available_width() - self.controls_w).max(0.0));
            }
            let start = ui.cursor().min.x;
            if mirrored {
                self.page_tabs(ui);
                ui.add_space(6.0);
                self.page(ui, info, out, true);
                ui.add_space(6.0);
                self.transport(ui, out);
            } else {
                self.transport(ui, out);
                ui.add_space(6.0);
                self.page(ui, info, out, false);
                ui.add_space(6.0);
                self.page_tabs(ui);
            }
            self.controls_w = ui.cursor().min.x - start - ui.spacing().item_spacing.x;
        });
    }

    /// The page the tabs picked, in a fixed box (as wide as the widest page, as tall
    /// as the controls) so switching doesn't move or resize anything. `mirrored`: the
    /// player page's groups run the other way (a left deck).
    fn page(&mut self, ui: &mut Ui, info: &DeckInfo, out: &mut Vec<Command>, mirrored: bool) {
        if self.page_dims != self.dims {
            self.page_dims = self.dims;
            self.page_widths = [None; 2];
        }
        let h = self.dims.controls_h();
        // The other page, measured once without being shown (nothing on it can be
        // clicked).
        let other = match self.page {
            Page::Player => Page::Grid,
            Page::Grid => Page::Player,
        };
        if self.page_widths[other as usize].is_none() {
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
        ui.spacing_mut().item_spacing = vec2(3.0, 4.0);
        let start = ui.cursor().min.x;
        match page {
            Page::Player => {
                if mirrored {
                    self.jumps(ui, info, out);
                    ui.add_space(6.0);
                    self.loops(ui, info, out);
                    ui.add_space(6.0);
                    self.pads(ui, info, out);
                } else {
                    self.pads(ui, info, out);
                    ui.add_space(6.0);
                    self.loops(ui, info, out);
                    ui.add_space(6.0);
                    self.jumps(ui, info, out);
                }
            }
            Page::Grid => self.grid_editor(ui, info, out),
        }
        ui.cursor().min.x - start - ui.spacing().item_spacing.x
    }

    /// The tabs on the controls' outer edge: the player's controls, or the grid editor.
    fn page_tabs(&mut self, ui: &mut Ui) {
        let h = (self.dims.controls_h() - 4.0) / 2.0;
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            for (page, tip) in [
                (Page::Player, "Hot cues, loops and beat jumps"),
                (Page::Grid, "Edit the beat grid"),
            ] {
                let (rect, r) = ui.allocate_exact_size(vec2(TABS_W, h), Sense::click());
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

/// A tab's icon: four pads for the player, bars for the grid.
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
    }
}

// ---- panels --------------------------------------------------------------------------

/// A Perform deck, `width` × `height`, on the mixer's `side`; `compact` with smaller
/// buttons.
pub struct DeckPanel<'a> {
    pub deck: &'a mut DeckUi,
    pub width: f32,
    pub height: f32,
    pub side: Side,
    pub compact: bool,
}

impl Panel for DeckPanel<'_> {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let info = cx.engine.deck(self.deck.id);
        self.deck.show(
            ui,
            self.width,
            self.height,
            self.side,
            self.compact,
            info,
            cx.out,
        );
    }
}

/// Prepare's player row (the Cues & Grid tool), `width` × `height`.
pub struct PreparePlayer<'a> {
    pub deck: &'a mut DeckUi,
    pub width: f32,
    pub height: f32,
}

impl Panel for PreparePlayer<'_> {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
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
                    deck.show(ui, 680.0, 200.0, Side::Left, false, &info, &mut out);
                },
                deck,
            );
        h.run_steps(2);
        assert!(h.query_by_label("Glide").is_some());
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
