//! One deck, laid out like Mixxx's: header and overview with a spinning platter, then
//! CUE and play, hot cue pads (or the beat grid editor), loop and beat jump buttons,
//! and on the right the BPM, SYNC and a tempo fader with nudge buttons. Its scrolling
//! waveform is drawn separately, in the stacked row above the decks.
//!
//! Drawing reads the engine's snapshot; every button becomes a [`Command`] (from the
//! control registry, where there is a control for it) that the app dispatches after
//! the frame, the same way as keyboard and MIDI.
//!
//! [`DeckUi`] keeps one player's view state. The panels at the end draw its parts
//! where a view puts them: [`DeckPanel`] (a Perform deck), [`WavePanel`] (its
//! scrolling waveform), [`PreparePlayer`] (Prepare's player row) and [`CuePane`]
//! (Prepare's memory cue list).

use std::f32::consts::TAU;
use std::sync::Arc;

use earshot_engine::beatgrid::Beatgrid;
use earshot_engine::{DeckCmd as EngineCmd, DeckId, DeckSnap, LEAD_IN, Set, TempoRange};

use egui::{
    Align, Color32, FontId, Layout, Rect, RichText, Sense, Stroke, StrokeKind, Ui, pos2, vec2,
};

pub mod wave;

use self::wave::{DeckWave, Marks};
use crate::core::bus::{Control, DeckControl, JUMP_STEPS, deck_letter};
use crate::core::command::{Command, DeckCmd, MixerCmd, ViewCmd};
use crate::core::player::GridOp;
use crate::core::store::{Memory, TrackCues};
use crate::core::track::Track;
use crate::ui::panels::library::drag;
use crate::ui::panels::{Cx, Panel};
use crate::ui::theme::{self, THEME};
use crate::ui::util::{self, Camelot};
use crate::ui::widgets::{Fader, HwButton, Icon, Range, held, key_chip};

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
    pub track_id: i64,
    pub title: String,
    pub artist: String,
    pub bpm: Option<f64>,
    pub key: Option<Camelot>,
    pub duration: Option<f64>,
}

impl PendingLoad {
    /// From the library's row for the track, if it has one.
    pub fn new(track_id: i64, t: Option<&Track>) -> PendingLoad {
        PendingLoad {
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

/// The tempo fader, in the registry's units (-1..1 across the deck's range), and
/// the range markings beside it (it runs minus at the top, like Mixxx's).
fn tempo_fader(range: TempoRange) -> (&'static Range, &'static [(f64, &'static str)]) {
    const fn fader(format: fn(f64) -> String) -> Range {
        Range {
            name: "Tempo",
            min: -1.0,
            max: 1.0,
            default: 0.0,
            centre: Some(0.0),
            format,
        }
    }
    fn pct(rate: f64) -> String {
        format!("{:+.2} %", rate * 100.0)
    }
    const SIX: Range = fader(|v| pct(TempoRange::Six.rate(v)));
    const TEN: Range = fader(|v| pct(TempoRange::Ten.rate(v)));
    const SIXTEEN: Range = fader(|v| pct(TempoRange::Sixteen.rate(v)));
    const WIDE: Range = fader(|v| pct(TempoRange::Wide.rate(v)));
    match range {
        TempoRange::Six => (&SIX, &[(0.0, "-6"), (0.5, "0"), (1.0, "+6")]),
        TempoRange::Ten => (&TEN, &[(0.0, "-10"), (0.5, "0"), (1.0, "+10")]),
        TempoRange::Sixteen => (&SIXTEEN, &[(0.0, "-16"), (0.5, "0"), (1.0, "+16")]),
        TempoRange::Wide => (&WIDE, &[(0.0, "0"), (0.5, "1x"), (1.0, "2.5")]),
    }
}

// Layout.
const TEMPO_W: f32 = 76.0;
const BTN_H: f32 = 40.0;
const BTN_W: f32 = 34.0;
/// CUE and play: circles this wide, one above the other.
const TRANSPORT: f32 = BTN_H;
/// Two rows of buttons.
const CONTROLS_H: f32 = 2.0 * BTN_H + 4.0;
/// The jog ring (with the BPM in it) fills the height of the controls.
const JOG: f32 = CONTROLS_H;
/// Prepare's cue pane: its width beside the waveform, and a row's height.
pub const CUE_PANE_W: f32 = 300.0;
const CUE_ROW_H: f32 = 20.0;

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

fn size_label(s: f64) -> String {
    if s < 1.0 {
        format!("1/{}", (1.0 / s).round())
    } else {
        format!("{s}")
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
    grid_mode: bool,
    /// Width of the controls row last frame, to right-align a left deck's.
    controls_w: f32,
    /// Where it was drawn last (see [`DeckUi::show`]).
    side: Side,
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
            grid_mode: false,
            controls_w: 0.0,
            side: Side::Left,
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

    /// A load finished (or failed). Returns false, and keeps waiting, if it is for an
    /// older request than the one shown.
    pub fn finish_load(&mut self, track_id: i64) -> bool {
        match &self.pending {
            Some(p) if p.track_id != track_id => false,
            _ => {
                self.pending = None;
                true
            }
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
            grid: info.grid.as_deref(),
            cue: self.state.cue_point,
            hotcues,
            hot_labels: &info.cues.hot_labels,
            memories: &info.cues.memories,
            loop_range: self.state.loop_in.zip(self.state.loop_out),
            loop_active: self.state.loop_active,
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

    // ---- the scrolling waveform (drawn in the stacked row above the decks) -----------

    /// `half_window`: real seconds visible either side of the playhead, shared by both
    /// decks. Each shows `half_window × tempo` of its track, so both scroll at the same
    /// speed and beat markers are spaced by the tempo you hear. (Tempo, not speed: sync's
    /// phase corrections and nudges would make the zoom wobble.)
    pub fn show_wave(
        &mut self,
        ui: &mut Ui,
        height: f32,
        info: &DeckInfo,
        half_window: f64,
        out: &mut Vec<Command>,
    ) {
        let (rect, resp) =
            ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click_and_drag());
        if self.pending.is_some() {
            self.loading(ui, rect);
            if let Some(id) = drag::take_track(&resp) {
                self.send(out, DeckCmd::Load(id));
            }
            return;
        }
        let half = half_window
            * if self.state.tempo > 0.0 {
                self.state.tempo
            } else {
                1.0
            };
        let hotcues = self.state.hotcues;
        let marks = self.marks(info, &hotcues);
        let painter = ui.painter_at(rect);
        wave::scrolling(&painter, rect, self.wave.as_ref(), self.shown, half, &marks);
        painter.rect_filled(
            Rect::from_min_size(rect.min, vec2(3.0, rect.height())),
            0.0,
            self.colour(),
        );
        if let Some(label) = self.bar_beat(info) {
            painter.text(
                pos2(rect.center().x - 6.0, rect.top() + 3.0),
                egui::Align2::RIGHT_TOP,
                label,
                FontId::proportional(12.0),
                self.colour(),
            );
        }

        // Drag to scrub: the track moves with the pointer, like pushing a record. A
        // playing deck is held while you drag and carries on when you let go.
        let duration = self.state.duration;
        if resp.drag_started() && duration > 0.0 {
            let x = resp.interact_pointer_pos().map_or(0.0, |p| p.x);
            self.scrub = Some((x, self.shown, self.state.playing));
            if self.state.playing {
                self.engine(out, EngineCmd::Play(Set::Off));
            }
        }
        if let (Some((x0, t0, _)), Some(p)) = (self.scrub, resp.interact_pointer_pos())
            && resp.dragged()
        {
            let t = (t0 - (p.x - x0) as f64 * (2.0 * half / rect.width() as f64))
                .clamp(-LEAD_IN, duration);
            self.shown = t;
            self.control(out, DeckControl::Position, t / duration);
        }
        if resp.drag_stopped() {
            if let Some((_, _, true)) = self.scrub {
                self.engine(out, EngineCmd::Play(Set::On));
            }
            self.scrub = None;
        }
        if resp.hovered() {
            let scroll = ui.input(|i| i.raw_scroll_delta.y);
            if scroll != 0.0 {
                // About 7% per wheel notch.
                let zoom = (half_window * (-(scroll as f64) * 0.0014).exp()).clamp(0.5, 30.0);
                out.push(ViewCmd::WaveZoom(zoom).into());
            }
        }
        if let Some(id) = drag::take_track(&resp) {
            self.send(out, DeckCmd::Load(id));
        }
    }

    fn bar_beat(&self, info: &DeckInfo) -> Option<String> {
        // Counts on before the first 1 and before 0 s (the lead-in): 0, -1 ….
        let (bar, beat) = info.grid.as_ref()?.bar_beat(self.shown)?;
        Some(format!("{bar}.{beat} Bars"))
    }

    // ---- the deck panel -----------------------------------------------------------------

    /// Draw the deck `width` points wide and at least `height` tall.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        width: f32,
        height: f32,
        side: Side,
        info: &DeckInfo,
        out: &mut Vec<Command>,
    ) {
        self.side = side;
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
                            (inner_h - used - CONTROLS_H - ui.spacing().item_spacing.y).max(24.0);
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
    /// ring, hot cues, loops, beat jumps and the grid editor, all in one row, `width` ×
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
            let overview_h = (inner_h - used - CONTROLS_H - ui.spacing().item_spacing.y).max(24.0);
            self.overview(ui, info, out, overview_h);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing = vec2(3.0, 4.0);
                self.transport(ui, out);
                ui.add_space(6.0);
                self.pads(ui, info, out);
                ui.add_space(6.0);
                self.loops(ui, info, out);
                ui.add_space(6.0);
                self.jumps(ui, info, out);
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.set_height(CONTROLS_H);
                    ui.set_width(330.0);
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
                    self.grid_editor(ui, info, out);
                });
            });
        });
    }

    /// Prepare's cue pane, beside its waveform: the track's memory cues and loops in
    /// order through it, as Rekordbox lists them. Each row: colour, time (and a loop's
    /// length), name. Click a row to go there (a cue becomes the cue point; a loop
    /// starts), double-click to name it, click the colour to change it, × to forget it.
    pub fn cue_pane(&mut self, ui: &mut Ui, info: &DeckInfo, out: &mut Vec<Command>) {
        let deck = self.id;
        let memories = &info.cues.memories;
        let size = ui.available_size();
        // Top to bottom, whatever the layout it's placed in.
        let ui = &mut ui.new_child(egui::UiBuilder::new().layout(Layout::top_down(Align::Min)));
        egui::Frame::new()
            .fill(THEME.panel)
            .stroke(Stroke::new(1.0_f32, THEME.line))
            .corner_radius(4)
            .inner_margin(egui::Margin::symmetric(6, 4))
            .show(ui, |ui| {
                ui.set_width(size.x - 14.0);
                ui.set_height(size.y - 10.0);
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.horizontal(|ui| {
                    ui.label(RichText::new("MEMORY").size(10.0).color(THEME.muted));
                    let rest = vec2(ui.available_width(), 18.0);
                    ui.allocate_ui_with_layout(rest, Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .small_button("+ M")
                            .on_hover_text("Save the cue point, or the loop while looping (M)")
                            .clicked()
                        {
                            out.push(Command::Deck(deck, DeckCmd::SaveMemory));
                        }
                    });
                });
                egui::ScrollArea::vertical()
                    .id_salt(("memories", deck.index()))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if memories.is_empty() {
                            ui.label(
                                RichText::new("C sets a cue, M saves it here")
                                    .size(11.0)
                                    .color(THEME.muted),
                            );
                        }
                        for m in memories {
                            self.cue_row(ui, m, info.grid.as_deref(), out);
                        }
                    });
            });
    }

    /// One memory cue or loop in the cue pane.
    fn cue_row(
        &mut self,
        ui: &mut Ui,
        m: &Memory,
        grid: Option<&Beatgrid>,
        out: &mut Vec<Command>,
    ) {
        let deck = self.id;
        let (rect, row) =
            ui.allocate_exact_size(vec2(ui.available_width(), CUE_ROW_H), Sense::click());
        let p = ui.painter_at(rect);
        if row.hovered() {
            p.rect_filled(rect, 3.0, THEME.panel_2);
        }
        let (cy, left) = (rect.center().y, rect.left());
        let colour = THEME.memory_colour(m);

        // The colour: a square for a cue, a wider bar for a loop. Click to pick one.
        let swatch = Rect::from_center_size(
            pos2(left + 9.0, cy),
            if m.end.is_some() {
                vec2(14.0, 8.0)
            } else {
                vec2(10.0, 10.0)
            },
        );
        let pick = ui
            .interact(swatch.expand(3.0), row.id.with("colour"), Sense::click())
            .on_hover_text("Colour");
        p.rect_filled(swatch, 2.0, colour);
        if pick.hovered() {
            p.rect_stroke(
                swatch.expand(1.5),
                2.0,
                Stroke::new(1.0_f32, THEME.text),
                StrokeKind::Outside,
            );
        }
        egui::Popup::menu(&pick).show(|ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                for (rgb, name) in theme::CUE_PALETTE {
                    let (r, resp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
                    ui.painter()
                        .rect_filled(r.shrink(1.0), 3.0, theme::rgb(rgb));
                    if m.color == Some(rgb) {
                        ui.painter().rect_stroke(
                            r,
                            3.0,
                            Stroke::new(1.5_f32, THEME.text),
                            StrokeKind::Inside,
                        );
                    }
                    if resp.on_hover_text(name).clicked() {
                        out.push(Command::Deck(
                            deck,
                            DeckCmd::SetMemoryColor(m.id, Some(rgb)),
                        ));
                        ui.close();
                    }
                }
            });
            if m.color.is_some() && ui.button("Default colour").clicked() {
                out.push(Command::Deck(deck, DeckCmd::SetMemoryColor(m.id, None)));
                ui.close();
            }
        });

        // The time, and a loop's length (in beats when there's a grid).
        let font = FontId::monospace(11.0);
        p.text(
            pos2(left + 22.0, cy),
            egui::Align2::LEFT_CENTER,
            util::fmt_tenths(m.start),
            font,
            THEME.text,
        );
        if let Some(end) = m.end {
            let len = match grid {
                Some(g) => {
                    let beats = g.beat_at(end) - g.beat_at(m.start);
                    format!("{} beats", (beats * 4.0).round() / 4.0)
                }
                None => format!("{:.1} s", end - m.start),
            };
            p.text(
                pos2(left + 76.0, cy),
                egui::Align2::LEFT_CENTER,
                len,
                FontId::proportional(10.0),
                THEME.muted,
            );
        }

        // The name, or the field to type it in.
        let name = Rect::from_x_y_ranges(left + 128.0..=rect.right() - 18.0, rect.y_range());
        let editing = self
            .edit
            .as_ref()
            .is_some_and(|e| e.target == LabelTarget::Memory(m.id));
        if editing {
            let edit = self.edit.as_mut().expect("editing");
            let r = ui.put(name.shrink2(vec2(0.0, 1.0)), edit.field());
            if let Some(save) = edit.end(ui, &r) {
                let edit = self.edit.take().expect("editing");
                if save {
                    out.push(Command::Deck(deck, edit.save()));
                }
            }
        } else if let Some(label) = &m.label {
            ui.painter_at(name).text(
                pos2(name.left(), cy),
                egui::Align2::LEFT_CENTER,
                label,
                FontId::proportional(11.0),
                THEME.text,
            );
        }

        let forget = ui
            .put(
                Rect::from_center_size(pos2(rect.right() - 8.0, cy), vec2(14.0, CUE_ROW_H)),
                egui::Label::new(RichText::new("×").color(THEME.muted)).sense(Sense::click()),
            )
            .on_hover_text("Forget this one");
        if forget.clicked() {
            out.push(Command::Deck(deck, DeckCmd::DeleteMemory(m.id)));
        }
        row.widget_info(|| {
            let what = if m.end.is_some() { "loop" } else { "cue" };
            let at = util::fmt_tenths(m.start);
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                true,
                format!("Memory {what} at {at}"),
            )
        });
        let row = row.on_hover_text("Click: go here · Double-click: name it");
        if row.double_clicked() {
            self.edit = Some(LabelEdit::new(
                LabelTarget::Memory(m.id),
                m.label.as_deref(),
            ));
        } else if row.clicked() && !editing {
            out.push(Command::Deck(deck, DeckCmd::RecallMemory(m.clone())));
        }
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

    /// A jog-wheel ring, like Rekordbox's: the BPM at the current tempo in the middle,
    /// the tempo change and fader range under it, and a dot on the rim that turns like
    /// a record at 33⅓ rpm (1.8 s a turn).
    fn jog(&self, ui: &mut Ui) {
        let s = self.state;
        let (rect, resp) = ui.allocate_exact_size(vec2(JOG, JOG), Sense::hover());
        resp.on_hover_text("BPM at the current tempo; the tempo change and the fader's range");
        let p = ui.painter();
        let c = rect.center();
        let r = JOG / 2.0 - 2.0;
        p.circle(c, r, THEME.jog_face, Stroke::new(3.0_f32, self.colour()));
        if self.pending.is_none() {
            let a = ((self.shown / 1.8).fract() as f32) * TAU - TAU / 4.0;
            p.circle_filled(c + vec2(a.cos(), a.sin()) * r, 3.5, THEME.jog_dot);
        }
        let bpm = match &self.pending {
            Some(pl) => pl.bpm.map(|b| b * (1.0 + s.rate)),
            None => s.bpm,
        };
        // "124" large, ".00" small, sitting on one baseline.
        let (whole, frac) = match bpm {
            Some(b) => {
                let t = format!("{b:.2}");
                let (w, f) = t.split_at(t.len() - 3);
                (w.to_string(), f.to_string())
            }
            None => ("-".into(), String::new()),
        };
        let big = p.layout_no_wrap(whole, FontId::monospace(20.0), THEME.text);
        let small = p.layout_no_wrap(frac, FontId::monospace(12.0), THEME.text);
        let w = big.size().x + small.size().x;
        let top = c.y - big.size().y + 4.0;
        let left = c.x - w / 2.0;
        let small_top = top + big.size().y - small.size().y - 2.0;
        p.galley(pos2(left, top), big.clone(), THEME.text);
        p.galley(pos2(left + big.size().x, small_top), small, THEME.text);
        let pct = p.layout_no_wrap(
            format!("{:+.1}%", s.rate * 100.0),
            FontId::monospace(9.5),
            THEME.muted,
        );
        let range = p.layout_no_wrap(
            format!(" {}", s.range.label()),
            FontId::monospace(9.5),
            self.colour(),
        );
        let w = pct.size().x + range.size().x;
        let y = c.y + 7.0;
        p.galley(pos2(c.x - w / 2.0, y), pct.clone(), THEME.muted);
        p.galley(pos2(c.x - w / 2.0 + pct.size().x, y), range, self.colour());
    }

    fn overview(&mut self, ui: &mut Ui, info: &DeckInfo, out: &mut Vec<Command>, height: f32) {
        let (rect, resp) =
            ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click_and_drag());
        if self.pending.is_some() {
            self.loading(ui, rect);
            return;
        }
        let hotcues = self.state.hotcues;
        let marks = self.marks(info, &hotcues);
        wave::overview(
            &ui.painter_at(rect),
            rect,
            self.wave.as_ref(),
            self.shown,
            self.state.duration,
            &marks,
            info.analysing.as_ref().map(|a| a.0),
        );
        if (resp.clicked() || resp.dragged())
            && let Some(p) = resp.interact_pointer_pos()
        {
            let at = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64;
            self.control(out, DeckControl::Position, at);
        }
    }

    /// CUE / play and the BPM ring nearest the mixer (like Rekordbox), then the hot cue
    /// pads (or the grid editor), loops and beat jumps towards the outside. A left
    /// deck's row is right-aligned, with those groups in the opposite order.
    fn controls(&mut self, ui: &mut Ui, info: &DeckInfo, out: &mut Vec<Command>) {
        #[derive(Clone, Copy)]
        enum Part {
            Transport,
            Pads,
            Loops,
            Jumps,
            GridEditor,
            GridButton,
            Gap(f32),
        }
        use Part::*;
        // CUE / play with the ring on the mixer side; the rest runs outwards from there.
        let centre = [Transport];
        let mut outer = vec![Gap(6.0)];
        if self.grid_mode {
            outer.push(GridEditor);
        } else {
            outer.extend([Pads, Gap(6.0), Loops, Gap(6.0), Jumps]);
        }
        outer.extend([Gap(6.0), GridButton]);
        let mirrored = self.side == Side::Left;
        let parts: Vec<Part> = if mirrored {
            outer.into_iter().rev().chain(centre).collect()
        } else {
            centre.into_iter().chain(outer).collect()
        };
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing = vec2(3.0, 4.0);
            if mirrored {
                // Push the row against the mixer side (its width is from last frame).
                ui.add_space((ui.available_width() - self.controls_w).max(0.0));
            }
            let start = ui.cursor().min.x;
            for part in parts {
                match part {
                    Transport => self.transport(ui, out),
                    Pads => self.pads(ui, info, out),
                    Loops => self.loops(ui, info, out),
                    Jumps => self.jumps(ui, info, out),
                    GridEditor => {
                        ui.vertical(|ui| {
                            ui.set_height(CONTROLS_H);
                            self.grid_editor(ui, info, out);
                        });
                    }
                    GridButton => {
                        let r = ui
                            .add(
                                HwButton::new("GRID", THEME.accent, vec2(BTN_W + 4.0, BTN_H))
                                    .font(10.0)
                                    .lit(self.grid_mode),
                            )
                            .on_hover_text("Edit the beat grid (in place of the hot cue pads)");
                        if r.clicked() {
                            self.grid_mode = !self.grid_mode;
                        }
                    }
                    Gap(w) => ui.add_space(w),
                }
            }
            self.controls_w = ui.cursor().min.x - start - ui.spacing().item_spacing.x;
        });
    }

    /// One unit, like Rekordbox's: CUE above play, and the BPM ring to their right.
    fn transport(&mut self, ui: &mut Ui, out: &mut Vec<Command>) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            self.cue_play(ui, out);
            self.jog(ui);
        });
    }

    fn cue_play(&mut self, ui: &mut Ui, out: &mut Vec<Command>) {
        ui.vertical(|ui| {
            let size = vec2(TRANSPORT, TRANSPORT);
            // CUE lights while the deck sits at its cue point, ready to go.
            let at_cue = !self.state.playing
                && self.state.duration > 0.0
                && (self.state.position - self.state.cue_point).abs() < 0.01;
            let r = ui
                .add(
                    HwButton::new("CUE", THEME.cue_button, size)
                        .round(true)
                        .font(11.0)
                        .tinted(true)
                        .lit(at_cue && self.pending.is_none()),
                )
                .on_hover_text("Cue: set, or jump back and preview while held");
            if let Some(d) = held(ui, &r) {
                self.control(out, DeckControl::Cue, d as u8 as f64);
            }
            let playing = self.state.playing;
            let r = ui
                .add(
                    HwButton::new("Play", THEME.play_button, size)
                        .round(true)
                        .icon(Icon::PlayPause)
                        .font(12.0)
                        .tinted(true)
                        .lit(playing),
                )
                .on_hover_text("Play / pause");
            if held(ui, &r) == Some(true) {
                self.control(out, DeckControl::Play, 1.0);
            }
        });
    }

    /// The 8 hot cue pads, each with its label under the number. Right-click one to
    /// name it.
    fn pads(&mut self, ui: &mut Ui, info: &DeckInfo, out: &mut Vec<Command>) {
        let shift = ui.input(|i| i.modifiers.shift);
        egui::Grid::new(("pads", self.id.index()))
            .spacing(vec2(3.0, 4.0))
            .show(ui, |ui| {
                for row in 0..2 {
                    for col in 0..4 {
                        let i = row * 4 + col;
                        let set = self.state.hotcues[i];
                        // Until the Store answers, a pad just cleared may still have
                        // a label here.
                        let name = info.cues.hot_labels[i].as_deref().filter(|_| set.is_some());
                        let label = (i + 1).to_string();
                        let tip = match set {
                            Some(t) => format!(
                                "Hot cue {}{} at {}\nClick: jump (held: preview) · Shift-click: \
                                 clear · Right-click: name",
                                i + 1,
                                name.map(|n| format!(" \"{n}\"")).unwrap_or_default(),
                                util::fmt_tenths(t)
                            ),
                            None => format!("Hot cue {}: not set\nClick to set it here", i + 1),
                        };
                        let r = ui
                            .add(
                                HwButton::new(&label, THEME.hotcue[i], vec2(BTN_W, BTN_H))
                                    .font(12.0)
                                    .caption(name)
                                    .tinted(set.is_some()),
                            )
                            .on_hover_text(tip);
                        let slot = i as u8;
                        r.context_menu(|ui| {
                            if set.is_none() {
                                ui.label(RichText::new("Set the hot cue first").color(THEME.muted));
                                return;
                            }
                            if ui.button("Label…").clicked() {
                                self.edit = Some(LabelEdit::new(LabelTarget::Hotcue(slot), name));
                                ui.close();
                            }
                            if name.is_some() && ui.button("Clear label").clicked() {
                                self.send(out, DeckCmd::SetHotcueLabel(slot, None));
                                ui.close();
                            }
                        });
                        match held(ui, &r) {
                            Some(true) if shift => {
                                self.control(out, DeckControl::HotcueClear(slot), 1.0)
                            }
                            Some(down) => {
                                self.control(out, DeckControl::Hotcue(slot), down as u8 as f64)
                            }
                            None => {}
                        }
                    }
                    ui.end_row();
                }
            });
        self.hotcue_label_dialog(ui, out);
    }

    /// The box a hot cue's label is typed in, over everything, while one is.
    fn hotcue_label_dialog(&mut self, ui: &Ui, out: &mut Vec<Command>) {
        let Some(edit) = self.edit.as_mut() else {
            return;
        };
        let LabelTarget::Hotcue(slot) = edit.target else {
            return;
        };
        let modal = egui::Modal::new(egui::Id::new(("hotcue-label", self.id.index()))).show(
            ui.ctx(),
            |ui| {
                ui.label(RichText::new(format!("Name hot cue {}", slot + 1)).strong());
                let r = ui.add(edit.field().desired_width(200.0));
                let end = edit.end(ui, &r);
                ui.label(
                    RichText::new("Enter saves · Esc cancels")
                        .size(10.0)
                        .color(THEME.muted),
                );
                end
            },
        );
        // Clicking outside it saves, like clicking away from a memory cue's name.
        let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
        let end = modal.inner.or(modal.should_close().then_some(!esc));
        if let Some(save) = end {
            let edit = self.edit.take().expect("editing");
            if save {
                self.send(out, edit.save());
            }
        }
    }

    /// A loop button `w` wide that sends `control` 1 on press and 0 on release.
    #[allow(clippy::too_many_arguments)]
    fn hold_button(
        &self,
        ui: &mut Ui,
        w: f32,
        label: &str,
        control: DeckControl,
        lit: bool,
        tip: &str,
        out: &mut Vec<Command>,
    ) {
        let r = ui
            .add(
                HwButton::new(label, THEME.loop_button, vec2(w, BTN_H))
                    .font(if label.len() > 4 { 9.0 } else { 10.0 })
                    .lit(lit),
            )
            .on_hover_text(tip);
        if let Some(d) = held(ui, &r) {
            self.control(out, control, d as u8 as f64);
        }
    }

    /// Loop: ½ / LOOP n / ×2 over IN / OUT / EXIT. With no loop running, ½ and ×2
    /// change the size the next loop will be, like Mixxx.
    fn loops(&mut self, ui: &mut Ui, info: &DeckInfo, out: &mut Vec<Command>) {
        let active = self.state.loop_active;
        let size = size_label(info.loop_size);
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                for (label, control) in [
                    ("½", DeckControl::LoopHalve),
                    ("×2", DeckControl::LoopDouble),
                ] {
                    if control == DeckControl::LoopDouble {
                        let r = ui
                            .add(
                                HwButton::new(
                                    &format!("⟲ {size}"),
                                    THEME.loop_button,
                                    vec2(BTN_W + 10.0, BTN_H),
                                )
                                .font(10.0)
                                .lit(active),
                            )
                            .on_hover_text(format!(
                                "Loop {size} beats from the current beat (or end the loop)"
                            ));
                        if let Some(d) = held(ui, &r) {
                            self.control(out, DeckControl::Loop, d as u8 as f64);
                        }
                    }
                    let tip = if control == DeckControl::LoopHalve {
                        "Halve"
                    } else {
                        "Double"
                    };
                    let r = ui
                        .add(HwButton::new(label, THEME.loop_button, vec2(BTN_W, BTN_H)).font(11.0))
                        .on_hover_text(format!("{tip} the loop (or the next loop's size)"));
                    if held(ui, &r) == Some(true) {
                        self.control(out, control, 1.0);
                    }
                }
            });
            // As wide as the row above.
            let w = (3.0 * BTN_W + 10.0) / 3.0;
            ui.horizontal(|ui| {
                self.hold_button(
                    ui,
                    w,
                    "IN",
                    DeckControl::LoopIn,
                    false,
                    "Loop in point",
                    out,
                );
                self.hold_button(
                    ui,
                    w,
                    "OUT",
                    DeckControl::LoopOut,
                    false,
                    "Loop out point: starts the loop",
                    out,
                );
                self.hold_button(
                    ui,
                    w,
                    if active { "EXIT" } else { "RELOOP" },
                    DeckControl::Reloop,
                    active,
                    "Leave the loop, or go back into the last one",
                    out,
                );
            });
        });
    }

    /// Beat jump: ◀ n / n ▶ over the step picker.
    fn jumps(&mut self, ui: &mut Ui, info: &DeckInfo, out: &mut Vec<Command>) {
        let n = size_label(info.jump_size);
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                for (label, control, tip) in [
                    (format!("◀ {n}"), DeckControl::JumpBack, "Jump back"),
                    (format!("{n} ▶"), DeckControl::JumpForward, "Jump forward"),
                ] {
                    let r = ui
                        .add(HwButton::new(&label, THEME.text, vec2(BTN_W + 6.0, BTN_H)).font(10.0))
                        .on_hover_text(format!("{tip} {n} beats"));
                    if let Some(d) = held(ui, &r) {
                        self.control(out, control, d as u8 as f64);
                    }
                }
            });
            // The step: click to pick 1-32 beats.
            let r = ui
                .add(
                    HwButton::new(
                        &format!("STEP {n}"),
                        THEME.text,
                        vec2(2.0 * (BTN_W + 6.0) + 3.0, BTN_H),
                    )
                    .font(10.0),
                )
                .on_hover_text("How many beats a jump (← / →) moves");
            egui::Popup::menu(&r).show(|ui| {
                for beats in JUMP_STEPS {
                    let label = format!("{beats} beat{}", if beats == 1.0 { "" } else { "s" });
                    if ui.radio(info.jump_size == beats, label).clicked() {
                        self.control(out, DeckControl::JumpSize, beats);
                        ui.close();
                    }
                }
            });
        });
    }

    fn grid_editor(&mut self, ui: &mut Ui, info: &DeckInfo, out: &mut Vec<Command>) {
        let deck = self.id;
        let mut grid = |op| out.push(Command::Deck(deck, DeckCmd::Grid(op)));
        ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
        ui.horizontal(|ui| {
            if ui
                .add(
                    HwButton::new("Anchor 1 here", self.colour(), vec2(30.0, 22.0))
                        .icon(Icon::Downbeat)
                        .tinted(true),
                )
                .on_hover_text(
                    "Put the 1 here: move the grid so a beat lands exactly on the playhead, \
                     and make it the 1",
                )
                .clicked()
            {
                grid(GridOp::AnchorHere);
            }
        });
        let cur = info.grid.as_ref().map(|g| g.mean_bpm());
        ui.horizontal(|ui| {
            ui.label(RichText::new("BPM").size(10.0).color(THEME.muted));
            for (label, d) in [("-0.1", -0.1), ("-0.01", -0.01)] {
                if ui.small_button(label).clicked() {
                    grid(GridOp::SetBpm(
                        ((cur.unwrap_or(120.0) + d) * 100.0).round() / 100.0,
                    ));
                }
            }
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.bpm_text)
                    .desired_width(52.0)
                    .horizontal_align(Align::Center),
            );
            if !edit.has_focus() {
                self.bpm_text = cur
                    .map(|b| format!("{:.2}", (b * 100.0).round() / 100.0))
                    .unwrap_or_default();
            }
            if edit.lost_focus()
                && let Ok(b) = self.bpm_text.trim().parse::<f64>()
            {
                grid(GridOp::SetBpm(b));
            }
            for (label, d) in [("+0.01", 0.01), ("+0.1", 0.1)] {
                if ui.small_button(label).clicked() {
                    grid(GridOp::SetBpm(
                        ((cur.unwrap_or(120.0) + d) * 100.0).round() / 100.0,
                    ));
                }
            }
            if ui
                .small_button("÷2")
                .on_hover_text("Half the tempo")
                .clicked()
            {
                grid(GridOp::Halve);
            }
            if ui
                .small_button("×2")
                .on_hover_text("Double the tempo")
                .clicked()
            {
                grid(GridOp::Double);
            }
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("NUDGE").size(10.0).color(THEME.muted));
            for (label, ms) in [
                ("<< 10 ms", -10.0),
                ("< 1 ms", -1.0),
                ("1 ms >", 1.0),
                ("10 ms >>", 10.0),
            ] {
                if ui
                    .small_button(label)
                    .on_hover_text("Shift the whole grid earlier or later")
                    .clicked()
                {
                    grid(GridOp::Nudge(ms));
                }
            }
        });
    }

    /// BPM and tempo readouts, SYNC, and the tempo fader with nudge buttons.
    /// SYNC, Q and KEY over the tempo fader, `height` tall in all.
    fn tempo(&mut self, ui: &mut Ui, info: &DeckInfo, out: &mut Vec<Command>, height: f32) {
        let s = self.state;
        ui.vertical(|ui| {
            ui.set_width(TEMPO_W);
            ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
            let following = match s.following.and_then(deck_letter) {
                Some(l) if s.sync => format!("\nFollowing deck {l}"),
                _ => String::new(),
            };
            let leading = if info.leader {
                "\nThe sync leader: synced decks follow this one"
            } else {
                ""
            };
            let r = ui
                .add(
                    HwButton::new(
                        if info.leader { "SYNC ★" } else { "SYNC" },
                        self.colour(),
                        vec2(TEMPO_W, 20.0),
                    )
                    .font(10.0)
                    .lit(s.sync),
                )
                .on_hover_text(format!(
                    "Match the leader's tempo and beat (right-click: make this deck the \
                     leader){following}{leading}"
                ));
            if held(ui, &r) == Some(true) {
                self.control(out, DeckControl::Sync, 1.0);
            }
            r.context_menu(|ui| {
                let (label, leader) = if info.leader {
                    ("Stop leading", None)
                } else {
                    ("Lead: synced decks follow this one", Some(self.id))
                };
                if ui.button(label).clicked() {
                    out.push(Command::Mixer(MixerCmd::SyncLeader(leader)));
                    ui.close();
                }
            });
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let r = ui
                    .add(
                        HwButton::new("Q", THEME.accent, vec2(24.0, 18.0))
                            .font(10.0)
                            .lit(s.quantize),
                    )
                    .on_hover_text(
                        "Quantise: snap this deck's cues, loops and hot cues to the beat",
                    );
                if held(ui, &r) == Some(true) {
                    self.control(out, DeckControl::Quantize, 1.0);
                }
                let r = ui
                    .add(
                        HwButton::new("KEY", THEME.accent, vec2(TEMPO_W - 28.0, 18.0))
                            .font(9.0)
                            .lit(s.keylock),
                    )
                    .on_hover_text("Keylock: the tempo fader changes the speed but not the key");
                if held(ui, &r) == Some(true) {
                    self.control(out, DeckControl::Keylock, 1.0);
                }
            });
            // The fader gets exactly what's left, so it never stretches the deck.
            let h = height - ui.min_rect().height() - ui.spacing().item_spacing.y;
            ui.horizontal_top(|ui| {
                ui.add_space(16.0); // the fader's range markings
                let (fader, marks) = tempo_fader(s.range);
                // The cap is where the fader was left; until it's moved past the
                // tempo (marked), it doesn't change it.
                let mut v = s.fader;
                let target = (!s.fader_live).then(|| s.range.fader(s.rate));
                let r = ui.add(
                    Fader::vertical(fader, &mut v, vec2(26.0, h))
                        .inverted(true)
                        .ticks(17)
                        .marks(marks)
                        .target(target)
                        .colour(self.colour()),
                );
                if r.changed() {
                    self.control(out, DeckControl::Tempo, v);
                }
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    for (label, control, tip) in [
                        ("−", DeckControl::NudgeDown, "Nudge slower (hold)"),
                        ("+", DeckControl::NudgeUp, "Nudge faster (hold)"),
                    ] {
                        let r = ui
                            .add(HwButton::new(label, THEME.text, vec2(24.0, 24.0)).font(14.0))
                            .on_hover_text(tip);
                        if let Some(d) = held(ui, &r) {
                            self.control(out, control, d as u8 as f64);
                        }
                    }
                    // The fader's range: click for the next, right-click to pick one.
                    let r = ui
                        .add(HwButton::new(s.range.label(), THEME.text, vec2(24.0, 18.0)).font(8.0))
                        .on_hover_text("Tempo range: ±6, ±10, ±16 % or WIDE (0 to 256 % speed)");
                    if r.clicked() {
                        self.control(out, DeckControl::TempoRangeNext, 1.0);
                    }
                    r.context_menu(|ui| {
                        for range in TempoRange::ALL {
                            if ui.radio(s.range == range, range.label()).clicked() {
                                self.engine(out, EngineCmd::TempoRange(range));
                                ui.close();
                            }
                        }
                    });
                });
            });
        });
    }
}

// ---- panels --------------------------------------------------------------------------

/// A Perform deck, `width` × `height`, on the mixer's `side`.
pub struct DeckPanel<'a> {
    pub deck: &'a mut DeckUi,
    pub width: f32,
    pub height: f32,
    pub side: Side,
}

impl Panel for DeckPanel<'_> {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let info = cx.engine.deck(self.deck.id);
        self.deck
            .show(ui, self.width, self.height, self.side, info, cx.out);
    }
}

/// A player's scrolling waveform, `height` tall, at the shared zoom.
pub struct WavePanel<'a> {
    pub deck: &'a mut DeckUi,
    pub height: f32,
}

impl Panel for WavePanel<'_> {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let info = cx.engine.deck(self.deck.id);
        self.deck
            .show_wave(ui, self.height, info, cx.prefs.wave_zoom, cx.out);
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

/// Prepare's cue pane, filling the space it's given.
pub struct CuePane<'a>(pub &'a mut DeckUi);

impl Panel for CuePane<'_> {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let info = cx.engine.deck(self.0.id);
        self.0.cue_pane(ui, info, cx.out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(id: i64, title: &str) -> PendingLoad {
        PendingLoad {
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
                    deck.show(ui, 680.0, 200.0, Side::Left, &info, &mut out);
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
