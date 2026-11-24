//! Debug (debug builds only): Perform's decks and mixer across the top, so you can
//! play while you look, then a bar to export what's been recorded, the app's log on
//! the left and a spectrogram of what's going to the sound card on the right.

use super::perform::decks_rows;
use super::{Panels, body, pane_frame, tab_bar};
use crate::ui::flex::{self, Flex, Tui, leaf, row};
use crate::ui::panels::Cx;

pub fn show(t: &mut Tui, panels: &mut Panels, cx: &mut Cx) {
    decks_rows(t, panels, cx);
    tab_bar(t, cx.theme, |ui| panels.debug.bar(ui, cx));
    // The log and the spectrogram, half the window each.
    body(t, egui::Frame::new().fill(cx.theme.bg), |ui| {
        flex::root(ui, "debug-body", row(), |t| {
            flex::pane(t, leaf().grow(), |ui| {
                flex::boxed(ui, pane_frame(cx.theme), |ui| panels.debug.log(ui))
            });
            flex::pane(t, leaf().grow(), |ui| {
                flex::boxed(ui, pane_frame(cx.theme), |ui| {
                    panels.debug.spectrogram(ui, cx)
                })
            });
        })
    });
}
