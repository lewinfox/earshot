//! The beat grid editor: anchor the 1, set the BPM, nudge the grid. The Grid page of
//! a player's controls, built like the Player page: two rows of single-height
//! buttons, and "put the 1 here" a double.

use egui::{Align, Layout, RichText, Ui, vec2};

use super::{DeckInfo, DeckUi};
use crate::core::command::{Command, DeckCmd};
use crate::core::player::GridOp;
use crate::ui::theme::THEME;
use crate::ui::widgets::{HwButton, Icon};

impl DeckUi {
    pub(super) fn grid_editor(&mut self, ui: &mut Ui, info: &DeckInfo, out: &mut Vec<Command>) {
        let deck = self.id;
        ui.spacing_mut().item_spacing = vec2(3.0, 4.0);
        ui.horizontal_top(|ui| {
            let r = ui
                .add(
                    HwButton::new(
                        "Anchor 1 here",
                        THEME.text,
                        vec2(self.dims.btn_w, self.dims.double()),
                    )
                    .icon(Icon::Downbeat),
                )
                .on_hover_text(
                    "Put the 1 here: move the grid so a beat lands exactly on the playhead, \
                     and make it the 1",
                );
            if r.clicked() {
                out.push(Command::Deck(deck, DeckCmd::Grid(GridOp::AnchorHere)));
            }
            ui.add_space(6.0);
            ui.vertical(|ui| {
                self.tempo_row(ui, info, out);
                self.nudge_row(ui, info, out);
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
            .add(HwButton::new(label, THEME.text, size).font(10.0))
            .on_hover_text(tip);
        if r.clicked() {
            out.push(Command::Deck(self.id, DeckCmd::Grid(op)));
        }
    }

    /// -0.1, -0.01, the BPM (type one), +0.01, +0.1, half and double.
    fn tempo_row(&mut self, ui: &mut Ui, info: &DeckInfo, out: &mut Vec<Command>) {
        let cur = info.grid.as_ref().map(|g| g.mean_bpm());
        let step = |d: f64| GridOp::SetBpm(((cur.unwrap_or(120.0) + d) * 100.0).round() / 100.0);
        let w = self.dims.btn_w + 4.0;
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
            if !edit.has_focus() {
                self.bpm_text = cur
                    .map(|b| format!("{:.2}", (b * 100.0).round() / 100.0))
                    .unwrap_or_default();
            }
            if edit.lost_focus()
                && let Ok(b) = self.bpm_text.trim().parse::<f64>()
            {
                out.push(Command::Deck(self.id, DeckCmd::Grid(GridOp::SetBpm(b))));
            }
            for (label, d) in [("+0.01", 0.01), ("+0.1", 0.1)] {
                self.grid_button(ui, out, label, w, "Faster: the grid's BPM", step(d));
            }
            self.grid_button(ui, out, "÷2", w, "Half the tempo", GridOp::Halve);
            self.grid_button(ui, out, "×2", w, "Double the tempo", GridOp::Double);
        });
    }

    /// Nudge the whole grid earlier or later, and where the bar's 1 nearest the
    /// playhead is, to the millisecond.
    fn nudge_row(&mut self, ui: &mut Ui, info: &DeckInfo, out: &mut Vec<Command>) {
        let w = 1.5 * self.dims.btn_w + 6.0;
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
            // Always there (dashes without a grid), so the page is the same width
            // whether or not a track is loaded. A nudge moves the lines by a hair;
            // this moves by the number.
            let at = info.grid.as_deref().map_or_else(
                || "1 at -:--.---".to_string(),
                |g| {
                    let beat = g.beat_at(self.state.position).round() as i64;
                    let pos = g.position_of(beat).max(1) as i64;
                    let t = g.time_at((beat - (pos - 1)) as f64);
                    let sign = if t < 0.0 { "-" } else { "" };
                    let (m, s) = ((t.abs() / 60.0).floor(), t.abs() % 60.0);
                    format!("1 at {sign}{m:.0}:{s:06.3}")
                },
            );
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.add_space(4.0);
                ui.label(RichText::new(at).monospace().size(10.0).color(THEME.muted))
                    .on_hover_text("The bar's 1 nearest the playhead");
            });
        });
    }
}
