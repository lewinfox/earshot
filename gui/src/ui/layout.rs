//! What the window shows, and the sizes its parts are laid out with.
//!
//! The layout itself is flexbox (`ui::flex`): each view says what its rows and panes
//! are and how they share the room, and taffy works out the sizes. This module holds
//! the names those layouts share: what the window shows this frame ([`Shell`]), the
//! least and design sizes of its parts, and the handles that drag a pane's edge.
//! `docs/ui-layout.md` says how to add or change a part of the UI.

use egui::{Rect, Sense, Ui, Vec2};

/// The body (the library, a tool) is never squeezed below this height by the rows
/// above it.
pub const BODY_MIN_H: f32 = 150.0;
/// The most of the window's height the decks' row takes.
pub const DECKS_SHARE: f32 = 0.45;
/// A scrolling waveform's least height, and the gap between two.
pub const WAVE_MIN_H: f32 = 14.0;
pub const WAVE_GAP: f32 = 3.0;
/// The most of the window's height the waveforms can be dragged to.
pub const WAVES_MAX_SHARE: f32 = 0.5;
/// Between the decks stacked on one side of the mixer.
pub const STACK_GAP: f32 = 4.0;

/// The library's panes' least widths, and the track list's.
pub const SOURCE_MIN_W: f32 = 140.0;
pub const ASIDE_MIN_W: f32 = 220.0;
pub const TABLE_MIN_W: f32 = 240.0;
/// A Prepare tool's, beside the library; and the library's beside it.
pub const TOOL_MIN_W: f32 = 320.0;
pub const BESIDE_MIN_W: f32 = 320.0;
/// The most of a row a dragged pane can take.
pub const PANE_MAX_SHARE: f32 = 0.6;

/// What the window under the top bar shows this frame, from the preferences.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Shell {
    /// The room under the top bar and over the status bar.
    pub size: Vec2,
    /// Decks showing: 1, 2 or 4.
    pub decks: usize,
    /// The mixer between the decks.
    pub mixer: bool,
    /// The scrolling waveforms' row.
    pub waves: bool,
    /// The decks' row (not in the Flow editor's full screen).
    pub decks_row: bool,
    /// A bar of tabs over the body (Prepare's tools, Controller's tabs).
    pub tab_bar: bool,
    /// The waveforms' row's share of the height, as last dragged.
    pub waves_share: f32,
}

impl Shell {
    /// The waveforms' row's height, as a share of the window's: as dragged, but never
    /// less than the waveforms' least nor more than their most.
    pub fn waves_pct(&self) -> f32 {
        if self.size.y <= 0.0 {
            return 0.0;
        }
        let n = self.decks.max(1) as f32;
        let least = n * WAVE_MIN_H + (n - 1.0) * WAVE_GAP + 8.0;
        self.waves_share.clamp(
            least / self.size.y,
            WAVES_MAX_SHARE.max(least / self.size.y),
        )
    }
}

// ---- dragging a pane's edge ------------------------------------------------------------

/// Which edge of a pane is dragged to resize it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    /// A pane on the left: its right edge.
    Right,
    /// A pane on the right: its left edge.
    Left,
    /// A row at the top: its bottom edge.
    Bottom,
}

/// How far in from the pane's edge the handle reaches.
const HANDLE: f32 = 5.0;

/// A handle along `edge` of the pane `ui` draws (call it inside the pane, last). While
/// it's dragged: the size, in points, the pane is being dragged to. The pane's size
/// comes from this module, so the caller turns that into a share of its row and saves
/// it (`ViewCmd::Resize`); the next frame's layout does the rest.
pub fn splitter(ui: &mut Ui, edge: Edge) -> Option<f32> {
    // The pane's whole rectangle, its frame's margins included.
    let pane = ui.clip_rect();
    let handle = match edge {
        Edge::Right => Rect::from_min_max(egui::pos2(pane.right() - HANDLE, pane.top()), pane.max),
        Edge::Left => Rect::from_min_max(pane.min, egui::pos2(pane.left() + HANDLE, pane.bottom())),
        Edge::Bottom => {
            Rect::from_min_max(egui::pos2(pane.left(), pane.bottom() - HANDLE), pane.max)
        }
    };
    let r = ui.interact(handle, ui.id().with("splitter"), Sense::drag());
    let cursor = match edge {
        Edge::Bottom => egui::CursorIcon::ResizeVertical,
        _ => egui::CursorIcon::ResizeHorizontal,
    };
    let r = r.on_hover_and_drag_cursor(cursor);
    if r.hovered() || r.dragged() {
        let stroke = if r.dragged() {
            ui.visuals().widgets.active.fg_stroke
        } else {
            ui.visuals().widgets.hovered.fg_stroke
        };
        let p = ui.painter();
        match edge {
            Edge::Right => p.vline(pane.right() - 1.0, pane.y_range(), stroke),
            Edge::Left => p.vline(pane.left() + 1.0, pane.y_range(), stroke),
            Edge::Bottom => p.hline(pane.x_range(), pane.bottom() - 1.0, stroke),
        };
    }
    let at = r.dragged().then(|| r.interact_pointer_pos()).flatten()?;
    Some(match edge {
        Edge::Right => at.x - pane.left(),
        Edge::Left => pane.right() - at.x,
        Edge::Bottom => at.y - pane.top(),
    })
}
