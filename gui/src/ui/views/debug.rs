//! Debug (debug builds only): Perform's decks and mixer across the top, so you can
//! play while you look, then a bar to export what's been recorded, the app's log on
//! the left and a spectrogram of what's going to the sound card on the right.

use super::perform::decks_rows;
use super::{Panels, body, pane_frame, tab_bar};
use crate::ui::panels::Cx;

pub fn show(ctx: &egui::Context, panels: &mut Panels, cx: &mut Cx) {
    decks_rows(ctx, panels, cx, "debug");
    tab_bar(ctx, "debug-bar", cx.theme, |ui| panels.debug.bar(ui, cx));
    // The log and the spectrogram, half the window each.
    egui::SidePanel::left("debug-log")
        .resizable(false)
        .exact_width(cx.shell.size.x * 0.5)
        .frame(pane_frame(cx.theme))
        .show(ctx, |ui| panels.debug.log(ui));
    body(ctx, pane_frame(cx.theme), |ui| {
        panels.debug.spectrogram(ui, cx)
    });
}
