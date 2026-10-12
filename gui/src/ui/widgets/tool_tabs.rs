//! Horizontal tabs for Prepare's tools: what we're doing to the track.

use egui::{RichText, Ui};

use crate::ui::theme::THEME;

/// One tab: its id, its name and a tooltip.
pub struct Tab<T> {
    pub id: T,
    pub name: &'static str,
    pub tip: &'static str,
}

/// Draw the tabs with `current` selected. Returns the tab clicked, if it's another.
pub fn tool_tabs<T: Copy + PartialEq>(ui: &mut Ui, tabs: &[Tab<T>], current: T) -> Option<T> {
    let mut picked = None;
    ui.horizontal(|ui| {
        for t in tabs {
            let on = t.id == current;
            let text = RichText::new(t.name).color(if on { THEME.text } else { THEME.muted });
            if ui.selectable_label(on, text).on_hover_text(t.tip).clicked() && !on {
                picked = Some(t.id);
            }
        }
    });
    picked
}
