//! Flows: the model, without the storage (that's in `store`).
//!
//! A Flow is a DAG of cards (nodes), one track each; the same track can be on several
//! cards. A card can follow others along edges, and there are never loops
//! ([`creates_cycle`] is the one check). A playlist is a separate thing: one ordered
//! path through a Flow, saved as its own list. [`playlist_order`] gives the tracks in
//! order when the whole Flow is a single chain.

use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub struct Folder {
    pub id: i64,
    /// `None`: top level.
    pub parent: Option<i64>,
    pub name: String,
}

/// A saved Flow as the folder tree lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct FlowEntry {
    pub id: i64,
    /// `None`: top level.
    pub folder: Option<i64>,
    pub name: String,
    pub cards: usize,
    /// One chain of cards (no branches), so it reads as one playlist.
    pub linear: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    pub id: u64,
    pub track_id: i64,
    /// The card this one follows; `None` for a starting card.
    pub parent: Option<u64>,
    pub x: f32,
    pub y: f32,
}

/// The tracks in order if the cards form a single chain (or there are none), else
/// `None`. Links to cards that aren't there count as no link.
pub fn playlist_order(cards: &[Card]) -> Option<Vec<i64>> {
    let by_id: HashMap<u64, &Card> = cards.iter().map(|c| (c.id, c)).collect();
    let parent = |c: &Card| c.parent.filter(|p| by_id.contains_key(p));
    let mut roots = cards.iter().filter(|c| parent(c).is_none());
    let mut next: HashMap<u64, &Card> = HashMap::new();
    for c in cards {
        if let Some(p) = parent(c)
            && next.insert(p, c).is_some()
        {
            return None; // two cards follow one: a branch
        }
    }
    let mut order = vec![];
    if let Some(mut at) = roots.next() {
        if roots.next().is_some() {
            return None; // two starting cards
        }
        loop {
            order.push(at.track_id);
            match next.get(&at.id) {
                Some(n) => at = n,
                None => break,
            }
        }
    }
    (order.len() == cards.len()).then_some(order)
}

/// The edges (from, to) the cards' `parent` links make.
pub fn edges(cards: &[Card]) -> Vec<(u64, u64)> {
    cards
        .iter()
        .filter_map(|c| c.parent.map(|p| (p, c.id)))
        .collect()
}

/// Would adding the edge `from` → `to` make a loop? The one place a Flow's "no
/// cycles" rule is checked: the canvas asks before linking, and the Store refuses
/// to save a Flow that has one ([`check_acyclic`]).
pub fn creates_cycle(edges: &[(u64, u64)], from: u64, to: u64) -> bool {
    if from == to {
        return true;
    }
    // Is `from` reachable from `to` already?
    let mut next: HashMap<u64, Vec<u64>> = HashMap::new();
    for &(a, b) in edges {
        next.entry(a).or_default().push(b);
    }
    let mut stack = vec![to];
    let mut seen = std::collections::HashSet::new();
    while let Some(n) = stack.pop() {
        if n == from {
            return true;
        }
        if seen.insert(n) {
            stack.extend(next.get(&n).into_iter().flatten());
        }
    }
    false
}

/// `Err` naming an edge that closes a loop, if the edges have one.
pub fn check_acyclic(edges: &[(u64, u64)]) -> Result<(), (u64, u64)> {
    let mut so_far = Vec::with_capacity(edges.len());
    for &(a, b) in edges {
        if creates_cycle(&so_far, a, b) {
            return Err((a, b));
        }
        so_far.push((a, b));
    }
    Ok(())
}

/// Whether `nodes` is a path through the graph: each node follows the one before
/// along an edge. A path is what gets exported from a Flow as a playlist.
pub fn is_path(edges: &[(u64, u64)], nodes: &[u64]) -> bool {
    !nodes.is_empty() && nodes.windows(2).all(|w| edges.contains(&(w[0], w[1])))
}

/// Cards from stored nodes and edges. The canvas gives each card one parent for
/// now, so a node with several incoming edges keeps the first.
pub fn cards_from(nodes: Vec<(u64, i64, f32, f32)>, edges: &[(u64, u64)]) -> Vec<Card> {
    nodes
        .into_iter()
        .map(|(id, track_id, x, y)| Card {
            id,
            track_id,
            parent: edges.iter().find(|e| e.1 == id).map(|e| e.0),
            x,
            y,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(id: u64, parent: Option<u64>) -> Card {
        Card {
            id,
            track_id: id as i64 * 10,
            parent,
            x: 0.0,
            y: 0.0,
        }
    }

    #[test]
    fn a_chain_is_a_playlist() {
        let cards = [card(3, Some(1)), card(1, None), card(2, Some(3))];
        assert_eq!(playlist_order(&cards), Some(vec![10, 30, 20]));
        assert_eq!(playlist_order(&[]), Some(vec![]));
        assert_eq!(playlist_order(&[card(1, None)]), Some(vec![10]));
        // A link to a missing card counts as no link.
        assert_eq!(playlist_order(&[card(1, Some(9))]), Some(vec![10]));
    }

    #[test]
    fn branches_and_islands_are_not() {
        let branch = [card(1, None), card(2, Some(1)), card(3, Some(1))];
        assert_eq!(playlist_order(&branch), None);
        let two_starts = [card(1, None), card(2, None)];
        assert_eq!(playlist_order(&two_starts), None);
    }

    #[test]
    fn cycles_are_found() {
        let e = [(1, 2), (2, 3)];
        assert!(creates_cycle(&e, 3, 1));
        assert!(creates_cycle(&e, 2, 2));
        assert!(!creates_cycle(&e, 1, 3), "a second route isn't a loop");
        assert!(check_acyclic(&[(1, 2), (2, 3), (1, 3)]).is_ok());
        assert_eq!(check_acyclic(&[(1, 2), (2, 3), (3, 1)]), Err((3, 1)));
        assert!(is_path(&e, &[1, 2, 3]));
        assert!(!is_path(&e, &[1, 3]));
    }
}
