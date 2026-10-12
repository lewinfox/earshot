//! Flow: a set-building canvas. Add a track or two, then grow the set from
//! suggestions. Each card has a "+" on its right that opens nine suggested next tracks
//! (with Drop / Hold / Build); picking one adds it as a new card joined to the first.
//! Drag cards to move them, drag the background to pan, wheel to zoom. Link two cards
//! by dragging from one's "+" (out) to another's left port (in), or the other way.
//! Saved with the window's other settings.

use std::collections::{HashMap, HashSet};

use egui::emath::TSTransform;
use egui::epaint::CubicBezierShape;
use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Pos2, Rect, RichText, Sense, Stroke, Ui,
    UiBuilder, pos2, vec2,
};
use serde::{Deserialize, Serialize};

use super::deck::key_chip;
use super::library_ui::{LibAction, LibraryUi, PreviewState, Track, want_art};
use super::util::{self, Query};
use super::wave;
use crate::core::flows::{self, Card};
use crate::core::suggest::{self, Direction, Reference, Scored};

const NODE_W: f32 = 236.0;
const NODE_H: f32 = 104.0;
/// The waveform seek bar along a card's bottom edge.
const SEEK_H: f32 = 20.0;
const GAP_X: f32 = 90.0;
const GAP_Y: f32 = 22.0;
/// Width of the "+" suggestions popup.
const POP_W: f32 = 600.0;
const CARD: Color32 = Color32::from_rgb(0x24, 0x26, 0x2d);
const CARD_LINE: Color32 = Color32::from_rgb(0x35, 0x38, 0x45);
const CHIPS_BG: Color32 = Color32::from_rgb(0x1a, 0x1b, 0x21);
const ENERGY_CHIP: Color32 = Color32::from_rgb(0x6f, 0xcf, 0x8e);

#[derive(Serialize, Deserialize, Clone)]
struct Node {
    id: u64,
    track_id: i64,
    parent: Option<u64>,
    pos: Pos2,
}

impl Node {
    fn rect(&self) -> Rect {
        Rect::from_min_size(self.pos, vec2(NODE_W, NODE_H))
    }
}

/// What is kept between runs. The cards themselves are in `earshot.db`.
#[derive(Serialize, Deserialize, Default)]
pub struct Saved {
    /// Cards from before Flows were saved in the database; moved there on start.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    nodes: Vec<Node>,
    /// The saved Flow on the canvas.
    #[serde(default)]
    flow: Option<i64>,
    scene: Option<Rect>,
    open: bool,
}

enum Pop {
    /// Nine suggestions after this card.
    Suggest(u64),
    /// Search the library for a starting track, below this point.
    Search(Pos2),
}

pub struct FlowUi {
    /// The saved Flow on the canvas (`None` until the first card is added).
    pub flow: Option<i64>,
    pub name: String,
    nodes: Vec<Node>,
    /// The cards as last saved, to tell when they need saving again.
    stored: Vec<Card>,
    /// The part of the canvas in view (egui's `Scene` keeps it as a rectangle).
    scene: Rect,
    next_id: u64,
    /// The Flow tab is showing (instead of the track list).
    pub open: bool,
    /// Hide the decks and side panes while planning.
    pub full: bool,
    direction: Direction,
    pop: Option<Pop>,
    search: String,
    focus_search: bool,
    /// Pan to show this card on the next frame.
    reveal: Option<u64>,
    fit: bool,
    /// Suggestions for the open "+" popup, and what they were computed for.
    picks: Vec<Scored>,
    picks_for: String,
    /// A link being dragged: from this card, starting at its out ("+") or in port.
    linking: Option<(u64, Port)>,
    /// The card clicked on last; Delete or Backspace removes it.
    selected: Option<u64>,
    /// Waveform thumbnails for the seek bars, by track.
    thumbs: HashMap<i64, egui::TextureHandle>,
    /// A card being dragged, and where it started. Dropped outside the canvas (e.g. on
    /// a deck, which loads it) it goes back there.
    moving: Option<(u64, Pos2)>,
}

#[derive(Clone, Copy, PartialEq)]
enum Port {
    In,
    Out,
}

enum CardAction {
    Remove(u64),
    Plus(u64),
    Move(u64, egui::Vec2),
    MoveEnd,
    StartLink(u64, Port),
    Unlink(u64),
    Select(u64),
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
}

const ZOOM: std::ops::RangeInclusive<f32> = 0.25..=1.5;

impl FlowUi {
    pub fn from_saved(s: Saved) -> FlowUi {
        FlowUi {
            flow: s.flow,
            name: String::new(),
            next_id: s.nodes.iter().map(|n| n.id + 1).max().unwrap_or(1),
            nodes: s.nodes,
            stored: vec![],
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
            picks_for: String::new(),
            linking: None,
            selected: None,
            thumbs: HashMap::new(),
            moving: None,
        }
    }

    pub fn saved(&self) -> Saved {
        Saved {
            nodes: vec![],
            flow: self.flow,
            scene: Some(self.scene),
            open: self.open,
        }
    }

    /// The cards on the canvas, for saving.
    pub fn cards(&self) -> Vec<Card> {
        self.nodes
            .iter()
            .map(|n| Card {
                id: n.id,
                track_id: n.track_id,
                parent: n.parent,
                x: n.pos.x,
                y: n.pos.y,
            })
            .collect()
    }

    /// Show a saved Flow (or an empty canvas with `None`).
    pub fn load(&mut self, flow: Option<(i64, String, Vec<Card>)>) {
        let (id, name, cards) = match flow {
            Some((id, name, cards)) => (Some(id), name, cards),
            None => (None, String::new(), vec![]),
        };
        self.flow = id;
        self.name = name;
        self.nodes = cards
            .iter()
            .map(|c| Node {
                id: c.id,
                track_id: c.track_id,
                parent: c.parent,
                pos: pos2(c.x, c.y),
            })
            .collect();
        self.next_id = self.nodes.iter().map(|n| n.id + 1).max().unwrap_or(1);
        self.stored = cards;
        self.pop = None;
        self.linking = None;
        self.moving = None;
        self.fit = !self.nodes.is_empty();
    }

    /// The cards, if they changed since they were last saved and nothing is being
    /// dragged. Counts them as saved.
    pub fn changed_cards(&mut self) -> Option<Vec<Card>> {
        if self.moving.is_some() || self.linking.is_some() {
            return None;
        }
        let cards = self.cards();
        if cards == self.stored {
            return None;
        }
        self.stored = cards.clone();
        Some(cards)
    }

    // ---- model --------------------------------------------------------------------------

    fn add(&mut self, track_id: i64, parent: Option<u64>, pos: Pos2) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.nodes.push(Node {
            id,
            track_id,
            parent,
            pos,
        });
        self.reveal = Some(id);
        id
    }

    /// Add a track as a new starting card, below the existing ones.
    pub fn add_root(&mut self, track_id: i64) {
        let bottom = self
            .nodes
            .iter()
            .map(|n| n.pos.y)
            .fold(-GAP_Y - NODE_H, f32::max);
        let left = self.nodes.iter().map(|n| n.pos.x).fold(0.0, f32::min);
        self.add(track_id, None, pos2(left, bottom + NODE_H + GAP_Y * 3.0));
    }

    fn add_child(&mut self, parent: u64, track_id: i64) {
        let Some(p) = self.nodes.iter().find(|n| n.id == parent) else {
            return;
        };
        let p = p.pos;
        let y = self
            .nodes
            .iter()
            .filter(|n| n.parent == Some(parent))
            .map(|n| n.pos.y + NODE_H + GAP_Y)
            .fold(f32::NEG_INFINITY, f32::max);
        let y = if y.is_finite() { y } else { p.y };
        self.add(track_id, Some(parent), pos2(p.x + NODE_W + GAP_X, y));
    }

    /// Remove a card; its children join its parent.
    fn remove(&mut self, id: u64) {
        let Some(i) = self.nodes.iter().position(|n| n.id == id) else {
            return;
        };
        let gone = self.nodes.remove(i);
        for n in &mut self.nodes {
            if n.parent == Some(id) {
                n.parent = gone.parent;
            }
        }
    }

    /// Open the "+" suggestions on the first card (for the screenshot test).
    #[cfg(test)]
    pub fn open_first_suggestions(&mut self) {
        self.pop = self.nodes.first().map(|n| Pop::Suggest(n.id));
    }

    #[cfg(test)]
    pub fn fit(&mut self) {
        self.fit = true;
    }

    /// Make `child` follow `parent`. Refused if it would make a loop.
    fn link(&mut self, parent: u64, child: u64) -> bool {
        // The link replaces the child's current one.
        let edges: Vec<(u64, u64)> = self
            .nodes
            .iter()
            .filter(|n| n.id != child)
            .filter_map(|n| n.parent.map(|p| (p, n.id)))
            .collect();
        if flows::creates_cycle(&edges, parent, child) {
            return false;
        }
        match self.nodes.iter_mut().find(|n| n.id == child) {
            Some(n) => {
                n.parent = Some(parent);
                true
            }
            None => false,
        }
    }

    fn in_flow(&self) -> HashSet<i64> {
        self.nodes.iter().map(|n| n.track_id).collect()
    }

    /// Lay the cards out tidily: a card's column is how many cards come before it, so
    /// the width shows the set's length. A chain stays on one row; a branch's other
    /// paths go on rows below. Separate chains are stacked, keeping their order.
    fn reflow(&mut self) {
        let ids: HashSet<u64> = self.nodes.iter().map(|n| n.id).collect();
        let parent = |n: &Node| n.parent.filter(|p| ids.contains(p));
        // Keep the current top-to-bottom order of starting cards and branches.
        let mut by_y: Vec<&Node> = self.nodes.iter().collect();
        by_y.sort_by(|a, b| a.pos.y.total_cmp(&b.pos.y));
        let mut kids: HashMap<u64, Vec<u64>> = HashMap::new();
        let mut roots = vec![];
        for n in &by_y {
            match parent(n) {
                Some(p) => kids.entry(p).or_default().push(n.id),
                None => roots.push(n.id),
            }
        }

        // (column, row) per card, depth first.
        let mut place: HashMap<u64, (usize, f32)> = HashMap::new();
        let mut row = 0.0;
        fn walk(
            id: u64,
            col: usize,
            row: &mut f32,
            kids: &HashMap<u64, Vec<u64>>,
            place: &mut HashMap<u64, (usize, f32)>,
        ) {
            if place.contains_key(&id) {
                return; // can't happen (links refuse loops), but never spin
            }
            place.insert(id, (col, *row));
            let children = kids.get(&id).map(Vec::as_slice).unwrap_or(&[]);
            for (i, &c) in children.iter().enumerate() {
                if i > 0 {
                    *row += 1.0;
                }
                walk(c, col + 1, row, kids, place);
            }
        }
        for (i, &r) in roots.iter().enumerate() {
            if i > 0 {
                row += 1.5; // a gap between separate chains
            }
            walk(r, 0, &mut row, &kids, &mut place);
        }
        for n in &mut self.nodes {
            if let Some(&(col, row)) = place.get(&n.id) {
                n.pos = pos2(col as f32 * (NODE_W + GAP_X), row * (NODE_H + GAP_Y));
            }
        }
    }

    fn bounds(&self) -> Option<Rect> {
        self.nodes.iter().map(Node::rect).reduce(|a, b| a.union(b))
    }

    // ---- drawing ------------------------------------------------------------------------

    /// `preview`: the track playing in the headphones, and how far through it is.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        lib: &LibraryUi,
        preview: Option<PreviewState>,
        actions: &mut Vec<LibAction>,
    ) {
        // To stop the preview if its card goes (removed, or Reset).
        let had_preview = preview.is_some_and(|p| self.nodes.iter().any(|n| n.track_id == p.id));
        self.toolbar(ui, lib);
        for n in &self.nodes {
            if let Some(p) = lib.previews.get(&n.track_id) {
                self.thumbs
                    .entry(n.track_id)
                    .or_insert_with(|| wave::thumbnail(ui.ctx(), n.track_id, p));
            }
        }

        if let Some(id) = self.reveal.take()
            && let Some(n) = self.nodes.iter().find(|n| n.id == id)
        {
            self.scene = reveal(self.scene, n.rect().expand(20.0));
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
        ui.painter().rect_filled(canvas, 0.0, util::BG);

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
                        .nodes
                        .iter()
                        .rev()
                        .find(|n| n.id != from && n.rect().expand(14.0).contains(at))
                        .map(|n| n.id);
                    if let Some(n) = self.nodes.iter().find(|n| n.id == from) {
                        let (a, b) = match port {
                            Port::Out => (pos2(n.pos.x + NODE_W, n.pos.y + NODE_H / 2.0), at),
                            Port::In => (at, pos2(n.pos.x, n.pos.y + NODE_H / 2.0)),
                        };
                        edge(ui, a, b, Stroke::new(2.0_f32, util::ACCENT));
                    }
                }
                for n in &self.nodes {
                    if let Some(t) = lib.track(n.track_id) {
                        let state = CardState {
                            preview: preview.filter(|p| p.id == t.lib.id),
                            link_target: link_target == Some(n.id),
                            selected: self.selected == Some(n.id),
                            // Near enough to reach the corner button.
                            near: at.is_some_and(|p| n.rect().expand(14.0).contains(p)),
                            thumb: self.thumbs.get(&t.lib.id).map(|t| t.id()),
                        };
                        card(ui, n, t, lib, state, &mut card_actions, actions);
                    }
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
                    Port::Out => self.link(from, to),
                    Port::In => self.link(to, from),
                };
            }
        }

        if self.nodes.is_empty() {
            ui.painter().text(
                canvas.center(),
                Align2::CENTER_CENTER,
                "Add a track to start: Add track above, drag one here from the list,\n\
                 or right-click a track in the list > Add to Flow.",
                FontId::proportional(13.0),
                util::MUTED,
            );
        }
        // A card carries its track too (so it can be dropped on a deck); don't add it again.
        if let Some(id) = resp.dnd_release_payload::<i64>()
            && self.moving.is_none()
            && let Some(p) = ui.input(|i| i.pointer.interact_pos())
        {
            let at = to_global.inverse() * p;
            self.add(*id, None, at - vec2(NODE_W, NODE_H) / 2.0);
        }
        if resp.clicked() || resp.drag_started() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.pop = None;
            self.selected = None;
        }
        if let Some(id) = self.selected
            && !ui.ctx().wants_keyboard_input()
            && ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace))
        {
            card_actions.push(CardAction::Remove(id));
        }
        for a in card_actions {
            match a {
                CardAction::Select(id) => self.selected = Some(id),
                CardAction::Remove(id) => {
                    self.remove(id);
                    if self.selected == Some(id) {
                        self.selected = None;
                    }
                    if matches!(self.pop, Some(Pop::Suggest(p)) if p == id) {
                        self.pop = None;
                    }
                }
                CardAction::Plus(id) => {
                    self.pop = match self.pop {
                        Some(Pop::Suggest(p)) if p == id => None,
                        _ => Some(Pop::Suggest(id)),
                    }
                }
                CardAction::Move(id, d) => {
                    if let Some(n) = self.nodes.iter_mut().find(|n| n.id == id) {
                        if self.moving.is_none() {
                            self.moving = Some((id, n.pos));
                        }
                        n.pos += d;
                    }
                }
                CardAction::MoveEnd => {
                    let outside = ui
                        .input(|i| i.pointer.latest_pos())
                        .is_some_and(|p| !canvas.contains(p));
                    if let Some((moved, start)) = self.moving.take()
                        && outside
                        && let Some(n) = self.nodes.iter_mut().find(|n| n.id == moved)
                    {
                        n.pos = start;
                    }
                }
                CardAction::StartLink(id, port) => {
                    self.linking = Some((id, port));
                    self.pop = None;
                }
                CardAction::Unlink(id) => {
                    if let Some(n) = self.nodes.iter_mut().find(|n| n.id == id) {
                        n.parent = None;
                    }
                }
            }
        }

        self.minimap(ui, canvas, preview);

        match self.pop {
            Some(Pop::Suggest(id)) => {
                if let Some(n) = self.nodes.iter().find(|n| n.id == id) {
                    // Beside the "+", or left of the card when there's no room.
                    let right = to_global * pos2(n.pos.x + NODE_W + 26.0, n.pos.y + NODE_H / 2.0);
                    let at = if right.x + POP_W + 20.0 <= canvas.right() {
                        (right, Align2::LEFT_CENTER)
                    } else {
                        let left = to_global * pos2(n.pos.x - 14.0, n.pos.y + NODE_H / 2.0);
                        (left, Align2::RIGHT_CENTER)
                    };
                    let (track_id, node) = (n.track_id, n.id);
                    self.suggest_pop(ui, canvas, at, node, track_id, lib);
                }
            }
            Some(Pop::Search(at)) => self.search_pop(ui, canvas, at, lib),
            None => {}
        }

        if had_preview
            && let Some(p) = preview
            && !self.nodes.iter().any(|n| n.track_id == p.id)
        {
            actions.push(LibAction::PreviewStop(p.id));
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
                    Color32::from_black_alpha(200),
                    Stroke::new(1.0_f32, CARD_LINE),
                    egui::StrokeKind::Outside,
                );
                for n in &self.nodes {
                    if let Some(parent) = self.nodes.iter().find(|m| Some(m.id) == n.parent) {
                        let a = to_map(pos2(parent.pos.x + NODE_W, parent.pos.y + NODE_H / 2.0));
                        let b = to_map(pos2(n.pos.x, n.pos.y + NODE_H / 2.0));
                        p.line_segment([a, b], Stroke::new(1.0_f32, util::MUTED));
                    }
                }
                for n in &self.nodes {
                    let lit = self.selected == Some(n.id)
                        || preview.is_some_and(|pv| pv.id == n.track_id);
                    let r = Rect::from_min_max(to_map(n.pos), to_map(n.rect().max));
                    p.rect_filled(
                        r,
                        1,
                        if lit {
                            util::ACCENT
                        } else {
                            Color32::from_gray(110)
                        },
                    );
                }
                let view = Rect::from_min_max(to_map(self.scene.min), to_map(self.scene.max))
                    .intersect(map);
                p.rect_stroke(
                    view,
                    2,
                    Stroke::new(1.5_f32, Color32::WHITE),
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

    fn toolbar(&mut self, ui: &mut Ui, lib: &LibraryUi) {
        ui.horizontal(|ui| {
            if self.flow.is_some() {
                ui.label(RichText::new(&self.name).strong());
            } else {
                ui.label(RichText::new("New Flow").color(util::MUTED))
                    .on_hover_text("Saved when you add a card");
            }
            let ts: Vec<&Track> = self
                .nodes
                .iter()
                .filter_map(|n| lib.track(n.track_id))
                .collect();
            if !ts.is_empty() {
                let secs: f64 = ts.iter().filter_map(|t| t.lib.duration).sum();
                let bpms: Vec<f64> = ts.iter().filter_map(|t| t.bpm()).collect();
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
                ui.label(RichText::new(s).color(util::MUTED));
            }
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.add_enabled_ui(!self.nodes.is_empty(), |ui| {
                    ui.menu_button("Reset", |ui| {
                        if ui.button("Remove every card").clicked() {
                            self.nodes.clear();
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
                    .add_enabled(!self.nodes.is_empty(), egui::Button::new("Reflow"))
                    .on_hover_text("Tidy the layout: one column per step through the set")
                    .clicked()
                {
                    self.reflow();
                    self.fit = true;
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
        let stroke = Stroke::new(2.0_f32, Color32::from_rgb(0x5a, 0x5f, 0x73));
        for n in &self.nodes {
            let Some(p) = n.parent.and_then(|p| self.nodes.iter().find(|m| m.id == p)) else {
                continue;
            };
            let a = pos2(p.pos.x + NODE_W, p.pos.y + NODE_H / 2.0);
            let b = pos2(n.pos.x, n.pos.y + NODE_H / 2.0);
            edge(ui, a, b, stroke);
        }
    }

    fn suggest_pop(
        &mut self,
        ui: &Ui,
        canvas: Rect,
        (anchor, pivot): (Pos2, Align2),
        node: u64,
        track_id: i64,
        lib: &LibraryUi,
    ) {
        let Some(t) = lib.track(track_id) else { return };
        let exclude = self.in_flow();
        let stamp = format!(
            "{track_id}|{:?}|{}|{}|{}",
            self.direction,
            exclude.len(),
            lib.played.len(),
            lib.version
        );
        if stamp != self.picks_for {
            let reference = Reference {
                id: track_id,
                bpm: t.bpm(),
                key: t.key,
                energy: t.energy,
                direction: self.direction,
                played: &lib.played,
                exclude: &exclude,
            };
            self.picks = suggest::suggest(&reference, &lib.candidates(), 9);
            self.picks_for = stamp;
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
                            .color(util::MUTED),
                        );
                    }
                    let w = (POP_W - 2.0 * 6.0) / 3.0;
                    for row in self.picks.chunks(3) {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            for s in row {
                                if let Some(c) = lib.track(s.id)
                                    && pick_tile(ui, w, c, lib, s.total).clicked()
                                {
                                    picked = Some(s.id);
                                }
                            }
                        });
                    }
                });
            });
        if let Some(id) = picked {
            self.add_child(node, id);
            self.pop = None;
        }
        if close {
            self.pop = None;
        }
    }

    fn search_pop(&mut self, ui: &Ui, canvas: Rect, at: Pos2, lib: &LibraryUi) {
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
                    let q = Query::parse(&self.search);
                    egui::ScrollArea::vertical()
                        .max_height(canvas.height() * 0.6)
                        .show(ui, |ui| {
                            for t in lib
                                .tracks
                                .iter()
                                .filter(|t| q.matches(&t.lib.title, &t.lib.artist, t.bpm(), t.key))
                                .take(50)
                            {
                                let row = ui.horizontal(|ui| {
                                    ui.add(
                                        egui::Label::new(format!(
                                            "{}  {}",
                                            t.lib.title, t.lib.artist
                                        ))
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
                                        Stroke::new(1.0_f32, util::ACCENT),
                                        egui::StrokeKind::Outside,
                                    );
                                }
                                if row.clicked() {
                                    picked = Some(t.lib.id);
                                }
                            }
                        });
                });
            });
        if let Some(id) = picked {
            self.add_root(id);
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

fn card(
    ui: &mut Ui,
    n: &Node,
    t: &Track,
    lib: &LibraryUi,
    state: CardState,
    out: &mut Vec<CardAction>,
    actions: &mut Vec<LibAction>,
) {
    let rect = n.rect();
    let resp = ui
        .interact(
            rect,
            ui.id().with(("flow-card", n.id)),
            Sense::click_and_drag(),
        )
        .on_hover_text(&t.lib.path);
    if resp.clicked() || resp.drag_started() {
        out.push(CardAction::Select(n.id));
    }
    if resp.dragged() {
        out.push(CardAction::Move(n.id, resp.drag_delta()));
    }
    if resp.drag_stopped() {
        out.push(CardAction::MoveEnd);
    }
    resp.dnd_set_drag_payload(t.lib.id);
    if resp.double_clicked() {
        actions.push(LibAction::Load {
            deck: None,
            track_id: t.lib.id,
        });
    }
    resp.context_menu(|ui| {
        for (deck, label) in [(0, "Load to deck A"), (1, "Load to deck B")] {
            if ui.button(label).clicked() {
                actions.push(LibAction::Load {
                    deck: Some(deck),
                    track_id: t.lib.id,
                });
                ui.close();
            }
        }
        ui.separator();
        if n.parent.is_some() && ui.button("Unlink from previous card").clicked() {
            out.push(CardAction::Unlink(n.id));
            ui.close();
        }
        if ui.button("Remove from the Flow").clicked() {
            out.push(CardAction::Remove(n.id));
            ui.close();
        }
    });
    let border = if resp.dragged() || state.link_target || state.selected {
        util::ACCENT
    } else if resp.hovered() {
        Color32::from_rgb(0x5a, 0x5f, 0x73)
    } else {
        CARD_LINE
    };
    let p = ui.painter();
    p.rect_filled(
        rect.translate(vec2(0.0, 4.0)),
        8,
        Color32::from_black_alpha(70),
    );
    p.rect(
        rect,
        8,
        CARD,
        Stroke::new(if state.selected { 2.0 } else { 1.0_f32 }, border),
        egui::StrokeKind::Inside,
    );
    seek_bar(ui, n, t, &state, actions);

    let inner = rect.shrink2(vec2(8.0, 7.0));
    let mut ui = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(egui::Layout::top_down(Align::Min)),
    );
    ui.spacing_mut().item_spacing = vec2(8.0, 7.0);
    ui.horizontal(|ui| {
        art(ui, 34.0, t, lib);
        let id_w = ui.available_width() - 28.0 - 8.0;
        ui.allocate_ui(vec2(id_w, 34.0), |ui| {
            ui.vertical(|ui| {
                ui.set_width(id_w);
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.add(
                    egui::Label::new(RichText::new(&t.lib.title).strong())
                        .truncate()
                        .selectable(false),
                );
                ui.add(
                    egui::Label::new(RichText::new(&t.lib.artist).size(11.0).color(util::MUTED))
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
            actions.push(LibAction::Preview(t.lib.id));
        }
    });
    chips_row(&mut ui, t, |ui| {
        ui.label(
            RichText::new(t.lib.duration.map(util::mmss).unwrap_or_default())
                .size(11.0)
                .color(util::MUTED),
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
        let col = if x.hovered() { util::WARN } else { util::MUTED };
        ui.painter().circle(c, 9.0, util::BG, Stroke::new(1.5_f32, col));
        for d in [vec2(3.5, 3.5), vec2(3.5, -3.5)] {
            ui.painter()
                .line_segment([c - d, c + d], Stroke::new(1.5_f32, col));
        }
        if x.clicked() {
            out.push(CardAction::Remove(n.id));
        }
    }

    // The in port on the left: drag from it to another card to follow that card.
    let c_in = pos2(rect.left(), rect.center().y);
    let port_in = ui
        .interact(
            Rect::from_center_size(c_in, vec2(18.0, 18.0)),
            ui.id().with(("flow-in", n.id)),
            Sense::drag(),
        )
        .on_hover_text("Drag to the card that comes before this one");
    if port_in.drag_started() {
        out.push(CardAction::StartLink(n.id, Port::In));
    }
    let col = if port_in.hovered() || port_in.dragged() {
        util::ACCENT
    } else {
        util::MUTED
    };
    ui.painter()
        .circle(c_in, 5.0, util::BG, Stroke::new(2.0_f32, col));

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
        out.push(CardAction::StartLink(n.id, Port::Out));
    }
    let col = if plus.hovered() {
        util::ACCENT
    } else {
        util::MUTED
    };
    ui.painter()
        .circle(c, 13.0, util::BG, Stroke::new(2.0_f32, col));
    ui.painter().text(
        c,
        Align2::CENTER_CENTER,
        "+",
        FontId::proportional(17.0),
        if plus.hovered() {
            util::ACCENT
        } else {
            Color32::WHITE
        },
    );
    if plus.clicked() {
        out.push(CardAction::Plus(n.id));
    }
}

/// The track's waveform along the card's bottom edge: click or drag to preview from
/// there. Shows how far the preview has got.
fn seek_bar(ui: &mut Ui, n: &Node, t: &Track, state: &CardState, actions: &mut Vec<LibAction>) {
    let card = n.rect();
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
        actions.push(LibAction::PreviewSeek(t.lib.id, frac(p.x)));
    }
    if let (Some(p), Some(d)) = (resp.hover_pos(), t.lib.duration) {
        resp.on_hover_text_at_pointer(util::mmss(d * frac(p.x) as f64));
    }

    let p = ui.painter();
    p.rect_filled(bar, 3, CHIPS_BG);
    let tint = if state.preview.is_some() {
        Color32::WHITE
    } else {
        Color32::from_gray(150)
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
            Color32::from_black_alpha(130),
        );
        p.line_segment(
            [pos2(x, bar.top() - 2.0), pos2(x, bar.bottom() + 2.0)],
            Stroke::new(2.0_f32, util::ACCENT),
        );
    }
}

/// BPM, key and energy chips on a dark strip, with `right` added at its right end.
fn chips_row(ui: &mut Ui, t: &Track, right: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(CHIPS_BG)
        .corner_radius(5)
        .inner_margin(egui::Margin::symmetric(5, 3))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                chip(
                    ui,
                    &t.bpm().map_or("-".into(), |b| format!("{:.1}", b)),
                    util::PANEL_2,
                    util::TEXT,
                );
                match t.key {
                    Some(k) => key_chip(ui, k),
                    None => {
                        chip(ui, "?", util::LINE, util::TEXT);
                    }
                }
                let e = (t.energy * 9.0 + 1.0).round().clamp(1.0, 10.0);
                chip(
                    ui,
                    &format!("{e:.0}"),
                    ENERGY_CHIP,
                    Color32::from_rgb(0x11, 0x11, 0x11),
                )
                .on_hover_text(
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

/// A coloured tile with the artist's initials, standing in for artwork.
/// The track's cover art, or a gradient with the artist's initials until it has one.
fn art(ui: &mut Ui, size: f32, t: &Track, lib: &LibraryUi) {
    let h = format!("{}{}", t.lib.artist, t.lib.title)
        .chars()
        .fold(0u32, |h, c| (h * 31 + c as u32) % 360) as f32;
    let initials: String = if t.lib.artist.is_empty() {
        &t.lib.title
    } else {
        &t.lib.artist
    }
    .split_whitespace()
    .take(2)
    .filter_map(|w| w.chars().next())
    .collect::<String>()
    .to_uppercase();
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    match lib.art.get(&t.lib.id) {
        Some(Some(tex)) => {
            egui::Image::new((tex.id(), rect.size()))
                .corner_radius(3)
                .paint_at(ui, rect);
            return;
        }
        Some(None) => {}
        None => want_art(ui.ctx(), t.lib.id),
    }
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
        Color32::WHITE,
    );
}

/// A white round button with a painted play triangle, or a stop square while
/// `playing` (the font has no ▶ or ■).
fn play_button(ui: &mut Ui, playing: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::click());
    let c = rect.center();
    let fill = if resp.hovered() {
        util::ACCENT
    } else {
        Color32::WHITE
    };
    ui.painter().circle_filled(c, 14.0, fill);
    if playing {
        for dx in [-3.0, 3.0] {
            ui.painter().rect_filled(
                Rect::from_center_size(c + vec2(dx, 0.0), vec2(3.5, 11.0)),
                1,
                Color32::from_rgb(0x11, 0x11, 0x11),
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
        Color32::from_rgb(0x11, 0x11, 0x11),
        Stroke::NONE,
    ));
    resp
}

/// A suggestion in the "+" grid: art, title, artist, chips and the match score.
fn pick_tile(ui: &mut Ui, w: f32, t: &Track, lib: &LibraryUi, score: f64) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 70.0), Sense::click());
    let resp = resp
        .on_hover_text(&t.lib.path)
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    ui.painter().rect(
        rect,
        7,
        CARD,
        Stroke::new(
            1.0_f32,
            if resp.hovered() {
                util::ACCENT
            } else {
                CARD_LINE
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
        art(ui, 28.0, t, lib);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.add(
                egui::Label::new(RichText::new(&t.lib.title).strong().size(12.0))
                    .truncate()
                    .selectable(false),
            );
            ui.add(
                egui::Label::new(RichText::new(&t.lib.artist).size(11.0).color(util::MUTED))
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
                .color(util::MUTED),
        )
        .on_hover_text("Match");
    });
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn children_stack_and_rejoin_on_remove() {
        let mut f = FlowUi::from_saved(Saved::default());
        f.add_root(10);
        let root = f.nodes[0].id;
        f.add_child(root, 11);
        f.add_child(root, 12);
        let (a, b) = (&f.nodes[1], &f.nodes[2]);
        assert_eq!(a.pos.x, NODE_W + GAP_X);
        assert_eq!(b.pos.y, a.pos.y + NODE_H + GAP_Y);
        let a = a.id;
        f.add_child(a, 13);
        f.remove(a);
        assert_eq!(f.nodes.len(), 3);
        assert_eq!(f.nodes[2].parent, Some(root)); // 13 now hangs off the root
        assert_eq!(f.in_flow(), HashSet::from([10, 12, 13]));
    }

    #[test]
    fn links_refuse_loops() {
        let mut f = FlowUi::from_saved(Saved::default());
        for t in [1, 2, 3] {
            f.add_root(t);
        }
        let ids: Vec<u64> = f.nodes.iter().map(|n| n.id).collect();
        assert!(f.link(ids[0], ids[1]));
        assert!(f.link(ids[1], ids[2]));
        assert!(!f.link(ids[2], ids[0])); // would loop back to the start
        assert!(!f.link(ids[0], ids[0]));
        assert!(f.link(ids[0], ids[2])); // re-linking moves the card's one parent
        assert_eq!(f.nodes[2].parent, Some(ids[0]));
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
