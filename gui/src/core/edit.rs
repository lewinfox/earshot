//! Edits the user makes to the library, and undo.
//!
//! - The UI asks for an edit with an [`EditCmd`] (`Command::Edit`). The shell hands
//!   it to the Store (`StoreCmd::Edit`), which reads what it's about to replace, makes
//!   the change and reports it as an [`Edit`] (`StoreEvent::Edited`). The shell keeps
//!   those in a [`History`].
//! - An [`Edit`] holds both sides (old and new), so [`Edit::inverse`] is exact. Undo
//!   applies the inverse (`StoreCmd::ApplyEdit`); redo applies the edit again.
//! - Metadata is written to `track_meta` with source 'user', so a rescan (which only
//!   writes 'tag') never overwrites it. Nothing is written back to the audio files.
//! - A grid edit adds a `beatgrids` row; its undo switches `active_grid_id` back.
//! - The Flow canvas keeps its graph itself: its edits are recorded by the shell when
//!   it saves them ([`graph_diff`]), and undone on the canvas while that Flow is open.

use std::time::{Duration, Instant};

use super::collections::{CollectionId, FlowGraph, GraphOp, NodeId};
use super::keys::key_name;
use super::track::{Track, TrackId};

/// A field the metadata editor (and the library's columns) can change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Field {
    Title,
    Artist,
    Album,
    Genre,
    Comment,
    /// The key as text ("8A", "Am"); the library reads it as a Camelot key.
    Key,
    /// 0-5 stars.
    Rating,
    /// 0xRRGGBB.
    Colour,
    Tags,
}

impl Field {
    /// In the order the editor shows them.
    pub const ALL: [Field; 9] = [
        Field::Title,
        Field::Artist,
        Field::Album,
        Field::Genre,
        Field::Comment,
        Field::Key,
        Field::Rating,
        Field::Colour,
        Field::Tags,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Field::Title => "Title",
            Field::Artist => "Artist",
            Field::Album => "Album",
            Field::Genre => "Genre",
            Field::Comment => "Comment",
            Field::Key => "Key",
            Field::Rating => "Rating",
            Field::Colour => "Colour",
            Field::Tags => "Tags",
        }
    }

    /// Its name in `track_meta`, for the fields kept there (as text).
    pub const fn meta(self) -> Option<&'static str> {
        match self {
            Field::Title => Some("title"),
            Field::Artist => Some("artist"),
            Field::Album => Some("album"),
            Field::Genre => Some("genre"),
            Field::Comment => Some("comment"),
            Field::Key => Some("key"),
            Field::Rating | Field::Colour | Field::Tags => None,
        }
    }

    /// The value the library shows now (whichever source it came from).
    pub fn get(self, t: &Track) -> Option<Value> {
        let text = |s: &str| (!s.is_empty()).then(|| Value::Text(s.to_string()));
        match self {
            Field::Title => text(&t.title),
            Field::Artist => text(&t.artist),
            Field::Album => text(&t.album),
            Field::Genre => text(&t.genre),
            Field::Comment => text(&t.comment),
            Field::Key => t.key.map(|k| Value::Text(key_name(k))),
            Field::Rating => t.rating.map(Value::Rating),
            Field::Colour => t.colour.map(Value::Colour),
            Field::Tags => (!t.tags.is_empty()).then(|| Value::Tags(t.tags.clone())),
        }
    }
}

/// A field's value. `None` (as `Option<Value>`) is no value: for the text fields,
/// no value of the user's, so the file's tag shows again.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Text(String),
    Rating(u8),
    Colour(u32),
    /// Tag names, sorted, each once.
    Tags(Vec<String>),
}

impl Value {
    /// Tags as typed: trimmed, sorted, each once, empty ones dropped.
    pub fn tags<S: AsRef<str>>(names: impl IntoIterator<Item = S>) -> Value {
        let mut v: Vec<String> = names
            .into_iter()
            .map(|s| s.as_ref().trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        v.sort();
        v.dedup();
        Value::Tags(v)
    }
}

/// One field of one track, before and after.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldChange {
    pub track: TrackId,
    pub field: Field,
    pub old: Option<Value>,
    pub new: Option<Value>,
}

/// A change as made, with what it replaced: one undo step.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    /// Metadata, rating, colour or tags of one or more tracks (a bulk edit is one).
    Fields(Vec<FieldChange>),
    /// The track's active grid (a `beatgrids` row id; `None`: no grid).
    Grid {
        track: TrackId,
        old: Option<i64>,
        new: Option<i64>,
    },
    /// A crate's or playlist's tracks (a playlist's in order): adds, removals and
    /// reorders.
    Tracks {
        collection: CollectionId,
        old: Vec<TrackId>,
        new: Vec<TrackId>,
    },
    /// The folder a collection is in (`None`: top level).
    Parent {
        collection: CollectionId,
        old: Option<CollectionId>,
        new: Option<CollectionId>,
    },
    Name {
        collection: CollectionId,
        old: String,
        new: String,
    },
    /// Changes to a Flow's cards and links: `ops` make it, `undo` takes it back.
    FlowGraph {
        flow: CollectionId,
        ops: Vec<GraphOp>,
        undo: Vec<GraphOp>,
    },
    /// Tracks taken out of the library (`removed`), or put back (its undo). Their
    /// files stay on disk; everything about them stays in the database.
    Removal {
        tracks: Vec<TrackId>,
        removed: bool,
    },
}

impl Edit {
    /// The edit that takes this one back.
    pub fn inverse(&self) -> Edit {
        match self {
            Edit::Fields(changes) => Edit::Fields(
                changes
                    .iter()
                    .map(|c| FieldChange {
                        track: c.track,
                        field: c.field,
                        old: c.new.clone(),
                        new: c.old.clone(),
                    })
                    .collect(),
            ),
            Edit::Grid { track, old, new } => Edit::Grid {
                track: *track,
                old: *new,
                new: *old,
            },
            Edit::Tracks {
                collection,
                old,
                new,
            } => Edit::Tracks {
                collection: *collection,
                old: new.clone(),
                new: old.clone(),
            },
            Edit::Parent {
                collection,
                old,
                new,
            } => Edit::Parent {
                collection: *collection,
                old: *new,
                new: *old,
            },
            Edit::Name {
                collection,
                old,
                new,
            } => Edit::Name {
                collection: *collection,
                old: new.clone(),
                new: old.clone(),
            },
            Edit::FlowGraph { flow, ops, undo } => Edit::FlowGraph {
                flow: *flow,
                ops: undo.clone(),
                undo: ops.clone(),
            },
            Edit::Removal { tracks, removed } => Edit::Removal {
                tracks: tracks.clone(),
                removed: !removed,
            },
        }
    }

    /// It changes nothing (so it isn't worth an undo step).
    pub fn is_noop(&self) -> bool {
        match self {
            Edit::Fields(changes) => changes.iter().all(|c| c.old == c.new),
            Edit::Grid { old, new, .. } => old == new,
            Edit::Tracks { old, new, .. } => old == new,
            Edit::Parent { old, new, .. } => old == new,
            Edit::Name { old, new, .. } => old == new,
            Edit::FlowGraph { ops, .. } => ops.is_empty(),
            Edit::Removal { tracks, .. } => tracks.is_empty(),
        }
    }

    /// What it did, for the Edit menu ("Undo <label>").
    pub fn label(&self) -> String {
        match self {
            Edit::Fields(changes) => {
                let mut tracks: Vec<TrackId> = changes.iter().map(|c| c.track).collect();
                tracks.dedup();
                let mut fields: Vec<Field> = changes.iter().map(|c| c.field).collect();
                fields.sort_by_key(|f| *f as u8);
                fields.dedup();
                let what = match fields.as_slice() {
                    [f] => f.label().to_lowercase(),
                    _ => "metadata".into(),
                };
                match tracks.len() {
                    1 => format!("edit {what}"),
                    n => format!("edit {what} of {n} tracks"),
                }
            }
            Edit::Grid { .. } => "grid edit".into(),
            Edit::Tracks { old, new, .. } if new.len() > old.len() => "add tracks".into(),
            Edit::Tracks { old, new, .. } if new.len() < old.len() => "remove tracks".into(),
            Edit::Tracks { .. } => "reorder".into(),
            Edit::Parent { .. } => "move".into(),
            Edit::Name { .. } => "rename".into(),
            Edit::FlowGraph { .. } => "Flow edit".into(),
            Edit::Removal { tracks, removed } => {
                let what = match tracks.len() {
                    1 => "track".to_string(),
                    n => format!("{n} tracks"),
                };
                if *removed {
                    format!("remove {what} from the library")
                } else {
                    format!("put {what} back in the library")
                }
            }
        }
    }
}

/// What the UI asks for (`Command::Edit`). The Store makes it and reports the
/// [`Edit`] it made; the collection tree's adds, removes, moves and renames come here
/// too (from `CollectionCmd`), so they can be undone.
#[derive(Clone, Debug, PartialEq)]
pub enum EditCmd {
    /// Open the metadata editor on these tracks (the shell does this one).
    Editor(Vec<TrackId>),
    /// Set these fields on every one of these tracks, as one undo step. Only the
    /// fields listed change: a bulk edit lists the ones the user touched.
    Set {
        tracks: Vec<TrackId>,
        values: Vec<(Field, Option<Value>)>,
    },
    /// Add a tag to each track (keeping their others).
    #[allow(dead_code)] // for the library's tag column (step 7, B2); tests use it
    AddTag { tracks: Vec<TrackId>, tag: String },
    #[allow(dead_code)] // for the library's tag column (step 7, B2); tests use it
    RemoveTag { tracks: Vec<TrackId>, tag: String },
    /// Add tracks to a crate (each once), the end of a playlist, or a Flow (as new
    /// starting cards).
    AddTracks {
        to: CollectionId,
        tracks: Vec<TrackId>,
    },
    /// Every place these tracks have in a crate, playlist or Flow.
    RemoveTracks {
        from: CollectionId,
        tracks: Vec<TrackId>,
    },
    /// A crate's or playlist's whole contents, in this order (e.g. a reorder).
    SetTracks {
        collection: CollectionId,
        tracks: Vec<TrackId>,
    },
    /// Into a folder (`None`: top level).
    Move {
        collection: CollectionId,
        parent: Option<CollectionId>,
    },
    Rename {
        collection: CollectionId,
        name: String,
    },
    /// Take tracks out of the library (they stay on disk, and a rescan doesn't bring
    /// them back; undo does).
    RemoveFromLibrary(Vec<TrackId>),
}

/// The ops that turn Flow `from` into `to`: unlink, remove, put back or move
/// cards, then link. Each step is valid along the way (removed cards have no links
/// left, so nothing is joined up; every link added is one of `to`'s, which has no
/// loops).
pub fn graph_diff(from: &FlowGraph, to: &FlowGraph) -> Vec<GraphOp> {
    let mut ops = vec![];
    for &(a, b) in &from.edges {
        if !to.edges.contains(&(a, b)) {
            ops.push(GraphOp::Unlink { from: a, to: b });
        }
    }
    let kept = |id: NodeId| to.node(id).is_some();
    for n in from.nodes.iter().filter(|n| !kept(n.id)) {
        ops.push(GraphOp::Remove(n.id));
    }
    for n in &to.nodes {
        if from.node(n.id) != Some(n) {
            ops.push(GraphOp::Put(n.clone()));
        }
    }
    for &(a, b) in &to.edges {
        if !from.edges.contains(&(a, b)) {
            ops.push(GraphOp::Link { from: a, to: b });
        }
    }
    ops
}

/// A Flow edit from the graph before and after.
pub fn flow_edit(flow: CollectionId, before: &FlowGraph, after: &FlowGraph) -> Edit {
    Edit::FlowGraph {
        flow,
        ops: graph_diff(before, after),
        undo: graph_diff(after, before),
    }
}

/// How many steps undo goes back.
pub const UNDO_STEPS: usize = 200;

/// Renames of one collection this close together are one step (typing a name).
pub const RENAME_RUN: Duration = Duration::from_secs(3);

/// The undo and redo stacks.
#[derive(Default)]
pub struct History {
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    /// When the last edit was recorded.
    last: Option<Instant>,
}

impl History {
    /// A new edit: it can be undone, and what was undone can no longer be redone.
    pub fn record(&mut self, e: Edit) {
        self.record_at(e, Instant::now());
    }

    /// [`Self::record`], as of `now`. A rename straight after a rename of the same
    /// collection (within [`RENAME_RUN`]) joins it, so a name typed a letter at a time
    /// is undone in one go.
    pub fn record_at(&mut self, e: Edit, now: Instant) {
        let recent = self.last.is_some_and(|t| now.duration_since(t) < RENAME_RUN);
        self.last = Some(now);
        if let (
            true,
            Edit::Name {
                collection, new, ..
            },
            Some(Edit::Name {
                collection: c,
                new: last,
                ..
            }),
        ) = (recent, &e, self.undo.last_mut())
            && c == collection
            && self.redo.is_empty()
        {
            *last = new.clone();
            if self.undo.last().is_some_and(Edit::is_noop) {
                self.undo.pop();
            }
            return;
        }
        if e.is_noop() {
            return;
        }
        self.redo.clear();
        self.undo.push(e);
        if self.undo.len() > UNDO_STEPS {
            self.undo.remove(0);
        }
    }

    /// The edit to apply to undo the last one (its inverse); it moves to redo.
    pub fn undo(&mut self) -> Option<Edit> {
        let e = self.undo.pop()?;
        let back = e.inverse();
        self.redo.push(e);
        Some(back)
    }

    /// The edit to apply again; it moves back to undo.
    pub fn redo(&mut self) -> Option<Edit> {
        let e = self.redo.pop()?;
        self.undo.push(e.clone());
        Some(e)
    }

    /// What undo would take back.
    pub fn next_undo(&self) -> Option<&Edit> {
        self.undo.last()
    }

    pub fn next_redo(&self) -> Option<&Edit> {
        self.redo.last()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::collections::{FlowNode, Spacing};

    const SP: Spacing = Spacing {
        col: 100.0,
        row: 10.0,
    };

    fn title(track: TrackId, old: Option<&str>, new: Option<&str>) -> FieldChange {
        FieldChange {
            track,
            field: Field::Title,
            old: old.map(|s| Value::Text(s.into())),
            new: new.map(|s| Value::Text(s.into())),
        }
    }

    #[test]
    fn inverses_swap_old_and_new() {
        let e = Edit::Fields(vec![
            title(1, None, Some("A")),
            title(2, Some("x"), Some("B")),
        ]);
        let back = e.inverse();
        assert_eq!(
            back,
            Edit::Fields(vec![
                title(1, Some("A"), None),
                title(2, Some("B"), Some("x"))
            ])
        );
        assert_eq!(back.inverse(), e);
        let g = Edit::Grid {
            track: 1,
            old: None,
            new: Some(5),
        };
        assert_eq!(
            g.inverse(),
            Edit::Grid {
                track: 1,
                old: Some(5),
                new: None
            }
        );
        assert_eq!(e.label(), "edit title of 2 tracks");
        assert!(Edit::Fields(vec![title(1, Some("a"), Some("a"))]).is_noop());
    }

    #[test]
    fn undo_redo_and_a_new_edit_clears_redo() {
        let mut h = History::default();
        assert!(h.undo().is_none() && h.redo().is_none());
        let a = Edit::Fields(vec![title(1, None, Some("A"))]);
        let b = Edit::Fields(vec![title(1, Some("A"), Some("B"))]);
        h.record(a.clone());
        h.record(b.clone());
        h.record(Edit::Name {
            collection: 1,
            old: "x".into(),
            new: "x".into(),
        });
        assert_eq!(h.next_undo(), Some(&b), "a no-op isn't a step");

        assert_eq!(h.undo(), Some(b.inverse()));
        assert_eq!(h.undo(), Some(a.inverse()));
        assert_eq!(h.undo(), None);
        assert_eq!(h.redo(), Some(a.clone()));
        assert_eq!(h.next_redo(), Some(&b));

        // Something new: b can't be redone any more.
        let c = Edit::Grid {
            track: 1,
            old: Some(1),
            new: Some(2),
        };
        h.record(c.clone());
        assert_eq!(h.redo(), None);
        assert_eq!(h.undo(), Some(c.inverse()));
        assert_eq!(h.undo(), Some(a.inverse()));
    }

    #[test]
    fn the_history_is_capped() {
        let mut h = History::default();
        for i in 0..UNDO_STEPS as i64 + 5 {
            h.record(Edit::Grid {
                track: 1,
                old: Some(i),
                new: Some(i + 1),
            });
        }
        let mut n = 0;
        while h.undo().is_some() {
            n += 1;
        }
        assert_eq!(n, UNDO_STEPS);
    }

    fn apply(g: &mut FlowGraph, ops: &[GraphOp]) {
        for op in ops {
            g.apply(op, SP).unwrap();
        }
    }

    /// Every kind of canvas edit is taken back exactly, and redone exactly.
    #[test]
    fn flow_ops_undo_and_redo() {
        let mut g = FlowGraph::default();
        let a = g.add(10, 0.0, 0.0);
        let b = g.add_child(a, 20, SP).unwrap();
        let c = g.add_child(b, 30, SP).unwrap();
        let start = g.clone();
        let ops = [
            GraphOp::AddRoot(40),
            GraphOp::AddChild {
                parent: c,
                track: 50,
            },
            GraphOp::Remove(b),
            GraphOp::Link { from: a, to: c },
            GraphOp::Unlink { from: a, to: b },
            GraphOp::Move {
                node: c,
                x: 9.0,
                y: 9.0,
            },
            GraphOp::Reflow,
            GraphOp::Clear,
        ];
        for op in ops {
            let mut after = start.clone();
            after.apply(&op, SP).unwrap();
            let Edit::FlowGraph { ops, undo, .. } = flow_edit(1, &start, &after) else {
                unreachable!()
            };
            let mut g = after.clone();
            apply(&mut g, &undo);
            assert_eq!(sorted(&g), sorted(&start), "undo {op:?}");
            apply(&mut g, &ops);
            assert_eq!(sorted(&g), sorted(&after), "redo {op:?}");
        }
        // Removing the middle card joins a → c; undo takes that link away again.
        let mut after = start.clone();
        after.remove(b);
        assert!(after.edges.contains(&(a, c)));
        let undo = graph_diff(&after, &start);
        assert!(undo.contains(&GraphOp::Unlink { from: a, to: c }));
        assert!(undo.contains(&GraphOp::Put(FlowNode {
            id: b,
            track: 20,
            x: start.node(b).unwrap().x,
            y: start.node(b).unwrap().y,
        })));
    }

    /// Order doesn't matter to a Flow.
    fn sorted(g: &FlowGraph) -> FlowGraph {
        let mut g = g.clone();
        g.nodes.sort_by_key(|n| n.id);
        g.edges.sort();
        g
    }

    #[test]
    fn a_name_typed_letter_by_letter_is_one_step() {
        let name = |old: &str, new: &str| Edit::Name {
            collection: 7,
            old: old.into(),
            new: new.into(),
        };
        let mut h = History::default();
        let t = Instant::now();
        h.record_at(name("Flow", "Flow N"), t);
        h.record_at(name("Flow N", "Flow Ni"), t + Duration::from_millis(200));
        h.record_at(name("Flow Ni", "Flow Nig"), t + Duration::from_millis(400));
        assert_eq!(h.undo(), Some(name("Flow Nig", "Flow")));
        assert!(h.undo().is_none(), "one step");
        h.redo();
        // A while later: a step of its own.
        h.record_at(name("Flow Nig", "Set"), t + RENAME_RUN * 2);
        assert_eq!(h.undo(), Some(name("Set", "Flow Nig")));
        assert!(h.undo().is_some());
    }
}
