//! The top bar (mode, menu, audio, MIDI and analysis pills, frame timing) and the
//! MIDI log along the bottom.

use std::collections::{HashMap, HashSet, VecDeque};

use earshot_engine::OutputInfo;
use egui::{RichText, Ui};

use super::{Cx, Panel};
use crate::core::command::{AppCmd, Command, ViewCmd};
use crate::core::index::LibraryCmd;
use crate::core::view::{Modal, Mode, SettingsTab};
use crate::ui::theme::fs;
use crate::ui::widgets::{Seg, segmented};

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
}

pub struct TopBar<'a> {
    pub audio: &'a Result<OutputInfo, String>,
    /// The audio stalled and was restarted, or keeps stalling (`core::audio_watch`).
    pub audio_note: Option<&'a str>,
    pub midi_ports: &'a [String],
    pub analysis: &'a AnalysisSummary,
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
                    ui.menu_button("Panes", |ui| {
                        let mut mixer = cx.prefs.mixer_shown();
                        if ui
                            .checkbox(&mut mixer, "Mixer controls")
                            .on_hover_text("Perform's mixer, between the decks")
                            .changed()
                        {
                            cx.send(ViewCmd::ShowMixer(mixer));
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
            if t.pill(ui, &audio, if ok { t.ok } else { t.warn })
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
            let colour = if self.midi_ports.is_empty() {
                t.line
            } else {
                t.ok
            };
            if t.pill(ui, &midi, colour)
                .on_hover_text("Click to reconnect MIDI")
                .clicked()
            {
                cx.send(AppCmd::ReconnectMidi);
            }
            self.analysis_pill(ui, cx);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Keys").clicked() {
                    cx.send(ViewCmd::ToggleModal(Modal::Keys));
                }
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

    fn analysis_pill(&self, ui: &mut Ui, cx: &mut Cx) {
        let (a, t) = (self.analysis, cx.theme);
        if let Some(why) = &a.worker_problem {
            t.pill(ui, "analysis worker out of date", t.bad)
                .on_hover_text(why);
            return;
        }
        let fail = if a.failed > 0 {
            format!(" · {} failed", a.failed)
        } else {
            String::new()
        };
        let n = a.busy.len();
        let text = if n > 0 {
            format!("analysing {n} track{}{fail}", if n == 1 { "" } else { "s" })
        } else if a.failed > 0 {
            format!("analysis: {} failed", a.failed)
        } else {
            return;
        };
        let r = t.pill(
            ui,
            &text,
            // Only failures, nothing running: red. Still working: amber.
            if a.failed > 0 && n == 0 {
                t.bad
            } else {
                t.warn
            },
        );
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
        let stop = if n > 0 {
            "Right-click to stop analysing"
        } else {
            ""
        };
        let r = r.on_hover_text(
            [running.join("\n"), failures, stop.into()]
                .join("\n")
                .trim()
                .to_string(),
        );
        if r.clicked() {
            cx.send(AppCmd::DismissFailures);
        }
        if n > 0 {
            r.context_menu(|ui| {
                if ui.button("Stop analysis jobs").clicked() {
                    cx.send(AppCmd::StopAnalysis);
                    ui.close();
                }
            });
        }
    }
}

/// The MIDI messages last received, newest first.
/// Perform's 2 / 4 deck toggle. Back to two only once decks 3 and 4 have stopped, so
/// nothing plays out of sight.
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

pub struct MidiLog<'a>(pub &'a VecDeque<String>);

impl Panel for MidiLog<'_> {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let log: Vec<&str> = self.0.iter().map(|s| s.as_str()).collect();
        ui.label(
            RichText::new(format!("MIDI  {}", log.join("   |   ")))
                .monospace()
                .size(fs(11.0))
                .color(cx.theme.muted),
        );
    }
}
