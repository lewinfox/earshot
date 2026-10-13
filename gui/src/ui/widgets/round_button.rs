//! Round icon buttons for play, pause and stop outside Perform's hardware: the Flow
//! cards' preview, Tempo lock's player. A bright disc for the main action (play or
//! pause), lit in the accent while it plays; a quieter outlined disc for the rest
//! (stop). Perform's decks keep their [`HwButton`](super::HwButton)s.

use egui::{Response, Sense, Stroke, Ui, Widget, vec2};

use super::Icon;
use crate::ui::theme::THEME;
use crate::ui::util;

/// How big a round button is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoundSize {
    /// In a list row.
    Small,
    /// On a Flow card, or a tool's transport.
    Normal,
}

impl RoundSize {
    /// Across, points.
    pub fn diameter(self) -> f32 {
        match self {
            RoundSize::Small => 22.0,
            RoundSize::Normal => 28.0,
        }
    }
}

/// A round button showing an [`Icon`]. `label` isn't drawn: it's the name screen
/// readers and tests find it by ("Play", "Stop"). Disable it with `ui.add_enabled`;
/// add a tooltip on the response it returns. Focusable, so Space or Enter press it.
pub struct RoundButton<'a> {
    icon: Icon,
    label: &'a str,
    size: RoundSize,
    lit: bool,
    quiet: bool,
}

impl<'a> RoundButton<'a> {
    pub fn new(icon: Icon, label: &'a str) -> Self {
        RoundButton {
            icon,
            label,
            size: RoundSize::Normal,
            lit: false,
            quiet: false,
        }
    }

    /// Play, or Pause while `playing` (then lit).
    pub fn play_pause(playing: bool) -> Self {
        if playing {
            RoundButton::new(Icon::Pause, "Pause").lit(true)
        } else {
            RoundButton::new(Icon::Play, "Play")
        }
    }

    /// Stop, outlined beside the play button.
    pub fn stop() -> Self {
        RoundButton::new(Icon::Stop, "Stop").quiet(true)
    }

    pub fn size(mut self, size: RoundSize) -> Self {
        self.size = size;
        self
    }

    /// In the accent: what it controls is on (playing).
    pub fn lit(mut self, on: bool) -> Self {
        self.lit = on;
        self
    }

    /// Outlined rather than bright: a secondary action beside the main one.
    pub fn quiet(mut self, on: bool) -> Self {
        self.quiet = on;
        self
    }
}

impl Widget for RoundButton<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let d = self.size.diameter();
        let (rect, resp) = ui.allocate_exact_size(vec2(d, d), Sense::click());
        resp.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::Button,
                ui.is_enabled(),
                self.lit,
                self.label,
            )
        });
        if !ui.is_rect_visible(rect) {
            return resp;
        }
        let enabled = ui.is_enabled();
        let hover = enabled && resp.hovered();
        let down = enabled && resp.is_pointer_button_down_on();
        let (fill, line, glyph) = if !enabled {
            (THEME.panel_2, THEME.line, THEME.muted)
        } else if self.lit {
            let fill = if hover {
                util::dim(THEME.accent, 1.15)
            } else {
                THEME.accent
            };
            (fill, fill, THEME.ink)
        } else if self.quiet {
            let fill = if hover {
                util::dim(THEME.button, 1.35)
            } else {
                THEME.button
            };
            let line = if hover { THEME.hover_line } else { THEME.line };
            (fill, line, THEME.text)
        } else {
            let fill = if hover { THEME.accent } else { THEME.marker };
            (fill, fill, THEME.ink)
        };
        let fill = if down { util::dim(fill, 0.8) } else { fill };
        let p = ui.painter();
        let c = rect.center();
        let r = d / 2.0;
        p.circle(c, r - 0.5, fill, Stroke::new(1.0_f32, line));
        if resp.has_focus() {
            p.circle_stroke(c, r + 1.5, Stroke::new(1.5_f32, THEME.selection));
        }
        self.icon.paint(p, rect, d * 0.42, glyph);
        resp
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    #[test]
    fn play_becomes_pause_and_clicks() {
        let mut h = Harness::new_ui_state(
            |ui, playing: &mut bool| {
                if ui.add(RoundButton::play_pause(*playing)).clicked() {
                    *playing = !*playing;
                }
            },
            false,
        );
        h.get_by_label("Play").click();
        h.run();
        assert!(*h.state());
        h.get_by_label("Pause").click();
        h.run();
        assert!(!*h.state());
    }

    #[test]
    fn disabled_does_not_click() {
        let mut h = Harness::new_ui_state(
            |ui, n: &mut u32| {
                if ui.add_enabled(false, RoundButton::stop()).clicked() {
                    *n += 1;
                }
            },
            0,
        );
        h.get_by_label("Stop").click();
        h.run();
        assert_eq!(*h.state(), 0);
    }
}
