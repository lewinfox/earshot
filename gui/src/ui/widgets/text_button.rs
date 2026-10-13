//! The app's plain buttons, for everything that isn't a hardware control: a normal
//! button, a primary one in the accent (the step a tool is for, like Straighten), and
//! a subtle one with no frame until hovered (small helpers like "median"). Built on
//! `egui::Button`, so menus, shortcuts and accessibility work as usual. Disable with
//! `ui.add_enabled`; add a tooltip on the response.

use egui::{Response, RichText, Ui, Widget};

use crate::ui::theme::THEME;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Normal,
    Primary,
    Subtle,
}

pub struct Button<'a> {
    text: &'a str,
    kind: Kind,
    small: bool,
}

impl<'a> Button<'a> {
    pub fn new(text: &'a str) -> Self {
        Button {
            text,
            kind: Kind::Normal,
            small: false,
        }
    }

    /// The step the panel is for, in the accent. One to a panel.
    pub fn primary(text: &'a str) -> Self {
        Button {
            kind: Kind::Primary,
            ..Button::new(text)
        }
    }

    /// No frame until hovered, in muted text: helpers beside another control.
    pub fn subtle(text: &'a str) -> Self {
        Button {
            kind: Kind::Subtle,
            ..Button::new(text)
        }
    }

    /// Less padding, for a crowded row.
    pub fn small(mut self) -> Self {
        self.small = true;
        self
    }
}

impl Widget for Button<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let text = RichText::new(self.text);
        let b = match self.kind {
            Kind::Normal => egui::Button::new(text),
            Kind::Primary => egui::Button::new(text.strong().color(THEME.ink)).fill(THEME.accent),
            Kind::Subtle => egui::Button::new(text.color(THEME.muted)).frame_when_inactive(false),
        };
        ui.add(if self.small { b.small() } else { b })
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    #[test]
    fn every_kind_clicks_by_its_text() {
        let mut h = Harness::new_ui_state(
            |ui, n: &mut u32| {
                for b in [
                    Button::new("Save"),
                    Button::primary("Straighten"),
                    Button::subtle("median").small(),
                ] {
                    if ui.add(b).clicked() {
                        *n += 1;
                    }
                }
            },
            0,
        );
        for label in ["Save", "Straighten", "median"] {
            h.get_by_label(label).click();
            h.run();
        }
        assert_eq!(*h.state(), 3);
    }
}
