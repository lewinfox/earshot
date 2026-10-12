//! The Tempo lock tab, Prepare's tool for tracks whose tempo wanders: analyse a track
//! (right-click it, "Normalise tempo"), choose a BPM, straighten it, listen to both
//! versions, and save the result into a library folder. tempo-lock's page in egui
//! (`../tempo-lock/static/`): its stats, warnings, tempo charts, histograms and
//! waveforms with their beat grids, for the original and the straightened version.
//!
//! The tool's work is in `core::tempo_lock`; this tab keeps what it last heard from
//! it ([`TempoLockUi::on_event`]) and sends `TempoLockCmd`s. Listening goes through
//! the headphones' preview player (the shell's `app/tempo_lock.rs`), which reports
//! back in [`ListenStatus`].

mod charts;

use std::path::PathBuf;
use std::sync::Arc;

use egui::{Color32, CornerRadius, Margin, Response, RichText, Sense, Stroke, Ui, vec2};

use self::charts::{HistChart, TempoChart, bpm_range, fmt_time, ruler, time_at};
use super::deck::wave::{self, DeckWave, Grid, Marks};
use super::library::drag;
use super::{Cx, Panel};
use crate::core::tempo_lock::{
    BeatLevel, Chart, Engine, ListenCmd, Stage, StraightenOptions, Straightened, TempoAnalysis,
    TempoLockCmd, TempoLockEvent, Version, Waveform, histogram_range, version_beats,
};
use crate::core::track::TrackId;
use crate::ui::theme::Theme;

/// The shortest stretch the waveforms zoom in to, s.
const MIN_SPAN: f64 = 1.0;
const WAVE_H: f32 = 140.0;
const OVERVIEW_H: f32 = 24.0;
const CHART_H: f32 = 120.0;

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
    /// Which version plays, and the click track on its grid.
    pub version: Version,
    pub clicks: bool,
    pub follow: bool,

    // ---- view ---------------------------------------------------------------------
    /// The waveforms' window, in the original's time (s).
    view: (f64, f64),
    /// Where playback is (or will start), in the original's time.
    cursor: f64,
    /// The original's and the straightened version's waveforms, as the decks have
    /// theirs: made from the tool's [`Waveform`]s when first drawn.
    waves: [Option<DeckWave>; 2],
    /// The preview player, as the shell last saw it.
    pub listen: ListenStatus,
}

/// How the last step ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Note {
    Failed(Stage, String),
    Cancelled(Stage),
    Saved(PathBuf),
}

/// The tab's A/B player as the shell sees it (set every frame).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ListenStatus {
    /// What of the tab's is in the preview player: a version, with clicks or not.
    pub loaded: Option<(Version, bool)>,
    /// Decoding a version to play it.
    pub decoding: bool,
    pub error: Option<String>,
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
            clicks: false,
            follow: true,
            view: (0.0, 30.0),
            cursor: 0.0,
            waves: [None, None],
            listen: ListenStatus::default(),
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
            clicks: self.clicks,
            follow: self.follow,
            listen: std::mem::take(&mut self.listen),
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
                self.view = (0.0, a.duration.min(30.0));
                self.cursor = 0.0;
                self.waves = [None, None];
                self.chart = Some(chart);
                self.busy = None;
            }
            TempoLockEvent::Straightened { result, .. } => {
                self.straightened = Some(result);
                self.waves[1] = None;
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

    fn duration(&self) -> f64 {
        self.chart
            .as_ref()
            .map_or(1.0, |c| c.analysis.duration.max(1e-3))
    }

    /// Show `v0..v1` (original time), kept inside the track.
    fn set_view(&mut self, v0: f64, v1: f64) {
        let dur = self.duration();
        let span = (v1 - v0).clamp(MIN_SPAN.min(dur), dur);
        let v0 = v0.clamp(0.0, (dur - span).max(0.0));
        self.view = (v0, v0 + span);
    }

    /// Whether the tab's audio is in the preview player, and playing.
    fn playing(&self, cx: &Cx) -> bool {
        self.listen.loaded.is_some() && cx.engine.snap.as_ref().is_some_and(|s| s.preview.playing)
    }

    /// Play from the cursor, in the version chosen.
    fn play(&self, cx: &mut Cx) {
        let version = self.version;
        cx.send(TempoLockCmd::Listen(ListenCmd::Play {
            version,
            at: self.from_original(self.cursor, version).max(0.0),
            clicks: self.clicks,
        }));
    }

    /// Move to `t` in `version`'s time, and play that version there if playing.
    fn seek(&mut self, version: Version, t: f64, cx: &mut Cx) {
        let playing = self.playing(cx);
        self.version = version;
        self.cursor = self.to_original(t, version).max(0.0);
        if playing {
            self.play(cx);
        }
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
        if let (Some((v, _)), Some(snap)) = (tab.listen.loaded, cx.engine.snap.as_ref()) {
            tab.cursor = tab.to_original(snap.preview.position, v);
            if tab.playing(cx) && tab.follow {
                let span = tab.view.1 - tab.view.0;
                if tab.cursor > tab.view.1 || tab.cursor < tab.view.0 {
                    tab.set_view(tab.cursor - span * 0.1, tab.cursor + span * 0.9);
                }
            }
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
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| body(ui, tab, &chart, cx));
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
                ui.label(RichText::new(&t.title).size(16.0).strong());
                if !t.artist.is_empty() {
                    ui.label(RichText::new(&t.artist).color(theme.muted));
                }
            }
            None => {
                ui.label(RichText::new("Tempo lock").size(16.0).strong());
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
                .size(11.0)
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
            if stage != Stage::SaveToLibrary && ui.button("Cancel").clicked() {
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
            .small_button("median")
            .on_hover_text("The median detected tempo")
            .clicked()
        {
            tab.bpm = (median * tab.level.factor() * 100.0).round() / 100.0;
        }
        if ui
            .small_button("round")
            .on_hover_text("Round to a whole number")
            .clicked()
        {
            tab.bpm = tab.bpm.round();
        }
        ui.label("Detected beats are").on_hover_text(
            "If the detector locked onto half- or double-time, say so here and type the real BPM",
        );
        let before = tab.level;
        egui::ComboBox::from_id_salt("tempo-lock-level")
            .selected_text(level_name(tab.level))
            .show_ui(ui, |ui| {
                for l in [BeatLevel::Beat, BeatLevel::HalfTime, BeatLevel::DoubleTime] {
                    ui.selectable_value(&mut tab.level, l, level_name(l));
                }
            });
        if tab.level != before {
            tab.bpm = (median * tab.level.factor()).round();
        }
        ui.label("Engine");
        egui::ComboBox::from_id_salt("tempo-lock-engine")
            .selected_text(engine_name(tab.engine))
            .show_ui(ui, |ui| {
                for e in [Engine::R3, Engine::R2] {
                    ui.selectable_value(&mut tab.engine, e, engine_name(e));
                }
            });
        let ok = tab.can_straighten();
        let b = ui.add_enabled(
            ok.is_ok(),
            egui::Button::new(RichText::new("Straighten").strong().color(theme.ink))
                .fill(theme.accent),
        );
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
            ui.label(RichText::new(why).size(11.0).color(theme.warn));
        }

        // Saving: once there's a straightened file.
        if tab.straightened.is_some() {
            ui.separator();
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
                    egui::Button::new("Save to library"),
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
        }
    });
}

/// Play, stop, A/B, the click track, follow, and the time.
fn transport(ui: &mut Ui, tab: &mut TempoLockUi, cx: &mut Cx) {
    let theme = cx.theme;
    let playing = tab.playing(cx);
    ui.horizontal_wrapped(|ui| {
        let play = if playing { "Pause" } else { "Play" };
        if ui
            .add(egui::Button::new(RichText::new(play).strong()).min_size(vec2(60.0, 0.0)))
            .on_hover_text("In the headphones (the preview player)")
            .clicked()
        {
            if playing {
                cx.send(TempoLockCmd::Listen(ListenCmd::Pause));
            } else {
                tab.play(cx);
            }
        }
        if ui.button("Stop").clicked() {
            cx.send(TempoLockCmd::Listen(ListenCmd::Stop));
            tab.cursor = 0.0;
        }
        ui.separator();
        for (v, name) in [
            (Version::Original, "Original"),
            (Version::Straightened, "Straightened"),
        ] {
            let enabled = v == Version::Original || tab.straightened.is_some();
            let r = ui.add_enabled(enabled, egui::Button::selectable(tab.version == v, name));
            if r.clicked() && tab.version != v {
                tab.version = v;
                if playing {
                    tab.play(cx);
                }
            }
        }
        ui.separator();
        if ui
            .checkbox(&mut tab.clicks, "click track on beats")
            .changed()
            && playing
        {
            tab.play(cx);
        }
        ui.checkbox(&mut tab.follow, "follow playhead");
        let at = tab.from_original(tab.cursor, tab.version);
        ui.label(RichText::new(fmt_time(at)).monospace().size(14.0));
        if tab.listen.decoding {
            ui.spinner();
            ui.label(RichText::new("decoding…").color(theme.muted));
        }
        if let Some(e) = &tab.listen.error {
            ui.label(RichText::new(e).color(theme.bad));
        }
    });
}

/// Everything charted: the original's stats, warning, charts and waveform; then the
/// straightened version's.
fn body(ui: &mut Ui, tab: &mut TempoLockUi, chart: &Chart, cx: &mut Cx) {
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
        "",
        cx,
    );

    ui.add_space(6.0);
    title(
        ui,
        theme,
        "Original",
        "grid = detected beats · red lines are downbeats · click to seek · wheel to zoom · drag to pan",
    );
    let (beats, downs) = version_beats(Version::Original, chart, None);
    wave(
        ui,
        tab,
        Version::Original,
        &chart.wave,
        a.duration,
        beats,
        downs,
        &a.dropped_beats,
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
            "{} BPM · {} · MP3 {} kbps · {file} · grid = where each original beat now sits",
            s.grid.target_bpm,
            fmt_time(s.duration),
            s.bitrate_kbps
        ),
    );
    let (beats, downs) = version_beats(Version::Straightened, chart, Some(&*s));
    wave(
        ui,
        tab,
        Version::Straightened,
        &s.wave,
        s.duration,
        beats,
        downs,
        &[],
        cx,
    );
    ui.add_space(6.0);
    let measuring = matches!(tab.busy, Some((Stage::Measure | Stage::Straighten, _)));
    ui.horizontal(|ui| {
        ui.label(RichText::new("Measured on the straightened audio").strong());
        if measuring {
            ui.spinner();
            ui.label(
                RichText::new("detecting beats in the output…")
                    .size(11.0)
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
            " · beats detected again in the output · same scale as the original",
            cx,
        );
    }
}

/// A section's title with a hint after it.
fn title(ui: &mut Ui, theme: &Theme, name: &str, hint: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(name).strong());
        ui.label(RichText::new(hint).size(11.0).color(theme.muted));
    });
}

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
    hint: &str,
    cx: &mut Cx,
) {
    let theme = cx.theme;
    let colour = version_colour(theme, version);
    let w = ui.available_width();
    let side = (w * 0.26).clamp(200.0, 420.0);
    let main = w - side - 10.0;
    let window = (
        tab.from_original(tab.view.0, version),
        tab.from_original(tab.view.1, version),
    );
    let playhead = Some(tab.from_original(tab.cursor, version));
    ui.horizontal_top(|ui| {
        ui.allocate_ui(vec2(main, CHART_H + 24.0), |ui| {
            ui.vertical(|ui| {
                title(
                    ui,
                    theme,
                    "Tempo over time",
                    &format!(
                        "BPM per beat · dashed = target · shaded bands = sections at a steady tempo{hint}"
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
                .show(ui, theme, CHART_H);
                if r.clicked()
                    && let Some(pos) = r.interact_pointer_pos()
                {
                    let t = time_at(r.rect, (0.0, a.duration), pos.x);
                    let t = tab.to_original(t, version);
                    let v = tab.version;
                    let at = tab.from_original(t, v);
                    tab.seek(v, at, cx);
                }
            });
        });
        ui.allocate_ui(vec2(side, CHART_H + 24.0), |ui| {
            ui.vertical(|ui| {
                title(ui, theme, "Beat intervals", "histogram of BPM per beat");
                HistChart {
                    a,
                    target,
                    range: hist,
                    colour,
                }
                .show(ui, theme, CHART_H);
            });
        });
    });
}

/// A version's waveform and its overview, drawn as a deck's are, with seeking, panning
/// and zooming.
#[allow(clippy::too_many_arguments)]
fn wave(
    ui: &mut Ui,
    tab: &mut TempoLockUi,
    version: Version,
    data: &Waveform,
    duration: f64,
    beats: &[f64],
    downbeats: &[bool],
    dropped: &[f64],
    cx: &mut Cx,
) {
    let i = match version {
        Version::Original => 0,
        Version::Straightened => 1,
    };
    if tab.waves[i].is_none() && !data.detail.is_empty() {
        tab.waves[i] = Some(DeckWave::new(
            ui.ctx(),
            &format!("tempo-lock-overview-{i}"),
            data.bands.clone(),
            data.detail.clone(),
        ));
    }
    let view = (
        tab.from_original(tab.view.0, version),
        tab.from_original(tab.view.1, version),
    );
    let at = tab.from_original(tab.cursor, version);
    // The other version's playhead is faint.
    let idle = tab.version != version;
    let width = ui.available_width();
    let (rect, r) = ui.allocate_exact_size(vec2(width, WAVE_H), Sense::click_and_drag());
    let marks = Marks {
        grid: Grid::Beats {
            times: beats,
            downbeats,
        },
        idle,
        ..Marks::default()
    };
    let painter = ui.painter_at(rect);
    wave::window(&painter, rect, tab.waves[i].as_ref(), view, at, &marks);
    ruler(&painter, cx.theme, rect, view, dropped);
    wave_input(ui, tab, &r, version, view, cx);

    let (rect, o) = ui.allocate_exact_size(vec2(width, OVERVIEW_H), Sense::click_and_drag());
    let marks = Marks {
        window: Some(view),
        idle,
        ..Marks::default()
    };
    let wv = tab.waves[i].as_ref();
    wave::overview(&ui.painter_at(rect), rect, wv, at, duration, &marks, None);
    if (o.clicked() || o.dragged())
        && let Some(pos) = o.interact_pointer_pos()
    {
        let t = time_at(o.rect, (0.0, duration), pos.x);
        let centre = tab.to_original(t, version);
        let span = tab.view.1 - tab.view.0;
        tab.set_view(centre - span / 2.0, centre + span / 2.0);
    }
}

/// Click to seek (and pick that version), drag to pan, wheel to zoom (Shift + wheel
/// pans).
fn wave_input(
    ui: &mut Ui,
    tab: &mut TempoLockUi,
    r: &Response,
    version: Version,
    view: (f64, f64),
    cx: &mut Cx,
) {
    let span = tab.view.1 - tab.view.0;
    let w = r.rect.width().max(1.0) as f64;
    if r.dragged() {
        let d = -r.drag_delta().x as f64 / w * span;
        tab.set_view(tab.view.0 + d, tab.view.1 + d);
    } else if r.clicked()
        && let Some(pos) = r.interact_pointer_pos()
    {
        tab.seek(version, time_at(r.rect, view, pos.x), cx);
    }
    if r.hovered() {
        let (delta, shift, pointer) = ui.input(|i| {
            (
                i.smooth_scroll_delta,
                i.modifiers.shift,
                i.pointer.hover_pos(),
            )
        });
        if delta != egui::Vec2::ZERO {
            // The wheel is the waveform's, not the page's.
            ui.ctx()
                .input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
            if shift || delta.x.abs() > delta.y.abs() {
                let d = -(if delta.x != 0.0 { delta.x } else { delta.y }) as f64 / w * span;
                tab.set_view(tab.view.0 + d, tab.view.1 + d);
            } else if let Some(p) = pointer {
                let frac = ((p.x - r.rect.left()) as f64 / w).clamp(0.0, 1.0);
                let anchor = tab.view.0 + frac * span;
                let ns = span * (-delta.y as f64 * 0.004).exp();
                let ns = ns.clamp(MIN_SPAN, tab.duration());
                tab.set_view(anchor - frac * ns, anchor + (1.0 - frac) * ns);
            }
        }
    }
}

// ---- stats ---------------------------------------------------------------------------------

/// A stat tile: its name, value and a note under it. `flag` marks it in amber.
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
                        .size(10.0)
                        .color(theme.muted),
                );
                ui.horizontal(|ui| {
                    let v = RichText::new(value).size(17.0).strong();
                    ui.label(if flag { v.color(theme.warn) } else { v });
                    ui.label(RichText::new(note).size(10.0).color(theme.muted));
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
                        let span = tab.view.1 - tab.view.0;
                        tab.set_view(c.time - span * 0.3, c.time + span * 0.7);
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
                .size(11.0)
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

fn engine_name(e: Engine) -> &'static str {
    match e {
        Engine::R3 => "Rubber Band R3 (best)",
        Engine::R2 => "Rubber Band R2 (faster)",
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
