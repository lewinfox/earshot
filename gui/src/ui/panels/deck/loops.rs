//! Loops (the loop button with ½ / ×2, IN / OUT / RELOOP) and beat jumps with the
//! step picker.

use egui::{Ui, vec2};

use super::{DeckInfo, DeckUi};
use crate::core::bus::{DeckControl, JUMP_STEPS};
use crate::core::command::Command;
use crate::ui::theme::THEME;
use crate::ui::widgets::{HwButton, held};

pub(super) fn size_label(s: f64) -> String {
    if s < 1.0 {
        format!("1/{}", (1.0 / s).round())
    } else {
        format!("{s}")
    }
}

impl DeckUi {
    /// A loop button `w` wide that sends `control` 1 on press and 0 on release.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn hold_button(
        &self,
        ui: &mut Ui,
        w: f32,
        label: &str,
        control: DeckControl,
        lit: bool,
        tip: &str,
        out: &mut Vec<Command>,
    ) {
        let r = ui
            .add(
                HwButton::new(label, THEME.loop_button, vec2(w, self.dims.btn_h))
                    .font(if label.len() > 4 { 9.0 } else { 10.0 })
                    .lit(lit),
            )
            .on_hover_text(tip);
        if let Some(d) = held(ui, &r) {
            self.control(out, control, d as u8 as f64);
        }
    }

    /// Loop: ½ / LOOP n / ×2 over IN / OUT / EXIT. With no loop running, ½ and ×2
    /// change the size the next loop will be, like Mixxx.
    pub(super) fn loops(&mut self, ui: &mut Ui, info: &DeckInfo, out: &mut Vec<Command>) {
        let active = self.state.loop_active;
        let size = size_label(info.loop_size);
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                for (label, control) in [
                    ("½", DeckControl::LoopHalve),
                    ("×2", DeckControl::LoopDouble),
                ] {
                    if control == DeckControl::LoopDouble {
                        let r = ui
                            .add(
                                HwButton::new(
                                    &format!("⟲ {size}"),
                                    THEME.loop_button,
                                    vec2(self.dims.btn_w + 10.0, self.dims.btn_h),
                                )
                                .font(10.0)
                                .lit(active),
                            )
                            .on_hover_text(format!(
                                "Loop {size} beats from the current beat (or end the loop)"
                            ));
                        if let Some(d) = held(ui, &r) {
                            self.control(out, DeckControl::Loop, d as u8 as f64);
                        }
                    }
                    let tip = if control == DeckControl::LoopHalve {
                        "Halve"
                    } else {
                        "Double"
                    };
                    let r = ui
                        .add(
                            HwButton::new(
                                label,
                                THEME.loop_button,
                                vec2(self.dims.btn_w, self.dims.btn_h),
                            )
                            .font(11.0),
                        )
                        .on_hover_text(format!("{tip} the loop (or the next loop's size)"));
                    if held(ui, &r) == Some(true) {
                        self.control(out, control, 1.0);
                    }
                }
            });
            // As wide as the row above.
            let w = (3.0 * self.dims.btn_w + 10.0) / 3.0;
            ui.horizontal(|ui| {
                self.hold_button(
                    ui,
                    w,
                    "IN",
                    DeckControl::LoopIn,
                    false,
                    "Loop in point",
                    out,
                );
                self.hold_button(
                    ui,
                    w,
                    "OUT",
                    DeckControl::LoopOut,
                    false,
                    "Loop out point: starts the loop",
                    out,
                );
                self.hold_button(
                    ui,
                    w,
                    if active { "EXIT" } else { "RELOOP" },
                    DeckControl::Reloop,
                    active,
                    "Leave the loop, or go back into the last one",
                    out,
                );
            });
        });
    }

    /// Beat jump: ◀ n / n ▶ over the step picker.
    pub(super) fn jumps(&mut self, ui: &mut Ui, info: &DeckInfo, out: &mut Vec<Command>) {
        let n = size_label(info.jump_size);
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                for (label, control, tip) in [
                    (format!("◀ {n}"), DeckControl::JumpBack, "Jump back"),
                    (format!("{n} ▶"), DeckControl::JumpForward, "Jump forward"),
                ] {
                    let r = ui
                        .add(
                            HwButton::new(
                                &label,
                                THEME.text,
                                vec2(self.dims.btn_w + 6.0, self.dims.btn_h),
                            )
                            .font(10.0),
                        )
                        .on_hover_text(format!("{tip} {n} beats"));
                    if let Some(d) = held(ui, &r) {
                        self.control(out, control, d as u8 as f64);
                    }
                }
            });
            // The step: click to pick 1-32 beats.
            let r = ui
                .add(
                    HwButton::new(
                        &format!("STEP {n}"),
                        THEME.text,
                        vec2(2.0 * (self.dims.btn_w + 6.0) + 3.0, self.dims.btn_h),
                    )
                    .font(10.0),
                )
                .on_hover_text("How many beats a jump (← / →) moves");
            egui::Popup::menu(&r).show(|ui| {
                for beats in JUMP_STEPS {
                    let label = format!("{beats} beat{}", if beats == 1.0 { "" } else { "s" });
                    if ui.radio(info.jump_size == beats, label).clicked() {
                        self.control(out, DeckControl::JumpSize, beats);
                        ui.close();
                    }
                }
            });
        });
    }
}
