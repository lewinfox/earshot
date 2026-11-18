//! A view per mode, each laid out from panels. The shell draws the top bar, the
//! MIDI log and the windows; the mode's view draws the rest of the window.

pub mod controller;
pub mod debug;
pub mod perform;
pub mod prepare;

use egui::Margin;

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

/// Divide the window (what the bars have left of it) for the mode's view: every row's
/// height and the decks' scale, before any of it is drawn.
pub fn shell(ctx: &egui::Context, panels: &Panels, cx: &Cx) -> layout::Shell {
    let prefs = cx.prefs;
    // The Flow editor's full screen hides the decks and their waveforms.
    let planning = prefs.flow_tab() && panels.flow.full;
    // What the bars left, and never wider than the window itself.
    let room = ctx.available_rect().intersect(ctx.content_rect());
    layout::shell(
        room.size(),
        layout::ShellInput {
            decks: prefs.deck_total(),
            mixer: prefs.mixer_shown(),
            // The waveforms make way for a bigger library (the decks stay).
            waves: !planning && !prefs.library_maximized,
            decks_row: !planning,
            tab_bar: !matches!(prefs.mode, Mode::Perform)
                && (cfg!(debug_assertions) || prefs.mode != Mode::Debug),
            waves_share: prefs.layout().sizes.waves,
        },
    )
}

/// A bar of tabs over the body, at the height the layout kept for it.
fn tab_bar(ctx: &egui::Context, id: &str, theme: &Theme, body: impl FnOnce(&mut egui::Ui)) {
    let sp = &theme.spacing;
    egui::TopBottomPanel::top(id.to_owned())
        .exact_height(sp.tab_bar_h)
        .frame(
            egui::Frame::new()
                .fill(theme.bg)
                .inner_margin(sp.row_margin),
        )
        .show(ctx, |ui| {
            ui.allocate_ui(egui::vec2(ui.available_width(), sp.tab_h), body);
        });
}

/// The body: what's left of the window under the rows above, in `frame`. Whatever is
/// drawn in it is cut off at its edges, so a pane with more than fits (it should
/// scroll) can never paint over the status bar. Every view's body goes through here.
fn body(ctx: &egui::Context, frame: egui::Frame, add: impl FnOnce(&mut egui::Ui)) {
    let room = ctx.available_rect();
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        ui.set_clip_rect(room.intersect(ui.clip_rect()));
        add(ui);
    });
}

/// The frame of a pane of the body that has none of its own.
fn pane_frame(theme: &Theme) -> egui::Frame {
    egui::Frame::new()
        .fill(theme.bg)
        .inner_margin(theme.spacing.pane_margin)
}

/// Draw the mode's view.
pub fn show(ctx: &egui::Context, panels: &mut Panels, cx: &mut Cx) {
    match cx.prefs.mode {
        Mode::Perform => perform::show(ctx, panels, cx),
        Mode::Prepare => prepare::show(ctx, panels, cx),
        Mode::Controller => controller::show(ctx, panels, cx),
        // Only reachable in a debug build; a release build left in it shows Perform.
        Mode::Debug if cfg!(debug_assertions) => debug::show(ctx, panels, cx),
        Mode::Debug => perform::show(ctx, panels, cx),
    }
}

/// The frame of the waveforms' row and the decks' row.
fn row_frame(cx: &Cx, margin: Margin) -> egui::Frame {
    egui::Frame::new().fill(cx.theme.bg).inner_margin(margin)
}

/// The library fills what's left, drawing its own panes' frames.
fn library(ctx: &egui::Context, panels: &mut Panels, cx: &mut Cx) {
    use super::panels::Panel;
    body(ctx, egui::Frame::new().fill(cx.theme.bg), |ui| {
        panels.library.ui(ui, cx)
    });
}
