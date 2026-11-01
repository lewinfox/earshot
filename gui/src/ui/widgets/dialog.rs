//! A small window in the middle of the screen asking one thing ("Add to STICK",
//! "Format STICK"), like a controlled modal: the caller keeps whether it's open
//! (usually an `Option` of its state) and acts on the [`Choice`] its body returns.
//!
//! ```ignore
//! match dialog(ctx, "Format STICK", |ui| {
//!     ui.label("Everything on it is erased.");
//!     go_cancel(ui, Button::danger("Erase and format"), true, "")
//! }) {
//!     Choice::Go(()) => { /* do it */ }
//!     Choice::Close => self.format = None,
//!     Choice::Stay => {}
//! }
//! ```

use egui::{Context, Ui, WidgetText};

use super::Button;

/// What the user did with a dialog this frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice<T = ()> {
    /// Nothing: it stays open.
    Stay,
    /// Its main button (or one of its actions, `T`).
    Go(T),
    /// Cancel, Close or the window's ×.
    Close,
}

/// Show the dialog: `body` draws it and says what was chosen. Closing it with the
/// window's × is [`Choice::Close`].
pub fn dialog<T>(
    ctx: &Context,
    title: impl Into<WidgetText>,
    body: impl FnOnce(&mut Ui) -> Choice<T>,
) -> Choice<T> {
    let mut open = true;
    let choice = egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, body)
        .and_then(|r| r.inner)
        .unwrap_or(Choice::Stay);
    if open { choice } else { Choice::Close }
}

/// The dialog's buttons, after a little space: `go` (enabled or not, with why not
/// on hover) and Cancel.
pub fn go_cancel(ui: &mut Ui, go: Button, enabled: bool, why_not: &str) -> Choice {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(enabled, go)
            .on_disabled_hover_text(why_not)
            .clicked()
        {
            Choice::Go(())
        } else if ui.add(Button::secondary("Cancel")).clicked() {
            Choice::Close
        } else {
            Choice::Stay
        }
    })
    .inner
}

/// A single Close button: the dialog has said its piece.
pub fn close_only(ui: &mut Ui) -> Choice<()> {
    ui.add_space(6.0);
    if ui.add(Button::secondary("Close")).clicked() {
        Choice::Close
    } else {
        Choice::Stay
    }
}
