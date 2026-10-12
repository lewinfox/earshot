//! Flow: a set-building canvas, Prepare's Flow tab. Add a track or two, then grow the
//! set from suggestions. Each card has a "+" on its right that opens nine suggested
//! next tracks (with Drop / Hold / Build); picking one adds it as a new card joined to
//! the first. Drag cards to move them, drag the background to pan, wheel to zoom. Link
//! two cards by dragging from one's "+" (out) to another's left port (in), or the other
//! way; a card can follow several. Drop tracks from the library to add cards. Save a
//! path through it as a playlist. The model is `core::collections::FlowGraph`; the
//! Store keeps it, and where the canvas was looking (`core::settings`).
//!
//! - [`tab`]: the tab's layout, `| library | canvas | suggestions |`.
//! - [`canvas`]: the canvas, its input and what the cards ask for.
//! - [`toolbar`]: the title (editable) and the buttons above the canvas.
//! - [`cards`], [`edges`], [`minimap`]: drawing.
//! - [`camera`]: where the canvas looks, and when that's saved.
//! - [`popups`]: the "+" suggestions and the "Add track" search.
//!
//! Every change to the graph goes through [`FlowUi::edit`].

mod camera;
mod canvas;
mod cards;
mod edges;
mod minimap;
mod popups;
pub mod tab;
mod toolbar;

use std::collections::HashSet;
use std::time::Duration;

use egui::emath::TSTransform;
use egui::{Pos2, Rect, Vec2, pos2, vec2};

use self::camera::Camera;
pub use self::tab::FlowTab;
use crate::core::collections::{
    CollectionId, FlowCmd, FlowGraph, FlowNode, GraphOp, NodeId, Spacing,
};
use crate::core::command::Command;
use crate::core::suggest::{Direction, Scored};
use crate::core::track::TrackId;

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
const ZOOM: std::ops::RangeInclusive<f32> = 0.25..=1.5;

/// A card's top-left corner, as stored.
fn pos(n: &FlowNode) -> Pos2 {
    pos2(n.x, n.y)
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

#[derive(Clone, Copy, PartialEq)]
enum Port {
    In,
    Out,
}

/// A card being dragged: drawn `by` from where it is. It moves (one `GraphOp::Move`)
/// when it's let go on the canvas; let go anywhere else (e.g. on a deck, which loads
/// it) it stays put.
#[derive(Clone, Copy)]
struct Moving {
    node: NodeId,
    by: Vec2,
    /// The pointer has left the canvas: the card carries its track (so a deck can
    /// take it) and the library's drag ghost follows the pointer instead.
    outside: bool,
}

pub struct FlowUi {
    /// The saved Flow on the canvas (`None` until the first card is added).
    pub flow: Option<CollectionId>,
    /// The Flow's name, as the title field has it.
    pub name: String,
    graph: FlowGraph,
    /// The graph as last saved, to tell when it needs saving again.
    stored: FlowGraph,
    camera: Camera,
    /// Canvas to screen, as of the last frame.
    to_screen: TSTransform,
    /// Hide the player while planning.
    pub full: bool,
    direction: Direction,
    pop: Option<Pop>,
    search: String,
    focus_search: bool,
    /// Suggestions for the open "+" popup, and what they were computed for.
    picks: Vec<Scored>,
    picks_for: Option<PickStamp>,
    /// A link being dragged: from this card, starting at its out ("+") or in port.
    linking: Option<(NodeId, Port)>,
    /// The card clicked on last; Delete or Backspace removes it.
    selected: Option<NodeId>,
    moving: Option<Moving>,
}

impl Default for FlowUi {
    fn default() -> FlowUi {
        FlowUi {
            flow: None,
            name: String::new(),
            graph: FlowGraph::default(),
            stored: FlowGraph::default(),
            camera: Camera::default(),
            to_screen: TSTransform::IDENTITY,
            full: false,
            direction: Direction::Hold,
            pop: None,
            search: String::new(),
            focus_search: false,
            picks: vec![],
            picks_for: None,
            linking: None,
            selected: None,
            moving: None,
        }
    }
}

impl FlowUi {
    /// Show a saved Flow (or an empty canvas with `None`), looking where it was
    /// looking last (`camera`), or at all of it.
    pub fn load(
        &mut self,
        flow: Option<(CollectionId, String, FlowGraph)>,
        camera: Option<[f32; 4]>,
    ) {
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
        self.camera
            .load(camera::from_array(camera), !self.graph.is_empty());
    }

    /// (as last saved, now), if the graph changed since it was last saved and nothing
    /// is being dragged. Counts it as saved. One finished gesture is one change (a
    /// card's move lands when it's let go): the shell saves it and records it for undo
    /// (`core::edit::flow_edit`).
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

    /// Where the canvas looks, once it has held still since it moved. Counts it as
    /// saved. `busy`: the pointer is down, so it may move again.
    pub fn camera_to_save(&mut self, busy: bool) -> Option<[f32; 4]> {
        self.camera.to_save(busy).map(camera::to_array)
    }

    /// Where the canvas looks if that isn't saved yet, held still or not.
    pub fn unsaved_camera(&mut self) -> Option<[f32; 4]> {
        self.camera.take().map(camera::to_array)
    }

    /// How long until [`Self::camera_to_save`] may have something.
    pub fn camera_wait(&self) -> Option<Duration> {
        self.camera.wait()
    }

    /// The track on the selected card (for suggestions after "Selected").
    pub fn selected_track(&self) -> Option<TrackId> {
        self.graph.node(self.selected?).map(|n| n.track)
    }

    /// The one place the graph changes.
    fn edit(&mut self, op: GraphOp) {
        match self.graph.apply(&op, SPACING) {
            Ok(Some(id)) => {
                if let Some(n) = self.graph.node(id) {
                    self.camera.reveal = Some(self.rect(n).expand(20.0));
                }
            }
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

    /// Add tracks as new cards, one under the other from `at` (canvas points, the
    /// first card's centre).
    fn add_at(&mut self, tracks: &[TrackId], at: Pos2) {
        let first = at - vec2(NODE_W, NODE_H) / 2.0;
        for (i, &track) in tracks.iter().enumerate() {
            self.edit(GraphOp::Add {
                track,
                x: first.x,
                y: first.y + i as f32 * SPACING.row,
            });
        }
    }

    /// Where a card is drawn: where it is, or where it's being dragged.
    fn pos(&self, n: &FlowNode) -> Pos2 {
        match self.moving {
            Some(m) if m.node == n.id && !m.outside => pos(n) + m.by,
            _ => pos(n),
        }
    }

    fn rect(&self, n: &FlowNode) -> Rect {
        Rect::from_min_size(self.pos(n), vec2(NODE_W, NODE_H))
    }

    /// A card's in port (left edge) and out port (the "+", right edge).
    fn port(&self, n: &FlowNode, port: Port) -> Pos2 {
        let r = self.rect(n);
        match port {
            Port::In => r.left_center(),
            Port::Out => r.right_center(),
        }
    }

    fn bounds(&self) -> Option<Rect> {
        self.graph
            .nodes
            .iter()
            .map(|n| self.rect(n))
            .reduce(|a, b| a.union(b))
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
            let name = format!("{} (path)", self.name.trim());
            out.push(FlowCmd::BakePath { flow, nodes, name }.into());
        }
    }
}

/// For tests: look inside without drawing.
#[cfg(test)]
impl FlowUi {
    /// Open the "+" suggestions on the first card (for the screenshot test).
    pub fn open_first_suggestions(&mut self) {
        self.pop = self.graph.nodes.first().map(|n| Pop::Suggest(n.id));
    }

    pub fn fit(&mut self) {
        self.camera.fit = true;
    }

    /// The graph now, if it changed since it was last saved.
    pub fn changed_graph(&mut self) -> Option<FlowGraph> {
        self.take_change().map(|(_, now)| now)
    }

    pub fn graph(&self) -> &FlowGraph {
        &self.graph
    }

    /// Where the canvas looks: `[left, top, right, bottom]`.
    pub fn view(&self) -> [f32; 4] {
        camera::to_array(self.camera.view)
    }

    pub fn set_view(&mut self, view: [f32; 4]) {
        if let Some(r) = camera::from_array(Some(view)) {
            self.camera.set(r);
        }
    }

    /// A card's centre on screen, as of the last frame.
    pub fn card_on_screen(&self, node: NodeId) -> Option<Pos2> {
        let n = self.graph.node(node)?;
        Some(self.to_screen * self.rect(n).center())
    }

    /// A point on the canvas, on screen (as of the last frame).
    pub fn card_on_screen_at(&self, p: Pos2) -> Pos2 {
        self.to_screen * p
    }

    /// A card is being dragged off the canvas (and carries its track, with the
    /// library's ghost under the pointer).
    pub fn dragged_off(&self) -> bool {
        self.moving.is_some_and(|m| m.outside)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changes_are_saved_once_and_not_mid_drag() {
        let mut f = FlowUi::default();
        assert!(f.changed_graph().is_none());
        f.add_tracks(&[10, 11]);
        assert_eq!(f.changed_graph().unwrap().nodes.len(), 2);
        assert!(f.changed_graph().is_none(), "counted as saved");
        let a = f.graph.nodes[0].id;
        let b = f.graph.nodes[1].id;
        f.edit(GraphOp::Link { from: a, to: b });
        f.moving = Some(Moving {
            node: a,
            by: vec2(5.0, 5.0),
            outside: false,
        });
        assert!(f.changed_graph().is_none(), "not while dragging");
        f.moving = None;
        assert_eq!(f.changed_graph().unwrap().edges, vec![(a, b)]);
        f.edit(GraphOp::Link { from: b, to: a });
        assert!(f.changed_graph().is_none(), "a loop is refused");
    }

    #[test]
    fn a_dragged_card_is_drawn_where_it_is_going() {
        let mut f = FlowUi::default();
        f.add_tracks(&[1]);
        let n = f.graph.nodes[0].clone();
        f.moving = Some(Moving {
            node: n.id,
            by: vec2(30.0, 10.0),
            outside: false,
        });
        assert_eq!(f.pos(&n), pos(&n) + vec2(30.0, 10.0));
        assert_eq!(f.graph.nodes[0], n, "the graph moves only when let go");
        f.moving.as_mut().unwrap().outside = true;
        assert_eq!(f.pos(&n), pos(&n), "off the canvas: back in its place");
    }

    #[test]
    fn dropped_tracks_go_one_under_the_other() {
        let mut f = FlowUi::default();
        f.add_at(&[1, 2, 3], pos2(500.0, 300.0));
        let ys: Vec<f32> = f.graph.nodes.iter().map(|n| n.y).collect();
        assert_eq!(f.graph.nodes.len(), 3);
        assert_eq!(f.graph.nodes[0].x, 500.0 - NODE_W / 2.0);
        assert_eq!(ys[1] - ys[0], SPACING.row);
        assert_eq!(ys[2] - ys[1], SPACING.row);
        assert!(f.graph.edges.is_empty(), "not linked");
    }

    #[test]
    fn the_path_to_bake() {
        let mut f = FlowUi::default();
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
}
