//! The beat grid editor: anchor the 1, set the BPM, nudge the grid.

use egui::{Align, RichText, Ui, vec2};

use super::{DeckInfo, DeckUi};
use crate::core::command::{Command, DeckCmd};
use crate::core::player::GridOp;
use crate::ui::theme::THEME;
use crate::ui::widgets::{HwButton, Icon};

impl DeckUi {
    pub(super) fn grid_editor(&mut self, ui: &mut Ui, info: &DeckInfo, out: &mut Vec<Command>) {
        let deck = self.id;
        ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
        // "Put the 1 here" down the left, as tall as the controls; the tempo and nudge
        // rows beside it.
        let tall = self.dims.controls_h();
        ui.horizontal_top(|ui| {
            if ui
                .add(
                    HwButton::new("Anchor 1 here", self.colour(), vec2(30.0, tall))
                        .icon(Icon::Downbeat)
                        .tinted(true),
                )
                .on_hover_text(
                    "Put the 1 here: move the grid so a beat lands exactly on the playhead, \
                     and make it the 1",
                )
                .clicked()
            {
                out.push(Command::Deck(deck, DeckCmd::Grid(GridOp::AnchorHere)));
            }
            ui.add_space(4.0);
            ui.vertical(|ui| self.grid_rows(ui, info, out));
        });
    }

    /// The tempo row (-0.1 … ×2) and the nudge row, with where the bar's 1 is.
    fn grid_rows(&mut self, ui: &mut Ui, info: &DeckInfo, out: &mut Vec<Command>) {
        let deck = self.id;
        let mut grid = |op| out.push(Command::Deck(deck, DeckCmd::Grid(op)));
        ui.spacing_mut().item_spacing = vec2(3.0, 6.0);
        let cur = info.grid.as_ref().map(|g| g.mean_bpm());
        ui.horizontal(|ui| {
            ui.label(RichText::new("BPM").size(10.0).color(THEME.muted));
            for (label, d) in [("-0.1", -0.1), ("-0.01", -0.01)] {
                if ui.small_button(label).clicked() {
                    grid(GridOp::SetBpm(
                        ((cur.unwrap_or(120.0) + d) * 100.0).round() / 100.0,
                    ));
                }
            }
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.bpm_text)
                    .desired_width(52.0)
                    .horizontal_align(Align::Center),
            );
            if !edit.has_focus() {
                self.bpm_text = cur
                    .map(|b| format!("{:.2}", (b * 100.0).round() / 100.0))
                    .unwrap_or_default();
            }
            if edit.lost_focus()
                && let Ok(b) = self.bpm_text.trim().parse::<f64>()
            {
                grid(GridOp::SetBpm(b));
            }
            for (label, d) in [("+0.01", 0.01), ("+0.1", 0.1)] {
                if ui.small_button(label).clicked() {
                    grid(GridOp::SetBpm(
                        ((cur.unwrap_or(120.0) + d) * 100.0).round() / 100.0,
                    ));
                }
            }
            if ui
                .small_button("÷2")
                .on_hover_text("Half the tempo")
                .clicked()
            {
                grid(GridOp::Halve);
            }
            if ui
                .small_button("×2")
                .on_hover_text("Double the tempo")
                .clicked()
            {
                grid(GridOp::Double);
            }
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("NUDGE").size(10.0).color(THEME.muted));
            for (label, ms) in [
                ("<< 10 ms", -10.0),
                ("< 1 ms", -1.0),
                ("1 ms >", 1.0),
                ("10 ms >>", 10.0),
            ] {
                if ui
                    .small_button(label)
                    .on_hover_text("Shift the whole grid earlier or later")
                    .clicked()
                {
                    grid(GridOp::Nudge(ms));
                }
            }
            // Where the bar's 1 nearest the playhead is, to the millisecond: a nudge
            // moves the lines by a hair, this by the number.
            // Always there (dashes without a grid), so the page is the same width
            // whether or not a track is loaded.
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
            ui.label(RichText::new(at).monospace().size(10.0).color(THEME.muted))
                .on_hover_text("The bar's 1 nearest the playhead");
        });
    }
}
