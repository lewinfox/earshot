//! Text: [`muted`] small grey notes, [`heading`]s, [`Note`]s in an [`Intent`]'s
//! colour ("Formatting failed: …"), and [`busy_line`], a spinner with what's going on.

use egui::{Response, RichText, Ui, Widget};

use super::Intent;
use crate::ui::theme::{THEME, fs};

/// A small grey note under or beside a control.
pub fn muted(ui: &mut Ui, text: impl Into<String>) -> Response {
    ui.label(RichText::new(text.into()).size(fs(11.0)).color(THEME.muted))
}

/// A pane's or a column's title.
pub fn heading(ui: &mut Ui, text: &str) -> Response {
    let r = ui.label(RichText::new(text).strong().size(fs(14.0)));
    ui.add_space(2.0);
    r
}

/// A spinner and `message` (or `fallback` while it's empty): something running.
pub fn busy_line(ui: &mut Ui, message: &str, fallback: &str) -> Response {
    ui.horizontal(|ui| {
        ui.spinner();
        muted(
            ui,
            if message.is_empty() {
                fallback
            } else {
                message
            },
        );
    })
    .response
}

/// A line of text in an intent's colour: `ui.add(Note::danger(format!(…)))`.
pub struct Note {
    text: String,
    intent: Intent,
    small: bool,
}

impl Note {
    pub fn new(intent: Intent, text: impl Into<String>) -> Note {
        Note {
            text: text.into(),
            intent,
            small: false,
        }
    }

    #[allow(dead_code)] // The set's complete; not used yet.
    pub fn info(text: impl Into<String>) -> Note {
        Note::new(Intent::Info, text)
    }

    pub fn success(text: impl Into<String>) -> Note {
        Note::new(Intent::Success, text)
    }

    pub fn warning(text: impl Into<String>) -> Note {
        Note::new(Intent::Warning, text)
    }

    pub fn danger(text: impl Into<String>) -> Note {
        Note::new(Intent::Danger, text)
    }

    /// At the small size [`muted`] notes have.
    pub fn small(mut self) -> Note {
        self.small = true;
        self
    }
}

impl Widget for Note {
    fn ui(self, ui: &mut Ui) -> Response {
        let mut text = RichText::new(self.text).color(self.intent.text_colour());
        if self.small {
            text = text.size(fs(11.0));
        }
        ui.label(text)
    }
}
