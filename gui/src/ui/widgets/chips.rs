//! Small coloured labels.

use egui::{CornerRadius, FontId, Sense, Ui, vec2};

use crate::ui::theme::THEME;
use crate::ui::util::{self, Camelot};

/// A Camelot key on its colour round the wheel.
pub fn key_chip(ui: &mut Ui, k: Camelot) {
    key_chip_sized(ui, k, 11.0);
}

/// [`key_chip`] with its text at `size`.
pub fn key_chip_sized(ui: &mut Ui, k: Camelot, size: f32) {
    let text = util::key_name(k);
    let galley = ui
        .painter()
        .layout_no_wrap(text, FontId::proportional(size), THEME.ink);
    // Every chip as wide as the widest key ("12B"), so "8A" lines up with "11B".
    let widest = ui
        .painter()
        .layout_no_wrap("12B".into(), FontId::proportional(size), THEME.ink)
        .size()
        .x;
    let size = vec2(galley.size().x.max(widest), galley.size().y);
    let (rect, _) = ui.allocate_exact_size(size + vec2(10.0, 2.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(4), util::key_colour(k));
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, THEME.ink);
}
