//! Crates, playlists, smart playlists, Flows and the folders that hold them: the
//! model, without the storage (that's in `store`).
//!
//! - A **crate** is a set of tracks: each at most once, no order.
//! - A **Flow** is a DAG of cards ([`FlowGraph`]), one track each; the same track
//!   can be on several cards, and a card can follow several others. There are never
//!   loops ([`creates_cycle`] is the one check, for Flows and the folder tree alike).
//! - A **playlist** is an ordered list (a track may repeat). One made from a Flow is
//!   a snapshot of one path through it ([`FlowGraph::bake`]), not a live view.
//! - A **smart playlist** is a saved query (`core::query`).

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use super::query;
use super::track::TrackId;

pub type CollectionId = i64;
/// A card's id, unique within its Flow.
pub type NodeId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CollectionKind {
    Folder,
    Crate,
    Playlist,
    Smart,
    Flow,
}

impl CollectionKind {
    /// As stored in `collections.kind`.
    pub const fn as_str(self) -> &'static str {
        match self {
            CollectionKind::Folder => "folder",
            CollectionKind::Crate => "crate",
            CollectionKind::Playlist => "playlist",
            CollectionKind::Smart => "smart",
            CollectionKind::Flow => "flow",
        }
    }

    pub fn parse(s: &str) -> Option<CollectionKind> {
        Some(match s {
            "folder" => CollectionKind::Folder,
            "crate" => CollectionKind::Crate,
            "playlist" => CollectionKind::Playlist,
            "smart" => CollectionKind::Smart,
            "flow" => CollectionKind::Flow,
            _ => return None,
        })
    }

    /// For menus: "New crate", "Add to playlist" …
    pub const fn label(self) -> &'static str {
        match self {
            CollectionKind::Folder => "folder",
            CollectionKind::Crate => "crate",
            CollectionKind::Playlist => "playlist",
            CollectionKind::Smart => "smart playlist",
            CollectionKind::Flow => "Flow",
        }
    }

    /// What a new one is called until it's renamed.
    pub const fn untitled(self) -> &'static str {
        match self {
            CollectionKind::Folder => "New folder",
            CollectionKind::Crate => "New crate",
            CollectionKind::Playlist => "New playlist",
            CollectionKind::Smart => "New smart playlist",
            CollectionKind::Flow => "Untitled Flow",
        }
    }

    /// Tracks can be added to it by hand.
    pub const fn takes_tracks(self) -> bool {
        matches!(
            self,
            CollectionKind::Crate | CollectionKind::Playlist | CollectionKind::Flow
        )
    }
}

/// A collection as the tree lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct Collection {
    pub id: CollectionId,
    /// The folder it's in; `None`: top level.
    pub parent: Option<CollectionId>,
    pub kind: CollectionKind,
    pub name: String,
    /// A crate's tracks; a playlist's in order (repeats kept); each Flow card's
    /// track. Empty for folders and smart playlists.
    pub tracks: Vec<TrackId>,
    /// A smart playlist's query text.
    pub query: Option<String>,
    /// The Flow a playlist was baked from.
    pub source_flow: Option<CollectionId>,
    /// A Flow whose cards are one chain, so it reads as one playlist.
    pub linear: bool,
}

/// Every collection, as the Store last listed them.
#[derive(Default)]
pub struct Collections {
    list: Vec<Collection>,
    by_id: HashMap<CollectionId, usize>,
    members: HashMap<CollectionId, HashSet<TrackId>>,
    /// Bumped on every change, so views built from it know to refresh.
    rev: u64,
}

impl Collections {
    pub fn set(&mut self, list: Vec<Collection>) {
        self.by_id = list.iter().enumerate().map(|(i, c)| (c.id, i)).collect();
        self.members = list
            .iter()
            .filter(|c| !c.tracks.is_empty())
            .map(|c| (c.id, c.tracks.iter().copied().collect()))
            .collect();
        self.list = list;
        self.rev += 1;
    }

    pub fn rev(&self) -> u64 {
        self.rev
    }

    pub fn all(&self) -> &[Collection] {
        &self.list
    }

    pub fn get(&self, id: CollectionId) -> Option<&Collection> {
        self.by_id.get(&id).map(|&i| &self.list[i])
    }

    /// What's directly inside `folder` (`None`: the top level): folders first, then
    /// the rest, each by name (the Store lists them by name).
    pub fn children(&self, folder: Option<CollectionId>) -> Vec<&Collection> {
        let mut v: Vec<&Collection> = self.list.iter().filter(|c| c.parent == folder).collect();
        v.sort_by_key(|c| c.kind != CollectionKind::Folder);
        v
    }

    fn parent_of(&self, id: CollectionId) -> Option<CollectionId> {
        self.get(id).and_then(|c| c.parent)
    }

    /// `id` is `ancestor` or somewhere inside it.
    pub fn is_within(&self, id: CollectionId, ancestor: CollectionId) -> bool {
        is_within(id, ancestor, |c| self.parent_of(c))
    }

    /// Folders as the tree shows them, with their depth.
    pub fn folders_in_order(&self) -> Vec<(&Collection, usize)> {
        fn walk<'a>(
            cs: &'a Collections,
            parent: Option<CollectionId>,
            depth: usize,
            out: &mut Vec<(&'a Collection, usize)>,
        ) {
            for c in cs.children(parent) {
                if c.kind == CollectionKind::Folder {
                    out.push((c, depth));
                    walk(cs, Some(c.id), depth + 1, out);
                }
            }
        }
        let mut out = vec![];
        walk(self, None, 0, &mut out);
        out
    }

    /// How many collections (not folders) are in a folder and its subfolders.
    pub fn count_inside(&self, folder: CollectionId) -> usize {
        self.list
            .iter()
            .filter(|c| c.kind != CollectionKind::Folder && c.id != folder)
            .filter(|c| c.parent.is_some_and(|p| self.is_within(p, folder)))
            .count()
    }

    /// Crates, playlists and Flows, for "Add to".
    pub fn takers(&self) -> impl Iterator<Item = &Collection> {
        self.list.iter().filter(|c| c.kind.takes_tracks())
    }
}

impl query::Context for Collections {
    fn contains(&self, collection: CollectionId, track: TrackId) -> bool {
        self.members
            .get(&collection)
            .is_some_and(|m| m.contains(&track))
    }
}

// ---- the one loop check ------------------------------------------------------------

/// Can `target` be reached from `start` by following `next`? Each node is visited
/// once, so it ends even if the links already loop.
pub fn reaches<N, I>(start: N, target: N, mut next: impl FnMut(N) -> I) -> bool
where
    N: Copy + Eq + Hash,
    I: IntoIterator<Item = N>,
{
    let mut stack = vec![start];
    let mut seen = HashSet::new();
    while let Some(n) = stack.pop() {
        if n == target {
            return true;
        }
        if seen.insert(n) {
            stack.extend(next(n));
        }
    }
    false
}

/// Would adding the edge `from` → `to` make a loop? Asked before every link, and
/// by the Store before it saves a Flow.
pub fn creates_cycle(edges: &[(NodeId, NodeId)], from: NodeId, to: NodeId) -> bool {
    reaches(to, from, |n| {
        edges
            .iter()
            .filter(move |e| e.0 == n)
            .map(|e| e.1)
            .collect::<Vec<_>>()
    })
}

/// `Err` naming an edge that closes a loop, if the edges have one.
pub fn check_acyclic(edges: &[(NodeId, NodeId)]) -> Result<(), (NodeId, NodeId)> {
    let mut so_far = Vec::with_capacity(edges.len());
    for &(a, b) in edges {
        if creates_cycle(&so_far, a, b) {
            return Err((a, b));
        }
        so_far.push((a, b));
    }
    Ok(())
}

/// `id` is `ancestor` or inside it, in a tree where `parent_of` gives each folder's
/// folder. Moving a folder into such a place would loop.
pub fn is_within<N: Copy + Eq + Hash>(
    id: N,
    ancestor: N,
    parent_of: impl Fn(N) -> Option<N>,
) -> bool {
    reaches(id, ancestor, parent_of)
}

// ---- Flows --------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct FlowNode {
    pub id: NodeId,
    pub track: TrackId,
    /// Top-left corner on the canvas.
    pub x: f32,
    pub y: f32,
}

/// Where new and tidied cards go: `col` apart across (a card's width plus the gap),
/// `row` apart down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spacing {
    pub col: f32,
    pub row: f32,
}

/// A change to a Flow, as the canvas makes it.
#[derive(Clone, Debug, PartialEq)]
pub enum GraphOp {
    Add {
        track: TrackId,
        x: f32,
        y: f32,
    },
    /// A new card for `track` after `parent`, below its other children.
    AddChild {
        parent: NodeId,
        track: TrackId,
    },
    /// A new starting card below the others.
    AddRoot(TrackId),
    /// Remove a card; whatever followed it now follows what it followed.
    Remove(NodeId),
    Link {
        from: NodeId,
        to: NodeId,
    },
    Unlink {
        from: NodeId,
        to: NodeId,
    },
    Move {
        node: NodeId,
        x: f32,
        y: f32,
    },
    Clear,
    Reflow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphError {
    NoCard,
    /// The link would loop back.
    Loop,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FlowGraph {
    pub nodes: Vec<FlowNode>,
    /// (from, to): `to` follows `from`.
    pub edges: Vec<(NodeId, NodeId)>,
}

impl FlowGraph {
    /// From stored nodes and edges. Edges to cards that aren't there are dropped.
    pub fn new(nodes: Vec<FlowNode>, edges: Vec<(NodeId, NodeId)>) -> FlowGraph {
        let ids: HashSet<NodeId> = nodes.iter().map(|n| n.id).collect();
        let mut seen = HashSet::new();
        let edges = edges
            .into_iter()
            .filter(|(a, b)| ids.contains(a) && ids.contains(b) && seen.insert((*a, *b)))
            .collect();
        FlowGraph { nodes, edges }
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn node(&self, id: NodeId) -> Option<&FlowNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    fn next_id(&self) -> NodeId {
        self.nodes.iter().map(|n| n.id + 1).max().unwrap_or(1)
    }

    pub fn parents(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        self.edges.iter().filter(move |e| e.1 == id).map(|e| e.0)
    }

    pub fn children(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        self.edges.iter().filter(move |e| e.0 == id).map(|e| e.1)
    }

    /// Every distinct track on the canvas.
    pub fn tracks(&self) -> HashSet<TrackId> {
        self.nodes.iter().map(|n| n.track).collect()
    }

    pub fn add(&mut self, track: TrackId, x: f32, y: f32) -> NodeId {
        let id = self.next_id();
        self.nodes.push(FlowNode { id, track, x, y });
        id
    }

    /// A new starting card, below the others.
    pub fn add_root(&mut self, track: TrackId, sp: Spacing) -> NodeId {
        if self.nodes.is_empty() {
            return self.add(track, 0.0, 0.0);
        }
        let bottom = self.nodes.iter().map(|n| n.y).fold(f32::MIN, f32::max);
        let left = self.nodes.iter().map(|n| n.x).fold(0.0, f32::min);
        // Half a row's gap, so it reads as a separate chain.
        self.add(track, left, bottom + sp.row * 1.5)
    }

    /// A new card after `parent`, one column right and below its other children.
    pub fn add_child(&mut self, parent: NodeId, track: TrackId, sp: Spacing) -> Option<NodeId> {
        let p = self.node(parent)?;
        let (px, py) = (p.x, p.y);
        let y = self
            .children(parent)
            .filter_map(|c| self.node(c))
            .map(|n| n.y + sp.row)
            .fold(py, f32::max);
        let id = self.add(track, px + sp.col, y);
        self.edges.push((parent, id));
        Some(id)
    }

    /// Remove a card. Each card that followed it now follows each card it followed.
    pub fn remove(&mut self, id: NodeId) -> bool {
        let Some(i) = self.nodes.iter().position(|n| n.id == id) else {
            return false;
        };
        self.nodes.remove(i);
        let parents: Vec<NodeId> = self.parents(id).collect();
        let children: Vec<NodeId> = self.children(id).collect();
        self.edges.retain(|e| e.0 != id && e.1 != id);
        for &p in &parents {
            for &c in &children {
                // Already reachable through `id`, so this can't loop.
                if !self.edges.contains(&(p, c)) {
                    self.edges.push((p, c));
                }
            }
        }
        true
    }

    /// Make `to` follow `from` too (a card can follow several).
    pub fn link(&mut self, from: NodeId, to: NodeId) -> Result<(), GraphError> {
        if self.node(from).is_none() || self.node(to).is_none() {
            return Err(GraphError::NoCard);
        }
        if self.edges.contains(&(from, to)) {
            return Ok(());
        }
        if creates_cycle(&self.edges, from, to) {
            return Err(GraphError::Loop);
        }
        self.edges.push((from, to));
        Ok(())
    }

    pub fn unlink(&mut self, from: NodeId, to: NodeId) -> bool {
        let before = self.edges.len();
        self.edges.retain(|e| *e != (from, to));
        self.edges.len() != before
    }

    /// Apply a canvas edit. Returns the new card's id for the ops that add one.
    pub fn apply(&mut self, op: &GraphOp, sp: Spacing) -> Result<Option<NodeId>, GraphError> {
        match *op {
            GraphOp::Add { track, x, y } => return Ok(Some(self.add(track, x, y))),
            GraphOp::AddRoot(track) => return Ok(Some(self.add_root(track, sp))),
            GraphOp::AddChild { parent, track } => {
                return self
                    .add_child(parent, track, sp)
                    .map(Some)
                    .ok_or(GraphError::NoCard);
            }
            GraphOp::Remove(id) => {
                if !self.remove(id) {
                    return Err(GraphError::NoCard);
                }
            }
            GraphOp::Link { from, to } => self.link(from, to)?,
            GraphOp::Unlink { from, to } => {
                self.unlink(from, to);
            }
            GraphOp::Move { node, x, y } => {
                let n = self
                    .nodes
                    .iter_mut()
                    .find(|n| n.id == node)
                    .ok_or(GraphError::NoCard)?;
                (n.x, n.y) = (x, y);
            }
            GraphOp::Clear => *self = FlowGraph::default(),
            GraphOp::Reflow => self.reflow(sp),
        }
        Ok(None)
    }

    /// Lay the cards out tidily. A card's column is the longest run of cards before
    /// it, so the width shows the set's length. A chain stays on one row; a branch's
    /// other paths go on rows below. Separate chains are stacked, keeping their
    /// top-to-bottom order.
    pub fn reflow(&mut self, sp: Spacing) {
        let y_of: HashMap<NodeId, f32> = self.nodes.iter().map(|n| (n.id, n.y)).collect();
        let by_y = |v: &mut Vec<NodeId>| v.sort_by(|a, b| y_of[a].total_cmp(&y_of[b]));
        let mut roots: Vec<NodeId> = self
            .nodes
            .iter()
            .map(|n| n.id)
            .filter(|&n| self.parents(n).next().is_none())
            .collect();
        by_y(&mut roots);

        // Column: longest path from a root (the graph has no loops).
        let mut col: HashMap<NodeId, usize> = HashMap::new();
        for n in self.topological() {
            let c = self
                .parents(n)
                .filter_map(|p| col.get(&p))
                .map(|c| c + 1)
                .max();
            col.insert(n, c.unwrap_or(0));
        }

        // Row: depth first from each root, a card placed where it's first reached.
        let mut row: HashMap<NodeId, f32> = HashMap::new();
        let mut at = 0.0;
        fn walk(
            g: &FlowGraph,
            id: NodeId,
            at: &mut f32,
            row: &mut HashMap<NodeId, f32>,
            by_y: &dyn Fn(&mut Vec<NodeId>),
        ) {
            row.insert(id, *at);
            let mut kids: Vec<NodeId> = g.children(id).filter(|c| !row.contains_key(c)).collect();
            by_y(&mut kids);
            for (i, c) in kids.into_iter().enumerate() {
                if row.contains_key(&c) {
                    continue; // reached through an earlier sibling
                }
                if i > 0 {
                    *at += 1.0;
                }
                walk(g, c, at, row, by_y);
            }
        }
        for (i, r) in roots.into_iter().enumerate() {
            if i > 0 {
                at += 1.5; // a gap between separate chains
            }
            walk(self, r, &mut at, &mut row, &by_y);
        }
        for n in &mut self.nodes {
            if let (Some(&c), Some(&r)) = (col.get(&n.id), row.get(&n.id)) {
                (n.x, n.y) = (c as f32 * sp.col, r * sp.row);
            }
        }
    }

    /// Cards in an order where each comes after everything it follows.
    fn topological(&self) -> Vec<NodeId> {
        let mut waiting: HashMap<NodeId, usize> = self
            .nodes
            .iter()
            .map(|n| (n.id, self.parents(n.id).count()))
            .collect();
        let mut ready: Vec<NodeId> = self
            .nodes
            .iter()
            .map(|n| n.id)
            .filter(|n| waiting[n] == 0)
            .collect();
        let mut out = vec![];
        while let Some(n) = ready.pop() {
            out.push(n);
            for c in self.children(n) {
                let w = waiting.get_mut(&c).unwrap();
                *w -= 1;
                if *w == 0 {
                    ready.push(c);
                }
            }
        }
        out
    }

    /// The tracks in order if the cards form a single chain (or there are none).
    pub fn linear_order(&self) -> Option<Vec<TrackId>> {
        let mut roots = self
            .nodes
            .iter()
            .filter(|n| self.parents(n.id).next().is_none());
        let mut order = vec![];
        if let Some(mut at) = roots.next() {
            if roots.next().is_some() {
                return None; // two starting cards
            }
            loop {
                order.push(at.track);
                let mut next = self.children(at.id);
                match (next.next(), next.next()) {
                    (None, _) => break,
                    (Some(n), None) => at = self.node(n)?,
                    (Some(_), Some(_)) => return None, // a branch
                }
            }
        }
        (order.len() == self.nodes.len()).then_some(order)
    }

    /// Whether `nodes` is a path through the graph: each follows the one before
    /// along an edge.
    pub fn is_path(&self, nodes: &[NodeId]) -> bool {
        !nodes.is_empty()
            && nodes.iter().all(|n| self.node(*n).is_some())
            && nodes.windows(2).all(|w| self.edges.contains(&(w[0], w[1])))
    }

    /// A path's tracks, for saving as a playlist (a snapshot of the path).
    pub fn bake(&self, nodes: &[NodeId]) -> Option<Vec<TrackId>> {
        self.is_path(nodes).then(|| {
            nodes
                .iter()
                .filter_map(|n| self.node(*n))
                .map(|n| n.track)
                .collect()
        })
    }

    /// A whole path through `node`: back through the topmost card it follows to a
    /// start, and on through the topmost card that follows it to an end.
    pub fn path_through(&self, node: NodeId) -> Vec<NodeId> {
        if self.node(node).is_none() {
            return vec![];
        }
        let top = |ids: Vec<NodeId>| {
            ids.into_iter().min_by(|a, b| {
                let y = |n| self.node(n).map_or(0.0, |n| n.y);
                y(*a).total_cmp(&y(*b))
            })
        };
        let mut back = vec![];
        let mut at = node;
        while let Some(p) = top(self.parents(at).collect()) {
            back.push(p);
            at = p;
        }
        back.reverse();
        back.push(node);
        let mut at = node;
        while let Some(c) = top(self.children(at).collect()) {
            back.push(c);
            at = c;
        }
        back
    }
}

// ---- what the UI asks for -----------------------------------------------------------

/// User actions on collections.
#[derive(Clone, Debug, PartialEq)]
pub enum CollectionCmd {
    /// Show it: a Flow on the canvas, anything else as the track list.
    Open(CollectionId),
    /// A new one in `parent` (`None`: top level), holding `tracks` (or, for a smart
    /// playlist, matching `query`).
    Create {
        kind: CollectionKind,
        parent: Option<CollectionId>,
        tracks: Vec<TrackId>,
        query: Option<String>,
    },
    Rename(CollectionId, String),
    /// Into a folder (`None`: top level).
    Move(CollectionId, Option<CollectionId>),
    Duplicate(CollectionId),
    /// With everything inside, for a folder.
    Delete(CollectionId),
    AddTracks {
        to: CollectionId,
        tracks: Vec<TrackId>,
    },
    /// Every place these tracks have in it.
    RemoveTracks {
        from: CollectionId,
        tracks: Vec<TrackId>,
    },
    SetQuery(CollectionId, String),
}

/// User actions on the Flow being built.
#[derive(Clone, Debug, PartialEq)]
pub enum FlowCmd {
    /// New starting cards on the open Flow (opening the canvas).
    AddTracks(Vec<TrackId>),
    /// Save a path through a Flow as a new playlist beside it.
    BakePath {
        flow: CollectionId,
        nodes: Vec<NodeId>,
        name: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    const SP: Spacing = Spacing {
        col: 100.0,
        row: 10.0,
    };

    /// Cards 1..=n, card i holding track i * 10.
    fn graph(n: u64, edges: &[(NodeId, NodeId)]) -> FlowGraph {
        let nodes = (1..=n)
            .map(|id| FlowNode {
                id,
                track: id as i64 * 10,
                x: 0.0,
                y: id as f32,
            })
            .collect();
        FlowGraph::new(nodes, edges.to_vec())
    }

    #[test]
    fn a_chain_is_linear() {
        let g = graph(3, &[(3, 2), (1, 3)]);
        assert_eq!(g.linear_order(), Some(vec![10, 30, 20]));
        assert_eq!(FlowGraph::default().linear_order(), Some(vec![]));
        assert_eq!(graph(1, &[]).linear_order(), Some(vec![10]));
        // A link to a missing card is dropped.
        assert_eq!(graph(1, &[(1, 9)]).edges, vec![]);
        assert_eq!(graph(3, &[(1, 2), (1, 3)]).linear_order(), None, "a branch");
        assert_eq!(graph(2, &[]).linear_order(), None, "two starts");
        assert_eq!(graph(3, &[(1, 3), (2, 3)]).linear_order(), None, "a join");
    }

    #[test]
    fn cycles_are_found() {
        let e = [(1, 2), (2, 3)];
        assert!(creates_cycle(&e, 3, 1));
        assert!(creates_cycle(&e, 2, 2));
        assert!(!creates_cycle(&e, 1, 3), "a second route isn't a loop");
        assert!(check_acyclic(&[(1, 2), (2, 3), (1, 3)]).is_ok());
        assert_eq!(check_acyclic(&[(1, 2), (2, 3), (3, 1)]), Err((3, 1)));
        // The same check guards the folder tree.
        let parent = |f: i64| match f {
            3 => Some(2),
            2 => Some(1),
            _ => None,
        };
        assert!(is_within(3, 1, parent));
        assert!(is_within(1, 1, parent));
        assert!(!is_within(1, 3, parent));
    }

    #[test]
    fn a_card_can_follow_several() {
        let mut g = graph(3, &[]);
        g.link(1, 3).unwrap();
        g.link(2, 3).unwrap();
        assert_eq!(g.parents(3).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(g.link(3, 1), Err(GraphError::Loop));
        assert_eq!(g.link(3, 3), Err(GraphError::Loop));
        assert_eq!(g.link(1, 9), Err(GraphError::NoCard));
        g.link(1, 3).unwrap();
        assert_eq!(g.edges.len(), 2, "linking twice adds one edge");
        assert!(g.unlink(1, 3));
        assert_eq!(g.parents(3).collect::<Vec<_>>(), vec![2]);
    }

    #[test]
    fn the_same_track_on_several_cards() {
        let mut g = FlowGraph::default();
        let a = g.add_root(7, SP);
        let b = g.add_child(a, 7, SP).unwrap();
        let c = g.add_child(b, 7, SP).unwrap();
        assert_eq!(g.nodes.len(), 3);
        assert_eq!(g.tracks(), HashSet::from([7]));
        assert_eq!(g.linear_order(), Some(vec![7, 7, 7]));
        assert_eq!(g.bake(&[a, b, c]), Some(vec![7, 7, 7]));
    }

    #[test]
    fn removing_a_card_joins_both_sides() {
        // 1 → 3, 2 → 3, 3 → 4, 3 → 5
        let mut g = graph(5, &[(1, 3), (2, 3), (3, 4), (3, 5)]);
        assert!(g.remove(3));
        let mut e = g.edges.clone();
        e.sort();
        assert_eq!(e, vec![(1, 4), (1, 5), (2, 4), (2, 5)]);
        assert!(!g.remove(3));
    }

    #[test]
    fn children_stack_below_each_other() {
        let mut g = FlowGraph::default();
        let root = g.add_root(10, SP);
        let a = g.add_child(root, 11, SP).unwrap();
        let b = g.add_child(root, 12, SP).unwrap();
        let (na, nb) = (g.node(a).unwrap(), g.node(b).unwrap());
        assert_eq!(na.x, SP.col);
        assert_eq!(nb.y, na.y + SP.row);
        let r2 = g.add_root(13, SP);
        assert!(
            g.node(r2).unwrap().y > g.node(b).unwrap().y,
            "a new start goes below the rest"
        );
    }

    #[test]
    fn baking_needs_a_path() {
        // 1 → 2 → 3, 1 → 4
        let g = graph(4, &[(1, 2), (2, 3), (1, 4)]);
        assert_eq!(g.bake(&[1, 2, 3]), Some(vec![10, 20, 30]));
        assert_eq!(g.bake(&[1, 3]), None, "skips a card");
        assert_eq!(g.bake(&[]), None);
        assert_eq!(g.bake(&[9]), None);
        // Through 2: back to 1, on to 3. Through 1: the topmost child (2, y = 2).
        assert_eq!(g.path_through(2), vec![1, 2, 3]);
        assert_eq!(g.path_through(1), vec![1, 2, 3]);
        assert_eq!(g.path_through(4), vec![1, 4]);
    }

    #[test]
    fn reflow_uses_the_longest_route_for_columns() {
        // 1 → 2 → 3, and 1 → 3 directly: 3 still goes in column 2.
        let mut g = graph(4, &[(1, 2), (2, 3), (1, 3)]);
        g.apply(&GraphOp::Reflow, SP).unwrap();
        let at = |id| {
            let n = g.node(id).unwrap();
            (n.x, n.y)
        };
        assert_eq!(at(1), (0.0, 0.0));
        assert_eq!(at(2), (SP.col, 0.0));
        assert_eq!(at(3), (SP.col * 2.0, 0.0));
        // 4 is a separate chain, a gap below.
        assert_eq!(at(4), (0.0, SP.row * 1.5));
    }

    #[test]
    fn ops_apply() {
        let mut g = FlowGraph::default();
        let a = g.apply(&GraphOp::AddRoot(1), SP).unwrap().unwrap();
        let b = g
            .apply(
                &GraphOp::AddChild {
                    parent: a,
                    track: 2,
                },
                SP,
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            g.apply(&GraphOp::Link { from: b, to: a }, SP),
            Err(GraphError::Loop)
        );
        g.apply(
            &GraphOp::Move {
                node: b,
                x: 5.0,
                y: 6.0,
            },
            SP,
        )
        .unwrap();
        assert_eq!((g.node(b).unwrap().x, g.node(b).unwrap().y), (5.0, 6.0));
        g.apply(&GraphOp::Unlink { from: a, to: b }, SP).unwrap();
        assert!(g.edges.is_empty());
        g.apply(&GraphOp::Clear, SP).unwrap();
        assert!(g.is_empty());
    }

    fn coll(id: i64, parent: Option<i64>, kind: CollectionKind, tracks: &[i64]) -> Collection {
        Collection {
            id,
            parent,
            kind,
            name: format!("c{id}"),
            tracks: tracks.to_vec(),
            query: None,
            source_flow: None,
            linear: false,
        }
    }

    #[test]
    fn the_tree_and_membership() {
        use crate::core::query::Context;
        let mut cs = Collections::default();
        cs.set(vec![
            coll(1, None, CollectionKind::Folder, &[]),
            coll(2, Some(1), CollectionKind::Folder, &[]),
            coll(3, Some(2), CollectionKind::Crate, &[5, 6]),
            coll(4, None, CollectionKind::Playlist, &[6, 6]),
        ]);
        assert!(cs.is_within(3, 1));
        assert!(!cs.is_within(1, 2));
        assert_eq!(cs.count_inside(1), 1);
        assert_eq!(
            cs.folders_in_order()
                .iter()
                .map(|(c, d)| (c.id, *d))
                .collect::<Vec<_>>(),
            vec![(1, 0), (2, 1)]
        );
        assert_eq!(
            cs.children(None).iter().map(|c| c.id).collect::<Vec<_>>(),
            vec![1, 4]
        );
        assert!(cs.contains(3, 5) && !cs.contains(3, 7) && cs.contains(4, 6));
        assert_eq!(cs.takers().count(), 2);
    }
}
