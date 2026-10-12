//! A view per mode, each laid out from panels. The shell draws the top bar, the
//! MIDI log and the windows; the mode's view draws the rest of the window.

pub mod perform;
pub mod prepare;

use egui::Margin;

use super::panels::Cx;
use super::panels::deck::DeckUi;
use super::panels::library::LibraryPane;
use crate::core::session::{self, SLOTS};
use crate::core::view::Mode;

/// The panels' own state, kept between frames.
pub struct Panels {
    /// Each player's view, indexed by `DeckId::index()`.
    pub decks: [DeckUi; SLOTS],
    pub library: LibraryPane,
}

impl Panels {
    pub fn new(library: LibraryPane) -> Panels {
        Panels {
            decks: std::array::from_fn(|i| DeckUi::new(session::slot(i))),
            library,
        }
    }
}

/// Draw the mode's view.
pub fn show(ctx: &egui::Context, panels: &mut Panels, cx: &mut Cx) {
    match cx.prefs.mode {
        Mode::Perform => perform::show(ctx, panels, cx),
        Mode::Prepare => prepare::show(ctx, panels, cx),
    }
}

/// The frame of the waveforms' row and the decks' row.
fn row_frame(cx: &Cx, margin: Margin) -> egui::Frame {
    egui::Frame::new().fill(cx.theme.bg).inner_margin(margin)
}

/// The library fills what's left, drawing its own panes' frames.
fn library(ctx: &egui::Context, panels: &mut Panels, cx: &mut Cx) {
    use super::panels::Panel;
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(cx.theme.bg))
        .show(ctx, |ui| panels.library.ui(ui, cx));
}
