//! Which tracks are selected in a list, by id rather than row, so the selection
//! survives the list being filtered, sorted or refreshed.

use std::collections::HashSet;

use super::track::TrackId;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Selection {
    chosen: HashSet<TrackId>,
    /// Has keyboard focus (for "load selected" and the browse knob).
    focus: Option<TrackId>,
    /// Where a Shift-click range starts.
    anchor: Option<TrackId>,
}

/// Modifier keys held during a click.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Mods {
    pub shift: bool,
    /// Ctrl (Cmd on a Mac).
    pub command: bool,
}

impl Selection {
    pub fn focused(&self) -> Option<TrackId> {
        self.focus
    }

    pub fn contains(&self, id: TrackId) -> bool {
        self.chosen.contains(&id)
    }

    /// The selected tracks in `rows`' order (each once).
    pub fn in_order(&self, rows: &[TrackId]) -> Vec<TrackId> {
        let mut seen = HashSet::new();
        rows.iter()
            .copied()
            .filter(|id| self.chosen.contains(id) && seen.insert(*id))
            .collect()
    }

    pub fn select_only(&mut self, id: TrackId) {
        self.chosen = HashSet::from([id]);
        self.focus = Some(id);
        self.anchor = Some(id);
    }

    /// A click on `id`: Shift selects the range from the anchor, Ctrl adds or
    /// removes it, a plain click selects only it.
    pub fn click(&mut self, id: TrackId, rows: &[TrackId], m: Mods) {
        let pos = |x: TrackId| rows.iter().position(|r| *r == x);
        if m.shift
            && let (Some(a), Some(b)) = (self.anchor.and_then(pos), pos(id))
        {
            let (lo, hi) = (a.min(b), a.max(b));
            self.chosen = rows[lo..=hi].iter().copied().collect();
            self.focus = Some(id);
        } else if m.command {
            if !self.chosen.remove(&id) {
                self.chosen.insert(id);
            }
            self.focus = Some(id);
            self.anchor = Some(id);
        } else {
            self.select_only(id);
        }
    }

    /// A right-click: inside the selection it acts on all of it; outside, on just
    /// that row.
    pub fn right_click(&mut self, id: TrackId) {
        if !self.chosen.contains(&id) {
            self.select_only(id);
        }
    }

    /// Move the focus `delta` rows (selecting just that row). Returns the row to
    /// scroll to.
    pub fn move_focus(&mut self, delta: i32, rows: &[TrackId]) -> Option<usize> {
        if rows.is_empty() {
            return None;
        }
        let at = self.focus.and_then(|f| rows.iter().position(|r| *r == f));
        let r = at.map_or(0, |r| {
            (r as i32 + delta).clamp(0, rows.len() as i32 - 1) as usize
        });
        self.select_only(rows[r]);
        Some(r)
    }

    pub fn select_all(&mut self, rows: &[TrackId]) {
        self.chosen = rows.iter().copied().collect();
    }

    /// Forget tracks no longer in the list.
    pub fn retain(&mut self, rows: &[TrackId]) {
        let ids: HashSet<TrackId> = rows.iter().copied().collect();
        self.chosen.retain(|id| ids.contains(id));
        self.focus = self.focus.filter(|f| ids.contains(f));
        self.anchor = self.anchor.filter(|a| ids.contains(a));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROWS: [TrackId; 5] = [10, 20, 30, 40, 50];

    #[test]
    fn clicks_select_by_id() {
        let mut s = Selection::default();
        s.click(20, &ROWS, Mods::default());
        s.click(
            40,
            &ROWS,
            Mods {
                shift: true,
                command: false,
            },
        );
        assert_eq!(s.in_order(&ROWS), vec![20, 30, 40]);
        s.click(
            30,
            &ROWS,
            Mods {
                shift: false,
                command: true,
            },
        );
        assert_eq!(s.in_order(&ROWS), vec![20, 40]);
        s.click(
            50,
            &ROWS,
            Mods {
                shift: false,
                command: true,
            },
        );
        assert_eq!(s.in_order(&ROWS), vec![20, 40, 50]);
        // Re-sorted list: the same tracks stay selected.
        assert_eq!(s.in_order(&[50, 40, 30, 20, 10]), vec![50, 40, 20]);
        s.right_click(40);
        assert_eq!(s.in_order(&ROWS).len(), 3, "inside the selection: kept");
        s.right_click(10);
        assert_eq!(s.in_order(&ROWS), vec![10]);
    }

    #[test]
    fn focus_moves_and_follows_filtering() {
        let mut s = Selection::default();
        assert_eq!(s.move_focus(1, &ROWS), Some(0), "starts at the top");
        assert_eq!(s.move_focus(2, &ROWS), Some(2));
        assert_eq!(s.focused(), Some(30));
        assert_eq!(s.move_focus(9, &ROWS), Some(4));
        // Filtered down: the focus stays on the track, wherever it now is.
        assert_eq!(s.move_focus(-1, &[50, 10]), Some(0));
        assert_eq!(s.focused(), Some(50));
        s.retain(&[10]);
        assert_eq!(s.focused(), None);
        assert!(s.in_order(&[10]).is_empty());
        assert_eq!(s.move_focus(1, &[]), None);
    }
}
