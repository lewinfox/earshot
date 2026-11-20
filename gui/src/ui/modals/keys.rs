//! The key help, made from the keymap: a column per deck (or player), then the library.

use egui::{RichText, Ui};

use crate::core::keymap::{self, Keymap};
use crate::ui::theme::THEME;

/// The window's title, which is also its id.
pub const TITLE: &str = "Keyboard";

/// Between the sections side by side.
const SECTION_GAP: f32 = 18.0;

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
            ("F11".into(), "full screen".into()),
        ]);
    }
    // The sections side by side, as many to a line as there's room for: the next one
    // that doesn't fit starts a new line (their widths are last frame's).
    let n = sections.len();
    let avail = ui.available_width();
    let id = egui::Id::new("keys-help-widths");
    let widths: Vec<f32> = ui.ctx().data(|d| d.get_temp(id)).unwrap_or_default();
    let known = widths.len() == n;
    let mut lines: Vec<Vec<usize>> = vec![];
    let mut x = 0.0;
    for i in 0..n {
        let w = if known { widths[i] } else { avail };
        if lines.is_empty() || x + w > avail {
            lines.push(vec![]);
            x = 0.0;
        }
        lines.last_mut().expect("a line").push(i);
        x += w + SECTION_GAP;
    }
    let mut now = vec![0.0; n];
    for line in lines {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = SECTION_GAP;
            for i in line {
                let s = &sections[i];
                let r = ui.vertical(|ui| {
                    let colour = s.deck.map_or(THEME.text, |d| THEME.deck_colour(d));
                    ui.label(RichText::new(&s.title).strong().color(colour));
                    egui::Grid::new(&s.title).show(ui, |ui| {
                        for (k, what) in &s.rows {
                            ui.label(RichText::new(k).monospace());
                            ui.add(egui::Label::new(what).extend());
                            ui.end_row();
                        }
                    });
                });
                now[i] = r.response.rect.width();
            }
        });
        ui.add_space(8.0);
    }
    if !known || now.iter().zip(&widths).any(|(a, b)| (a - b).abs() > 0.5) {
        ui.ctx().data_mut(|d| d.insert_temp(id, now));
        ui.ctx().request_discard("the key help's sections' widths");
    }
}
