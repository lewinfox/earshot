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

/// What a knob or fader controls: its range, default and how its value reads.
#[derive(Clone, Copy)]
pub struct Range {
    pub name: &'static str,
    pub min: f64,
    pub max: f64,
    /// Double-click goes back here.
    pub default: f64,
    /// The value at the middle of the travel, for controls whose middle means "as is"
    /// (EQ, filter, balance). The travel either side maps linearly to min..centre and
    /// centre..max, and value arcs start from it.
    pub centre: Option<f64>,
    pub format: fn(f64) -> String,
}

impl Range {
    /// Value to travel, 0..1.
    pub fn to_pos(&self, v: f64) -> f64 {
        let v = v.clamp(self.min, self.max);
        match self.centre {
            Some(c) if v < c => 0.5 * (v - self.min) / (c - self.min),
            Some(c) => 0.5 + 0.5 * (v - c) / (self.max - c),
            None => (v - self.min) / (self.max - self.min),
        }
    }

    /// Travel (0..1) to value.
    pub fn from_pos(&self, p: f64) -> f64 {
        let p = p.clamp(0.0, 1.0);
        match self.centre {
            Some(c) if p < 0.5 => self.min + (c - self.min) * p * 2.0,
            Some(c) => c + (self.max - c) * (p - 0.5) * 2.0,
            None => self.min + (self.max - self.min) * p,
        }
    }

    /// Where value arcs and fills start, as travel.
    pub fn origin(&self) -> f64 {
        self.centre.map_or(0.0, |c| self.to_pos(c))
    }

    pub fn tooltip(&self, v: f64) -> String {
        format!(
            "{}: {}\nDrag (Shift: fine) or scroll · double-click resets to {}",
            self.name,
            (self.format)(v),
            (self.format)(self.default)
        )
    }
}

// ---- value formats ------------------------------------------------------------------------

/// Linear gain as decibels.
pub fn fmt_db(v: f64) -> String {
    if v <= 1e-4 {
        "-∞ dB (kill)".into()
    } else {
        format!("{:+.1} dB", 20.0 * v.log10())
    }
}

pub fn fmt_percent(v: f64) -> String {
    format!("{:.0} %", v * 100.0)
}

/// Mixxx's filter knob (0..1, off at 0.5).
pub fn fmt_filter(v: f64) -> String {
    let x = v * 2.0 - 1.0;
    if x.abs() < 0.02 {
        "off".into()
    } else {
        format!(
            "{} {:.0} %",
            if x < 0.0 { "low-pass" } else { "high-pass" },
            x.abs() * 100.0
        )
    }
}

/// -1 left .. 1 right.
pub fn fmt_balance(v: f64) -> String {
    if v.abs() < 0.005 {
        "centre".into()
    } else {
        format!(
            "{:.0} % {}",
            v.abs() * 100.0,
            if v < 0.0 { "left" } else { "right" }
        )
    }
}

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
        assert_eq!(fmt_filter(0.5), "off");
    }
}
