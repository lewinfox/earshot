//! Where the canvas looks, and when that's saved: a moment after it stops moving, so
//! a pan or a zoom is one write. The shell saves it per Flow (`core::settings`).

use std::time::{Duration, Instant};

use egui::{Rect, pos2, vec2};

/// Save the view once it has held still this long.
const SETTLE: Duration = Duration::from_millis(600);

pub struct Camera {
    /// The part of the canvas in view (egui's `Scene` keeps it as a rectangle).
    pub view: Rect,
    /// The view as last saved or loaded.
    saved: Option<Rect>,
    /// When the view last moved, while that isn't saved.
    moved: Option<Instant>,
    /// Fit every card in view on the next frame.
    pub fit: bool,
    /// Pan to show this (canvas points) on the next frame.
    pub reveal: Option<Rect>,
}

impl Default for Camera {
    fn default() -> Camera {
        Camera {
            view: Rect::from_min_size(pos2(-40.0, -40.0), vec2(1200.0, 500.0)),
            saved: None,
            moved: None,
            fit: false,
            reveal: None,
        }
    }
}

impl Camera {
    /// Look where a Flow was looking last; with nothing saved, fit its cards in view
    /// if it has any.
    pub fn load(&mut self, saved: Option<Rect>, has_cards: bool) {
        self.saved = saved;
        self.moved = None;
        self.reveal = None;
        match saved {
            Some(r) => {
                self.view = r;
                self.fit = false;
            }
            None => self.fit = has_cards,
        }
    }

    /// Look here now.
    pub fn set(&mut self, view: Rect) {
        if view != self.view {
            self.view = view;
            self.moved = Some(Instant::now());
        }
    }

    /// The view, once it has held still a moment since it moved and isn't what was
    /// saved. Counts it as saved. `busy`: the pointer is down, so it may move again.
    pub fn to_save(&mut self, busy: bool) -> Option<Rect> {
        if busy || self.moved?.elapsed() < SETTLE {
            return None;
        }
        self.take()
    }

    /// The view if it isn't what was saved, held still or not. Counts it as saved.
    pub fn take(&mut self) -> Option<Rect> {
        self.moved = None;
        if self.saved == Some(self.view) {
            return None;
        }
        self.saved = Some(self.view);
        Some(self.view)
    }

    /// How long until [`Self::to_save`] may have something.
    pub fn wait(&self) -> Option<Duration> {
        Some(SETTLE.saturating_sub(self.moved?.elapsed()))
    }

    /// Fit `bounds` in view on a canvas `size` big, but no closer than 1:1 so a few
    /// cards don't fill the screen.
    pub fn fit_to(&mut self, bounds: Rect, size: egui::Vec2) {
        let size = (bounds.size() + vec2(80.0, 80.0)).max(size);
        self.set(Rect::from_center_size(bounds.center(), size));
    }
}

/// Pan the view (keeping its zoom) so `want` is inside it.
pub fn reveal(view: Rect, want: Rect) -> Rect {
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

/// As the Store keeps it: `[left, top, right, bottom]`.
pub fn to_array(r: Rect) -> [f32; 4] {
    [r.left(), r.top(), r.right(), r.bottom()]
}

/// A saved view, if it's a sensible one.
pub fn from_array(a: Option<[f32; 4]>) -> Option<Rect> {
    let [l, t, r, b] = a?;
    let rect = Rect::from_min_max(pos2(l, t), pos2(r, b));
    (rect.is_finite() && rect.width() > 1.0 && rect.height() > 1.0).then_some(rect)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn saved_once_it_holds_still() {
        let mut c = Camera::default();
        let saved = c.view;
        c.load(Some(saved), true);
        assert!(!c.fit, "a saved view is used as it is");
        assert!(c.to_save(false).is_none(), "nothing moved");
        let moved = saved.translate(vec2(10.0, 0.0));
        c.set(moved);
        assert!(c.to_save(false).is_none(), "still settling");
        assert!(c.wait().is_some());
        c.moved = Some(Instant::now() - SETTLE);
        assert!(c.to_save(true).is_none(), "not while the pointer is down");
        assert_eq!(c.to_save(false), Some(moved));
        assert!(c.to_save(false).is_none(), "counted as saved");
        assert!(c.take().is_none());
        c.load(None, true);
        assert!(c.fit, "nothing saved: fit the cards");
    }

    #[test]
    fn saved_views_round_trip() {
        let r = Rect::from_min_max(pos2(-5.0, 2.0), pos2(300.0, 200.0));
        assert_eq!(from_array(Some(to_array(r))), Some(r));
        assert_eq!(from_array(Some([0.0, 0.0, 0.0, 0.0])), None);
        assert_eq!(from_array(Some([0.0, 0.0, f32::NAN, 9.0])), None);
    }
}
