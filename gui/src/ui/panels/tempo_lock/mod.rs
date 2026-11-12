//! The Tempo lock tab, Prepare's tool for tracks whose tempo wanders: analyse a track
//! (right-click it, "Normalise tempo"), choose a BPM, straighten it, listen to both
//! versions, and save the result into a library folder. tempo-lock's page in egui
//! (`../tempo-lock/static/`): its stats, warnings, tempo charts, histograms and
//! waveforms with their beat grids, for the original and the straightened version.
//!
//! The tool's work is in `core::tempo_lock`; this tab keeps what it last heard from
//! it ([`TempoLockUi::on_event`]) and sends `TempoLockCmd`s. Listening is on Prepare's
//! player: the shell (`app/tempo_lock.rs`) loads the version asked for and says which
//! is in ([`TempoLockUi::player`]).

mod charts;

use std::path::PathBuf;
use std::sync::Arc;

use egui::{Color32, CornerRadius, Margin, Pos2, RichText, Sense, Stroke, Ui, vec2};

use self::charts::{HistChart, TempoChart, bpm_range, fmt_time, ruler, time_at};
use super::deck::wave::{Layer, Timeline};
use super::deck::zoomed;
use super::library::drag;
use super::{Cx, Panel};
use crate::core::command::ViewCmd;
use crate::core::tempo_lock::{
    BeatLevel, Chart, Engine, Stage, StraightenOptions, Straightened, TempoAnalysis, TempoLockCmd,
    TempoLockEvent, Version, histogram_range, version_beats,
};
use crate::core::track::TrackId;
use crate::ui::theme::{THEME, Theme, fs};
use crate::ui::widgets::{Button, Seg, segmented};

/// The shortest stretch the waveforms zoom in to, s.
/// The charts' heights: they share what's left of the tab.
const CHART_H: (f32, f32) = (48.0, 260.0);

/// What the tab has heard from the tool, its controls, and its view.
pub struct TempoLockUi {
    /// The track the tool is working on.
    pub track: Option<TrackId>,
    pub chart: Option<Arc<Chart>>,
    pub straightened: Option<Arc<Straightened>>,
    /// The beats found again in the straightened audio.
    pub measured: Option<Arc<TempoAnalysis>>,
    /// What's running, and how far through (0..1).
    pub busy: Option<(Stage, f64)>,
    /// How the last step ended, when there's something to say.
    pub note: Option<Note>,

    // ---- controls -----------------------------------------------------------------
    pub bpm: f64,
    pub level: BeatLevel,
    pub engine: Engine,
    /// The library folder to save into (one of the Store's roots).
    pub root: Option<String>,
    /// Which version plays.
    pub version: Version,

    // ---- view ---------------------------------------------------------------------
    /// Where playback is (or will start), in the original's time.
    cursor: f64,
    /// The height of everything but the charts, as drawn last frame (with this many
    /// rows of charts), for sizing the charts to fit.
    rest: (usize, f32),
    /// Which version is in Prepare's player, if either (set every frame by the shell).
    pub player: Option<Version>,
    /// A version loading into the player.
    pub loading: bool,
}

/// How the last step ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Note {
    Failed(Stage, String),
    Cancelled(Stage),
    Saved(PathBuf),
}

impl Default for TempoLockUi {
    fn default() -> Self {
        TempoLockUi {
            track: None,
            chart: None,
            straightened: None,
            measured: None,
            busy: None,
            note: None,
            bpm: 120.0,
            level: BeatLevel::Beat,
            engine: Engine::R3,
            root: None,
            version: Version::Original,
            cursor: 0.0,
            rest: (0, 0.0),
            player: None,
            loading: false,
        }
    }
}

impl TempoLockUi {
    /// A new track: forget the last one (keeping the engine and the folder chosen).
    fn reset(&mut self, track: TrackId) {
        *self = TempoLockUi {
            track: Some(track),
            engine: self.engine,
            root: self.root.take(),
            ..TempoLockUi::default()
        };
    }

    /// News from the tool. Events about another track start afresh with it when
    /// they're an analysis starting or done, and are stale otherwise.
    pub fn on_event(&mut self, e: TempoLockEvent) {
        let track = match &e {
            TempoLockEvent::Progress { track, .. }
            | TempoLockEvent::Analysed { track, .. }
            | TempoLockEvent::Straightened { track, .. }
            | TempoLockEvent::Measured { track, .. }
            | TempoLockEvent::Saved { track, .. }
            | TempoLockEvent::Failed { track, .. }
            | TempoLockEvent::Cancelled { track, .. } => *track,
        };
        let new_session = matches!(
            e,
            TempoLockEvent::Analysed { .. }
                | TempoLockEvent::Progress {
                    stage: Stage::Analyse,
                    ..
                }
        );
        if self.track != Some(track) {
            if !new_session {
                return;
            }
            self.reset(track);
        }
        match e {
            TempoLockEvent::Progress {
                stage, fraction, ..
            } => {
                if stage == Stage::Analyse && fraction == 0.0 && self.chart.is_some() {
                    self.reset(track); // analysed again
                }
                if fraction == 0.0 {
                    self.note = None;
                }
                self.busy = Some((stage, fraction));
            }
            TempoLockEvent::Analysed { chart, .. } => {
                let a = &chart.analysis;
                self.bpm = a.suggested_bpm;
                self.level = BeatLevel::Beat;
                self.cursor = 0.0;
                self.chart = Some(chart);
                self.busy = None;
            }
            TempoLockEvent::Straightened { result, .. } => {
                self.straightened = Some(result);
                self.measured = None;
                if self.version == Version::Straightened {
                    self.version = Version::Original;
                }
            }
            TempoLockEvent::Measured { analysis, .. } => {
                self.measured = Some(analysis);
                self.busy = None;
            }
            TempoLockEvent::Saved { path, .. } => {
                self.busy = None;
                self.note = Some(Note::Saved(path));
            }
            TempoLockEvent::Failed { stage, error, .. } => {
                self.busy = None;
                self.note = Some(Note::Failed(stage, error));
            }
            TempoLockEvent::Cancelled { stage, .. } => {
                self.busy = None;
                self.note = Some(Note::Cancelled(stage));
            }
        }
    }

    /// Whether Straighten can be pressed, or why not.
    pub fn can_straighten(&self) -> Result<(), String> {
        let Some(chart) = &self.chart else {
            return Err("analyse a track first".into());
        };
        if matches!(self.busy, Some((s, _)) if s != Stage::SaveToLibrary) {
            return Err("wait for the step running to finish, or cancel it".into());
        }
        chart.analysis.check_target(self.bpm, self.level)
    }

    /// `t` in `version`'s time, in the original's.
    fn to_original(&self, t: f64, version: Version) -> f64 {
        match (version, &self.straightened) {
            (Version::Straightened, Some(s)) => s.map_time(t, true),
            _ => t,
        }
    }

    /// `t` in the original's time, in `version`'s.
    fn from_original(&self, t: f64, version: Version) -> f64 {
        match (version, &self.straightened) {
            (Version::Straightened, Some(s)) => s.map_time(t, false),
            _ => t,
        }
    }

    /// Where playback is (or will start), in the original's time.
    pub fn cursor(&self) -> f64 {
        self.cursor
    }

    /// The stretch Prepare's waveform shows, in `version`'s time: the shared zoom
    /// (× the player's tempo) either side of the playhead.
    fn window(&self, version: Version, cx: &Cx) -> (f64, f64) {
        let tempo = cx
            .engine
            .snap
            .as_ref()
            .map(|s| s.deck(cx.prefs.target()).tempo)
            .filter(|t| *t > 0.0)
            .unwrap_or(1.0);
        let half = cx.prefs.wave_zoom * tempo;
        let shown = self.player.unwrap_or(self.version);
        let c = self.from_original(self.cursor, shown);
        let ends =
            [c - half, c + half].map(|t| self.from_original(self.to_original(t, shown), version));
        (ends[0], ends[1])
    }

    /// Put the version chosen in the player, at the cursor.
    fn listen(&self, cx: &mut Cx) {
        let version = self.version;
        cx.send(TempoLockCmd::Listen {
            version,
            at: self.from_original(self.cursor, version).max(0.0),
        });
    }

    /// Move to `t` in `version`'s time, putting that version in the player there.
    fn seek(&mut self, version: Version, t: f64, cx: &mut Cx) {
        self.version = version;
        self.cursor = self.to_original(t, version).max(0.0);
        self.listen(cx);
    }
}

/// The tab, with the library folders to offer for saving.
pub struct TempoLockPanel<'a> {
    pub tab: &'a mut TempoLockUi,
    pub roots: &'a [String],
}

impl Panel for TempoLockPanel<'_> {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        // A track dropped anywhere on the tab charts it, as "Normalise tempo" does.
        let area = ui.max_rect();
        let drop = ui.interact(area, ui.id().with("drop"), Sense::hover());
        self.body(ui, cx);
        if drag::hovering_tracks(&drop) {
            ui.painter().rect_stroke(
                area,
                6.0,
                Stroke::new(2.0_f32, cx.theme.accent),
                egui::StrokeKind::Inside,
            );
        }
        if let Some(id) = drag::take_track(&drop) {
            cx.send(TempoLockCmd::Analyse(id));
        }
    }
}

impl TempoLockPanel<'_> {
    fn body(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let tab = &mut *self.tab;
        // Where the player is: the cursor follows it while the tab's audio plays.
        if let (Some(v), Some(snap)) = (tab.player, cx.engine.snap.as_ref())
            && !tab.loading
        {
            tab.cursor = tab
                .to_original(snap.deck(cx.prefs.target()).position, v)
                .max(0.0);
        }
        ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
        header(ui, tab, cx);
        let Some(chart) = tab.chart.clone() else {
            if tab.track.is_none() {
                ui.add_space(12.0);
                ui.label(
                    RichText::new(
                        "Drag a track here (or right-click it in the library and choose \
                         \"Normalise tempo\") to chart its tempo. Then pick a BPM and straighten it: every \
                         beat moves onto one steady grid, without changing the pitch.",
                    )
                    .color(cx.theme.muted),
                );
            }
            return;
        };
        controls(ui, tab, self.roots, cx);
        transport(ui, tab, cx);
        ui.separator();
        body(ui, tab, &chart, cx);
    }
}

// ---- the rows ------------------------------------------------------------------------------

/// The track's name, and what the tool is doing (with Cancel) or how it last ended.
fn header(ui: &mut Ui, tab: &mut TempoLockUi, cx: &mut Cx) {
    let theme = cx.theme;
    // Wrapping, and the status on a row of its own: the tool sits beside the library,
    // so a long path mustn't push the whole tab wider than the window.
    ui.horizontal_wrapped(|ui| {
        match tab.track.and_then(|id| cx.core.lib.get(id)) {
            Some(t) => {
                ui.label(RichText::new(&t.title).size(fs(16.0)).strong());
                if !t.artist.is_empty() {
                    ui.label(RichText::new(&t.artist).color(theme.muted));
                }
            }
            None => {
                ui.label(RichText::new("Tempo lock").size(fs(16.0)).strong());
            }
        }
        if let Some(c) = &tab.chart {
            let a = &c.analysis;
            ui.label(
                RichText::new(format!(
                    "{} · {} beats · detector {}",
                    fmt_time(a.duration),
                    a.beats.len(),
                    detector_name(&a.detector)
                ))
                .size(fs(11.0))
                .color(theme.muted),
            );
        }
    });
    if let Some((stage, fraction)) = tab.busy {
        ui.horizontal(|ui| {
            ui.label(RichText::new(stage_text(stage)).color(theme.warn));
            ui.spinner();
            ui.add(
                egui::ProgressBar::new(fraction as f32)
                    .desired_width(160.0)
                    .show_percentage(),
            );
            if stage != Stage::SaveToLibrary && ui.add(Button::new("Cancel")).clicked() {
                cx.send(TempoLockCmd::Cancel);
            }
        });
    } else if let Some(note) = &tab.note {
        note_label(ui, theme, note);
    }
}

/// The BPM to straighten to, how the detected beats relate to it, the engine, the
/// Straighten button, and saving the result.
fn controls(ui: &mut Ui, tab: &mut TempoLockUi, roots: &[String], cx: &mut Cx) {
    let theme = cx.theme;
    let median = tab.chart.as_ref().map_or(0.0, |c| c.analysis.median_bpm);
    ui.horizontal_wrapped(|ui| {
        ui.label("Target BPM");
        ui.add(
            egui::DragValue::new(&mut tab.bpm)
                .speed(0.05)
                .range(1.0..=999.0)
                .max_decimals(2),
        );
        if ui
            .add(Button::subtle("median").small())
            .on_hover_text("The median detected tempo")
            .clicked()
        {
            tab.bpm = (median * tab.level.factor() * 100.0).round() / 100.0;
        }
        if ui
            .add(Button::subtle("round").small())
            .on_hover_text("Round to a whole number")
            .clicked()
        {
            tab.bpm = tab.bpm.round();
        }
        ui.separator();
        ui.label("Detected beats are").on_hover_text(
            "If the detector locked onto half- or double-time, say so here and type the real BPM",
        );
        let levels = [BeatLevel::Beat, BeatLevel::HalfTime, BeatLevel::DoubleTime]
            .map(|l| Seg::new(l, level_name(l)));
        if let Some(l) = segmented(ui, tab.level, &levels) {
            tab.level = l;
            tab.bpm = (median * tab.level.factor()).round();
        }
        ui.separator();
        ui.label("Rubber Band");
        let engines = [
            Seg::new(Engine::R3, "R3").tip("Rubber Band's R3 engine: the best sound"),
            Seg::new(Engine::R2, "R2").tip("Rubber Band's R2 engine: faster"),
        ];
        if let Some(e) = segmented(ui, tab.engine, &engines) {
            tab.engine = e;
        }
        ui.separator();
        let ok = tab.can_straighten();
        let b = ui.add_enabled(ok.is_ok(), Button::primary("Straighten"));
        if b.clicked() {
            cx.send(TempoLockCmd::Straighten(StraightenOptions {
                bpm: tab.bpm,
                level: tab.level,
                engine: tab.engine,
            }));
        }
        if let Err(why) = ok
            && tab.busy.is_none()
        {
            b.on_disabled_hover_text(&why);
            ui.label(RichText::new(why).size(fs(11.0)).color(theme.warn));
        }
    });

    // Saving, on a row of its own: once there's a straightened file.
    if tab.straightened.is_none() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.label("Save into");
        if tab.root.as_ref().is_none_or(|r| !roots.contains(r)) {
            tab.root = roots.first().cloned();
        }
        egui::ComboBox::from_id_salt("tempo-lock-root")
            .selected_text(tab.root.as_deref().map_or("no library folders", short_path))
            .width(180.0)
            .show_ui(ui, |ui| {
                for r in roots {
                    ui.selectable_value(&mut tab.root, Some(r.clone()), r.as_str());
                }
            })
            .response
            .on_hover_text("The library folder to save into");
        let saving = matches!(tab.busy, Some((Stage::SaveToLibrary, _)));
        let save = ui
            .add_enabled(
                tab.root.is_some() && !saving,
                Button::new("Save to library"),
            )
            .on_hover_text(
                "Copy the straightened MP3 into this folder (never over a file) and add it \
                     to the library",
            );
        if save.clicked()
            && let Some(r) = &tab.root
        {
            cx.send(TempoLockCmd::SaveToLibrary(PathBuf::from(r)));
        }
    });
}

/// A/B (which version is in Prepare's player), and the time.
fn transport(ui: &mut Ui, tab: &mut TempoLockUi, cx: &mut Cx) {
    let theme = cx.theme;
    ui.horizontal_wrapped(|ui| {
        let versions = [
            Seg::new(Version::Original, "Original"),
            Seg::new(Version::Straightened, "Straightened")
                .enabled(tab.straightened.is_some())
                .disabled_tip("Straighten it first"),
        ];
        if let Some(v) = segmented(ui, tab.version, &versions) {
            tab.version = v;
            tab.listen(cx);
        }
        ui.separator();
        let at = tab.from_original(tab.cursor, tab.version);
        ui.label(RichText::new(fmt_time(at)).monospace().size(fs(14.0)));
        if tab.loading {
            ui.spinner();
            ui.label(RichText::new("loading…").color(theme.muted));
        } else if tab.track.is_some() && tab.player.is_none() {
            ui.label(RichText::new("another track is in the player").color(theme.muted));
        }
    });
}

/// Everything charted, the charts sized so it all fits the tab.
fn body(ui: &mut Ui, tab: &mut TempoLockUi, chart: &Chart, cx: &mut Cx) {
    let rows = if tab.straightened.is_some() && tab.measured.is_some() {
        2
    } else {
        1
    };
    // Measured last frame; a frame with a new number of rows guesses.
    let rest = match tab.rest {
        (r, h) if r == rows => h,
        (_, h) => h + 150.0 * (rows as f32 - 1.0),
    };
    let (top, avail) = (ui.cursor().top(), ui.available_height());
    let chart_h = ((avail - rest) / rows as f32).clamp(CHART_H.0, CHART_H.1);
    charted(ui, tab, chart, chart_h, cx);
    tab.rest = (rows, ui.cursor().top() - top - rows as f32 * chart_h);
}

/// The original's stats, warning and charts; then the straightened version's, the
/// charts `chart_h` tall.
fn charted(ui: &mut Ui, tab: &mut TempoLockUi, chart: &Chart, chart_h: f32, cx: &mut Cx) {
    let theme = cx.theme;
    let a = &chart.analysis;
    let straight = tab.straightened.clone();
    let measured = tab.measured.clone();

    // The charts of both versions share their scales.
    let target_a = tab.bpm / tab.level.factor();
    let target_b = straight.as_ref().map(|s| s.bpm / s.level);
    let mut charted = vec![(a, target_a)];
    if let (Some(m), Some(t)) = (&measured, target_b) {
        charted.push((m, t));
    }
    let range = bpm_range(&charted);
    let analyses: Vec<&TempoAnalysis> = charted.iter().map(|c| c.0).collect();
    let targets: Vec<f64> = charted.iter().map(|c| c.1).collect();
    let hist = histogram_range(&analyses, &targets);

    stats_a(ui, theme, a);
    tempo_changes(ui, tab, a, cx);
    charts_row(
        ui,
        tab,
        Version::Original,
        a,
        target_a,
        tab.level.factor(),
        range,
        hist,
        chart_h,
        "",
        cx,
    );

    let Some(s) = straight else {
        return;
    };
    ui.add_space(10.0);
    let file = s
        .output
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    title(
        ui,
        theme,
        "Straightened",
        &format!(
            "{} BPM · {} · MP3 {} kbps · {file}",
            s.grid.target_bpm,
            fmt_time(s.duration),
            s.bitrate_kbps
        ),
    );
    let measuring = matches!(tab.busy, Some((Stage::Measure | Stage::Straighten, _)));
    ui.horizontal(|ui| {
        ui.label(RichText::new("Measured on the straightened audio").strong());
        if measuring {
            ui.spinner();
            ui.label(
                RichText::new("detecting beats in the output…")
                    .size(fs(11.0))
                    .color(theme.warn),
            );
        } else if let Some(Note::Failed(Stage::Measure, e)) = &tab.note {
            ui.label(RichText::new(format!("could not measure: {e}")).color(theme.bad));
        }
    });
    if let Some(m) = measured {
        stats_b(ui, theme, &m, &s);
        charts_row(
            ui,
            tab,
            Version::Straightened,
            &m,
            target_b.unwrap_or(s.bpm),
            s.level,
            range,
            hist,
            chart_h,
            " · beats detected again in the output · same scale as the original",
            cx,
        );
    }
}

/// A section's title with a hint after it.
fn title(ui: &mut Ui, theme: &Theme, name: &str, hint: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(name).strong());
        ui.label(RichText::new(hint).size(fs(11.0)).color(theme.muted));
    });
}

/// A version's tempo over the stretch Prepare's waveform shows (click to seek there,
/// wheel to zoom both), and its histogram.
#[allow(clippy::too_many_arguments)]
fn charts_row(
    ui: &mut Ui,
    tab: &mut TempoLockUi,
    version: Version,
    a: &TempoAnalysis,
    target: f64,
    level: f64,
    range: (f64, f64),
    hist: (f64, f64, f64),
    height: f32,
    hint: &str,
    cx: &mut Cx,
) {
    let theme = cx.theme;
    let colour = version_colour(theme, version);
    let w = ui.available_width();
    let side = (w * 0.26).clamp(200.0, 420.0);
    let main = w - side - 10.0;
    let window = tab.window(version, cx);
    let playhead = Some(tab.from_original(tab.cursor, version));
    // Thrown-away detections only mean something on the original.
    let dropped: &[f64] = match version {
        Version::Original => &a.dropped_beats,
        Version::Straightened => &[],
    };
    ui.horizontal_top(|ui| {
        ui.allocate_ui(vec2(main, height + 24.0), |ui| {
            ui.vertical(|ui| {
                title(
                    ui,
                    theme,
                    "Tempo over time",
                    &format!(
                        "BPM per beat · dashed = target · shaded bands = sections at a steady \
                         tempo · red ticks = dropped beats · click to seek · wheel to zoom{hint}"
                    ),
                );
                let r = TempoChart {
                    a,
                    target,
                    level,
                    range,
                    window,
                    playhead,
                    colour,
                }
                .show(ui, theme, height);
                ruler(&ui.painter_at(r.rect), theme, r.rect, window, dropped);
                if r.clicked()
                    && let Some(pos) = r.interact_pointer_pos()
                {
                    let t = time_at(r.rect, window, pos.x);
                    let t = tab.to_original(t, version);
                    let v = tab.version;
                    let at = tab.from_original(t, v);
                    tab.seek(v, at, cx);
                }
                if r.hovered() {
                    let scroll = ui.input(|i| i.raw_scroll_delta.y);
                    if scroll != 0.0 {
                        cx.send(ViewCmd::WaveZoom(zoomed(cx.prefs.wave_zoom, scroll)));
                    }
                }
            });
        });
        ui.allocate_ui(vec2(side, height + 24.0), |ui| {
            ui.vertical(|ui| {
                title(ui, theme, "Beat intervals", "histogram of BPM per beat");
                HistChart {
                    a,
                    target,
                    range: hist,
                    colour,
                }
                .show(ui, theme, height);
            });
        });
    });
}

fn stat(ui: &mut Ui, theme: &Theme, name: &str, value: &str, note: &str, flag: bool) {
    let stroke = if flag { theme.warn } else { theme.line };
    // A wrapped row can't see a card's width before drawing it, so it never wraps one:
    // start a new row when last frame's width doesn't fit.
    let id = ui.id().with(("stat", name));
    let last: Option<f32> = ui.data(|d| d.get_temp(id));
    // (A wrapped row's `available_width` is the whole row's.)
    let left = ui.max_rect().right() - ui.cursor().min.x;
    if last.is_some_and(|w| w > left) {
        ui.end_row();
    }
    let r = egui::Frame::new()
        .fill(theme.wave_bg)
        .stroke(Stroke::new(1.0_f32, stroke))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(8, 5))
        .show(ui, |ui| {
            ui.set_min_width(118.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.label(
                    RichText::new(name.to_uppercase())
                        .size(fs(10.0))
                        .color(theme.muted),
                );
                ui.horizontal(|ui| {
                    let v = RichText::new(value).size(fs(17.0)).strong();
                    ui.label(if flag { v.color(theme.warn) } else { v });
                    ui.label(RichText::new(note).size(fs(10.0)).color(theme.muted));
                });
            });
        });
    let w = r.response.rect.width();
    ui.data_mut(|d| d.insert_temp(id, w));
}

fn stats_a(ui: &mut Ui, theme: &Theme, a: &TempoAnalysis) {
    let spread = a.max_bpm - a.min_bpm;
    let downbeats = a.is_downbeat.iter().filter(|d| **d).count();
    let changes = a.tempo_changes.len();
    ui.horizontal_wrapped(|ui| {
        stat(
            ui,
            theme,
            "Detector",
            &detector_name(&a.detector),
            "",
            false,
        );
        stat(
            ui,
            theme,
            "Beats",
            &a.beats.len().to_string(),
            &format!("{downbeats} downbeats"),
            false,
        );
        stat(
            ui,
            theme,
            "Median tempo",
            &format!("{:.2}", a.median_bpm),
            "BPM",
            false,
        );
        stat(
            ui,
            theme,
            "Tempo range",
            &format!("{:.1}–{:.1}", a.min_bpm, a.max_bpm),
            "BPM, 2nd–98th pct",
            false,
        );
        stat(
            ui,
            theme,
            "Drift",
            &format!("±{:.1}%", a.drift_pct()),
            &format!("{spread:.1} BPM spread"),
            false,
        );
        let (v, note) = match changes {
            0 => ("none".to_string(), "drift only"),
            n => (n.to_string(), "look deliberate"),
        };
        stat(ui, theme, "Tempo changes", &v, note, changes > 0);
        stat(
            ui,
            theme,
            "Dropped",
            &a.dropped_beats.len().to_string(),
            "detections thrown away",
            false,
        );
        stat(
            ui,
            theme,
            "Kick shift",
            &format!("{:+.0}", a.kick_shift * 1000.0),
            "ms, beats onto the kicks",
            false,
        );
        stat(ui, theme, "Length", &fmt_time(a.duration), "", false);
    });
}

fn stats_b(ui: &mut Ui, theme: &Theme, m: &TempoAnalysis, s: &Straightened) {
    let spread = m.max_bpm - m.min_bpm;
    let (median, p95) = s.off_grid_ms(m);
    ui.horizontal_wrapped(|ui| {
        stat(
            ui,
            theme,
            "Median tempo",
            &format!("{:.2}", m.median_bpm),
            "BPM",
            false,
        );
        stat(
            ui,
            theme,
            "Tempo range",
            &format!("{:.1}–{:.1}", m.min_bpm, m.max_bpm),
            "BPM, 2nd–98th pct",
            false,
        );
        stat(
            ui,
            theme,
            "Drift",
            &format!("±{:.1}%", m.drift_pct()),
            &format!("{spread:.1} BPM spread"),
            false,
        );
        stat(
            ui,
            theme,
            "Off the grid",
            &format!("{median:.0} ms"),
            &format!("median · 95% within {p95:.0} ms"),
            false,
        );
        stat(
            ui,
            theme,
            "Beats",
            &m.beats.len().to_string(),
            &format!("found again of {}", s.grid.target_beats.len()),
            false,
        );
    });
}

/// tempo-lock's warning when the tempo steps rather than drifts, with a jump to each
/// change.
fn tempo_changes(ui: &mut Ui, tab: &mut TempoLockUi, a: &TempoAnalysis, cx: &mut Cx) {
    let theme = cx.theme;
    if a.tempo_changes.is_empty() {
        return;
    }
    let n = a.tempo_changes.len();
    egui::Frame::new()
        .fill(theme.warn.gamma_multiply(0.08))
        .stroke(Stroke::new(1.0_f32, theme.warn.gamma_multiply(0.5)))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let head = if n == 1 {
                "A tempo change that looks deliberate:".to_string()
            } else {
                format!("{n} tempo changes that look deliberate:")
            };
            let sections: Vec<String> =
                a.sections.iter().map(|s| format!("{:.1}", s.bpm)).collect();
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(head).strong().color(theme.warn));
                ui.label(format!(
                    "the tempo steps to a new value and stays there, rather than drifting. \
                     Sections: {} BPM.",
                    sections.join(" / ")
                ));
            });
            for c in &a.tempo_changes {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(fmt_time(c.time)).strong());
                    let what = match c.kind {
                        crate::core::tempo_lock::ChangeKind::TempoChange => format!(
                            "{:.1} to {:.1} BPM ({:+.0}%)",
                            c.from_bpm,
                            c.to_bpm,
                            (c.ratio - 1.0) * 100.0
                        ),
                        k => format!(
                            "{} feel, {:.1} to {:.1} BPM",
                            if k == crate::core::tempo_lock::ChangeKind::HalfTime {
                                "half-time"
                            } else {
                                "double-time"
                            },
                            c.from_bpm,
                            c.to_bpm
                        ),
                    };
                    ui.label(what);
                    if ui.link("jump to").clicked() {
                        let v = tab.version;
                        let at = tab.from_original(c.time, v);
                        tab.seek(v, at, cx);
                    }
                });
            }
            ui.label(
                RichText::new(
                    "Straightening to one BPM will speed up or slow down whole sections, not \
                     just tidy the drift. Check the histogram: separate humps are separate tempos.",
                )
                .size(fs(11.0))
                .color(theme.muted),
            );
        });
}

// ---- words ---------------------------------------------------------------------------------

fn stage_text(stage: Stage) -> &'static str {
    match stage {
        Stage::Analyse => "Detecting beats…",
        Stage::Straighten => "Stretching with Rubber Band…",
        Stage::Measure => "Detecting beats in the output…",
        Stage::SaveToLibrary => "Saving to the library…",
    }
}

fn stage_name(stage: Stage) -> &'static str {
    match stage {
        Stage::Analyse => "Beat detection",
        Stage::Straighten => "Straightening",
        Stage::Measure => "Measuring the output",
        Stage::SaveToLibrary => "Saving",
    }
}

fn note_label(ui: &mut Ui, theme: &Theme, note: &Note) {
    match note {
        Note::Failed(stage, e) => {
            let missing = ["rubberband", "rubber band", "ffmpeg"]
                .iter()
                .any(|w| e.to_lowercase().contains(w));
            if missing {
                ui.label(
                    RichText::new("Install rubberband-cli and ffmpeg (apt install rubberband-cli ffmpeg), then try again.")
                        .color(theme.warn),
                );
            }
            ui.label(RichText::new(format!("{} failed: {e}", stage_name(*stage))).color(theme.bad));
        }
        Note::Cancelled(stage) => {
            ui.label(RichText::new(format!("{} cancelled", stage_name(*stage))).color(theme.muted));
        }
        Note::Saved(path) => {
            ui.label(RichText::new(format!("Saved to {}", path.display())).color(theme.ok));
        }
    }
}

fn detector_name(d: &str) -> String {
    match d {
        "beat_this" => "Beat This!".into(),
        d => d.into(),
    }
}

fn level_name(l: BeatLevel) -> &'static str {
    match l {
        BeatLevel::Beat => "the beat",
        BeatLevel::HalfTime => "half-time (×2)",
        BeatLevel::DoubleTime => "double-time (÷2)",
    }
}

fn version_colour(theme: &Theme, v: Version) -> Color32 {
    match v {
        Version::Original => theme.versions[0],
        Version::Straightened => theme.versions[1],
    }
}

/// A folder's last two parts, for the dropdown's closed face.
fn short_path(p: &str) -> &str {
    let mut cut = p.len();
    for _ in 0..2 {
        match p[..cut].rfind('/') {
            Some(i) if i > 0 => cut = i,
            _ => return p,
        }
    }
    &p[cut + 1..]
}

// ---- over Prepare's waveform ---------------------------------------------------------------

/// The beats the tool found, over Prepare's waveform while one of its versions is in
/// the player: in the original, where they were detected (and the detections thrown
/// away, as red ticks); in the straightened version, where each was put. A line
/// across the middle in the version's colour, longer on a downbeat, so the player's
/// own beatgrid still shows around it.
pub struct DetectedBeats {
    pub version: Version,
    pub chart: Arc<Chart>,
    pub straightened: Option<Arc<Straightened>>,
}

impl Layer for DetectedBeats {
    fn draw(&self, tl: &Timeline) {
        let (beats, downs) = version_beats(self.version, &self.chart, self.straightened.as_deref());
        let (p, rect) = (tl.painter, tl.rect);
        let colour = version_colour(&THEME, self.version);
        let (cy, h) = (rect.center().y, rect.height());
        let i0 = beats.partition_point(|&t| t < tl.view.0);
        let i1 = beats.partition_point(|&t| t <= tl.view.1);
        for i in i0..i1 {
            let x = tl.x_of(beats[i]);
            let (reach, width) = match downs.get(i).copied().unwrap_or(false) {
                true => (0.32, 2.0),
                false => (0.16, 1.5_f32),
            };
            p.line_segment(
                [Pos2::new(x, cy - h * reach), Pos2::new(x, cy + h * reach)],
                Stroke::new(width, colour),
            );
        }
        if self.version == Version::Original {
            for &d in &self.chart.analysis.dropped_beats {
                if (tl.view.0..=tl.view.1).contains(&d) {
                    let x = tl.x_of(d);
                    p.line_segment(
                        [
                            Pos2::new(x, rect.bottom() - 8.0),
                            Pos2::new(x, rect.bottom()),
                        ],
                        Stroke::new(1.0_f32, THEME.bad),
                    );
                }
            }
        }
    }
}
