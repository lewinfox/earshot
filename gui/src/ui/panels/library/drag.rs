//! What can be dragged round the app, and the small ghost that follows the mouse
//! while it is.
//!
//! - A track list sets [`DragPayload::Tracks`] with the whole selection (or the
//!   row grabbed, outside it). Decks, the Flow canvas, collections and playlist rows
//!   take it: [`take_tracks`], [`hovering_tracks`].
//! - The collection tree sets [`DragPayload::Collection`] to move one into a folder.
//!
//! The shell paints the ghost once a frame ([`paint_ghost`]); anything else that
//! drags (Flow cards) can show one too with [`ghost`].

use egui::{Color32, FontId, Id, LayerId, Order, Response, Stroke, StrokeKind, vec2};

use crate::core::collections::CollectionId;
use crate::core::index::LibraryIndex;
use crate::core::track::TrackId;
use crate::ui::theme::{THEME, fs};

/// Where a track drag started, so a drop target can tell (a playlist reorders its
/// own rows; it adds anyone else's).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The library's track table.
    Library,
    /// The track table showing this collection.
    Collection(CollectionId),
    Suggest,
    /// A card on the Flow canvas.
    Flow,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DragPayload {
    /// Tracks, in list order: the selection, or the one row grabbed outside it.
    Tracks { ids: Vec<TrackId>, origin: Origin },
    /// A collection being moved round its tree.
    Collection(CollectionId),
}

impl DragPayload {
    /// One track (a Flow card, say).
    pub fn track(id: TrackId, origin: Origin) -> DragPayload {
        DragPayload::Tracks {
            ids: vec![id],
            origin,
        }
    }

    /// What the ghost says: the track's title, or how many.
    pub fn label(&self, lib: &LibraryIndex) -> String {
        match self {
            DragPayload::Tracks { ids, .. } => match ids.as_slice() {
                [one] => lib
                    .get(*one)
                    .map_or_else(|| "1 track".into(), |t| t.title.clone()),
                many => format!("{} tracks", many.len()),
            },
            DragPayload::Collection(_) => String::new(),
        }
    }
}

/// The payload being dragged now, if it's ours.
pub fn payload(ctx: &egui::Context) -> Option<std::sync::Arc<DragPayload>> {
    egui::DragAndDrop::payload::<DragPayload>(ctx)
}

/// Tracks are being dragged over `resp`.
pub fn hovering_tracks(resp: &Response) -> bool {
    matches!(
        resp.dnd_hover_payload::<DragPayload>().as_deref(),
        Some(DragPayload::Tracks { .. })
    )
}

/// Tracks dropped on `resp` this frame: (tracks, where from). Leaves any other
/// payload alone.
pub fn take_tracks(resp: &Response) -> Option<(Vec<TrackId>, Origin)> {
    if !hovering_tracks(resp) {
        return None;
    }
    match resp.dnd_release_payload::<DragPayload>()?.as_ref() {
        DragPayload::Tracks { ids, origin } => Some((ids.clone(), *origin)),
        DragPayload::Collection(_) => None,
    }
}

/// The first track dropped on `resp` (a deck takes one).
pub fn take_track(resp: &Response) -> Option<TrackId> {
    take_tracks(resp).and_then(|(ids, _)| ids.first().copied())
}

/// The ghost for whatever track drag is under way.
pub fn paint_ghost(ctx: &egui::Context, lib: &LibraryIndex) {
    if let Some(p) = payload(ctx)
        && let DragPayload::Tracks { ids, .. } = p.as_ref()
    {
        ghost(ctx, &p.label(lib), ids.len());
    }
}

/// A small label just below-right of the mouse, over everything, while a button is
/// held. `count` above 1 shows a stack behind it.
pub fn ghost(ctx: &egui::Context, label: &str, count: usize) {
    let Some(at) = ctx.input(|i| {
        i.pointer
            .primary_down()
            .then(|| i.pointer.hover_pos())
            .flatten()
    }) else {
        return;
    };
    let painter = ctx.layer_painter(LayerId::new(Order::Tooltip, Id::new("drag-ghost")));
    let font = FontId::proportional(fs(12.0));
    let text: String = if label.chars().count() > 40 {
        label.chars().take(39).chain(['…']).collect()
    } else {
        label.to_string()
    };
    let galley = painter.layout_no_wrap(text, font, THEME.text);
    let rect = egui::Rect::from_min_size(at + vec2(14.0, 10.0), galley.size() + vec2(16.0, 8.0));
    if count > 1 {
        painter.rect(
            rect.translate(vec2(3.0, 3.0)),
            4,
            THEME.panel,
            Stroke::new(1.0_f32, THEME.line),
            StrokeKind::Inside,
        );
    }
    painter.rect(
        rect,
        4,
        THEME.panel_2.gamma_multiply(0.95),
        Stroke::new(1.0_f32, THEME.accent),
        StrokeKind::Inside,
    );
    painter.galley(rect.min + vec2(8.0, 4.0), galley, Color32::PLACEHOLDER);
}
