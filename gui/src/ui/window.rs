//! The window itself: how small it can be, how big it starts, the UI size (egui's
//! zoom), and keeping all of it inside the screen.
//!
//! Sizes here are in points at the UI size in force: egui's units, which the layout
//! (`ui::layout`) is worked out in. At UI size 1.25 a point is 1.25 times as big on
//! screen, so the same window holds fewer of them and the layout gets less room; the
//! smallest window stays [`MIN_SIZE`] points, so it grows on screen with the UI size.
//! That's why the UI size is capped by the screen ([`zoom_for`]): the smallest window
//! must still fit on it.

use egui::{Vec2, ViewportCommand, vec2};

/// The smallest the window can be, which every view must lay out in
/// (`ui::app::tests::every_view_fits_every_window`).
pub const MIN_SIZE: Vec2 = vec2(1024.0, 640.0);
/// The window's size on a first run (smaller on a smaller screen).
pub const DEFAULT_SIZE: Vec2 = vec2(1440.0, 900.0);
/// How much of the screen a window can take: the rest is the title bar's and the
/// desktop's panels'.
const SCREEN_SHARE: Vec2 = vec2(0.98, 0.92);

/// The UI size to draw at: the one `wanted`, but no bigger than lets the smallest
/// window fit a screen of `monitor` points (at UI size 1; `None` while it isn't known).
pub fn zoom_for(wanted: f32, monitor: Option<Vec2>) -> f32 {
    let Some(m) = monitor.filter(|m| m.x > 0.0 && m.y > 0.0) else {
        return wanted;
    };
    let room = m * SCREEN_SHARE / MIN_SIZE;
    // A screen too small even at the smallest UI size: smaller still, so it all shows.
    wanted.min(room.x.min(room.y)).max(0.5)
}

/// The size a window of `inner` points should be made, if it's bigger than a screen of
/// `monitor` points (both at the same UI size).
pub fn size_on_screen(inner: Vec2, monitor: Vec2) -> Option<Vec2> {
    if monitor.x <= 0.0 || monitor.y <= 0.0 {
        return None;
    }
    let over = inner.x > monitor.x + 1.0 || inner.y > monitor.y + 1.0;
    over.then(|| inner.min(monitor * SCREEN_SHARE).max(MIN_SIZE.min(monitor)))
}

/// Keeps the window to the screen and the UI size applied, frame by frame.
#[derive(Default)]
pub struct WindowFit {
    /// Frames left to tell the window its smallest size (again after the UI size
    /// changes: the size is in points, and the window keeps it in pixels).
    tell_min: u8,
    started: bool,
    /// The size last asked for, so a system that won't give it isn't asked for ever.
    asked: Option<Vec2>,
}

impl WindowFit {
    /// Call once a frame, before anything is drawn. `wanted`: the UI size chosen.
    pub fn fit(&mut self, ctx: &egui::Context, wanted: f32) {
        let zoom_now = ctx.zoom_factor();
        let (monitor, inner, free) = ctx.input(|i| {
            let v = i.viewport();
            let free = !v.fullscreen.unwrap_or(false) && !v.maximized.unwrap_or(false);
            (v.monitor_size, v.inner_rect.map(|r| r.size()), free)
        });
        // The screen in points at UI size 1 (egui gives it at the size in force).
        let zoom = zoom_for(wanted, monitor.map(|m| m * zoom_now));
        if (zoom - zoom_now).abs() > 0.001 {
            ctx.set_zoom_factor(zoom);
            self.tell_min = 2;
            self.asked = None;
        }
        if !self.started {
            self.started = true;
            self.tell_min = 2;
        }
        if self.tell_min > 0 {
            self.tell_min -= 1;
            ctx.send_viewport_cmd(ViewportCommand::MinInnerSize(MIN_SIZE));
            return;
        }
        // A window bigger than the screen (a size saved on a bigger one): brought in.
        if let (Some(m), Some(inner), true) = (monitor, inner, free)
            && let Some(size) = size_on_screen(inner, m)
            && self.asked != Some(size)
        {
            self.asked = Some(size);
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ui_size_is_capped_by_the_screen() {
        // A big screen: as asked.
        assert_eq!(zoom_for(1.5, Some(vec2(2560.0, 1440.0))), 1.5);
        assert_eq!(zoom_for(1.5, None), 1.5);
        // A 13" laptop (1280 x 800 points): the smallest window must still fit.
        let z = zoom_for(1.5, Some(vec2(1280.0, 800.0)));
        assert!(z < 1.5 && z >= 1.0, "{z}");
        assert!(MIN_SIZE.x * z <= 1280.0 && MIN_SIZE.y * z <= 800.0);
        // Normal size is untouched there.
        assert_eq!(zoom_for(1.0, Some(vec2(1280.0, 800.0))), 1.0);
    }

    #[test]
    fn a_window_bigger_than_the_screen_is_brought_in() {
        let screen = vec2(1440.0, 900.0);
        assert_eq!(size_on_screen(vec2(1200.0, 800.0), screen), None);
        // Full screen is exactly the screen: left alone.
        assert_eq!(size_on_screen(screen, screen), None);
        let s = size_on_screen(vec2(1840.0, 1000.0), screen).unwrap();
        assert!(s.x <= screen.x && s.y <= screen.y, "{s:?}");
        assert!(s.x >= MIN_SIZE.x && s.y >= MIN_SIZE.y, "{s:?}");
    }
}
