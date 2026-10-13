//! Above the canvas: the Flow's title (type to rename it; the library's tree follows
//! as you type), what's on it (tracks, length, BPM range) and the buttons.

use egui::{Align, RichText, Ui, vec2};

use super::{FlowUi, Pop};
use crate::core::collections::{CollectionCmd, GraphOp};
use crate::core::command::Command;
use crate::core::track::Track;
use crate::ui::panels::CoreView;
use crate::ui::theme::THEME;
use crate::ui::util;
use crate::ui::widgets::{Button, Toggle};

impl FlowUi {
    pub(super) fn toolbar(&mut self, ui: &mut Ui, v: &CoreView, out: &mut Vec<Command>) {
        // The buttons first, from the right; the title and summary get what's left.
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                self.buttons(ui, out);
                ui.with_layout(egui::Layout::left_to_right(Align::Center), |ui| {
                    self.title(ui, v, out);
                    let ts: Vec<&Track> = self
                        .graph
                        .nodes
                        .iter()
                        .filter_map(|n| v.lib.get(n.track))
                        .collect();
                    if !ts.is_empty() {
                        let s = summary(&ts);
                        ui.add(egui::Label::new(RichText::new(&s).color(THEME.muted)).truncate())
                            .on_hover_text(s);
                    }
                });
            });
        });
    }

    /// The Flow's name, editable once it's saved. Every change is a rename, so the
    /// library's tree shows it as it's typed.
    fn title(&mut self, ui: &mut Ui, v: &CoreView, out: &mut Vec<Command>) {
        let Some(id) = self.flow else {
            ui.label(RichText::new("New Flow").color(THEME.muted))
                .on_hover_text("Saved when you add a card");
            return;
        };
        let r = ui
            .add(
                egui::TextEdit::singleline(&mut self.name)
                    .font(egui::TextStyle::Heading)
                    .hint_text("Name this Flow")
                    .desired_width((ui.available_width() * 0.6).min(260.0)),
            )
            .on_hover_text("Rename the Flow");
        if r.changed() && !self.name.trim().is_empty() {
            out.push(CollectionCmd::Rename(id, self.name.clone()).into());
        }
        if r.lost_focus() {
            let name = self.name.trim();
            if name.is_empty() {
                // Back to what it was called.
                self.name = v
                    .collections
                    .get(id)
                    .map_or_else(String::new, |c| c.name.clone());
            } else if name != self.name {
                self.name = name.to_string();
                out.push(CollectionCmd::Rename(id, self.name.clone()).into());
            }
        }
    }

    fn buttons(&mut self, ui: &mut Ui, out: &mut Vec<Command>) {
        ui.add_enabled_ui(!self.graph.is_empty(), |ui| {
            ui.menu_button("Reset", |ui| {
                if ui.button("Remove every card").clicked() {
                    self.edit(GraphOp::Clear);
                    self.pop = None;
                    ui.close();
                }
            });
        });
        if ui
            .add(Button::new("Fit"))
            .on_hover_text("Fit everything in view")
            .clicked()
        {
            self.camera.fit = true;
        }
        if ui
            .add_enabled(!self.graph.is_empty(), Button::new("Reflow"))
            .on_hover_text("Tidy the layout: one column per step through the set")
            .clicked()
        {
            self.edit(GraphOp::Reflow);
            self.camera.fit = true;
        }
        let path = self.bake_path().filter(|_| self.flow.is_some());
        if ui
            .add_enabled(path.is_some(), Button::new("Save as playlist"))
            .on_hover_text(
                "Save a path through the Flow as a playlist: through the selected \
                 card, or the whole Flow when it's one chain",
            )
            .clicked()
            && let Some(p) = path
        {
            self.bake(p, out);
        }
        ui.add(Toggle::new(&mut self.full, "Full screen"))
            .on_hover_text("Hide the player while planning");
        let add = ui.add(Button::new("Add track"));
        if add.clicked() {
            self.pop = match self.pop {
                Some(Pop::Search(_)) => None,
                _ => {
                    self.focus_search = true;
                    Some(Pop::Search(add.rect.right_bottom() + vec2(0.0, 4.0)))
                }
            };
        }
    }
}

/// "3 tracks · 14:02 · 122-126 BPM"
fn summary(ts: &[&Track]) -> String {
    let secs: f64 = ts.iter().filter_map(|t| t.duration).sum();
    let bpms: Vec<f64> = ts.iter().filter_map(|t| t.bpm).collect();
    let mut s = format!(
        "{} track{} · {}",
        ts.len(),
        if ts.len() > 1 { "s" } else { "" },
        util::mmss(secs)
    );
    if !bpms.is_empty() {
        let lo = bpms.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = bpms.iter().copied().fold(0.0, f64::max);
        s += &format!(" · {lo:.0}-{hi:.0} BPM");
    }
    s
}
