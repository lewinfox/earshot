//! A segmented level meter (VU), green to yellow to red.

use egui::{Color32, Rect, Response, Sense, Ui, Widget, pos2};

use super::GROOVE;
use crate::ui::util;

const SEGMENTS: usize = 16;

pub struct Meter {
    /// Peak level, linear: 1.0 is full scale.
    level: f64,
    size: egui::Vec2,
}

impl Meter {
    pub fn new(level: f64, size: egui::Vec2) -> Meter {
        Meter { level, size }
    }
}

/// Segments lit for a linear peak level: the meter spans -36 dB..0 dB.
pub fn lit(level: f64) -> usize {
    if level <= 0.0 {
        return 0;
    }
    let db = 20.0 * level.log10();
    (((db + 36.0) / 36.0 * SEGMENTS as f64).ceil().max(0.0) as usize).min(SEGMENTS)
}

impl Widget for Meter {
    fn ui(self, ui: &mut Ui) -> Response {
        let (rect, resp) = ui.allocate_exact_size(self.size, Sense::hover());
        let level = self.level;
        let resp = resp.on_hover_text(if level > 1e-4 {
            format!("Peak {:+.1} dB", 20.0 * level.log10())
        } else {
            "Silent".into()
        });
        if !ui.is_rect_visible(rect) {
            return resp;
        }
        let p = ui.painter();
        p.rect_filled(rect, 1.0, GROOVE);
        let n = lit(level);
        let h = rect.height() / SEGMENTS as f32;
        for i in 0..SEGMENTS {
            let colour = match i {
                _ if i + 1 == SEGMENTS => util::BAD,
                _ if i + 4 >= SEGMENTS => util::WARN,
                _ => util::OK,
            };
            let y = rect.bottom() - (i as f32 + 1.0) * h;
            let seg = Rect::from_min_max(
                pos2(rect.left() + 1.0, y + 1.0),
                pos2(rect.right() - 1.0, y + h),
            );
            p.rect_filled(
                seg,
                0.0,
                if i < n {
                    colour
                } else {
                    Color32::from_rgb(0x1a, 0x1b, 0x20)
                },
            );
        }
        resp
    }
}
