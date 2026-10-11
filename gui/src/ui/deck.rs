//! One deck, laid out like Mixxx's: header and overview with a spinning platter, then
//! CUE and play, hot cue pads (or the beat grid editor), loop and beat jump buttons,
//! and on the right the BPM, SYNC and a tempo fader with nudge buttons. Its scrolling
//! waveform is drawn separately, in the stacked row above the decks.
//!
//! Drawing reads the engine's atomics directly; every button becomes an [`Action`] the
//! app applies after the frame, through the same control bus as keyboard and MIDI.

use std::f32::consts::TAU;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

use earshot_engine::RATE_RANGE;
use earshot_engine::beatgrid::Beatgrid;
use earshot_engine::engine::DeckShared;
use egui::{
    Align, Button, Color32, CornerRadius, FontId, Layout, Rect, RichText, Sense, Stroke,
    StrokeKind, Ui, pos2, vec2,
};

use super::controls::{Fader, HwButton, Icon, Range, held};
use super::library_ui::Track;
use super::util::{self, Camelot};
use super::wave::{self, DeckWave, Marks};
use crate::core::player::GridOp;

pub enum Action {
    Control {
        group: &'static str,
        key: String,
        value: f64,
    },
    Grid {
        deck: usize,
        op: GridOp,
    },
    Load {
        deck: usize,
        track_id: i64,
    },
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
            title: t.map(|t| t.lib.title.clone()).unwrap_or_default(),
            artist: t.map(|t| t.lib.artist.clone()).unwrap_or_default(),
            bpm: t.and_then(|t| t.lib.bpm.or(t.lib.tag_bpm)),
            key: t.and_then(|t| t.key),
            duration: t.and_then(|t| t.lib.duration),
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
        }
    }
}

/// One snapshot of the engine's deck state.
#[derive(Default, Clone, Copy)]
struct State {
    position: f64,
    duration: f64,
    playing: bool,
    speed: f64,
    tempo: f64,
    rate: f64,
    bpm: Option<f64>,
    sync: bool,
    loop_in: Option<f64>,
    loop_out: Option<f64>,
    loop_active: bool,
    cue: f64,
    hotcues: [Option<f64>; 8],
    generation: u64,
}

fn finite(x: f64) -> Option<f64> {
    x.is_finite().then_some(x)
}

fn read(sh: &DeckShared) -> State {
    let mut hotcues = [None; 8];
    for (h, v) in hotcues.iter_mut().zip(&sh.hotcues) {
        *h = finite(v.get());
    }
    State {
        position: sh.position.get(),
        duration: sh.duration.get(),
        playing: sh.playing.load(Relaxed),
        speed: sh.speed.get(),
        tempo: sh.tempo.get(),
        rate: sh.rate.get(),
        bpm: finite(sh.bpm.get()),
        sync: sh.sync.load(Relaxed),
        loop_in: finite(sh.loop_in.get()),
        loop_out: finite(sh.loop_out.get()),
        loop_active: sh.loop_active.load(Relaxed),
        cue: sh.cue_point.get(),
        hotcues,
        generation: sh.generation.load(Relaxed),
    }
}

/// The tempo fader, in the control bus's units (-1..1 across the ±8 % range).
pub const TEMPO: Range = Range {
    name: "Tempo",
    min: -1.0,
    max: 1.0,
    default: 0.0,
    centre: Some(0.0),
    format: |v| format!("{:+.2} %", v * RATE_RANGE * 100.0),
};
/// Range markings beside the tempo fader; it runs minus at the top, like Mixxx's.
const TEMPO_MARKS: [(f64, &str); 3] = [(0.0, "-8"), (0.5, "0"), (1.0, "+8")];

// Layout.
const TEMPO_W: f32 = 76.0;
const OVERVIEW_H: f32 = 52.0;
const BTN_H: f32 = 34.0;
const BTN_W: f32 = 34.0;
const TRANSPORT_W: f32 = 52.0;
/// Two rows of buttons.
const CONTROLS_H: f32 = 2.0 * BTN_H + 4.0;
const SIZES: [f64; 9] = [0.125, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0];

const CUE_COLOUR: Color32 = Color32::from_rgb(0xf0, 0xa0, 0x30);
const PLAY_COLOUR: Color32 = Color32::from_rgb(0x4f, 0xd4, 0x7a);
const LOOP_COLOUR: Color32 = Color32::from_rgb(0x3c, 0xd3, 0x6b);

fn size_label(s: f64) -> String {
    if s < 1.0 {
        format!("1/{}", (1.0 / s).round())
    } else {
        format!("{s}")
    }
}

/// The next size up (`dir` 1) or down (-1) in [`SIZES`].
fn step_size(s: f64, dir: i32) -> f64 {
    let i = SIZES
        .iter()
        .position(|&x| x >= s)
        .unwrap_or(SIZES.len() - 1) as i32;
    SIZES[(i + dir).clamp(0, SIZES.len() as i32 - 1) as usize]
}

pub struct DeckUi {
    pub index: usize,
    group: &'static str,
    /// Smoothed playhead used for drawing.
    shown: f64,
    seen_generation: u64,
    /// While dragging the waveform: (start x, start time, was playing).
    scrub: Option<(f32, f64, bool)>,
    pub wave: Option<DeckWave>,
    grid_mode: bool,
    bpm_text: String,
    jump_size: f64,
    loop_size: f64,
    state: State,
    /// A load in progress; see [`PendingLoad`].
    pending: Option<PendingLoad>,
}

impl DeckUi {
    pub fn new(index: usize) -> DeckUi {
        DeckUi {
            index,
            group: ["[Channel1]", "[Channel2]"][index],
            shown: 0.0,
            seen_generation: u64::MAX,
            scrub: None,
            wave: None,
            grid_mode: false,
            bpm_text: String::new(),
            jump_size: 4.0,
            loop_size: 4.0,
            state: State::default(),
            pending: None,
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

    #[cfg(test)]
    pub fn pending(&self) -> Option<&PendingLoad> {
        self.pending.as_ref()
    }

    fn colour(&self) -> Color32 {
        util::DECK[self.index]
    }

    fn control(&self, actions: &mut Vec<Action>, key: &str, value: f64) {
        actions.push(Action::Control {
            group: self.group,
            key: key.into(),
            value,
        });
    }

    /// Read the engine and advance the smoothed playhead. Once per frame, before drawing.
    pub fn tick(&mut self, ctx: &egui::Context, sh: Option<&DeckShared>, info: &DeckInfo) {
        let Some(sh) = sh else { return };
        let s = read(sh);
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
            self.wave = (!info.detail.is_empty()).then(|| {
                DeckWave::new(
                    ctx,
                    &format!("overview-{}", self.index),
                    info.waveform.clone(),
                    info.detail.clone(),
                )
            });
        }
    }

    fn marks<'a>(&self, info: &'a DeckInfo, hotcues: &'a [Option<f64>]) -> Marks<'a> {
        Marks {
            grid: info.grid.as_deref(),
            cue: self.state.cue,
            hotcues,
            loop_range: self.state.loop_in.zip(self.state.loop_out),
            loop_active: self.state.loop_active,
        }
    }

    /// The quiet stand-in for a waveform while a track decodes.
    fn loading(&self, ui: &Ui, rect: Rect) {
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 4.0, util::WAVE_BG);
        p.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Loading…",
            FontId::proportional(12.0),
            util::MUTED,
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
        half_window: &mut f64,
        actions: &mut Vec<Action>,
    ) {
        let (rect, resp) =
            ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click_and_drag());
        if self.pending.is_some() {
            self.loading(ui, rect);
            if let Some(id) = resp.dnd_release_payload::<i64>() {
                actions.push(Action::Load {
                    deck: self.index,
                    track_id: *id,
                });
            }
            return;
        }
        let half = *half_window
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
                self.control(actions, "play", 0.0);
            }
        }
        if let (Some((x0, t0, _)), Some(p)) = (self.scrub, resp.interact_pointer_pos())
            && resp.dragged()
        {
            let t =
                (t0 - (p.x - x0) as f64 * (2.0 * half / rect.width() as f64)).clamp(0.0, duration);
            self.shown = t;
            self.control(actions, "playposition", t / duration);
        }
        if resp.drag_stopped() {
            if let Some((_, _, true)) = self.scrub {
                self.control(actions, "play", 1.0);
            }
            self.scrub = None;
        }
        if resp.hovered() {
            let scroll = ui.input(|i| i.raw_scroll_delta.y);
            if scroll != 0.0 {
                // About 7% per wheel notch.
                *half_window = (*half_window * (-(scroll as f64) * 0.0014).exp()).clamp(0.5, 30.0);
            }
        }
        if let Some(id) = resp.dnd_release_payload::<i64>() {
            actions.push(Action::Load {
                deck: self.index,
                track_id: *id,
            });
        }
    }

    fn bar_beat(&self, info: &DeckInfo) -> Option<String> {
        let g = info.grid.as_ref()?;
        let t = g.times();
        if t.is_empty() || self.shown < t[0] {
            return None;
        }
        let i = t
            .partition_point(|&x| x <= self.shown + 1e-6)
            .saturating_sub(1);
        let bar = g.positions()[..=i]
            .iter()
            .filter(|&&p| p == 1)
            .count()
            .max(1);
        Some(format!("{bar}.{} Bars", g.positions()[i]))
    }

    // ---- the deck panel -----------------------------------------------------------------

    /// Draw the deck `width` points wide and at least `height` tall.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        width: f32,
        height: f32,
        info: &DeckInfo,
        actions: &mut Vec<Action>,
    ) {
        let loading = self.pending.as_ref().map(PendingLoad::info);
        let info = loading.as_ref().unwrap_or(info);
        let frame = egui::Frame::new()
            .fill(util::PANEL)
            .stroke(Stroke::new(1.0, util::LINE))
            .corner_radius(8)
            .inner_margin(egui::Margin::symmetric(8, 6));
        let resp = frame
            .show(ui, |ui| {
                let inner = width - 18.0;
                ui.set_width(inner);
                ui.set_min_height(height - 12.0);
                ui.spacing_mut().item_spacing = vec2(4.0, 5.0);
                let left = inner - TEMPO_W - 8.0;
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(left);
                        self.header(ui, info);
                        ui.horizontal(|ui| {
                            self.platter(ui);
                            self.overview(ui, info, actions);
                        });
                        self.controls(ui, info, actions);
                    });
                    ui.add_space(4.0);
                    self.tempo(ui, actions);
                });
            })
            .response;
        if resp.dnd_hover_payload::<i64>().is_some() {
            ui.painter().rect_stroke(
                resp.rect,
                8.0,
                Stroke::new(2.0, util::ACCENT),
                StrokeKind::Inside,
            );
        }
        if let Some(id) = resp.dnd_release_payload::<i64>() {
            actions.push(Action::Load {
                deck: self.index,
                track_id: *id,
            });
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
            ui.label(
                RichText::new((self.index + 1).to_string())
                    .strong()
                    .size(16.0)
                    .color(self.colour()),
            );
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
                        ui.label(RichText::new(format!("{b:.2}")).color(util::MUTED));
                    }
                    match &info.analysing {
                        Some((p, stage)) => {
                            ui.label(
                                RichText::new(format!("{stage}… {:.0}%", p * 100.0))
                                    .size(11.0)
                                    .color(util::WARN),
                            );
                        }
                        None if !info.grid_source.is_empty() => {
                            ui.label(
                                RichText::new(format!("grid: {}", info.grid_source))
                                    .size(11.0)
                                    .color(util::MUTED),
                            );
                        }
                        None => {}
                    }
                    ui.add(
                        egui::Label::new(RichText::new(&info.artist).color(util::MUTED)).truncate(),
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
                        .color(if ending { util::BAD } else { util::TEXT }),
                );
                ui.label(
                    RichText::new(util::fmt_tenths(position))
                        .size(11.0)
                        .monospace()
                        .color(util::MUTED),
                );
            });
        });
    }

    /// A small platter that turns like a record at 33⅓ rpm (1.8 s a turn).
    fn platter(&self, ui: &mut Ui) {
        let (rect, _) = ui.allocate_exact_size(vec2(OVERVIEW_H, OVERVIEW_H), Sense::hover());
        let p = ui.painter();
        let c = rect.center();
        let r = rect.width() / 2.0 - 1.5;
        p.circle(
            c,
            r,
            Color32::from_rgb(0x0c, 0x0d, 0x10),
            Stroke::new(2.0, self.colour()),
        );
        p.circle_filled(c, r * 0.35, Color32::from_rgb(0x1b, 0x1d, 0x24));
        if self.pending.is_none() {
            let a = ((self.shown / 1.8).fract() as f32) * TAU - TAU / 4.0;
            p.line_segment(
                [
                    c + vec2(a.cos(), a.sin()) * r * 0.4,
                    c + vec2(a.cos(), a.sin()) * (r - 2.0),
                ],
                Stroke::new(2.0, util::TEXT),
            );
        }
    }

    fn overview(&mut self, ui: &mut Ui, info: &DeckInfo, actions: &mut Vec<Action>) {
        let (rect, resp) = ui.allocate_exact_size(
            vec2(ui.available_width(), OVERVIEW_H),
            Sense::click_and_drag(),
        );
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
            self.control(
                actions,
                "playposition",
                ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64,
            );
        }
    }

    /// CUE and play, then hot cue pads (or the grid editor), loops and beat jumps.
    fn controls(&mut self, ui: &mut Ui, info: &DeckInfo, actions: &mut Vec<Action>) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing = vec2(3.0, 4.0);
            self.transport(ui, actions);
            ui.add_space(6.0);
            if self.grid_mode {
                ui.vertical(|ui| {
                    ui.set_height(CONTROLS_H);
                    self.grid_editor(ui, info, actions);
                });
            } else {
                self.pads(ui, actions);
                ui.add_space(6.0);
                self.loops(ui, actions);
                ui.add_space(6.0);
                self.jumps(ui, actions);
            }
            ui.add_space(6.0);
            let r = ui
                .add(
                    HwButton::new("GRID", util::ACCENT, vec2(BTN_W + 4.0, BTN_H))
                        .font(10.0)
                        .lit(self.grid_mode),
                )
                .on_hover_text("Edit the beat grid (in place of the hot cue pads)");
            if r.clicked() {
                self.grid_mode = !self.grid_mode;
            }
        });
    }

    fn transport(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.vertical(|ui| {
            let size = vec2(TRANSPORT_W, BTN_H);
            // CUE lights while the deck sits at its cue point, ready to go.
            let at_cue = !self.state.playing
                && self.state.duration > 0.0
                && (self.state.position - self.state.cue).abs() < 0.01;
            let r = ui
                .add(
                    HwButton::new("CUE", CUE_COLOUR, size)
                        .font(12.0)
                        .tinted(true)
                        .lit(at_cue && self.pending.is_none()),
                )
                .on_hover_text("Cue: set, or jump back and preview while held");
            if let Some(d) = held(ui, &r) {
                self.control(actions, "cue_default", d as u8 as f64);
            }
            let playing = self.state.playing;
            let r = ui
                .add(
                    HwButton::new("Play", PLAY_COLOUR, size)
                        .icon(Icon::PlayPause)
                        .font(14.0)
                        .tinted(true)
                        .lit(playing),
                )
                .on_hover_text("Play / pause");
            if held(ui, &r) == Some(true) {
                self.control(actions, "play", if playing { 0.0 } else { 1.0 });
            }
        });
    }

    fn pads(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let shift = ui.input(|i| i.modifiers.shift);
        egui::Grid::new(("pads", self.index))
            .spacing(vec2(3.0, 4.0))
            .show(ui, |ui| {
                for row in 0..2 {
                    for col in 0..4 {
                        let i = row * 4 + col;
                        let set = self.state.hotcues[i];
                        let label = (i + 1).to_string();
                        let tip = match set {
                            Some(t) => format!(
                                "Hot cue {} at {}\nClick: jump (held: preview) · Shift-click: clear",
                                i + 1,
                                util::fmt_tenths(t)
                            ),
                            None => format!("Hot cue {}: not set\nClick to set it here", i + 1),
                        };
                        let r = ui
                            .add(
                                HwButton::new(&label, util::HOTCUE[i], vec2(BTN_W, BTN_H))
                                    .font(12.0)
                                    .tinted(set.is_some()),
                            )
                            .on_hover_text(tip);
                        match held(ui, &r) {
                            Some(true) if shift => {
                                self.control(actions, &format!("hotcue_{}_clear", i + 1), 1.0)
                            }
                            Some(down) => self.control(
                                actions,
                                &format!("hotcue_{}_activate", i + 1),
                                down as u8 as f64,
                            ),
                            None => {}
                        }
                    }
                    ui.end_row();
                }
            });
    }

    /// A loop button `w` wide that sends `key` 1 on press and 0 on release.
    #[allow(clippy::too_many_arguments)]
    fn hold_button(
        &self,
        ui: &mut Ui,
        w: f32,
        label: &str,
        key: &str,
        lit: bool,
        tip: &str,
        actions: &mut Vec<Action>,
    ) {
        let r = ui
            .add(
                HwButton::new(label, LOOP_COLOUR, vec2(w, BTN_H))
                    .font(if label.len() > 4 { 9.0 } else { 10.0 })
                    .lit(lit),
            )
            .on_hover_text(tip);
        if let Some(d) = held(ui, &r) {
            self.control(actions, key, d as u8 as f64);
        }
    }

    /// Loop: ½ / LOOP n / ×2 over IN / OUT / EXIT. With no loop running, ½ and ×2
    /// change the size the next loop will be, like Mixxx.
    fn loops(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let active = self.state.loop_active;
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                for (label, dir) in [("½", -1), ("×2", 1)] {
                    if dir == 1 {
                        let r = ui
                            .add(
                                HwButton::new(
                                    &format!("⟲ {}", size_label(self.loop_size)),
                                    LOOP_COLOUR,
                                    vec2(BTN_W + 10.0, BTN_H),
                                )
                                .font(10.0)
                                .lit(active),
                            )
                            .on_hover_text(format!(
                                "Loop {} beats from the current beat (or end the loop)",
                                size_label(self.loop_size)
                            ));
                        if let Some(d) = held(ui, &r) {
                            self.control(actions, "beatloop_activate", d as u8 as f64);
                        }
                    }
                    let tip = if dir < 0 { "Halve" } else { "Double" };
                    let r = ui
                        .add(HwButton::new(label, LOOP_COLOUR, vec2(BTN_W, BTN_H)).font(11.0))
                        .on_hover_text(format!("{tip} the loop (or the next loop's size)"));
                    let pressed = held(ui, &r);
                    if active {
                        if let Some(d) = pressed {
                            let key = if dir < 0 { "loop_halve" } else { "loop_double" };
                            self.control(actions, key, d as u8 as f64);
                        }
                    } else if pressed == Some(true) {
                        self.loop_size = step_size(self.loop_size, dir);
                        self.control(actions, "beatloop_size", self.loop_size);
                    }
                }
            });
            // As wide as the row above.
            let w = (3.0 * BTN_W + 10.0) / 3.0;
            ui.horizontal(|ui| {
                self.hold_button(ui, w, "IN", "loop_in", false, "Loop in point", actions);
                self.hold_button(
                    ui,
                    w,
                    "OUT",
                    "loop_out",
                    false,
                    "Loop out point: starts the loop",
                    actions,
                );
                self.hold_button(
                    ui,
                    w,
                    if active { "EXIT" } else { "RELOOP" },
                    "reloop_toggle",
                    active,
                    "Leave the loop, or go back into the last one",
                    actions,
                );
            });
        });
    }

    /// Beat jump: ◀ n / n ▶ over size ½ / ×2.
    fn jumps(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let n = size_label(self.jump_size);
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                for (label, key, tip) in [
                    (format!("◀ {n}"), "beatjump_backward", "Jump back"),
                    (format!("{n} ▶"), "beatjump_forward", "Jump forward"),
                ] {
                    let r = ui
                        .add(HwButton::new(&label, util::TEXT, vec2(BTN_W + 6.0, BTN_H)).font(10.0))
                        .on_hover_text(format!("{tip} {n} beats"));
                    if let Some(d) = held(ui, &r) {
                        self.control(actions, key, d as u8 as f64);
                    }
                }
            });
            ui.horizontal(|ui| {
                for (label, dir) in [("½", -1), ("×2", 1)] {
                    let r = ui
                        .add(HwButton::new(label, util::TEXT, vec2(BTN_W + 6.0, BTN_H)).font(11.0))
                        .on_hover_text("Beat jump size");
                    if held(ui, &r) == Some(true) {
                        self.jump_size = step_size(self.jump_size, dir);
                        self.control(actions, "beatjump_size", self.jump_size);
                    }
                }
            });
        });
    }

    fn grid_editor(&mut self, ui: &mut Ui, info: &DeckInfo, actions: &mut Vec<Action>) {
        let deck = self.index;
        let mut grid = |op| actions.push(Action::Grid { deck, op });
        ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
        ui.horizontal(|ui| {
            if ui
                .add(
                    Button::new(RichText::new("Anchor 1 here").strong())
                        .stroke(Stroke::new(1.0, self.colour())),
                )
                .on_hover_text(
                    "Move the grid so a beat lands exactly on the playhead, and make it the 1",
                )
                .clicked()
            {
                grid(GridOp::AnchorHere);
            }
            if ui
                .button("Nearest beat = 1")
                .on_hover_text(
                    "Keep the grid where it is; make the beat nearest the playhead the 1",
                )
                .clicked()
            {
                grid(GridOp::SetDownbeat);
            }
        });
        let cur = info.grid.as_ref().map(|g| g.mean_bpm());
        ui.horizontal(|ui| {
            ui.label(RichText::new("BPM").size(10.0).color(util::MUTED));
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
            ui.label(RichText::new("NUDGE").size(10.0).color(util::MUTED));
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
    fn tempo(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let s = self.state;
        ui.vertical(|ui| {
            ui.set_width(TEMPO_W);
            ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
            let bpm = match &self.pending {
                Some(p) => p.bpm.map(|b| b * (1.0 + s.rate)),
                None => s.bpm,
            };
            ui.with_layout(Layout::top_down(Align::Max), |ui| {
                ui.label(
                    RichText::new(bpm.map_or("-".into(), |b| format!("{b:.2}")))
                        .size(19.0)
                        .monospace()
                        .color(util::TEXT),
                )
                .on_hover_text("BPM at the current tempo");
                ui.label(
                    RichText::new(format!("{:+.2} %", s.rate * 100.0))
                        .size(10.0)
                        .monospace()
                        .color(util::MUTED),
                );
            });
            let r = ui
                .add(
                    HwButton::new("SYNC", self.colour(), vec2(TEMPO_W, 20.0))
                        .font(10.0)
                        .lit(s.sync),
                )
                .on_hover_text("Match the other deck's tempo and beat");
            if r.clicked() {
                self.control(actions, "sync_enabled", if s.sync { 0.0 } else { 1.0 });
            }
            let h = (ui.available_height()).max(70.0);
            ui.horizontal_top(|ui| {
                ui.add_space(16.0); // the fader's range markings
                let mut v = s.rate / RATE_RANGE;
                let r = ui.add(
                    Fader::vertical(&TEMPO, &mut v, vec2(26.0, h))
                        .inverted(true)
                        .ticks(17)
                        .marks(&TEMPO_MARKS)
                        .colour(self.colour()),
                );
                if r.changed() {
                    self.control(actions, "rate", v);
                }
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    for (label, key, tip) in [
                        ("−", "rate_temp_down", "Nudge slower (hold)"),
                        ("+", "rate_temp_up", "Nudge faster (hold)"),
                    ] {
                        let r = ui
                            .add(HwButton::new(label, util::TEXT, vec2(24.0, 24.0)).font(14.0))
                            .on_hover_text(tip);
                        if let Some(d) = held(ui, &r) {
                            self.control(actions, key, d as u8 as f64);
                        }
                    }
                });
            });
        });
    }
}

pub fn key_chip(ui: &mut Ui, k: Camelot) {
    let text = util::key_name(k);
    let galley = ui.painter().layout_no_wrap(
        text,
        FontId::proportional(11.0),
        Color32::from_rgb(0x11, 0x11, 0x11),
    );
    let (rect, _) = ui.allocate_exact_size(galley.size() + vec2(10.0, 2.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(4), util::key_colour(k));
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, Color32::BLACK);
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

    #[test]
    fn newest_load_wins() {
        let mut d = DeckUi::new(0);
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
        let mut deck = DeckUi::new(0);
        deck.start_load(pending(7, "Glide"));
        let info = DeckInfo {
            title: "Old track".into(),
            artist: String::new(),
            grid: None,
            grid_source: String::new(),
            generation: 0,
            waveform: Arc::default(),
            detail: Arc::default(),
            tag_bpm: None,
            key: None,
            analysing: None,
        };
        let mut h = egui_kittest::Harness::builder()
            .with_size(vec2(700.0, 260.0))
            .build_ui_state(
                move |ui, deck: &mut DeckUi| {
                    let mut actions = vec![];
                    deck.show(ui, 680.0, 200.0, &info, &mut actions);
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
    fn sizes_step() {
        assert_eq!(step_size(4.0, 1), 8.0);
        assert_eq!(step_size(4.0, -1), 2.0);
        assert_eq!(step_size(32.0, 1), 32.0);
        assert_eq!(size_label(0.25), "1/4");
    }
}
