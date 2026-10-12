//! The 8 hot cue pads, and the box a pad's label is typed in.

use egui::{RichText, Ui, vec2};

use super::{DeckInfo, DeckUi, LabelEdit, LabelTarget};
use crate::core::bus::DeckControl;
use crate::core::command::{Command, DeckCmd};
use crate::ui::theme::THEME;
use crate::ui::util;
use crate::ui::widgets::{HwButton, held};

impl DeckUi {
    /// The 8 hot cue pads, each with its label under the number. Right-click one to
    /// name it.
    pub(super) fn pads(&mut self, ui: &mut Ui, info: &DeckInfo, out: &mut Vec<Command>) {
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
                                HwButton::new(
                                    &label,
                                    THEME.hotcue[i],
                                    vec2(self.dims.btn_w, self.dims.btn_h),
                                )
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
    pub(super) fn hotcue_label_dialog(&mut self, ui: &Ui, out: &mut Vec<Command>) {
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
}
