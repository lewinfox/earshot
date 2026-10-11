//! One deck, laid out like Rekordbox's performance view: header, overview, hot cue pads
//! (or the beat grid editor), transport and a jog display. Its scrolling waveform is
//! drawn separately, in the stacked row above the decks.
//!
//! Drawing reads the engine's atomics directly; every button becomes an [`Action`] the
//! app applies after the frame, through the same control bus as keyboard and MIDI.

use std::f32::consts::TAU;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

use earshot_engine::beatgrid::Beatgrid;
use earshot_engine::engine::DeckShared;
use egui::{
    Align, Button, Color32, CornerRadius, FontId, Layout, Rect, RichText, Sense, Stroke,
    StrokeKind, Ui, pos2, vec2,
};

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
    pfl: bool,
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
        pfl: sh.pfl.load(Relaxed),
        generation: sh.generation.load(Relaxed),
    }
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
    pitch: f64,
    state: State,
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
            pitch: 0.0,
            state: State::default(),
        }
    }

    pub fn playing(&self) -> bool {
        self.state.playing
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

    /// Draw the deck `width` points wide.
    pub fn show(&mut self, ui: &mut Ui, width: f32, info: &DeckInfo, actions: &mut Vec<Action>) {
        let frame = egui::Frame::new()
            .fill(util::PANEL)
            .stroke(Stroke::new(1.0, util::LINE))
            .corner_radius(8)
            .inner_margin(egui::Margin::symmetric(8, 6));
        let resp = frame
            .show(ui, |ui| {
                // The frame inherits its parent's left-to-right layout; stack instead.
                ui.vertical(|ui| {
                    let inner = width - 18.0;
                    ui.set_width(inner);
                    ui.set_max_width(inner);
                    ui.spacing_mut().item_spacing = vec2(4.0, 5.0);
                    self.header(ui, info, actions);
                    self.overview(ui, info, actions);
                    // Transport (150) + jog (112) + gaps; the pads get the rest.
                    let pad_w = (inner - 150.0 - 112.0 - 3.0 * ui.spacing().item_spacing.x - 4.0)
                        .max(160.0);
                    ui.horizontal(|ui| {
                        let mirrored = self.index == 1;
                        if mirrored {
                            self.jog(ui);
                            self.transport(ui, actions);
                            self.pad_area(ui, pad_w, info, actions);
                        } else {
                            self.pad_area(ui, pad_w, info, actions);
                            self.transport(ui, actions);
                            self.jog(ui);
                        }
                    });
                    self.tools(ui, actions);
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

    fn header(&mut self, ui: &mut Ui, info: &DeckInfo, actions: &mut Vec<Action>) {
        let s = self.state;
        ui.horizontal(|ui| {
            ui.label(
                RichText::new((self.index + 1).to_string())
                    .strong()
                    .size(16.0)
                    .color(self.colour()),
            );
            // Title and details on the left, as wide as the times and buttons allow.
            let right = 230.0;
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
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if toggle(ui, "PFL", s.pfl, "Headphone cue") {
                    self.control(actions, "pfl", if s.pfl { 0.0 } else { 1.0 });
                }
                if toggle(
                    ui,
                    "BEAT SYNC",
                    s.sync,
                    "Match the other deck's tempo and beat",
                ) {
                    self.control(actions, "sync_enabled", if s.sync { 0.0 } else { 1.0 });
                }
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    ui.with_layout(Layout::top_down(Align::Max), |ui| {
                        let ending = s.playing && s.duration - s.position < 30.0;
                        let remain = if s.duration > 0.0 {
                            util::fmt_tenths(s.position - s.duration)
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
                            RichText::new(util::fmt_tenths(s.position))
                                .size(11.0)
                                .monospace()
                                .color(util::MUTED),
                        );
                    });
                });
            });
        });
    }

    fn overview(&mut self, ui: &mut Ui, info: &DeckInfo, actions: &mut Vec<Action>) {
        let (rect, resp) =
            ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click_and_drag());
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

    fn pad_area(&mut self, ui: &mut Ui, w: f32, info: &DeckInfo, actions: &mut Vec<Action>) {
        ui.allocate_ui_with_layout(vec2(w, 132.0), Layout::top_down(Align::Min), |ui| {
            ui.set_width(w);
            if self.grid_mode {
                self.grid_editor(ui, info, actions);
            } else {
                self.pads(ui, w, actions);
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                if ui
                    .selectable_label(
                        !self.grid_mode,
                        RichText::new("HOT CUE").size(10.0).strong(),
                    )
                    .clicked()
                {
                    self.grid_mode = false;
                }
                if ui
                    .selectable_label(self.grid_mode, RichText::new("GRID").size(10.0).strong())
                    .clicked()
                {
                    self.grid_mode = true;
                }
            });
        });
    }

    fn pads(&mut self, ui: &mut Ui, w: f32, actions: &mut Vec<Action>) {
        let pad = vec2((w - 4.0) / 2.0, 23.0);
        let shift = ui.input(|i| i.modifiers.shift);
        egui::Grid::new(("pads", self.index))
            .spacing(vec2(4.0, 3.0))
            .show(ui, |ui| {
                for row in 0..4 {
                    for col in 0..2 {
                        let i = col * 4 + row;
                        let set = self.state.hotcues[i];
                        let c = util::HOTCUE[i];
                        let label = RichText::new(format!(
                            "{}   {}",
                            &"ABCDEFGH"[i..i + 1],
                            set.map(util::mmss).unwrap_or_default()
                        ))
                        .size(11.0)
                        .color(if set.is_some() {
                            c
                        } else {
                            util::MUTED
                        });
                        let b = Button::new(label)
                            .min_size(pad)
                            .fill(if set.is_some() {
                                util::dim(c, 0.22)
                            } else {
                                Color32::from_rgb(0x12, 0x13, 0x17)
                            })
                            .stroke(Stroke::new(1.0, if set.is_some() { c } else { util::LINE }));
                        let r = ui
                            .add_sized(pad, b)
                            .on_hover_text("Click: set / jump · Shift-click: clear");
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

    fn grid_editor(&mut self, ui: &mut Ui, info: &DeckInfo, actions: &mut Vec<Action>) {
        let deck = self.index;
        let mut grid = |op| actions.push(Action::Grid { deck, op });
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
                    .desired_width(56.0)
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

    fn transport(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.vertical(|ui| {
            ui.set_width(150.0);
            ui.spacing_mut().item_spacing = vec2(2.0, 4.0);
            ui.horizontal(|ui| {
                let r = ui.small_button("‹").on_hover_text("Beat jump back");
                if let Some(d) = held(ui, &r) {
                    self.control(actions, "beatjump_backward", d as u8 as f64);
                }
                if size_combo(
                    ui,
                    ("jump", self.index),
                    &mut self.jump_size,
                    &[1.0, 2.0, 4.0, 8.0, 16.0, 32.0],
                ) {
                    self.control(actions, "beatjump_size", self.jump_size);
                }
                let r = ui.small_button("›").on_hover_text("Beat jump forward");
                if let Some(d) = held(ui, &r) {
                    self.control(actions, "beatjump_forward", d as u8 as f64);
                }
            });
            ui.horizontal(|ui| {
                if size_combo(
                    ui,
                    ("loop", self.index),
                    &mut self.loop_size,
                    &[1.0, 2.0, 4.0, 8.0, 16.0],
                ) {
                    self.control(actions, "beatloop_size", self.loop_size);
                }
                for (label, key, lit) in [
                    ("LOOP", "beatloop_activate", self.state.loop_active),
                    ("½", "loop_halve", false),
                    ("×2", "loop_double", false),
                ] {
                    let r = ui.add(Button::new(RichText::new(label).size(11.0)).selected(lit));
                    if let Some(d) = held(ui, &r) {
                        self.control(actions, key, d as u8 as f64);
                    }
                }
            });
            ui.horizontal(|ui| {
                for (label, key, lit) in [
                    ("IN", "loop_in", false),
                    ("OUT", "loop_out", false),
                    ("EXIT", "reloop_toggle", self.state.loop_active),
                ] {
                    let r = ui.add(Button::new(RichText::new(label).size(11.0)).selected(lit));
                    if let Some(d) = held(ui, &r) {
                        self.control(actions, key, d as u8 as f64);
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                let cue = round_button(ui, "CUE", Color32::from_rgb(0xf0, 0xb0, 0x40), false);
                if let Some(d) = cue {
                    self.control(actions, "cue_default", d as u8 as f64);
                }
                ui.add_space(8.0);
                if round_button(
                    ui,
                    "▶",
                    Color32::from_rgb(0x4f, 0xd4, 0x7a),
                    self.state.playing,
                ) == Some(true)
                {
                    self.control(actions, "play", if self.state.playing { 0.0 } else { 1.0 });
                }
            });
        });
    }

    fn jog(&self, ui: &mut Ui) {
        let (rect, _) = ui.allocate_exact_size(vec2(112.0, 112.0), Sense::hover());
        let p = ui.painter_at(rect.expand(2.0));
        let c = rect.center();
        let r = rect.width() / 2.0 - 2.0;
        p.circle(
            c,
            r,
            Color32::from_rgb(0x0c, 0x0d, 0x10),
            Stroke::new(3.0, self.colour()),
        );
        p.circle_filled(c, r * 0.62, Color32::from_rgb(0x1b, 0x1d, 0x24));
        // A marker that turns like a record at 33⅓ rpm (1.8 s a turn).
        let a = ((self.shown / 1.8).fract() as f32) * TAU - TAU / 4.0;
        p.circle_filled(c + vec2(a.cos(), a.sin()) * (r - 6.0), 3.5, util::BAD);
        let bpm = self
            .state
            .bpm
            .map(|b| format!("{b:.2}"))
            .unwrap_or_else(|| "-".into());
        p.text(
            c - vec2(0.0, 6.0),
            egui::Align2::CENTER_CENTER,
            bpm,
            FontId::proportional(21.0),
            util::TEXT,
        );
        let rate = self.state.rate * 100.0;
        p.text(
            c + vec2(0.0, 14.0),
            egui::Align2::CENTER_CENTER,
            format!("{}{rate:.1}%", if rate >= 0.0 { "+" } else { "" }),
            FontId::proportional(11.0),
            util::MUTED,
        );
    }

    fn tools(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            let r = ui.small_button("-").on_hover_text("Nudge slower (hold)");
            if let Some(d) = held(ui, &r) {
                self.control(actions, "rate_temp_down", d as u8 as f64);
            }
            ui.spacing_mut().slider_width = ui.available_width() - 30.0;
            let slider = ui
                .add(egui::Slider::new(&mut self.pitch, -1.0..=1.0).show_value(false))
                .on_hover_text("Pitch ±8% (double-click to reset)");
            if slider.double_clicked() {
                self.pitch = 0.0;
                self.control(actions, "rate", 0.0);
            } else if slider.changed() {
                self.control(actions, "rate", self.pitch);
            } else if !slider.dragged() {
                self.pitch = self.state.rate / earshot_engine::RATE_RANGE;
            }
            let r = ui.small_button("+").on_hover_text("Nudge faster (hold)");
            if let Some(d) = held(ui, &r) {
                self.control(actions, "rate_temp_up", d as u8 as f64);
            }
        });
    }
}

// ---- small widgets -------------------------------------------------------------------

/// Press and release edges of a button that acts while held: `Some(true)` on press,
/// `Some(false)` on release.
pub fn held(ui: &Ui, r: &egui::Response) -> Option<bool> {
    let down = r.is_pointer_button_down_on();
    let id = r.id.with("held");
    let was = ui.ctx().data(|d| d.get_temp::<bool>(id).unwrap_or(false));
    if down == was {
        return None;
    }
    ui.ctx().data_mut(|d| d.insert_temp(id, down));
    Some(down)
}

fn toggle(ui: &mut Ui, label: &str, on: bool, tip: &str) -> bool {
    ui.add(Button::new(RichText::new(label).size(10.0).strong()).selected(on))
        .on_hover_text(tip)
        .clicked()
}

fn size_combo(ui: &mut Ui, id: (&str, usize), value: &mut f64, sizes: &[f64]) -> bool {
    let before = *value;
    egui::ComboBox::from_id_salt(id)
        .width(40.0)
        .selected_text(format!("{value}"))
        .show_ui(ui, |ui| {
            for s in sizes {
                ui.selectable_value(value, *s, format!("{s}"));
            }
        });
    *value != before
}

/// A round button like Rekordbox's CUE and PLAY. Returns press/release edges.
fn round_button(ui: &mut Ui, label: &str, colour: Color32, lit: bool) -> Option<bool> {
    let (rect, resp) = ui.allocate_exact_size(vec2(42.0, 42.0), Sense::click_and_drag());
    let p = ui.painter();
    let fill = if lit {
        util::dim(colour, 0.4)
    } else {
        Color32::from_rgb(0x12, 0x13, 0x17)
    };
    let ring = if resp.hovered() {
        colour
    } else {
        util::dim(colour, 0.75)
    };
    p.circle(rect.center(), 20.0, fill, Stroke::new(2.0, ring));
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        FontId::proportional(12.0),
        colour,
    );
    held(ui, &resp)
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
