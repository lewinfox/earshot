//! The player page with a controller connected: its 8 performance pads, 4 by 2, each
//! labelled with what it does in the pad mode picked on the controller (Rekordbox
//! shows the same). They fill the page box: the room to spare is theirs, for long
//! labels (a sample's name). For now, the grid without labels: the modes come next.

use egui::{Ui, vec2};

use super::design::*;
use super::{DeckInfo, DeckUi};
use crate::core::command::Command;
use crate::ui::theme::THEME;
use crate::ui::widgets::HwButton;

impl DeckUi {
    pub(super) fn controller_pads(
        &mut self,
        ui: &mut Ui,
        _info: &DeckInfo,
        _out: &mut Vec<Command>,
    ) {
        let d = self.dims;
        // Shown: as wide as the page box allows; measured: their least.
        let gap = d.px(GAP_X);
        let w = self.page_box.map_or(d.px(PAD_W), |b| {
            ((b - 3.0 * gap) / 4.0).max(d.px(PAD_W)).floor()
        });
        egui::Grid::new(("controller-pads", self.id.index()))
            .spacing(vec2(d.px(GAP_X), d.px(GAP_Y)))
            .show(ui, |ui| {
                for row in 0..2 {
                    for col in 0..4 {
                        let i = row * 4 + col;
                        ui.add(
                            HwButton::new("", THEME.line, vec2(w, d.px(BTN_H)))
                                .font(d.px(FONT_PAD)),
                        )
                        .on_hover_text(format!("Pad {}", i + 1));
                    }
                    ui.end_row();
                }
            });
    }
}
