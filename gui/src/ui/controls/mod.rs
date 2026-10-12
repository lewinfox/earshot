//! Hardware-style controls, modelled on Mixxx's decks and mixer: knobs, faders, lit
//! buttons and level meters.
//!
//! Knobs and faders share one behaviour ([`interact`]): drag to change (Shift for fine
//! adjust), double-click to reset, mouse wheel to nudge, and a tooltip with the exact
//! value. What a control means (its range, default and how its value reads) is a
//! [`Range`]. They edit a plain `f64` and report `changed()`; the caller sends it on.

pub mod button;
pub mod fader;
pub mod knob;
pub mod meter;

pub use button::{HwButton, Icon, held};
pub use fader::Fader;
pub use knob::Knob;
pub use meter::Meter;

use egui::{Color32, Response, Ui, Vec2};

use super::util;

// ---- shared styling --------------------------------------------------------------------

/// Knob bodies and fader caps.
pub const FACE: Color32 = Color32::from_rgb(0x2a, 0x2c, 0x34);
pub const FACE_HOVER: Color32 = Color32::from_rgb(0x36, 0x39, 0x44);
/// Value arcs and the knob pointer.
pub const ARC: Color32 = util::ACCENT;
/// Fader grooves and meter backs.
pub const GROOVE: Color32 = Color32::from_rgb(0x0b, 0x0c, 0x0f);
/// The unlit part of a knob's arc.
pub const ARC_TRACK: Color32 = Color32::from_rgb(0x3a, 0x3d, 0x4a);
/// Unlit buttons.
pub const BUTTON: Color32 = Color32::from_rgb(0x22, 0x24, 0x2b);
pub const LABEL_SIZE: f32 = 9.0;

// ---- values -------------------------------------------------------------------------------

// What a knob or fader controls lives with the control registry, in core.
pub use crate::core::bus::Range;
#[cfg(test)]
use crate::core::bus::{fmt_db, fmt_filter};

// ---- shared behaviour -------------------------------------------------------------------

/// Points of drag for the whole range.
pub const DRAG_TRAVEL: f32 = 200.0;
/// Shift divides drags and wheel steps by this.
pub const FINE: f64 = 10.0;
/// Travel per point of scrolling (one wheel notch is a few dozen points): ~2 % a notch.
const WHEEL_PER_POINT: f64 = 0.02 / 50.0;

/// Drag state kept between frames: pointer last frame and travel so far. Kept apart
/// from the value so a drag isn't held back while the engine catches up.
#[derive(Clone, Copy)]
struct Drag {
    last: egui::Pos2,
    pos: f64,
}

/// The behaviour every knob and fader shares. `along` turns a pointer movement into
/// points along the control (positive = more), `travel` is points for the whole range.
/// Edits `value`, marks the response changed and adds the tooltip.
pub fn interact(
    ui: &Ui,
    mut resp: Response,
    range: &Range,
    value: &mut f64,
    along: impl Fn(Vec2) -> f32,
    travel: f32,
) -> Response {
    let fine = if ui.input(|i| i.modifiers.shift) {
        1.0 / FINE
    } else {
        1.0
    };
    let id = resp.id.with("drag");
    let before = *value;
    if resp.double_clicked() {
        *value = range.default;
        ui.ctx().data_mut(|d| d.remove::<Drag>(id));
    } else if resp.dragged() {
        let now = resp.interact_pointer_pos();
        let start = || Drag {
            // Count the movement egui held back before calling it a drag.
            last: ui
                .input(|i| i.pointer.press_origin())
                .or(now)
                .unwrap_or_default(),
            pos: range.to_pos(*value),
        };
        let mut drag = if resp.drag_started() {
            start()
        } else {
            ui.ctx()
                .data(|d| d.get_temp::<Drag>(id))
                .unwrap_or_else(start)
        };
        if let Some(p) = now {
            drag.pos =
                (drag.pos + along(p - drag.last) as f64 / travel as f64 * fine).clamp(0.0, 1.0);
            drag.last = p;
        }
        ui.ctx().data_mut(|d| d.insert_temp(id, drag));
        *value = range.from_pos(drag.pos);
    } else if resp.hovered() {
        let scroll = ui.input(|i| i.raw_scroll_delta);
        // Up or right is more, like dragging.
        let s = (scroll.y + scroll.x) as f64;
        if s != 0.0 {
            *value = range.from_pos(range.to_pos(*value) + s * WHEEL_PER_POINT * fine);
        }
    }
    if *value != before {
        resp.mark_changed();
    }
    let v = *value;
    resp.widget_info(|| egui::WidgetInfo::slider(true, v, range.name));
    resp.on_hover_text(range.tooltip(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centred_ranges_put_the_centre_mid_travel() {
        let eq = Range {
            name: "EQ",
            min: 0.0,
            max: 4.0,
            default: 1.0,
            centre: Some(1.0),
            format: fmt_db,
        };
        assert_eq!(eq.to_pos(1.0), 0.5);
        assert_eq!(eq.from_pos(0.75), 2.5);
        assert_eq!(eq.from_pos(0.25), 0.5);
        for v in [0.0, 0.3, 1.0, 3.7, 4.0] {
            assert!((eq.from_pos(eq.to_pos(v)) - v).abs() < 1e-12);
        }
        assert_eq!(fmt_db(1.0), "+0.0 dB");
        assert_eq!(fmt_filter(0.0), "off");
        assert_eq!(fmt_filter(-0.5), "low-pass 50 %");
    }
}
