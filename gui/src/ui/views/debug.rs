//! Debug (debug builds only): Perform's decks and mixer across the top, so you can
//! play while you look, then a bar to export what's been recorded, the app's log on
//! the left and a spectrogram of what's going to the sound card on the right.

use egui::Margin;

use super::perform::decks_rows;
use super::{Panels, row_frame};
use crate::ui::panels::Cx;

pub fn show(ctx: &egui::Context, panels: &mut Panels, cx: &mut Cx) {
    decks_rows(ctx, panels, cx, "debug", true);
    egui::TopBottomPanel::top("debug-bar")
        .exact_height(30.0)
        .frame(row_frame(cx, Margin::symmetric(6, 4)))
        .show(ctx, |ui| panels.debug.bar(ui, cx));
    egui::SidePanel::left("debug-log")
        .resizable(true)
        .default_width(ctx.content_rect().width() * 0.5)
        .frame(egui::Frame::new().fill(cx.theme.bg).inner_margin(6))
        .show(ctx, |ui| panels.debug.log(ui));
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(cx.theme.bg).inner_margin(6))
        .show(ctx, |ui| panels.debug.spectrogram(ui, cx));
}
