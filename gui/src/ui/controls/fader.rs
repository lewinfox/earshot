//! A fader: a groove with tick marks and a cap you drag, vertical (channel volume,
//! tempo) or horizontal (crossfader).

use egui::{
    Align2, Color32, FontId, Rect, Response, Sense, Stroke, StrokeKind, Ui, Widget, pos2, vec2,
};

use super::{ARC, FACE, FACE_HOVER, GROOVE, LABEL_SIZE, Range, interact};
use crate::ui::util;

pub struct Fader<'a> {
    range: &'a Range,
    value: &'a mut f64,
    size: egui::Vec2,
    vertical: bool,
    /// Vertical faders normally go up for more; tempo faders go down for more (the
    /// Pioneer and Mixxx default).
    inverted: bool,
    ticks: usize,
    /// Text at points along the travel (0..1), drawn beside the groove.
    marks: &'a [(f64, &'a str)],
    colour: Color32,
}

impl<'a> Fader<'a> {
    pub fn vertical(range: &'a Range, value: &'a mut f64, size: egui::Vec2) -> Fader<'a> {
        Fader {
            range,
            value,
            size,
            vertical: true,
            inverted: false,
            ticks: 11,
            marks: &[],
            colour: ARC,
        }
    }

    pub fn horizontal(range: &'a Range, value: &'a mut f64, size: egui::Vec2) -> Fader<'a> {
        Fader {
            vertical: false,
            ..Fader::vertical(range, value, size)
        }
    }

    pub fn inverted(mut self, on: bool) -> Self {
        self.inverted = on;
        self
    }

    pub fn ticks(mut self, n: usize) -> Self {
        self.ticks = n;
        self
    }

    pub fn marks(mut self, marks: &'a [(f64, &'a str)]) -> Self {
        self.marks = marks;
        self
    }

    /// Colour of the line on the cap and the fill from the origin.
    pub fn colour(mut self, c: Color32) -> Self {
        self.colour = c;
        self
    }
}

/// Cap length along the groove, and how far the cap stays from the ends.
const CAP: f32 = 12.0;

impl Widget for Fader<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let (rect, resp) = ui.allocate_exact_size(self.size, Sense::click_and_drag());
        let (vertical, inverted) = (self.vertical, self.inverted);
        let len = if vertical {
            rect.height()
        } else {
            rect.width()
        } - CAP;
        // Points along the travel (towards "more") for a pointer movement.
        let along = move |d: egui::Vec2| match (vertical, inverted) {
            (true, false) => -d.y,
            (true, true) => d.y,
            (false, _) => d.x,
        };
        let resp = interact(ui, resp, self.range, self.value, along, len.max(1.0));
        if !ui.is_rect_visible(rect) {
            return resp;
        }
        let p = ui.painter();
        // Screen point for travel `t`.
        let at = |t: f64| -> egui::Pos2 {
            let t = t as f32;
            if vertical {
                let t = if inverted { t } else { 1.0 - t };
                pos2(rect.center().x, rect.top() + CAP / 2.0 + t * len)
            } else {
                pos2(rect.left() + CAP / 2.0 + t * len, rect.center().y)
            }
        };
        let across = if vertical {
            vec2(1.0, 0.0)
        } else {
            vec2(0.0, 1.0)
        };
        let thick = if vertical {
            rect.width()
        } else {
            rect.height()
        };

        for i in 0..self.ticks {
            let t = i as f64 / (self.ticks - 1).max(1) as f64;
            let c = at(t);
            let half = if i == 0 || i + 1 == self.ticks || (2 * i + 1 == self.ticks) {
                thick * 0.42
            } else {
                thick * 0.28
            };
            p.line_segment(
                [c - across * half, c + across * half],
                Stroke::new(1.0, util::LINE),
            );
        }
        for (t, text) in self.marks {
            let c = at(*t);
            // Outside the fader (left of a vertical one): leave room for it.
            let (pos, align) = if vertical {
                (c - across * (thick / 2.0 + 2.0), Align2::RIGHT_CENTER)
            } else {
                (c + across * (thick / 2.0 + 1.0), Align2::CENTER_TOP)
            };
            p.text(
                pos,
                align,
                *text,
                FontId::proportional(LABEL_SIZE),
                util::MUTED,
            );
        }
        // Groove, with the travel from the origin lit faintly.
        let groove_w = 4.0;
        let ends = [at(0.0), at(1.0)];
        p.line_segment(ends, Stroke::new(groove_w, GROOVE));
        let pos = self.range.to_pos(*self.value);
        p.line_segment(
            [at(self.range.origin()), at(pos)],
            Stroke::new(2.0, util::dim(self.colour, 0.6)),
        );
        // The cap.
        let c = at(pos);
        let cap_size = if vertical {
            vec2(thick - 2.0, CAP)
        } else {
            vec2(CAP, thick - 2.0)
        };
        let cap = Rect::from_center_size(c, cap_size);
        let face = if resp.hovered() || resp.dragged() {
            FACE_HOVER
        } else {
            FACE
        };
        p.rect(
            cap,
            2.0,
            face,
            Stroke::new(1.0, util::LINE),
            StrokeKind::Inside,
        );
        let half = (if vertical { cap_size.x } else { cap_size.y }) / 2.0 - 2.0;
        p.line_segment(
            [c - across * half, c + across * half],
            Stroke::new(2.0, self.colour),
        );
        resp
    }
}
