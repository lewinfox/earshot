//! The top bar (mode, menu, audio, MIDI and analysis pills, frame timing) and the
//! MIDI log along the bottom.

use std::collections::{HashMap, HashSet, VecDeque};

use earshot_engine::OutputInfo;
use egui::{RichText, Ui};

use super::{Cx, Panel};
use crate::core::command::{AppCmd, ViewCmd};
use crate::core::index::LibraryCmd;
use crate::core::view::{Modal, Mode};

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
                        .on_hover_text("Two decks and the mixer");
                });
            if mode != cx.prefs.mode {
                cx.send(ViewCmd::SetMode(mode));
            }
            ui.menu_button("Menu", |ui| {
                ui.menu_button("Import", |ui| {
                    if ui.button("Folder…").clicked() {
                        cx.send(LibraryCmd::PickRoots);
                        ui.close();
                    }
                });
                if ui.button("Audio…").clicked() {
                    cx.send(ViewCmd::ToggleModal(Modal::Audio));
                    ui.close();
                }
                if ui.button("Rescan library").clicked() {
                    cx.send(LibraryCmd::Rescan);
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
                cx.send(ViewCmd::ToggleModal(Modal::Audio));
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
