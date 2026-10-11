//! A segmented level meter (VU): green, yellow for the last 6 dB below full scale,
//! red only above it (over 0 dB: the master limiter is turning it down).

use egui::{Color32, Rect, Response, Sense, Ui, Widget, pos2};

use super::GROOVE;
use crate::ui::util;

/// 2 dB a segment from -32 to +4 dB, so 0 dB falls between two segments.
const SEGMENTS: usize = 18;
const MIN_DB: f64 = -32.0;
const MAX_DB: f64 = 4.0;

fn segment_db(i: usize) -> f64 {
    MIN_DB + (MAX_DB - MIN_DB) * i as f64 / SEGMENTS as f64
}

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

/// Segments lit for a linear peak level.
pub fn lit(level: f64) -> usize {
    if level <= 0.0 {
        return 0;
    }
    let db = 20.0 * level.log10();
    (((db - MIN_DB) / (MAX_DB - MIN_DB) * SEGMENTS as f64)
        .ceil()
        .max(0.0) as usize)
        .min(SEGMENTS)
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
            let colour = match segment_db(i) {
                db if db >= 0.0 => util::BAD,
                db if db >= -6.0 => util::WARN,
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
        // A tick at 0 dB (full scale).
        let zero = (0.0 - MIN_DB) / (MAX_DB - MIN_DB);
        let y = rect.bottom() - zero as f32 * rect.height();
        p.line_segment(
            [pos2(rect.left(), y), pos2(rect.right(), y)],
            egui::Stroke::new(1.0, util::TEXT),
        );
        resp
    }
}
