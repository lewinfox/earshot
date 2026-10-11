//! Drawing: the window and its panels. See app.rs for the layout.

pub mod app;
pub mod audio_ui;
pub mod controls;
pub mod deck;
pub mod flow;
pub mod flows_ui;
pub mod library_ui;
pub mod mixer;
pub mod suggest_ui;
pub mod util;
pub mod wave;

/// earshot's dark look.
pub fn theme(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = util::BG;
    v.window_fill = util::PANEL;
    v.extreme_bg_color = util::WAVE_BG;
    v.faint_bg_color = util::PANEL_2;
    v.selection.bg_fill = egui::Color32::from_rgb(0x2b, 0x5f, 0xb8);
    v.widgets.inactive.weak_bg_fill = util::PANEL_2;
    // Slider rails and other inactive fills need to stand out from the panels.
    v.widgets.inactive.bg_fill = egui::Color32::from_rgb(0x34, 0x37, 0x44);
    v.slider_trailing_fill = true;
    v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, util::LINE);
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(0x44, 0x47, 0x56));
    v.widgets.noninteractive.fg_stroke.color = util::TEXT;
    v.widgets.inactive.fg_stroke.color = util::TEXT;
    ctx.set_visuals(v);
    ctx.style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(6.0, 4.0);
        s.spacing.button_padding = egui::vec2(7.0, 3.0);
    });
}
