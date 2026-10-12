//! Flow: a set-building canvas. Add a track or two, then grow the set from
//! suggestions. Each card has a "+" on its right that opens nine suggested next tracks
//! (with Drop / Hold / Build); picking one adds it as a new card joined to the first.
//! Drag cards to move them, drag the background to pan, wheel to zoom. Link two cards
//! by dragging from one's "+" (out) to another's left port (in), or the other way; a
//! card can follow several. Save a path through it as a playlist. The model is
//! `core::collections::FlowGraph`; the Store keeps it.

use std::collections::HashSet;

use egui::emath::TSTransform;
use egui::epaint::CubicBezierShape;
use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Pos2, Rect, RichText, Sense, Stroke, Ui,
    UiBuilder, pos2, vec2,
};
use serde::{Deserialize, Serialize};

use super::library::drag::{self, DragPayload};
use super::library::tracks::{Media, MenuAt, track_menu};
use super::{CoreView, PreviewState};
use crate::core::collections::{
    CollectionId, FlowCmd, FlowGraph, FlowNode, GraphOp, NodeId, Spacing,
};
use crate::core::command::{Command, PreviewCmd};
use crate::core::history::RefTrack;
use crate::core::index::LibraryCmd;
use crate::core::query;
use crate::core::suggest::{Direction, Scored};
use crate::core::track::{Track, TrackId};
use crate::ui::theme::THEME;
use crate::ui::util;
use crate::ui::widgets::key_chip;

const NODE_W: f32 = 236.0;
const NODE_H: f32 = 104.0;
/// The waveform seek bar along a card's bottom edge.
const SEEK_H: f32 = 20.0;
const GAP_X: f32 = 90.0;
const GAP_Y: f32 = 22.0;
/// Where new and tidied cards go.
const SPACING: Spacing = Spacing {
    col: NODE_W + GAP_X,
    row: NODE_H + GAP_Y,
};
/// Width of the "+" suggestions popup.
const POP_W: f32 = 600.0;

fn pos(n: &FlowNode) -> Pos2 {
    pos2(n.x, n.y)
}

fn rect(n: &FlowNode) -> Rect {
    Rect::from_min_size(pos(n), vec2(NODE_W, NODE_H))
}

/// What is kept between runs (with the window's settings). The cards are in the
/// database.
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Saved {
    /// The saved Flow on the canvas.
    #[serde(default)]
    flow: Option<i64>,
    scene: Option<Rect>,
    open: bool,
}

enum Pop {
    /// Nine suggestions after this card.
    Suggest(NodeId),
    /// Search the library for a starting track, below this point.
    Search(Pos2),
}

/// What the "+" suggestions were worked out for.
#[derive(PartialEq)]
struct PickStamp {
    track: TrackId,
    direction: Direction,
    lib: u64,
    history: u64,
    exclude: HashSet<TrackId>,
}

pub struct FlowUi {
    /// The saved Flow on the canvas (`None` until the first card is added).
    pub flow: Option<CollectionId>,
    pub name: String,
    graph: FlowGraph,
    /// The graph as last saved, to tell when it needs saving again.
    stored: FlowGraph,
    /// The part of the canvas in view (egui's `Scene` keeps it as a rectangle).
    scene: Rect,
    /// The Flow tab is showing (instead of the track list).
    pub open: bool,
    /// Hide the decks and side panes while planning.
    pub full: bool,
    direction: Direction,
    pop: Option<Pop>,
    search: String,
    focus_search: bool,
    /// Pan to show this card on the next frame.
    reveal: Option<NodeId>,
    fit: bool,
    /// Suggestions for the open "+" popup, and what they were computed for.
    picks: Vec<Scored>,
    picks_for: Option<PickStamp>,
    /// A link being dragged: from this card, starting at its out ("+") or in port.
    linking: Option<(NodeId, Port)>,
    /// The card clicked on last; Delete or Backspace removes it.
    selected: Option<NodeId>,
    /// A card being dragged, and where it started. Dropped outside the canvas (e.g. on
    /// a deck, which loads it) it goes back there.
    moving: Option<(NodeId, Pos2)>,
}

#[derive(Clone, Copy, PartialEq)]
enum Port {
    In,
    Out,
}

enum CardAction {
    Edit(GraphOp),
    Plus(NodeId),
    Drag(NodeId, egui::Vec2),
    DragEnd,
    StartLink(NodeId, Port),
    Select(NodeId),
    Bake(NodeId),
}

/// How a card is drawn this frame.
struct CardState {
    /// This track is in the headphones' preview player.
    preview: Option<PreviewState>,
    /// A link being dragged would land here.
    link_target: bool,
    /// Clicked on: Delete removes it.
    selected: bool,
    /// The pointer is over the card (or just outside it).
    near: bool,
    /// The track's waveform thumbnail, once it has one.
    thumb: Option<egui::TextureId>,
    art: Option<egui::TextureId>,
}

const ZOOM: std::ops::RangeInclusive<f32> = 0.25..=1.5;

impl FlowUi {
    pub fn from_saved(s: Saved) -> FlowUi {
        FlowUi {
            flow: s.flow,
            name: String::new(),
            graph: FlowGraph::default(),
            stored: FlowGraph::default(),
            scene: s
                .scene
                .unwrap_or(Rect::from_min_size(pos2(-40.0, -40.0), vec2(1200.0, 500.0))),
            open: s.open,
            full: false,
            direction: Direction::Hold,
            pop: None,
            search: String::new(),
            focus_search: false,
            reveal: None,
            fit: false,
            picks: vec![],
            picks_for: None,
            linking: None,
            selected: None,
            moving: None,
        }
    }

    pub fn saved(&self) -> Saved {
        Saved {
            flow: self.flow,
            scene: Some(self.scene),
            open: self.open,
        }
    }

    /// Show a saved Flow (or an empty canvas with `None`).
    pub fn load(&mut self, flow: Option<(CollectionId, String, FlowGraph)>) {
        let (id, name, graph) = match flow {
            Some((id, name, graph)) => (Some(id), name, graph),
            None => (None, String::new(), FlowGraph::default()),
        };
        self.flow = id;
        self.name = name;
        self.stored = graph.clone();
        self.graph = graph;
        self.pop = None;
        self.linking = None;
        self.moving = None;
        self.selected = None;
        self.fit = !self.graph.is_empty();
    }

    /// The graph, if it changed since it was last saved and nothing is being
    /// dragged. Counts it as saved.
    #[cfg(test)]
    pub fn changed_graph(&mut self) -> Option<FlowGraph> {
        self.take_change().map(|(_, now)| now)
    }

    /// (as last saved, now), if the graph changed since it was last saved and nothing
    /// is being dragged. Counts it as saved. One finished gesture is one change: the
    /// shell saves it and records it for undo (`core::edit::flow_edit`).
    pub fn take_change(&mut self) -> Option<(FlowGraph, FlowGraph)> {
        if self.moving.is_some() || self.linking.is_some() || self.graph == self.stored {
            return None;
        }
        let before = std::mem::replace(&mut self.stored, self.graph.clone());
        Some((before, self.graph.clone()))
    }

    /// Undo or redo on the open Flow (`Edit::FlowGraph`'s ops). Not recorded again:
    /// the shell saves it straight away.
    pub fn apply_ops(&mut self, ops: &[GraphOp]) {
        for op in ops {
            let _ = self.graph.apply(op, SPACING);
        }
        let gone = |id: Option<NodeId>| id.is_some_and(|id| self.graph.node(id).is_none());
        if gone(self.selected) {
            self.selected = None;
        }
        if gone(match self.pop {
            Some(Pop::Suggest(id)) => Some(id),
            _ => None,
        }) {
            self.pop = None;
        }
        self.linking = None;
        self.moving = None;
    }

    /// The track on the selected card (for suggestions after "Selected").
    pub fn selected_track(&self) -> Option<TrackId> {
        self.graph.node(self.selected?).map(|n| n.track)
    }

    fn edit(&mut self, op: GraphOp) {
        match self.graph.apply(&op, SPACING) {
            Ok(Some(id)) => self.reveal = Some(id),
            Ok(None) => {}
            // A refused link (it would loop) or a card that's gone: nothing to do.
            Err(_) => {}
        }
    }

    /// Add tracks as new starting cards, below the existing ones.
    pub fn add_tracks(&mut self, tracks: &[TrackId]) {
        for &t in tracks {
            self.edit(GraphOp::AddRoot(t));
        }
    }

    /// Open the "+" suggestions on the first card (for the screenshot test).
    #[cfg(test)]
    pub fn open_first_suggestions(&mut self) {
        self.pop = self.graph.nodes.first().map(|n| Pop::Suggest(n.id));
    }

    #[cfg(test)]
    pub fn fit(&mut self) {
        self.fit = true;
    }

    fn bounds(&self) -> Option<Rect> {
        self.graph.nodes.iter().map(rect).reduce(|a, b| a.union(b))
    }

    /// The path to save as a playlist: through the selected card, or the whole Flow
    /// when it's one chain.
    fn bake_path(&self) -> Option<Vec<NodeId>> {
        if let Some(s) = self.selected {
            return Some(self.graph.path_through(s));
        }
        self.graph.linear_order()?;
        let root = self.graph.nodes.first()?;
        Some(self.graph.path_through(root.id))
    }

    fn bake(&self, nodes: Vec<NodeId>, out: &mut Vec<Command>) {
        if let Some(flow) = self.flow {
            let name = format!("{} (path)", self.name);
            out.push(FlowCmd::BakePath { flow, nodes, name }.into());
        }
    }

    // ---- drawing ------------------------------------------------------------------------

    /// `preview`: the track playing in the headphones, and how far through it is.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        v: &CoreView,
        media: &mut Media,
        preview: Option<PreviewState>,
        out: &mut Vec<Command>,
    ) {
        // To stop the preview if its card goes (removed, or Reset).
        let had_preview = preview.is_some_and(|p| self.graph.nodes.iter().any(|n| n.track == p.id));
        self.toolbar(ui, v, out);

        if let Some(id) = self.reveal.take()
            && let Some(n) = self.graph.node(id)
        {
            self.scene = reveal(self.scene, rect(n).expand(20.0));
        }

        let canvas = ui.available_rect_before_wrap();
        if std::mem::take(&mut self.fit)
            && let Some(b) = self.bounds()
        {
            // Everything in view, but no closer than 1:1 so a few cards don't fill
            // the screen.
            let size = (b.size() + vec2(80.0, 80.0)).max(canvas.size());
            self.scene = Rect::from_center_size(b.center(), size);
        }
        ui.painter().rect_filled(canvas, 0.0, THEME.bg);

        // The wheel zooms (egui's Scene would pan with it): take the scroll before the
        // Scene sees it, unless a popup or menu is under the pointer.
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let over_canvas = pointer.is_some_and(|p| {
            canvas.contains(p)
                && ui
                    .ctx()
                    .layer_id_at(p)
                    .is_none_or(|l| l.order < egui::Order::Foreground)
        });
        let wheel = if over_canvas {
            ui.ctx()
                .input_mut(|i| std::mem::take(&mut i.smooth_scroll_delta))
                .y
        } else {
            0.0
        };

        let mut scene = self.scene;
        let mut to_global = TSTransform::IDENTITY;
        let mut card_actions = vec![];
        let mut link_target = None;
        let ctx = ui.ctx().clone();
        let resp = egui::Scene::new()
            .zoom_range(ZOOM)
            .show(ui, &mut scene, |ui| {
                to_global = ui
                    .ctx()
                    .layer_transform_to_global(ui.layer_id())
                    .unwrap_or_default();
                let at = pointer.map(|p| to_global.inverse() * p);
                self.edges(ui);
                if let (Some((from, port)), Some(at)) = (self.linking, at) {
                    link_target = self
                        .graph
                        .nodes
                        .iter()
                        .rev()
                        .find(|n| n.id != from && rect(n).expand(14.0).contains(at))
                        .map(|n| n.id);
                    if let Some(n) = self.graph.node(from) {
                        let (a, b) = match port {
                            Port::Out => (pos2(n.x + NODE_W, n.y + NODE_H / 2.0), at),
                            Port::In => (at, pos2(n.x, n.y + NODE_H / 2.0)),
                        };
                        edge(ui, a, b, Stroke::new(2.0_f32, THEME.accent));
                    }
                }
                for n in &self.graph.nodes {
                    let Some(t) = v.lib.get(n.track) else {
                        continue;
                    };
                    let state = CardState {
                        preview: preview.filter(|p| p.id == t.id),
                        link_target: link_target == Some(n.id),
                        selected: self.selected == Some(n.id),
                        // Near enough to reach the corner button.
                        near: at.is_some_and(|p| rect(n).expand(14.0).contains(p)),
                        thumb: media.thumb(&ctx, t.id),
                        art: media.art(&ctx, t.id),
                    };
                    card(ui, &self.graph, n, t, v, state, &mut card_actions, out);
                }
            })
            .response;
        self.scene = scene;
        if wheel != 0.0
            && let Some(p) = pointer
        {
            // Zoom about the pointer, as the Scene does for Ctrl + wheel.
            let k = (to_global.scaling * (wheel * 0.0015).exp()).clamp(*ZOOM.start(), *ZOOM.end());
            let f = k / to_global.scaling;
            let t = TSTransform::from_translation(p.to_vec2())
                * TSTransform::from_scaling(f)
                * TSTransform::from_translation(-p.to_vec2())
                * to_global;
            self.scene = t.inverse() * canvas;
        }
        if let Some((from, port)) = self.linking
            && ui.input(|i| !i.pointer.any_down())
        {
            self.linking = None;
            if let Some(to) = link_target {
                match port {
                    Port::Out => self.edit(GraphOp::Link { from, to }),
                    Port::In => self.edit(GraphOp::Link { from: to, to: from }),
                };
            }
        }

        if self.graph.is_empty() {
            ui.painter().text(
                canvas.center(),
                Align2::CENTER_CENTER,
                "Add a track to start: Add track above, drag one here from the list,\n\
                 or right-click a track in the list > Add to Flow.",
                FontId::proportional(13.0),
                THEME.muted,
            );
        }
        // A card carries its track too (so it can be dropped on a deck); don't add it
        // again. Several tracks: a card each, stacked down from the drop.
        if let Some((ids, origin)) = drag::take_tracks(&resp)
            && origin != drag::Origin::Flow
            && self.moving.is_none()
            && let Some(p) = ui.input(|i| i.pointer.interact_pos())
        {
            let at = to_global.inverse() * p - vec2(NODE_W, NODE_H) / 2.0;
            for (i, id) in ids.into_iter().enumerate() {
                self.edit(GraphOp::Add {
                    track: id,
                    x: at.x,
                    y: at.y + i as f32 * SPACING.row,
                });
            }
        }
        if resp.clicked() || resp.drag_started() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.pop = None;
            self.selected = None;
        }
        if let Some(id) = self.selected
            && !ui.ctx().wants_keyboard_input()
            && ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace))
        {
            card_actions.push(CardAction::Edit(GraphOp::Remove(id)));
        }
        for a in card_actions {
            match a {
                CardAction::Select(id) => self.selected = Some(id),
                CardAction::Edit(op) => {
                    if let GraphOp::Remove(id) = op {
                        if self.selected == Some(id) {
                            self.selected = None;
                        }
                        if matches!(self.pop, Some(Pop::Suggest(p)) if p == id) {
                            self.pop = None;
                        }
                    }
                    self.edit(op);
                }
                CardAction::Plus(id) => {
                    self.pop = match self.pop {
                        Some(Pop::Suggest(p)) if p == id => None,
                        _ => Some(Pop::Suggest(id)),
                    }
                }
                CardAction::Drag(id, d) => {
                    if let Some(n) = self.graph.nodes.iter_mut().find(|n| n.id == id) {
                        if self.moving.is_none() {
                            self.moving = Some((id, pos(n)));
                        }
                        n.x += d.x;
                        n.y += d.y;
                    }
                }
                CardAction::DragEnd => {
                    let outside = ui
                        .input(|i| i.pointer.latest_pos())
                        .is_some_and(|p| !canvas.contains(p));
                    if let Some((moved, start)) = self.moving.take()
                        && outside
                    {
                        self.edit(GraphOp::Move {
                            node: moved,
                            x: start.x,
                            y: start.y,
                        });
                    }
                }
                CardAction::StartLink(id, port) => {
                    self.linking = Some((id, port));
                    self.pop = None;
                }
                CardAction::Bake(id) => self.bake(self.graph.path_through(id), out),
            }
        }

        self.minimap(ui, canvas, preview);

        match self.pop {
            Some(Pop::Suggest(id)) => {
                if let Some(n) = self.graph.node(id) {
                    // Beside the "+", or left of the card when there's no room.
                    let right = to_global * pos2(n.x + NODE_W + 26.0, n.y + NODE_H / 2.0);
                    let at = if right.x + POP_W + 20.0 <= canvas.right() {
                        (right, Align2::LEFT_CENTER)
                    } else {
                        let left = to_global * pos2(n.x - 14.0, n.y + NODE_H / 2.0);
                        (left, Align2::RIGHT_CENTER)
                    };
                    let (track, node) = (n.track, n.id);
                    self.suggest_pop(ui, canvas, at, node, track, v, media);
                }
            }
            Some(Pop::Search(at)) => self.search_pop(ui, canvas, at, v),
            None => {}
        }

        if had_preview
            && let Some(p) = preview
            && !self.graph.nodes.iter().any(|n| n.track == p.id)
        {
            out.push(Command::Preview(PreviewCmd::Stop(p.id)));
        }
    }

    /// Every card in miniature in the canvas's bottom-right corner, with the part in
    /// view outlined. Click or drag on it to look somewhere else.
    fn minimap(&mut self, ui: &mut Ui, canvas: Rect, preview: Option<PreviewState>) {
        let Some(bounds) = self.bounds() else {
            return;
        };
        // From the cards alone, so the map holds still while you drag the view around.
        let world = bounds.expand(NODE_W / 2.0);
        let max = vec2(220.0, 140.0);
        let scale = (max.x / world.width()).min(max.y / world.height());
        let size = world.size() * scale;
        let map = Rect::from_min_size(canvas.right_bottom() - size - vec2(12.0, 12.0), size);
        let to_map = |p: Pos2| map.min + (p - world.min) * scale;

        egui::Area::new(ui.id().with("flow-minimap"))
            .fixed_pos(map.min)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                let (_, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
                let p = ui.painter();
                p.rect(
                    map,
                    4,
                    THEME.minimap_bg,
                    Stroke::new(1.0_f32, THEME.card_line),
                    egui::StrokeKind::Outside,
                );
                for &(a, b) in &self.graph.edges {
                    if let (Some(a), Some(b)) = (self.graph.node(a), self.graph.node(b)) {
                        let a = to_map(pos2(a.x + NODE_W, a.y + NODE_H / 2.0));
                        let b = to_map(pos2(b.x, b.y + NODE_H / 2.0));
                        p.line_segment([a, b], Stroke::new(1.0_f32, THEME.muted));
                    }
                }
                for n in &self.graph.nodes {
                    let lit =
                        self.selected == Some(n.id) || preview.is_some_and(|pv| pv.id == n.track);
                    let r = Rect::from_min_max(to_map(pos(n)), to_map(rect(n).max));
                    p.rect_filled(
                        r,
                        1,
                        if lit {
                            THEME.accent
                        } else {
                            THEME.minimap_card
                        },
                    );
                }
                let view = Rect::from_min_max(to_map(self.scene.min), to_map(self.scene.max))
                    .intersect(map);
                p.rect_stroke(
                    view,
                    2,
                    Stroke::new(1.5_f32, THEME.marker),
                    egui::StrokeKind::Inside,
                );
                if (resp.clicked() || resp.dragged())
                    && let Some(at) = resp.interact_pointer_pos()
                {
                    let centre = world.min + (at - map.min) / scale;
                    self.scene = Rect::from_center_size(centre, self.scene.size());
                }
            });
    }

    fn toolbar(&mut self, ui: &mut Ui, v: &CoreView, out: &mut Vec<Command>) {
        ui.horizontal(|ui| {
            if self.flow.is_some() {
                ui.label(RichText::new(&self.name).strong());
            } else {
                ui.label(RichText::new("New Flow").color(THEME.muted))
                    .on_hover_text("Saved when you add a card");
            }
            let ts: Vec<&Track> = self
                .graph
                .nodes
                .iter()
                .filter_map(|n| v.lib.get(n.track))
                .collect();
            if !ts.is_empty() {
                let secs: f64 = ts.iter().filter_map(|t| t.duration).sum();
                let bpms: Vec<f64> = ts.iter().filter_map(|t| t.bpm).collect();
                let mut s = format!(
                    "{} track{} · {}",
                    ts.len(),
                    if ts.len() > 1 { "s" } else { "" },
                    util::mmss(secs)
                );
                if !bpms.is_empty() {
                    let lo = bpms.iter().copied().fold(f64::INFINITY, f64::min);
                    let hi = bpms.iter().copied().fold(0.0, f64::max);
                    s += &format!(" · {lo:.0}-{hi:.0} BPM");
                }
                ui.label(RichText::new(s).color(THEME.muted));
            }
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.add_enabled_ui(!self.graph.is_empty(), |ui| {
                    ui.menu_button("Reset", |ui| {
                        if ui.button("Remove every card").clicked() {
                            self.edit(GraphOp::Clear);
                            self.pop = None;
                            ui.close();
                        }
                    });
                });
                if ui
                    .button("Fit")
                    .on_hover_text("Fit everything in view")
                    .clicked()
                {
                    self.fit = true;
                }
                if ui
                    .add_enabled(!self.graph.is_empty(), egui::Button::new("Reflow"))
                    .on_hover_text("Tidy the layout: one column per step through the set")
                    .clicked()
                {
                    self.edit(GraphOp::Reflow);
                    self.fit = true;
                }
                let path = self.bake_path().filter(|_| self.flow.is_some());
                if ui
                    .add_enabled(path.is_some(), egui::Button::new("Save as playlist"))
                    .on_hover_text(
                        "Save a path through the Flow as a playlist: through the selected \
                         card, or the whole Flow when it's one chain",
                    )
                    .clicked()
                    && let Some(p) = path
                {
                    self.bake(p, out);
                }
                ui.toggle_value(&mut self.full, "Full screen")
                    .on_hover_text("Hide the decks while planning");
                let add = ui.button("Add track");
                if add.clicked() {
                    self.pop = match self.pop {
                        Some(Pop::Search(_)) => None,
                        _ => {
                            self.focus_search = true;
                            Some(Pop::Search(add.rect.right_bottom() + vec2(0.0, 4.0)))
                        }
                    };
                }
            });
        });
    }

    fn edges(&self, ui: &Ui) {
        let stroke = Stroke::new(2.0_f32, THEME.card_hover);
        for &(a, b) in &self.graph.edges {
            let (Some(p), Some(n)) = (self.graph.node(a), self.graph.node(b)) else {
                continue;
            };
            let a = pos2(p.x + NODE_W, p.y + NODE_H / 2.0);
            let b = pos2(n.x, n.y + NODE_H / 2.0);
            edge(ui, a, b, stroke);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn suggest_pop(
        &mut self,
        ui: &Ui,
        canvas: Rect,
        (anchor, pivot): (Pos2, Align2),
        node: NodeId,
        track: TrackId,
        v: &CoreView,
        media: &Media,
    ) {
        let Some(t) = v.lib.get(track) else { return };
        let stamp = PickStamp {
            track,
            direction: self.direction,
            lib: v.lib.rev(),
            history: v.history.rev(),
            exclude: self.graph.tracks(),
        };
        if self.picks_for.as_ref() != Some(&stamp) {
            // Tracks already in the Flow are left out; tracks played this session
            // still count as played.
            self.picks =
                v.history
                    .suggest(v.lib, &RefTrack::of(t), self.direction, &stamp.exclude, 9);
            self.picks_for = Some(stamp);
        }
        let mut picked = None;
        let mut close = false;
        egui::Area::new(ui.id().with("flow-suggest"))
            .order(egui::Order::Foreground)
            .pivot(pivot)
            .fixed_pos(anchor)
            .constrain_to(canvas)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(POP_W);
                    ui.horizontal(|ui| {
                        for (d, name) in [
                            (Direction::Drop, "Drop"),
                            (Direction::Hold, "Hold"),
                            (Direction::Build, "Build"),
                        ] {
                            if ui.selectable_label(self.direction == d, name).clicked() {
                                self.direction = d;
                            }
                        }
                        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                            if ui.button("Close").clicked() {
                                close = true;
                            }
                        });
                    });
                    ui.add_space(4.0);
                    if self.picks.is_empty() {
                        ui.label(
                            RichText::new(
                                "No suggestions: the library needs more tracks with BPM and key.",
                            )
                            .color(THEME.muted),
                        );
                    }
                    let w = (POP_W - 2.0 * 6.0) / 3.0;
                    for row in self.picks.chunks(3) {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            for s in row {
                                if let Some(c) = v.lib.get(s.id) {
                                    let art = media.art(ui.ctx(), c.id);
                                    if pick_tile(ui, w, c, art, s.total).clicked() {
                                        picked = Some(s.id);
                                    }
                                }
                            }
                        });
                    }
                });
            });
        if let Some(track) = picked {
            self.edit(GraphOp::AddChild {
                parent: node,
                track,
            });
            self.pop = None;
        }
        if close {
            self.pop = None;
        }
    }

    fn search_pop(&mut self, ui: &Ui, canvas: Rect, at: Pos2, v: &CoreView) {
        let mut picked = None;
        egui::Area::new(ui.id().with("flow-search"))
            .order(egui::Order::Foreground)
            .pivot(Align2::RIGHT_TOP)
            .fixed_pos(at)
            .constrain_to(canvas)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(440.0);
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.search)
                            .hint_text("Search the library   bpm:120-126  key:8A")
                            .desired_width(f32::INFINITY),
                    );
                    if std::mem::take(&mut self.focus_search) {
                        r.request_focus();
                    }
                    let q = match query::parse(&self.search) {
                        Ok(q) => q,
                        Err(e) => {
                            ui.label(RichText::new(e.to_string()).color(THEME.bad));
                            return;
                        }
                    };
                    egui::ScrollArea::vertical()
                        .max_height(canvas.height() * 0.6)
                        .show(ui, |ui| {
                            for id in v.lib.query(&q, v.collections, None).into_iter().take(50) {
                                let Some(t) = v.lib.get(id) else { continue };
                                let row = ui.horizontal(|ui| {
                                    ui.add(
                                        egui::Label::new(format!("{}  {}", t.title, t.artist))
                                            .truncate()
                                            .selectable(false),
                                    );
                                });
                                let row = ui
                                    .interact(row.response.rect, row.response.id, Sense::click())
                                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                                if row.hovered() {
                                    ui.painter().rect_stroke(
                                        row.rect.expand(1.0),
                                        3,
                                        Stroke::new(1.0_f32, THEME.accent),
                                        egui::StrokeKind::Outside,
                                    );
                                }
                                if row.clicked() {
                                    picked = Some(id);
                                }
                            }
                        });
                });
            });
        if let Some(id) = picked {
            self.edit(GraphOp::AddRoot(id));
            self.pop = None;
        }
    }
}

/// A curve from an out port at `a` to an in port at `b`.
fn edge(ui: &Ui, a: Pos2, b: Pos2, stroke: Stroke) {
    let dx = ((b.x - a.x).abs() / 2.0).max(40.0);
    ui.painter().add(CubicBezierShape::from_points_stroke(
        [a, a + vec2(dx, 0.0), b - vec2(dx, 0.0), b],
        false,
        Color32::TRANSPARENT,
        stroke,
    ));
}

/// Pan the view (keeping its zoom) so `want` is inside it.
fn reveal(view: Rect, want: Rect) -> Rect {
    let mut d = vec2(0.0, 0.0);
    if want.right() > view.right() {
        d.x = want.right() - view.right();
    }
    if want.left() < view.left() + d.x {
        d.x = want.left() - view.left();
    }
    if want.bottom() > view.bottom() {
        d.y = want.bottom() - view.bottom();
    }
    if want.top() < view.top() + d.y {
        d.y = want.top() - view.top();
    }
    view.translate(d)
}

#[allow(clippy::too_many_arguments)]
fn card(
    ui: &mut Ui,
    g: &FlowGraph,
    n: &FlowNode,
    t: &Track,
    v: &CoreView,
    state: CardState,
    acts: &mut Vec<CardAction>,
    out: &mut Vec<Command>,
) {
    let rect = rect(n);
    let resp = ui
        .interact(
            rect,
            ui.id().with(("flow-card", n.id)),
            Sense::click_and_drag(),
        )
        .on_hover_text(&t.path);
    if resp.clicked() || resp.drag_started() {
        acts.push(CardAction::Select(n.id));
    }
    if resp.dragged() {
        acts.push(CardAction::Drag(n.id, resp.drag_delta()));
    }
    if resp.drag_stopped() {
        acts.push(CardAction::DragEnd);
    }
    resp.dnd_set_drag_payload(DragPayload::track(t.id, drag::Origin::Flow));
    if resp.double_clicked() {
        out.push(LibraryCmd::Load(t.id).into());
    }
    resp.context_menu(|ui| {
        track_menu(
            ui,
            &[t.id],
            v,
            MenuAt {
                viewing: v.open_flow,
            },
            out,
        );
        ui.separator();
        if ui
            .add_enabled(
                v.open_flow.is_some(),
                egui::Button::new("Save this path as a playlist"),
            )
            .on_hover_text(
                "Back to a start and on to an end through this card, taking the top branch",
            )
            .clicked()
        {
            acts.push(CardAction::Bake(n.id));
            ui.close();
        }
        for p in g.parents(n.id) {
            let title = g
                .node(p)
                .and_then(|p| v.lib.get(p.track))
                .map_or("?", |t| t.title.as_str());
            if ui.button(format!("Unlink from \"{title}\"")).clicked() {
                acts.push(CardAction::Edit(GraphOp::Unlink { from: p, to: n.id }));
                ui.close();
            }
        }
        if ui.button("Remove from the Flow").clicked() {
            acts.push(CardAction::Edit(GraphOp::Remove(n.id)));
            ui.close();
        }
    });
    let border = if resp.dragged() || state.link_target || state.selected {
        THEME.accent
    } else if resp.hovered() {
        THEME.card_hover
    } else {
        THEME.card_line
    };
    let p = ui.painter();
    p.rect_filled(rect.translate(vec2(0.0, 4.0)), 8, THEME.card_shadow);
    p.rect(
        rect,
        8,
        THEME.card,
        Stroke::new(if state.selected { 2.0 } else { 1.0_f32 }, border),
        egui::StrokeKind::Inside,
    );
    seek_bar(ui, n, t, &state, out);

    let inner = rect.shrink2(vec2(8.0, 7.0));
    let mut ui = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(egui::Layout::top_down(Align::Min)),
    );
    ui.spacing_mut().item_spacing = vec2(8.0, 7.0);
    ui.horizontal(|ui| {
        art(ui, 34.0, t, state.art);
        let id_w = ui.available_width() - 28.0 - 8.0;
        ui.allocate_ui(vec2(id_w, 34.0), |ui| {
            ui.vertical(|ui| {
                ui.set_width(id_w);
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.add(
                    egui::Label::new(RichText::new(&t.title).strong())
                        .truncate()
                        .selectable(false),
                );
                ui.add(
                    egui::Label::new(RichText::new(&t.artist).size(11.0).color(THEME.muted))
                        .truncate()
                        .selectable(false),
                );
            });
        });
        let playing = state.preview.is_some_and(|p| p.playing);
        if play_button(ui, playing)
            .on_hover_text(if playing {
                "Pause the preview"
            } else {
                "Preview in the headphones"
            })
            .clicked()
        {
            out.push(Command::Preview(PreviewCmd::Toggle(t.id)));
        }
    });
    chips_row(&mut ui, t, |ui| {
        ui.label(
            RichText::new(t.duration.map(util::mmss).unwrap_or_default())
                .size(11.0)
                .color(THEME.muted),
        );
    });

    // A remove button on the top-right corner, while the pointer is near or it's selected.
    if state.selected || state.near {
        let c = rect.right_top();
        let x = ui
            .interact(
                Rect::from_center_size(c, vec2(20.0, 20.0)),
                ui.id().with(("flow-remove", n.id)),
                Sense::click(),
            )
            .on_hover_text("Remove from the Flow (Delete)");
        let col = if x.hovered() { THEME.warn } else { THEME.muted };
        ui.painter()
            .circle(c, 9.0, THEME.bg, Stroke::new(1.5_f32, col));
        for d in [vec2(3.5, 3.5), vec2(3.5, -3.5)] {
            ui.painter()
                .line_segment([c - d, c + d], Stroke::new(1.5_f32, col));
        }
        if x.clicked() {
            acts.push(CardAction::Edit(GraphOp::Remove(n.id)));
        }
    }

    // The in port on the left: drag from it to another card to follow that card too.
    let c_in = pos2(rect.left(), rect.center().y);
    let port_in = ui
        .interact(
            Rect::from_center_size(c_in, vec2(18.0, 18.0)),
            ui.id().with(("flow-in", n.id)),
            Sense::drag(),
        )
        .on_hover_text("Drag to a card that comes before this one");
    if port_in.drag_started() {
        acts.push(CardAction::StartLink(n.id, Port::In));
    }
    let col = if port_in.hovered() || port_in.dragged() {
        THEME.accent
    } else {
        THEME.muted
    };
    ui.painter()
        .circle(c_in, 5.0, THEME.bg, Stroke::new(2.0_f32, col));

    // "+" on the right edge: click for suggestions, drag to link to the next card.
    let c = pos2(rect.right(), rect.center().y);
    let plus = ui
        .interact(
            Rect::from_center_size(c, vec2(26.0, 26.0)),
            ui.id().with(("flow-plus", n.id)),
            Sense::click_and_drag(),
        )
        .on_hover_text("Click: suggest what to play next\nDrag: link to the next card");
    if plus.drag_started() {
        acts.push(CardAction::StartLink(n.id, Port::Out));
    }
    let col = if plus.hovered() {
        THEME.accent
    } else {
        THEME.muted
    };
    ui.painter()
        .circle(c, 13.0, THEME.bg, Stroke::new(2.0_f32, col));
    ui.painter().text(
        c,
        Align2::CENTER_CENTER,
        "+",
        FontId::proportional(17.0),
        if plus.hovered() {
            THEME.accent
        } else {
            THEME.marker
        },
    );
    if plus.clicked() {
        acts.push(CardAction::Plus(n.id));
    }
}

/// The track's waveform along the card's bottom edge: click or drag to preview from
/// there. Shows how far the preview has got.
fn seek_bar(ui: &mut Ui, n: &FlowNode, t: &Track, state: &CardState, out: &mut Vec<Command>) {
    let card = rect(n);
    let bar = Rect::from_min_max(
        pos2(card.left() + 8.0, card.bottom() - 6.0 - SEEK_H),
        pos2(card.right() - 8.0, card.bottom() - 6.0),
    );
    let resp = ui
        .interact(
            bar,
            ui.id().with(("flow-seek", n.id)),
            Sense::click_and_drag(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let frac = |x: f32| ((x - bar.left()) / bar.width()).clamp(0.0, 1.0);
    if (resp.clicked() || resp.dragged())
        && let Some(p) = resp.interact_pointer_pos()
    {
        out.push(Command::Preview(PreviewCmd::Seek(t.id, frac(p.x))));
    }
    if let (Some(p), Some(d)) = (resp.hover_pos(), t.duration) {
        resp.on_hover_text_at_pointer(util::mmss(d * frac(p.x) as f64));
    }

    let p = ui.painter();
    p.rect_filled(bar, 3, THEME.chips_bg);
    let tint = if state.preview.is_some() {
        THEME.marker
    } else {
        THEME.thumb_idle
    };
    if let Some(tex) = state.thumb {
        p.image(
            tex,
            bar,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            tint,
        );
    }
    if let Some(s) = state.preview {
        // Dim what's been played; a line at the playhead.
        let x = bar.left() + bar.width() * s.at.clamp(0.0, 1.0);
        p.rect_filled(
            Rect::from_min_max(bar.min, pos2(x, bar.bottom())),
            3,
            THEME.thumb_played,
        );
        p.line_segment(
            [pos2(x, bar.top() - 2.0), pos2(x, bar.bottom() + 2.0)],
            Stroke::new(2.0_f32, THEME.accent),
        );
    }
}

/// BPM, key and energy chips on a dark strip, with `right` added at its right end.
fn chips_row(ui: &mut Ui, t: &Track, right: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(THEME.chips_bg)
        .corner_radius(5)
        .inner_margin(egui::Margin::symmetric(5, 3))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                chip(
                    ui,
                    &t.bpm.map_or("-".into(), |b| format!("{:.1}", b)),
                    THEME.panel_2,
                    THEME.text,
                );
                match t.key {
                    Some(k) => key_chip(ui, k),
                    None => {
                        chip(ui, "?", THEME.line, THEME.text);
                    }
                }
                let e = t
                    .energy_level()
                    .map_or("-".into(), |e| format!("{:.0}", e.round()));
                chip(ui, &e, THEME.energy_chip, THEME.ink).on_hover_text(
                    "Energy (stand-in: loudness and brightness, ranked across your library)",
                );
                ui.with_layout(egui::Layout::right_to_left(Align::Center), right);
            });
        });
}

fn chip(ui: &mut Ui, text: &str, fill: Color32, colour: Color32) -> egui::Response {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), FontId::proportional(11.0), colour);
    let (rect, resp) = ui.allocate_exact_size(galley.size() + vec2(10.0, 2.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(4), fill);
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, colour);
    resp
}

/// The track's cover art, or a gradient with the artist's initials until it has one.
fn art(ui: &mut Ui, size: f32, t: &Track, tex: Option<egui::TextureId>) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    if let Some(tex) = tex {
        egui::Image::new((tex, rect.size()))
            .corner_radius(3)
            .paint_at(ui, rect);
        return;
    }
    let h = format!("{}{}", t.artist, t.title)
        .chars()
        .fold(0u32, |h, c| (h * 31 + c as u32) % 360) as f32;
    let initials: String = if t.artist.is_empty() {
        &t.title
    } else {
        &t.artist
    }
    .split_whitespace()
    .take(2)
    .filter_map(|w| w.chars().next())
    .collect::<String>()
    .to_uppercase();
    // A diagonal two-colour gradient.
    let (a, b) = (util::hsl(h, 0.55, 0.42), util::hsl(h + 60.0, 0.6, 0.28));
    let mut mesh = egui::Mesh::default();
    let mid = util::hsl(h + 30.0, 0.57, 0.35);
    mesh.colored_vertex(rect.left_top(), a);
    mesh.colored_vertex(rect.right_top(), mid);
    mesh.colored_vertex(rect.right_bottom(), b);
    mesh.colored_vertex(rect.left_bottom(), mid);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    ui.painter().add(mesh);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        initials,
        FontId::proportional(size * 0.35),
        THEME.marker,
    );
}

/// A white round button with a painted play triangle, or a stop square while
/// `playing` (the font has no ▶ or ■).
fn play_button(ui: &mut Ui, playing: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::click());
    let c = rect.center();
    let fill = if resp.hovered() {
        THEME.accent
    } else {
        THEME.marker
    };
    ui.painter().circle_filled(c, 14.0, fill);
    if playing {
        for dx in [-3.0, 3.0] {
            ui.painter().rect_filled(
                Rect::from_center_size(c + vec2(dx, 0.0), vec2(3.5, 11.0)),
                1,
                THEME.ink,
            );
        }
        return resp;
    }
    ui.painter().add(egui::Shape::convex_polygon(
        vec![
            c + vec2(-3.5, -6.0),
            c + vec2(6.5, 0.0),
            c + vec2(-3.5, 6.0),
        ],
        THEME.ink,
        Stroke::NONE,
    ));
    resp
}

/// A suggestion in the "+" grid: art, title, artist, chips and the match score.
fn pick_tile(
    ui: &mut Ui,
    w: f32,
    t: &Track,
    tex: Option<egui::TextureId>,
    score: f64,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 70.0), Sense::click());
    let resp = resp
        .on_hover_text(&t.path)
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    ui.painter().rect(
        rect,
        7,
        THEME.card,
        Stroke::new(
            1.0_f32,
            if resp.hovered() {
                THEME.accent
            } else {
                THEME.card_line
            },
        ),
        egui::StrokeKind::Inside,
    );
    let mut ui = ui.new_child(
        UiBuilder::new()
            .max_rect(rect.shrink(5.0))
            .layout(egui::Layout::top_down(Align::Min)),
    );
    ui.spacing_mut().item_spacing = vec2(7.0, 4.0);
    ui.horizontal(|ui| {
        art(ui, 28.0, t, tex);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.add(
                egui::Label::new(RichText::new(&t.title).strong().size(12.0))
                    .truncate()
                    .selectable(false),
            );
            ui.add(
                egui::Label::new(RichText::new(&t.artist).size(11.0).color(THEME.muted))
                    .truncate()
                    .selectable(false),
            );
        });
    });
    chips_row(&mut ui, t, |ui| {
        ui.label(
            RichText::new(format!("{:.0}", score * 100.0))
                .size(11.0)
                .strong()
                .color(THEME.muted),
        )
        .on_hover_text("Match");
    });
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changes_are_saved_once_and_not_mid_drag() {
        let mut f = FlowUi::from_saved(Saved::default());
        assert!(f.changed_graph().is_none());
        f.add_tracks(&[10, 11]);
        assert_eq!(f.changed_graph().unwrap().nodes.len(), 2);
        assert!(f.changed_graph().is_none(), "counted as saved");
        let a = f.graph.nodes[0].id;
        let b = f.graph.nodes[1].id;
        f.edit(GraphOp::Link { from: a, to: b });
        f.moving = Some((a, pos2(0.0, 0.0)));
        assert!(f.changed_graph().is_none(), "not while dragging");
        f.moving = None;
        assert_eq!(f.changed_graph().unwrap().edges, vec![(a, b)]);
        f.edit(GraphOp::Link { from: b, to: a });
        assert!(f.changed_graph().is_none(), "a loop is refused");
    }

    #[test]
    fn the_path_to_bake() {
        let mut f = FlowUi::from_saved(Saved::default());
        f.add_tracks(&[1]);
        let root = f.graph.nodes[0].id;
        f.edit(GraphOp::AddChild {
            parent: root,
            track: 2,
        });
        assert_eq!(
            f.bake_path().unwrap().len(),
            2,
            "one chain: the whole of it"
        );
        f.edit(GraphOp::AddChild {
            parent: root,
            track: 3,
        });
        assert!(f.bake_path().is_none(), "branching: pick a card");
        let last = f.graph.nodes[2].id;
        f.selected = Some(last);
        assert_eq!(f.bake_path(), Some(vec![root, last]));
        assert_eq!(f.selected_track(), Some(3));
    }

    #[test]
    fn reveal_pans_without_zooming() {
        let view = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 100.0));
        let r = reveal(
            view,
            Rect::from_min_size(pos2(150.0, 10.0), vec2(20.0, 20.0)),
        );
        assert_eq!(r.size(), view.size());
        assert_eq!(r.right(), 170.0);
        assert_eq!(
            reveal(view, Rect::from_min_size(pos2(10.0, 10.0), vec2(5.0, 5.0))),
            view
        );
    }
}
