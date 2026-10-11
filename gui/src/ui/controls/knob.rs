//! A round knob with a value arc, like Mixxx's EQ and gain knobs. Drag up or right
//! to turn it up, down or left to turn it down.

use std::f32::consts::PI;

use egui::{Align2, Color32, FontId, Response, Sense, Shape, Stroke, Ui, Widget, pos2, vec2};

use super::{ARC, ARC_TRACK, DRAG_TRAVEL, FACE, FACE_HOVER, LABEL_SIZE, Range, interact};
use crate::ui::util;

/// The arc runs 135° either side of straight up, like most hardware.
const SWEEP: f32 = 0.75 * PI;

pub struct Knob<'a> {
    range: &'a Range,
    value: &'a mut f64,
    diameter: f32,
    label: Option<&'a str>,
    colour: Color32,
}

impl<'a> Knob<'a> {
    pub fn new(range: &'a Range, value: &'a mut f64) -> Knob<'a> {
        Knob {
            range,
            value,
            diameter: 26.0,
            label: None,
            colour: ARC,
        }
    }

    pub fn diameter(mut self, d: f32) -> Self {
        self.diameter = d;
        self
    }

    /// A short caption under the knob.
    pub fn label(mut self, label: &'a str) -> Self {
        self.label = Some(label);
        self
    }

    /// Arc colour.
    pub fn colour(mut self, c: Color32) -> Self {
        self.colour = c;
        self
    }
}

/// Angle (radians, screen coordinates) for travel `p` (0..1).
fn angle(p: f64) -> f32 {
    -PI / 2.0 - SWEEP + 2.0 * SWEEP * p as f32
}

fn arc(centre: egui::Pos2, r: f32, from: f64, to: f64) -> Vec<egui::Pos2> {
    let (a, b) = (angle(from.min(to)), angle(from.max(to)));
    let n = (((b - a) / 0.08).ceil() as usize).max(1);
    (0..=n)
        .map(|i| {
            let t = a + (b - a) * i as f32 / n as f32;
            centre + vec2(t.cos(), t.sin()) * r
        })
        .collect()
}

impl Widget for Knob<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let label_h = if self.label.is_some() {
            LABEL_SIZE + 2.0
        } else {
            0.0
        };
        let size = vec2(self.diameter + 4.0, self.diameter + 4.0 + label_h);
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
        // Up or right turns it up.
        let resp = interact(ui, resp, self.range, self.value, |d| d.x - d.y, DRAG_TRAVEL);
        if !ui.is_rect_visible(rect) {
            return resp;
        }
        let p = ui.painter();
        let c = pos2(rect.center().x, rect.top() + 2.0 + self.diameter / 2.0);
        let r = self.diameter / 2.0;
        let pos = self.range.to_pos(*self.value);

        // Groove, then the lit arc from the origin (the centre for detented knobs).
        p.add(Shape::line(
            arc(c, r, 0.0, 1.0),
            Stroke::new(3.0_f32, ARC_TRACK),
        ));
        let origin = self.range.origin();
        if (pos - origin).abs() > 1e-3 {
            p.add(Shape::line(
                arc(c, r, origin, pos),
                Stroke::new(3.0_f32, self.colour),
            ));
        }
        let face = if resp.hovered() || resp.dragged() {
            FACE_HOVER
        } else {
            FACE
        };
        p.circle(c, r - 4.0, face, Stroke::new(1.0_f32, util::LINE));
        let a = angle(pos);
        let dir = vec2(a.cos(), a.sin());
        p.line_segment(
            [c + dir * (r - 12.0).max(1.0), c + dir * (r - 4.5)],
            Stroke::new(2.0_f32, util::TEXT),
        );
        if let Some(label) = self.label {
            p.text(
                pos2(c.x, rect.bottom()),
                Align2::CENTER_BOTTOM,
                label,
                FontId::proportional(LABEL_SIZE),
                util::MUTED,
            );
        }
        resp
    }
}

#[cfg(test)]
mod tests {
    use egui::{Event, Modifiers, PointerButton, Pos2, vec2};
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::super::{FINE, Range, fmt_db};
    use super::*;

    const EQ: Range = Range {
        name: "EQ",
        min: 0.0,
        max: 4.0,
        default: 1.0,
        centre: Some(1.0),
        format: fmt_db,
    };

    fn harness(start: f64) -> Harness<'static, f64> {
        // Real frame times, so two clicks land inside the double-click window.
        Harness::builder().with_step_dt(1.0 / 60.0).build_ui_state(
            |ui, v: &mut f64| {
                ui.add(Knob::new(&EQ, v));
            },
            start,
        )
    }

    fn centre(h: &Harness<'_, f64>) -> Pos2 {
        h.get_by_label("EQ").rect().center()
    }

    /// Press on the knob, move by `delta` in small steps, release. Returns the value.
    fn drag(h: &mut Harness<'_, f64>, delta: egui::Vec2, modifiers: Modifiers) -> f64 {
        let c = centre(h);
        h.hover_at(c);
        h.event(Event::PointerButton {
            pos: c,
            button: PointerButton::Primary,
            pressed: true,
            modifiers,
        });
        h.run_steps(1);
        for i in 1..=10 {
            h.event_modifiers(
                Event::PointerMoved(c + delta * (i as f32 / 10.0)),
                modifiers,
            );
        }
        h.run_steps(1);
        h.drop_at(c + delta);
        h.run_steps(1);
        *h.state()
    }

    #[test]
    fn up_and_right_turn_it_up() {
        let start = 1.0;
        for (delta, up) in [
            (vec2(0.0, -40.0), true),
            (vec2(40.0, 0.0), true),
            (vec2(0.0, 40.0), false),
            (vec2(-40.0, 0.0), false),
        ] {
            let mut h = harness(start);
            let v = drag(&mut h, delta, Modifiers::NONE);
            assert_eq!(v > start, up, "{delta:?} gave {v}");
            assert!(v != start);
        }
    }

    #[test]
    fn whole_drag_counts() {
        // 40 points up is 40 / DRAG_TRAVEL of the travel, from the centre.
        let mut h = harness(1.0);
        let v = drag(&mut h, vec2(0.0, -40.0), Modifiers::NONE);
        let want = EQ.from_pos(0.5 + 40.0 / DRAG_TRAVEL as f64);
        assert!((v - want).abs() < 1e-3, "{v} vs {want}");
    }

    #[test]
    fn shift_is_fine() {
        let mut h = harness(1.0);
        let coarse = drag(&mut h, vec2(0.0, -40.0), Modifiers::NONE) - 1.0;
        let mut h = harness(1.0);
        let fine = drag(&mut h, vec2(0.0, -40.0), Modifiers::SHIFT) - 1.0;
        assert!(fine > 0.0);
        assert!(
            (coarse / fine - FINE).abs() < 0.5,
            "coarse {coarse}, fine {fine}"
        );
    }

    #[test]
    fn double_click_resets() {
        let mut h = harness(3.0);
        let c = centre(&h);
        for _ in 0..2 {
            h.hover_at(c);
            h.drag_at(c);
            h.event(Event::PointerButton {
                pos: c,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            });
        }
        h.run_steps(2);
        assert_eq!(*h.state(), EQ.default);
    }

    #[test]
    fn wheel_nudges() {
        let mut h = harness(1.0);
        h.hover_at(centre(&h));
        h.event(Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: vec2(0.0, 1.0),
            modifiers: Modifiers::NONE,
        });
        h.run_steps(2);
        let up = *h.state();
        assert!(up > 1.0 && up < 1.5, "{up}");
    }
}
