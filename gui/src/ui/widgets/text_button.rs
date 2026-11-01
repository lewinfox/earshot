//! The app's text buttons, for everything that isn't a hardware control, by intent:
//!
//! - [`Button::primary`]: the step a panel or window is for (Export, Add), in the
//!   accent. One to a panel.
//! - [`Button::secondary`] (or [`Button::new`]): everything else, framed and plain.
//! - [`Button::warning`]: go on, but look first (amber).
//! - [`Button::danger`]: can't be undone, like erasing a stick (red).
//! - [`Button::subtle`]: no frame until hovered, muted (small helpers like "median").
//!
//! Built on `egui::Button`, so menus, shortcuts and accessibility work as usual.
//! Disable with `ui.add_enabled`; add a tooltip on the response.

use egui::{Response, RichText, Ui, Widget};

use super::Intent;
use crate::ui::theme::THEME;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Primary,
    Secondary,
    /// Filled in the intent's colour (warning, danger).
    Filled(Intent),
    Subtle,
}

pub struct Button<'a> {
    text: &'a str,
    kind: Kind,
    small: bool,
}

impl<'a> Button<'a> {
    /// A [`Button::secondary`].
    pub fn new(text: &'a str) -> Self {
        Button {
            text,
            kind: Kind::Secondary,
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

    /// Anything that isn't the panel's main step.
    pub fn secondary(text: &'a str) -> Self {
        Button::new(text)
    }

    /// Go on, but look first: in amber.
    #[allow(dead_code)] // The set's complete; not used yet.
    pub fn warning(text: &'a str) -> Self {
        Button {
            kind: Kind::Filled(Intent::Warning),
            ..Button::new(text)
        }
    }

    /// What can't be undone (erasing a stick), in red.
    pub fn danger(text: &'a str) -> Self {
        Button {
            kind: Kind::Filled(Intent::Danger),
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
            Kind::Secondary => egui::Button::new(text),
            Kind::Primary => egui::Button::new(text.strong().color(THEME.ink)).fill(THEME.accent),
            Kind::Filled(i) => egui::Button::new(text.strong().color(THEME.ink)).fill(i.colour()),
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
                    Button::warning("Overwrite"),
                    Button::danger("Erase"),
                    Button::subtle("median").small(),
                ] {
                    if ui.add(b).clicked() {
                        *n += 1;
                    }
                }
            },
            0,
        );
        for label in ["Save", "Straighten", "Overwrite", "Erase", "median"] {
            h.get_by_label(label).click();
            h.run();
        }
        assert_eq!(*h.state(), 5);
    }
}
