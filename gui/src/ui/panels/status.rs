//! The top bar (mode, menu, audio, MIDI and analysis pills, frame timing) and the
//! MIDI log along the bottom.

use std::collections::{HashMap, HashSet, VecDeque};

use earshot_engine::OutputInfo;
use egui::{RichText, Ui};

use super::{Cx, Panel};
use crate::core::command::{AppCmd, Command, ViewCmd};
use crate::core::index::LibraryCmd;
use crate::core::view::{Modal, Mode, SettingsTab};

/// Tracks being analysed, and failures since they were last dismissed.
#[derive(Default)]
pub struct AnalysisSummary {
    pub busy: HashSet<i64>,
    pub running: HashMap<i64, f64>,
    pub failed: u32,
    pub last_error: String,
}

pub struct TopBar<'a> {
    pub audio: &'a Result<OutputInfo, String>,
    pub midi_ports: &'a [String],
    pub analysis: &'a AnalysisSummary,
    /// Frame timing, and the audio callback's.
    pub hud: [&'a str; 2],
    /// What undo and redo would do ("edit title"), if anything.
    pub undo: Option<String>,
    pub redo: Option<String>,
}

impl Panel for TopBar<'_> {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let t = cx.theme;
        ui.horizontal(|ui| {
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
                    .on_hover_text("Settings > Analysis: how many are out of date, and re-analyse them")
                    .clicked()
                {
                    cx.send(ViewCmd::Settings(SettingsTab::Analysis));
                    ui.close();
                }
                ui.menu_button("View", |ui| {
                    ui.menu_button("Size", |ui| {
                        let now = ui.ctx().zoom_factor();
                        for z in [0.8, 0.9, 1.0, 1.1, 1.25] {
                            if ui
                                .radio((now - z).abs() < 0.01, format!("{:.0}%", z * 100.0))
                                .clicked()
                            {
                                cx.send(ViewCmd::UiScale(z));
                                ui.close();
                            }
                        }
                    });
                });
            });
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
            let (audio, ok) = match self.audio {
                Ok(i) => (
                    format!(
                        "{}{} · {} kHz · {} frames · {} ch",
                        if i.null { "⚠ " } else { "" },
                        i.device,
                        i.sample_rate / 1000,
                        i.buffer_frames.map_or("default".into(), |b| b.to_string()),
                        i.channels
                    ),
                    !i.null,
                ),
                Err(e) => (format!("no audio: {e}"), false),
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
                ui.label(RichText::new(self.hud[1]).monospace().size(11.0));
                ui.label(RichText::new(self.hud[0]).monospace().size(11.0));
            });
        });
    }
}

impl TopBar<'_> {
    fn analysis_pill(&self, ui: &mut Ui, cx: &mut Cx) {
        let (a, t) = (self.analysis, cx.theme);
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
        let r = r.on_hover_text([running.join("\n"), failures].join("\n").trim().to_string());
        if r.clicked() {
            cx.send(AppCmd::DismissFailures);
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
    let gap = std::mem::replace(&mut ui.spacing_mut().item_spacing.x, 2.0);
    let r = ui
        .add_enabled(!hidden_playing, egui::Button::selectable(!four, "2 decks"))
        .on_hover_text("Two decks")
        .on_disabled_hover_text("Stop decks 3 and 4 first");
    if r.clicked() && four {
        cx.send(ViewCmd::FourDecks(false));
    }
    let r = ui
        .add(egui::Button::selectable(four, "4 decks"))
        .on_hover_text("Four decks, smaller");
    if r.clicked() && !four {
        cx.send(ViewCmd::FourDecks(true));
    }
    ui.spacing_mut().item_spacing.x = gap;
}

pub struct MidiLog<'a>(pub &'a VecDeque<String>);

impl Panel for MidiLog<'_> {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let log: Vec<&str> = self.0.iter().map(|s| s.as_str()).collect();
        ui.label(
            RichText::new(format!("MIDI  {}", log.join("   |   ")))
                .monospace()
                .size(11.0)
                .color(cx.theme.muted),
        );
    }
}
