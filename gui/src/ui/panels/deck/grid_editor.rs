//! The beat grid editor: anchor the 1, set the BPM, nudge the grid. The Grid page of
//! a player's controls, built like the Player page: two rows of single-height
//! buttons, and "put the 1 here" a double.

use egui::{Align, Ui, vec2};

use super::design::*;
use super::{DeckInfo, DeckUi};
use crate::core::command::{Command, DeckCmd};
use crate::core::player::GridOp;
use crate::ui::theme::THEME;
use crate::ui::widgets::{HwButton, Icon, held};

impl DeckUi {
    pub(super) fn grid_editor(&mut self, ui: &mut Ui, info: &DeckInfo, out: &mut Vec<Command>) {
        let deck = self.id;
        let d = self.dims;
        ui.spacing_mut().item_spacing = vec2(self.wx(GAP_X), d.px(GAP_Y));
        let (anchor_w, group, gap) = (self.wx(BTN_W), self.wx(GROUP_GAP), self.wx(GAP_X));
        // The two rows fill the page box beside the anchor (flexbox-like): the tempo
        // row is 8 buttons wide (the BPM box is two) with 6 gaps; the nudge row is 4
        // buttons with 3 gaps. Measured, they're their design widths.
        let (tempo_w, nudge_w) = match self.page_box {
            Some(b) => {
                let rows = (b - anchor_w - group - 2.0 * gap).floor();
                ((rows - 6.0 * gap) / 8.0, (rows - 3.0 * gap) / 4.0)
            }
            None => (self.wx(GRID_W), self.wx(NUDGE_W)),
        };
        ui.horizontal_top(|ui| {
            let r = ui
                .add(
                    HwButton::new("Anchor 1 here", THEME.text, vec2(anchor_w, d.double()))
                        .icon(Icon::Downbeat),
                )
                .on_hover_text(
                    "Put the 1 here: move the grid so a beat lands exactly on the playhead, \
                     and make it the 1",
                );
            if held(ui, &r) == Some(true) {
                out.push(Command::Deck(deck, DeckCmd::Grid(GridOp::AnchorHere)));
            }
            ui.add_space(group);
            ui.vertical(|ui| {
                self.tempo_row(ui, info, out, tempo_w);
                self.nudge_row(ui, out, nudge_w);
            });
        });
    }

    /// A single-height grid button; its op goes out when clicked.
    fn grid_button(
        &self,
        ui: &mut Ui,
        out: &mut Vec<Command>,
        label: &str,
        width: f32,
        tip: &str,
        op: GridOp,
    ) {
        let size = vec2(width, self.dims.single());
        let r = ui
            .add(HwButton::new(label, THEME.text, size).font(self.dims.px(FONT_GRID)))
            .on_hover_text(tip);
        if held(ui, &r) == Some(true) {
            out.push(Command::Deck(self.id, DeckCmd::Grid(op)));
        }
    }

    /// -0.1, -0.01, the BPM (type one), +0.01, +0.1, half and double.
    fn tempo_row(&mut self, ui: &mut Ui, info: &DeckInfo, out: &mut Vec<Command>, w: f32) {
        let cur = info.grid.as_ref().map(|g| g.mean_bpm());
        let step = |d: f64| GridOp::SetBpm(((cur.unwrap_or(120.0) + d) * 100.0).round() / 100.0);
        ui.horizontal(|ui| {
            ui.set_height(self.dims.single());
            for (label, d) in [("-0.1", -0.1), ("-0.01", -0.01)] {
                self.grid_button(ui, out, label, w, "Slower: the grid's BPM", step(d));
            }
            let edit = ui
                .add_sized(
                    [2.0 * w, self.dims.single()],
                    egui::TextEdit::singleline(&mut self.bpm_text)
                        .horizontal_align(Align::Center)
                        .vertical_align(Align::Center),
                )
                .on_hover_text("The grid's BPM: type one and press Enter");
            // Enter or clicking away sets it (Esc leaves it be). Read before the box is
            // put back to the grid's BPM below.
            let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
            if edit.lost_focus()
                && !esc
                && let Ok(b) = self.bpm_text.trim().parse::<f64>()
                && b > 0.0
                && cur.is_some_and(|c| (c - b).abs() >= 0.005)
            {
                out.push(Command::Deck(self.id, DeckCmd::Grid(GridOp::SetBpm(b))));
            }
            if !edit.has_focus() {
                self.bpm_text = cur
                    .map(|b| format!("{:.2}", (b * 100.0).round() / 100.0))
                    .unwrap_or_default();
            }
            for (label, d) in [("+0.01", 0.01), ("+0.1", 0.1)] {
                self.grid_button(ui, out, label, w, "Faster: the grid's BPM", step(d));
            }
            self.grid_button(ui, out, "÷2", w, "Half the tempo", GridOp::Halve);
            self.grid_button(ui, out, "×2", w, "Double the tempo", GridOp::Double);
        });
    }

    /// Nudge the whole grid earlier or later.
    fn nudge_row(&mut self, ui: &mut Ui, out: &mut Vec<Command>, w: f32) {
        ui.horizontal(|ui| {
            ui.set_height(self.dims.single());
            for (label, ms) in [
                ("◀◀ 10 ms", -10.0),
                ("◀ 1 ms", -1.0),
                ("1 ms ▶", 1.0),
                ("10 ms ▶▶", 10.0),
            ] {
                let tip = "Shift the whole grid earlier or later (Shift + ← / →: 10 ms)";
                self.grid_button(ui, out, label, w, tip, GridOp::Nudge(ms));
            }
        });
    }
}
