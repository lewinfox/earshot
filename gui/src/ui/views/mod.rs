//! A view per mode, each laid out from panels. The shell draws the top bar, the
//! MIDI log and the windows; the mode's view draws the rest of the window.

pub mod controller;
pub mod debug;
pub mod perform;
pub mod prepare;

use super::flex::{self, Flex, Tui, col, leaf};
use super::layout;
use super::panels::Cx;
use super::panels::controller::ControllerUi;
use super::panels::deck::DeckUi;
use super::panels::export::ExportUi;
use super::panels::flow::FlowUi;
use super::panels::library::LibraryPane;
use super::panels::stems::StemsUi;
use super::panels::tempo_lock::TempoLockUi;
use super::panels::trim::TrimUi;
use super::theme::Theme;
use crate::core::session::{self, SLOTS};
use crate::core::view::Mode;

/// The panels' own state, kept between frames.
pub struct Panels {
    /// Each player's view, indexed by `DeckId::index()`.
    pub decks: [DeckUi; SLOTS],
    pub library: LibraryPane,
    /// Prepare's Tempo lock tab.
    pub tempo_lock: TempoLockUi,
    /// Prepare's Flow editor.
    pub flow: FlowUi,
    /// Prepare's Export tab.
    pub export: ExportUi,
    /// Prepare's Stems tab.
    pub stems: StemsUi,
    /// Prepare's Trim tab.
    pub trim: TrimUi,
    /// Controller mode's tabs.
    pub controller: ControllerUi,
    /// Debug mode's log, spectrogram and export.
    pub debug: super::panels::debug::DebugUi,
}

impl Panels {
    pub fn new(library: LibraryPane) -> Panels {
        Panels {
            decks: std::array::from_fn(|i| DeckUi::new(session::slot(i))),
            library,
            tempo_lock: TempoLockUi::default(),
            flow: FlowUi::default(),
            export: ExportUi::default(),
            stems: StemsUi::default(),
            trim: TrimUi::default(),
            controller: ControllerUi::default(),
            debug: Default::default(),
        }
    }
}

/// What the window under the bars shows this frame, from the preferences.
pub fn shell(ctx: &egui::Context, panels: &Panels, cx: &Cx) -> layout::Shell {
    let prefs = cx.prefs;
    // The Flow editor's full screen hides the decks and their waveforms.
    let planning = prefs.flow_tab() && panels.flow.full;
    // What the bars left, and never wider than the window itself.
    let room = ctx.available_rect().intersect(ctx.content_rect());
    layout::Shell {
        size: room.size(),
        decks: prefs.deck_total(),
        mixer: prefs.mixer_shown(),
        // The waveforms make way for a bigger library (the decks stay).
        waves: !planning && !prefs.library_maximized,
        decks_row: !planning,
        tab_bar: !matches!(prefs.mode, Mode::Perform)
            && (cfg!(debug_assertions) || prefs.mode != Mode::Debug),
        waves_share: prefs.layout().sizes.waves,
    }
}

/// A bar of tabs over the body.
fn tab_bar(t: &mut Tui, theme: &Theme, body: impl FnOnce(&mut egui::Ui)) {
    let sp = &theme.spacing;
    flex::pane(t, leaf().h(sp.tab_bar_h), |ui| {
        let frame = egui::Frame::new()
            .fill(theme.bg)
            .inner_margin(sp.row_margin);
        flex::boxed(ui, frame, |ui| {
            ui.allocate_ui(egui::vec2(ui.available_width(), sp.tab_h), body);
        });
    });
}

/// The body: what the rows above leave of the window, in `frame`. Whatever is drawn
/// in it is cut off at its edges, so a pane with more than fits (it should scroll)
/// can never paint over the status bar. Every view's body goes through here.
fn body(t: &mut Tui, frame: egui::Frame, add: impl FnOnce(&mut egui::Ui)) {
    flex::pane(t, leaf().grow().min_h(layout::BODY_MIN_H), |ui| {
        flex::boxed(ui, frame, add)
    });
}

/// The frame of a pane of the body that has none of its own.
fn pane_frame(theme: &Theme) -> egui::Frame {
    egui::Frame::new()
        .fill(theme.bg)
        .inner_margin(theme.spacing.pane_margin)
}

/// Draw the mode's view: one column, top to bottom, in what the bars leave.
pub fn show(ctx: &egui::Context, panels: &mut Panels, cx: &mut Cx) {
    let mode = match cx.prefs.mode {
        // Only reachable in a debug build; a release build left in it shows Perform.
        Mode::Debug if !cfg!(debug_assertions) => Mode::Perform,
        m => m,
    };
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(cx.theme.bg))
        .show(ctx, |ui| {
            flex::root(ui, egui::Id::new(("shell", mode)), col(), |t| match mode {
                Mode::Perform => perform::show(t, panels, cx),
                Mode::Prepare => prepare::show(t, panels, cx),
                Mode::Controller => controller::show(t, panels, cx),
                Mode::Debug => debug::show(t, panels, cx),
            });
        });
}

/// The library fills what's left, drawing its own panes' frames.
fn library(t: &mut Tui, panels: &mut Panels, cx: &mut Cx) {
    use super::panels::Panel;
    body(t, egui::Frame::new().fill(cx.theme.bg), |ui| {
        panels.library.ui(ui, cx)
    });
}
