//! The top bar (mode, menu, audio, MIDI and analysis pills, frame timing) and the
//! status line along the bottom: what earshot is doing in the background (with a
//! progress bar when it can be counted), then the MIDI log.

use std::collections::{HashMap, HashSet};

use earshot_engine::OutputInfo;
use egui::{RichText, Ui};

use super::{Cx, Panel};
use crate::core::command::{AppCmd, Command, ViewCmd};
use crate::core::index::LibraryCmd;
use crate::core::jobs::Running;
use crate::core::notice::{Level, Notice};
use crate::core::view::{Modal, Mode, SettingsTab};
use crate::ui::theme::fs;
use crate::ui::widgets::{Intent, Pill, Seg, Toggle, muted, segmented};

/// Tracks being analysed, and failures since they were last dismissed.
#[derive(Default)]
pub struct AnalysisSummary {
    pub busy: HashSet<i64>,
    pub running: HashMap<i64, f64>,
    pub failed: u32,
    pub last_error: String,
    /// The analysis worker was refused (it doesn't match this app): why. Until a
    /// restart, nothing is analysed.
    pub worker_problem: Option<String>,
}

/// The CPU meter: how much of its time the audio engine needs.
pub struct Meter<'a> {
    /// The slowest recent audio callback, as a share of its time (1 or more: too
    /// slow, the sound drops out). `None` without audio.
    pub load: Option<f32>,
    /// Frame timing, and the audio callback's: the tooltip.
    pub detail: [&'a str; 2],
    /// The memory watch's latest reading (`None` until it has one).
    pub memory: Option<crate::core::log::Memory>,
}

pub struct TopBar<'a> {
    pub audio: &'a Result<OutputInfo, String>,
    /// The audio stalled and was restarted, or keeps stalling (`core::audio_watch`).
    pub audio_note: Option<&'a str>,
    pub midi_ports: &'a [String],
    pub analysis: &'a AnalysisSummary,
    /// The jobs running and queued, for the jobs pill.
    pub jobs: &'a [Running],
    pub hud: Meter<'a>,
    /// What undo and redo would do ("edit title"), if anything.
    pub undo: Option<String>,
    pub redo: Option<String>,
}

impl Panel for TopBar<'_> {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let t = cx.theme;
        ui.horizontal(|ui| {
            ui.menu_button("Menu", |ui| {
                ui.menu_button("Import", |ui| {
                    if ui.button("Folder…").clicked() {
                        cx.send(LibraryCmd::PickRoots);
                        ui.close();
                    }
                });
                if ui.button("Settings…").clicked() {
                    cx.send(ViewCmd::ToggleModal(Modal::Settings));
                    ui.close();
                }
                if ui.button("Rescan library").clicked() {
                    cx.send(LibraryCmd::Rescan);
                    ui.close();
                }
                if ui
                    .button("Re-analyse stale tracks…")
                    .on_hover_text(
                        "Settings > Analysis: how many are out of date, and re-analyse them",
                    )
                    .clicked()
                {
                    cx.send(ViewCmd::Settings(SettingsTab::Analysis));
                    ui.close();
                }
                ui.menu_button("View", |ui| {
                    ui.menu_button("Text size", |ui| {
                        let now = cx.prefs.ui_scale;
                        for (z, name) in crate::ui::prefs::TEXT_SIZES {
                            if ui.radio((now - z).abs() < 0.01, name).clicked() {
                                cx.send(ViewCmd::UiScale(z));
                                ui.close();
                            }
                        }
                    });
                });
            });
            if cx.prefs.mode == Mode::Prepare {
                ui.menu_button("Edit", |ui| {
                    for (label, what, keys, cmd) in [
                        ("Undo", &self.undo, "Ctrl+Z", Command::Undo),
                        ("Redo", &self.redo, "Ctrl+Shift+Z", Command::Redo),
                    ] {
                        let text = match what {
                            Some(w) => format!("{label} {w}"),
                            None => label.to_string(),
                        };
                        let b = egui::Button::new(text).shortcut_text(keys);
                        if ui.add_enabled(what.is_some(), b).clicked() {
                            cx.send(cmd);
                            ui.close();
                        }
                    }
                });
            }
            ui.label(RichText::new("earshot").strong().color(t.accent));
            let mut mode = cx.prefs.mode;
            egui::ComboBox::from_id_salt("mode")
                .selected_text(match mode {
                    Mode::Prepare => "Prepare",
                    Mode::Perform => "Perform",
                })
                .width(90.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut mode, Mode::Prepare, "Prepare")
                        .on_hover_text("One player: set cues, fix beat grids, build playlists");
                    ui.selectable_value(&mut mode, Mode::Perform, "Perform")
                        .on_hover_text("Two or four decks and the mixer");
                });
            if mode != cx.prefs.mode {
                cx.send(ViewCmd::SetMode(mode));
            }
            if mode == Mode::Perform {
                deck_count(ui, cx);
                panes(ui, cx);
            }
            let (audio, ok) = match self.audio {
                Ok(i) => (
                    format!(
                        "{}{}{} · {} kHz · {} frames · {} ch",
                        if i.null || i.phones_error.is_some() {
                            "⚠ "
                        } else {
                            ""
                        },
                        i.device,
                        i.phones
                            .as_ref()
                            .map_or(String::new(), |(p, _)| format!(" · headphones: {p}")),
                        i.sample_rate / 1000,
                        i.buffer_frames.map_or("default".into(), |b| b.to_string()),
                        i.channels
                    ),
                    !i.null && i.phones_error.is_none(),
                ),
                Err(e) => (format!("no audio: {e}"), false),
            };
            let (audio, ok) = match self.audio_note {
                Some(note) => (format!("⚠ {note}"), false),
                None => (audio, ok),
            };
            let intent = if ok { Intent::Success } else { Intent::Warning };
            if ui
                .add(Pill::new(&audio, intent))
                .on_hover_text("Audio settings")
                .clicked()
            {
                cx.send(ViewCmd::Settings(SettingsTab::Audio));
            }
            let midi = if self.midi_ports.is_empty() {
                "MIDI: none".to_string()
            } else {
                format!("MIDI: {}", self.midi_ports.join(", "))
            };
            let intent = if self.midi_ports.is_empty() {
                Intent::Neutral
            } else {
                Intent::Success
            };
            if ui
                .add(Pill::new(&midi, intent))
                .on_hover_text("Click to reconnect MIDI")
                .clicked()
            {
                cx.send(AppCmd::ReconnectMidi);
            }
            self.analysis_pill(ui, cx);
            self.jobs_pill(ui, cx);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Keys").clicked() {
                    cx.send(ViewCmd::ToggleModal(Modal::Keys));
                }
                self.memory_meter(ui, cx);
                ui.add_space(6.0);
                self.cpu_meter(ui, cx);
            });
        });
    }
}

impl TopBar<'_> {
    /// "CPU" and a bar: green, amber from half, red from 80 % (where a callback
    /// counts as slow).
    fn cpu_meter(&self, ui: &mut Ui, cx: &mut Cx) {
        let t = cx.theme;
        let load = self.hud.load;
        let (rect, r) = ui.allocate_exact_size(egui::vec2(60.0, 10.0), egui::Sense::hover());
        let p = ui.painter();
        p.rect_filled(rect, 2.0, t.line);
        if let Some(l) = load {
            let colour = match l {
                l if l >= 0.8 => t.bad,
                l if l >= 0.5 => t.warn,
                _ => t.ok,
            };
            let mut fill = rect;
            fill.set_width(rect.width() * l.clamp(0.02, 1.0));
            p.rect_filled(fill, 2.0, colour);
        }
        let tip = match load {
            Some(l) => format!(
                "Audio engine load: {:.0} % of the time it has, at worst lately\n{}\n{}",
                l * 100.0,
                self.hud.detail[1],
                self.hud.detail[0]
            ),
            None => format!("No audio\n{}", self.hud.detail[0]),
        };
        r.on_hover_text(tip);
        ui.label(RichText::new("CPU").size(fs(11.0)));
    }

    /// "MEM" and a bar: the machine's RAM in use. Amber when it's low (little free,
    /// or the swap mostly used), red when it's stalling for memory (swapping hard,
    /// which can stall the audio).
    fn memory_meter(&self, ui: &mut Ui, cx: &mut Cx) {
        let t = cx.theme;
        let mem = self.hud.memory;
        let (rect, r) = ui.allocate_exact_size(egui::vec2(60.0, 10.0), egui::Sense::hover());
        let p = ui.painter();
        p.rect_filled(rect, 2.0, t.line);
        if let Some(m) = mem {
            let colour = if m.stalled() {
                t.bad
            } else if m.low() {
                t.warn
            } else {
                t.ok
            };
            let mut fill = rect;
            fill.set_width(rect.width() * m.used().clamp(0.02, 1.0));
            p.rect_filled(fill, 2.0, colour);
        }
        let mb = |b: u64| b >> 20;
        let tip = match mem {
            Some(m) => {
                let mut tip = format!(
                    "Memory: {} of {} MB in use; swap {} of {} MB
\
                     Earshot: {} MB ({} MB of deck audio kept in RAM)",
                    mb(m.total - m.available.min(m.total)),
                    mb(m.total),
                    mb(m.swap_total - m.swap_free.min(m.swap_total)),
                    mb(m.swap_total),
                    mb(m.ours),
                    mb(m.locked),
                );
                if m.stalled() {
                    tip += &format!(
                        "\nThe system is waiting for memory {:.0} % of the time: \
                         close something",
                        m.stall
                    );
                }
                tip
            }
            None => "Memory: not measured".into(),
        };
        r.on_hover_text(tip);
        ui.label(RichText::new("MEM").size(fs(11.0)));
    }

    /// "3 jobs", with a spinner, while any run or wait; clicked, a dropdown with a
    /// row for each (and Stop analysis, while there's analysis).
    fn jobs_pill(&self, ui: &mut Ui, cx: &mut Cx) {
        let n = self.jobs.len();
        if n == 0 {
            return;
        }
        let text = format!("{n} job{}", if n == 1 { "" } else { "s" });
        let analysing = !self.analysis.busy.is_empty();
        let r = ui.add(Pill::new(&text, Intent::Info).busy());
        egui::Popup::menu(&r)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                ui.set_min_width(fs(320.0));
                for j in self.jobs {
                    job_line(ui, j, cx);
                }
                if analysing {
                    ui.separator();
                    if ui.button("Stop analysis").clicked() {
                        cx.send(AppCmd::StopAnalysis);
                        ui.close();
                    }
                }
            });
    }

    fn analysis_pill(&self, ui: &mut Ui, cx: &mut Cx) {
        let a = self.analysis;
        if let Some(why) = &a.worker_problem {
            ui.add(Pill::new("analysis worker out of date", Intent::Danger))
                .on_hover_text(why);
            return;
        }
        // What's being analysed is in the jobs pill: this is for failures.
        let n = a.busy.len();
        if a.failed == 0 {
            return;
        }
        let text = format!("analysis: {} failed", a.failed);
        // Only failures, nothing running: red. Still working: amber.
        let intent = if n == 0 {
            Intent::Danger
        } else {
            Intent::Warning
        };
        let r = ui.add(Pill::new(&text, intent));
        let running: Vec<String> = a
            .running
            .keys()
            .filter_map(|id| cx.core.lib.get(*id))
            .map(|t| format!("Now: {}", t.title))
            .collect();
        let failures = if a.last_error.is_empty() {
            "Click to dismiss failures".to_string()
        } else {
            format!("Last error: {}\nClick to dismiss.", a.last_error)
        };
        let r = r.on_hover_text([running.join("\n"), failures].join("\n").trim().to_string());
        if r.clicked() {
            cx.send(AppCmd::DismissFailures);
        }
    }
}

/// The MIDI messages last received, newest first.
/// Perform's 2 / 4 deck toggle. Back to two only once decks 3 and 4 have stopped, so
/// nothing plays out of sight.
/// Switches for Perform's mixer and effect units, to show or hide them mid-set.
fn panes(ui: &mut Ui, cx: &mut Cx) {
    let mut mixer = cx.prefs.mixer_shown();
    if ui
        .add(Toggle::new(&mut mixer, "Mixer"))
        .on_hover_text("The mixer, between the decks")
        .changed()
    {
        cx.send(ViewCmd::ShowMixer(mixer));
    }
    let mut fx = cx.prefs.show_fx;
    if ui
        .add(Toggle::new(&mut fx, "FX"))
        .on_hover_text("The effect units, in a strip under the decks")
        .changed()
    {
        cx.send(ViewCmd::ShowFx(fx));
    }
}

fn deck_count(ui: &mut Ui, cx: &mut Cx) {
    let four = cx.prefs.four_decks;
    let hidden_playing = cx
        .engine
        .snap
        .as_ref()
        .is_some_and(|s| s.live().iter().skip(2).any(|d| d.playing));
    let counts = [
        Seg::new(false, "2 decks")
            .tip("Two decks")
            .enabled(!hidden_playing)
            .disabled_tip("Stop decks 3 and 4 first"),
        Seg::new(true, "4 decks").tip("Four decks, smaller"),
    ];
    if let Some(four) = segmented(ui, four, &counts) {
        cx.send(ViewCmd::FourDecks(four));
    }
}

/// A job's line: spinner (or "queued"), what it's doing, and its bar when counted.
fn job_line(ui: &mut Ui, j: &Running, cx: &mut Cx) {
    let t = cx.theme;
    ui.horizontal(|ui| {
        if j.queued {
            muted(ui, "queued");
        } else {
            ui.spinner();
        }
        let text = RichText::new(j.text()).size(fs(11.0));
        ui.label(if j.queued { text.color(t.muted) } else { text });
        if let Some(f) = j.fraction() {
            ui.add(
                egui::ProgressBar::new(f)
                    .desired_width(fs(160.0))
                    .desired_height(fs(8.0))
                    .fill(t.accent),
            );
            muted(ui, format!("{:.0} %", f * 100.0));
        }
        if j.cancellable
            && !j.cancelling
            && ui
                .small_button("Cancel")
                .on_hover_text(format!("Stop {}", j.title))
                .clicked()
        {
            cx.send(AppCmd::CancelJob(j.id));
        }
    });
}

/// The bottom line: messages for the user, newest first, then the first job with
/// its bar (the others in the top bar's jobs pill).
pub struct StatusLine<'a> {
    pub jobs: &'a [Running],
    /// Messages for the user, oldest first (each drawn with a close button).
    pub notices: &'a [Notice],
}

impl Panel for StatusLine<'_> {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let t = cx.theme;
        ui.horizontal(|ui| {
            // Newest first.
            for n in self.notices.iter().rev() {
                let (colour, icon) = match n.level {
                    Level::Info => (t.warn, ""),
                    Level::Warning => (t.bad, "⚠ "),
                };
                egui::Frame::new()
                    .fill(colour.gamma_multiply(0.18))
                    .stroke(egui::Stroke::new(1.0_f32, colour))
                    .corner_radius(3)
                    .inner_margin(egui::Margin::symmetric(6, 0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("{icon}{}", n.text))
                                    .size(fs(11.0))
                                    .color(colour),
                            );
                            if ui
                                .add(
                                    egui::Button::new(RichText::new("×").size(fs(13.0)))
                                        .frame(false),
                                )
                                .on_hover_text("Dismiss")
                                .clicked()
                            {
                                cx.send(AppCmd::DismissNotice(n.id));
                            }
                        });
                    });
            }
            if let Some(first) = self.jobs.first() {
                if !self.notices.is_empty() {
                    ui.separator();
                }
                job_line(ui, first, cx);
                let more = self.jobs.len() - 1;
                if more > 0 {
                    muted(ui, format!("+{more} more"));
                }
            }
        });
    }
}
