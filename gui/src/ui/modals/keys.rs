//! The key help, made from the keymap: a column per deck (or player), then the library.

use egui::{RichText, Ui};

use crate::core::keymap::{self, Keymap};
use crate::ui::theme::THEME;

/// The window's title, which is also its id.
pub const TITLE: &str = "Keyboard";

pub fn keys_help(ui: &mut Ui, keymap: &Keymap, mode: keymap::Mode) {
    ui.label(
        RichText::new(
            "Deck keys work unless a text field has focus. Esc leaves the field. \
             Shift + a hot cue key clears it.",
        )
        .color(THEME.muted),
    );
    ui.add_space(6.0);
    let mut sections = keymap.help(mode);
    // Built into egui and the lists rather than the keymap.
    if let Some(lib) = sections.iter_mut().find(|s| s.title == "Library") {
        lib.rows.extend([
            ("Ctrl A".into(), "select all".into()),
            ("Ctrl - / = / 0".into(), "UI size".into()),
        ]);
    }
    ui.columns(sections.len().max(1), |c| {
        for (ui, s) in c.iter_mut().zip(&sections) {
            let colour = s.deck.map_or(THEME.text, |d| THEME.deck_colour(d));
            ui.label(RichText::new(&s.title).strong().color(colour));
            egui::Grid::new(&s.title).show(ui, |ui| {
                for (k, what) in &s.rows {
                    ui.label(RichText::new(k).monospace());
                    ui.label(what);
                    ui.end_row();
                }
            });
        }
    });
}
