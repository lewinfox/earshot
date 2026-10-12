//! tempo-lock's pictures, painted: the waveform with its beat grid and the overview
//! under it, tempo over time, and the histogram of beat intervals. Each takes the
//! space it's given and returns its response; the tab turns clicks and drags into
//! seeks and zooms.

use egui::{Align2, Color32, CornerRadius, FontId, Pos2, Rect, Response, Sense, Shape, Stroke, Ui};

use crate::core::tempo_lock::{Peaks, TempoAnalysis, nice_step};
use crate::ui::theme::Theme;

/// "1:23.456".
pub fn fmt_time(t: f64) -> String {
    if !t.is_finite() {
        return "0:00.000".into();
    }
    let t = t.max(0.0);
    let m = (t / 60.0).floor();
    format!("{m:.0}:{:06.3}", t - m * 60.0)
}

fn small(size: f32) -> FontId {
    FontId::proportional(size)
}

/// x of time `t` in a window `(v0, v1)` across `rect`.
fn x_of(rect: Rect, view: (f64, f64), t: f64) -> f32 {
    rect.left() + ((t - view.0) / (view.1 - view.0)) as f32 * rect.width()
}

/// The time under `x`.
pub fn time_at(rect: Rect, view: (f64, f64), x: f32) -> f64 {
    view.0 + ((x - rect.left()) / rect.width()) as f64 * (view.1 - view.0)
}

fn vline(p: &egui::Painter, x: f32, y0: f32, y1: f32, width: f32, c: Color32) {
    p.line_segment([Pos2::new(x, y0), Pos2::new(x, y1)], Stroke::new(width, c));
}

fn dashed(p: &egui::Painter, a: Pos2, b: Pos2, c: Color32) {
    p.extend(Shape::dashed_line(
        &[a, b],
        Stroke::new(1.0_f32, c),
        4.0,
        4.0,
    ));
}

/// The min and max of the peaks between times `t0` and `t1`, -1..1.
fn peak_range(peaks: &Peaks, duration: f64, t0: f64, t1: f64) -> Option<(f32, f32)> {
    let n = peaks.min.len().min(peaks.max.len());
    if n == 0 || duration <= 0.0 {
        return None;
    }
    let b0 = ((t0 / duration) * n as f64).floor().max(0.0) as usize;
    let b1 = (((t1 / duration) * n as f64).floor() as usize)
        .max(b0 + 1)
        .min(n);
    if b0 >= n {
        return None;
    }
    let (mut lo, mut hi) = (i8::MAX, i8::MIN);
    for b in b0..b1 {
        lo = lo.min(peaks.min[b]);
        hi = hi.max(peaks.max[b]);
    }
    (lo <= hi).then(|| (lo as f32 / 127.0, hi as f32 / 127.0))
}

// ---- the waveform ------------------------------------------------------------------------

/// A version's waveform and beat grid, between two times.
pub struct Wave<'a> {
    pub peaks: &'a Peaks,
    pub duration: f64,
    pub beats: &'a [f64],
    pub downbeats: &'a [bool],
    /// Detections thrown away: small red ticks at the bottom.
    pub dropped: &'a [f64],
    /// What's shown, in this version's time.
    pub view: (f64, f64),
    /// Where the player is in this version's time, and whether it's this one playing.
    pub playhead: Option<(f64, bool)>,
    pub colour: Color32,
}

impl Wave<'_> {
    pub fn show(self, ui: &mut Ui, theme: &Theme, height: f32) -> Response {
        let (rect, resp) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), height),
            Sense::click_and_drag(),
        );
        let p = ui.painter_at(rect);
        p.rect_filled(rect, CornerRadius::same(4), theme.wave_bg);
        let (v0, v1) = self.view;
        let mid = rect.center().y;
        let half = rect.height() / 2.0 * 0.95;
        let w = rect.width().max(1.0);
        let span = v1 - v0;
        // One bar per point across.
        let mut x = 0.0;
        while x < w {
            let t0 = v0 + (x / w) as f64 * span;
            let t1 = v0 + ((x + 1.0) / w) as f64 * span;
            if let Some((lo, hi)) = peak_range(self.peaks, self.duration, t0, t1) {
                let top = mid - hi * half;
                let bottom = (mid - lo * half).max(top + 1.0);
                p.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(rect.left() + x, top),
                        Pos2::new(rect.left() + x + 1.0, bottom),
                    ),
                    CornerRadius::ZERO,
                    self.colour,
                );
            }
            x += 1.0;
        }

        // The grid: thick lines on downbeats, with bar numbers when there's room.
        let i0 = self.beats.partition_point(|&b| b < v0);
        let i1 = self.beats.partition_point(|&b| b < v1);
        let numbers = ((i1 - i0) as f32) < w / 28.0;
        let mut bar = self.downbeats[..i0.min(self.downbeats.len())]
            .iter()
            .filter(|d| **d)
            .count();
        let beat_c = theme.accent.gamma_multiply(0.55);
        for i in i0..i1 {
            let x = x_of(rect, self.view, self.beats[i]);
            let down = self.downbeats.get(i).copied().unwrap_or(false);
            if down {
                bar += 1;
                vline(&p, x, rect.top(), rect.bottom(), 2.0, theme.accent);
                if numbers {
                    p.text(
                        Pos2::new(x + 3.0, rect.top() + 2.0),
                        Align2::LEFT_TOP,
                        bar.to_string(),
                        small(11.0),
                        theme.accent,
                    );
                }
            } else {
                vline(&p, x, rect.top(), rect.bottom(), 1.0, beat_c);
            }
        }
        for &d in self.dropped {
            if d >= v0 && d <= v1 {
                let x = x_of(rect, self.view, d);
                vline(&p, x, rect.bottom() - 8.0, rect.bottom(), 1.0, theme.bad);
            }
        }

        // A time ruler along the bottom.
        let step = nice_step(span / (w as f64 / 90.0));
        let mut t = (v0 / step).ceil() * step;
        while t < v1 {
            let x = x_of(rect, self.view, t);
            vline(
                &p,
                x,
                rect.bottom() - 14.0,
                rect.bottom() - 10.0,
                1.0,
                theme.muted,
            );
            let label = fmt_time(t);
            let label = if step < 1.0 {
                label[..label.len() - 2].to_string()
            } else {
                label[..label.len() - 4].to_string()
            };
            p.text(
                Pos2::new(x + 2.0, rect.bottom() - 2.0),
                Align2::LEFT_BOTTOM,
                label,
                small(10.0),
                theme.muted,
            );
            t += step;
        }

        if let Some((at, active)) = self.playhead
            && at >= v0
            && at <= v1
        {
            let c = if active {
                theme.marker
            } else {
                theme.marker.gamma_multiply(0.45)
            };
            vline(
                &p,
                x_of(rect, self.view, at),
                rect.top(),
                rect.bottom(),
                1.5,
                c,
            );
        }
        resp
    }
}

/// The whole track small, with the part the waveform shows outlined.
pub fn overview(
    ui: &mut Ui,
    theme: &Theme,
    peaks: &Peaks,
    duration: f64,
    view: (f64, f64),
) -> Response {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 22.0),
        Sense::click_and_drag(),
    );
    let p = ui.painter_at(rect);
    p.rect_filled(rect, CornerRadius::same(3), theme.wave_bg);
    let w = rect.width().max(1.0);
    let whole = (0.0, duration);
    let mut x = 0.0;
    while x < w {
        let t0 = (x / w) as f64 * duration;
        let t1 = ((x + 1.0) / w) as f64 * duration;
        if let Some((lo, hi)) = peak_range(peaks, duration, t0, t1) {
            let (c, h) = (rect.center().y, rect.height() / 2.0);
            p.rect_filled(
                Rect::from_min_max(
                    Pos2::new(rect.left() + x, c - hi * h),
                    Pos2::new(rect.left() + x + 1.0, (c - lo * h).max(c - hi * h + 1.0)),
                ),
                CornerRadius::ZERO,
                theme.muted.gamma_multiply(0.6),
            );
        }
        x += 1.0;
    }
    let win = Rect::from_x_y_ranges(
        x_of(rect, whole, view.0.max(0.0))..=x_of(rect, whole, view.1.min(duration)),
        rect.y_range(),
    );
    p.rect_filled(
        win,
        CornerRadius::ZERO,
        theme.selection.gamma_multiply(0.25),
    );
    p.rect_stroke(
        win,
        CornerRadius::ZERO,
        Stroke::new(1.0_f32, theme.selection),
        egui::StrokeKind::Inside,
    );
    resp
}

// ---- tempo over time ---------------------------------------------------------------------

/// The tempo between each pair of beats across the track.
pub struct TempoChart<'a> {
    pub a: &'a TempoAnalysis,
    /// The tempo asked for, at the detected beats' level.
    pub target: f64,
    /// [`crate::core::tempo_lock::BeatLevel::factor`], noted on the target's label.
    pub level: f64,
    /// BPM shown, bottom to top (shared with the other version's chart).
    pub range: (f64, f64),
    /// The part of the track the waveform shows, shaded.
    pub window: (f64, f64),
    pub playhead: Option<f64>,
    pub colour: Color32,
}

/// BPM range for the tempo charts of these analyses, with their targets: padded by a
/// quarter (at least 1 BPM) each side, as tempo-lock's.
pub fn bpm_range(charted: &[(&TempoAnalysis, f64)]) -> (f64, f64) {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for (a, target) in charted {
        lo = lo.min(a.min_bpm).min(*target);
        hi = hi.max(a.max_bpm).max(*target);
        for s in &a.sections {
            lo = lo.min(s.bpm);
            hi = hi.max(s.bpm);
        }
    }
    if !lo.is_finite() {
        return (0.0, 1.0);
    }
    let pad = ((hi - lo) * 0.25).max(1.0);
    (lo - pad, hi + pad)
}

impl TempoChart<'_> {
    pub fn show(self, ui: &mut Ui, theme: &Theme, height: f32) -> Response {
        let (rect, resp) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::click());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, CornerRadius::same(4), theme.wave_bg);
        let a = self.a;
        let whole = (0.0, a.duration.max(1e-6));
        let (lo, hi) = self.range;
        let y = |b: f64| rect.bottom() - ((b - lo) / (hi - lo)) as f32 * rect.height();
        let x = |t: f64| x_of(rect, whole, t);
        // Steady stretches (only worth showing when there's more than one).
        if a.sections.len() > 1 {
            for (i, s) in a.sections.iter().enumerate() {
                let c = theme.sections[i % theme.sections.len()];
                p.rect_filled(
                    Rect::from_x_y_ranges(x(s.start)..=x(s.end), rect.y_range()),
                    CornerRadius::ZERO,
                    c.gamma_multiply(0.16),
                );
                p.line_segment(
                    [
                        Pos2::new(x(s.start), y(s.bpm)),
                        Pos2::new(x(s.end), y(s.bpm)),
                    ],
                    Stroke::new(1.0_f32, c.gamma_multiply(0.7)),
                );
            }
        }
        // BPM gridlines.
        let step = nice_step((hi - lo) / 4.0);
        let mut b = (lo / step).ceil() * step;
        while b < hi {
            p.line_segment(
                [Pos2::new(rect.left(), y(b)), Pos2::new(rect.right(), y(b))],
                Stroke::new(1.0_f32, theme.line),
            );
            p.text(
                Pos2::new(rect.left() + 4.0, (y(b) - 2.0).max(rect.top() + 10.0)),
                Align2::LEFT_BOTTOM,
                format!("{b:.*}", if step < 1.0 { 1 } else { 0 }),
                small(10.0),
                theme.muted,
            );
            b += step;
        }
        p.rect_filled(
            Rect::from_x_y_ranges(x(self.window.0)..=x(self.window.1), rect.y_range()),
            CornerRadius::ZERO,
            theme.selection.gamma_multiply(0.15),
        );
        let points: Vec<Pos2> = a
            .bpm_times
            .iter()
            .zip(&a.bpm_curve)
            .map(|(&t, &v)| Pos2::new(x(t), y(v.clamp(lo, hi))))
            .collect();
        p.add(Shape::line(points, Stroke::new(1.5_f32, self.colour)));
        // The target.
        let ty = y(self.target);
        dashed(
            &p,
            Pos2::new(rect.left(), ty),
            Pos2::new(rect.right(), ty),
            theme.accent,
        );
        let level = if self.level != 1.0 {
            format!(" (detected level, ×{})", self.level)
        } else {
            String::new()
        };
        p.text(
            Pos2::new(rect.right() - 4.0, ty - 3.0),
            Align2::RIGHT_BOTTOM,
            format!("{:.2} BPM{level}", self.target),
            small(10.0),
            theme.accent,
        );
        // Deliberate tempo changes: a dotted line and a warning mark.
        for c in &a.tempo_changes {
            let cx = x(c.time);
            dashed(
                &p,
                Pos2::new(cx, rect.top() + 12.0),
                Pos2::new(cx, rect.bottom()),
                theme.warn,
            );
            warning_mark(&p, Pos2::new(cx, rect.top() + 2.0), theme.warn);
        }
        if let Some(t) = self.playhead {
            vline(&p, x(t), rect.top(), rect.bottom(), 1.5, theme.marker);
        }
        resp
    }
}

/// A small warning triangle, its top at `top`.
pub fn warning_mark(p: &egui::Painter, top: Pos2, c: Color32) {
    let (w, h) = (5.0, 9.0);
    p.add(Shape::convex_polygon(
        vec![
            top,
            Pos2::new(top.x + w, top.y + h),
            Pos2::new(top.x - w, top.y + h),
        ],
        c,
        Stroke::NONE,
    ));
}

// ---- the histogram -----------------------------------------------------------------------

/// How many beat intervals came out at each tempo, stacked by section.
pub struct HistChart<'a> {
    pub a: &'a TempoAnalysis,
    pub target: f64,
    /// (lo, hi, bin width) from [`crate::core::tempo_lock::histogram_range`], shared
    /// with the other version's histogram.
    pub range: (f64, f64, f64),
    pub colour: Color32,
}

impl HistChart<'_> {
    pub fn show(self, ui: &mut Ui, theme: &Theme, height: f32) -> Response {
        let (rect, resp) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::hover());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, CornerRadius::same(4), theme.wave_bg);
        let (lo, hi, bw) = self.range;
        let h = self.a.histogram(lo, hi, bw);
        let bins = h.counts.first().map_or(0, |c| c.len());
        if bins == 0 {
            return resp;
        }
        let sections = h.counts.len();
        let peak = (0..bins)
            .map(|i| h.counts.iter().map(|c| c[i]).sum::<u32>())
            .max()
            .unwrap_or(1)
            .max(1) as f32;
        let (pad_l, pad_b) = (6.0, 16.0);
        let pad_t = if sections > 1 { 16.0 } else { 6.0 };
        let plot_h = rect.height() - pad_b - pad_t;
        let col_w = (rect.width() - pad_l * 2.0) / bins as f32;
        let x = |bpm: f64| rect.left() + pad_l + ((bpm - h.first_bpm) / h.bin_width) as f32 * col_w;
        let base = rect.bottom() - pad_b;
        for i in 0..bins {
            let mut top = base;
            for (s, c) in h.counts.iter().enumerate() {
                if c[i] == 0 {
                    continue;
                }
                let bh = c[i] as f32 / peak * plot_h;
                let colour = if sections > 1 {
                    theme.sections[s % theme.sections.len()]
                } else {
                    self.colour
                };
                let x0 = rect.left() + pad_l + i as f32 * col_w;
                p.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(x0 + 0.5, top - bh),
                        Pos2::new(x0 + (col_w - 1.0).max(1.0), top),
                    ),
                    CornerRadius::ZERO,
                    colour.gamma_multiply(0.85),
                );
                top -= bh;
            }
        }
        // BPM labels.
        let step = nice_step((hi - lo) / 4.0);
        let end = h.first_bpm + bins as f64 * h.bin_width;
        let mut b = (h.first_bpm / step).ceil() * step;
        while b <= end {
            let bx = x(b);
            if bx >= rect.left() + pad_l && bx <= rect.right() - pad_l {
                vline(&p, bx, base, base + 3.0, 1.0, theme.muted);
                p.text(
                    Pos2::new(bx, rect.bottom() - 2.0),
                    Align2::CENTER_BOTTOM,
                    format!("{b:.*}", if step < 1.0 { 1 } else { 0 }),
                    small(10.0),
                    theme.muted,
                );
            }
            b += step;
        }
        let tx = x(self.target);
        dashed(
            &p,
            Pos2::new(tx, rect.top() + 4.0),
            Pos2::new(tx, base),
            theme.accent,
        );
        if sections > 1 {
            for (s, sec) in self.a.sections.iter().enumerate() {
                p.text(
                    Pos2::new(
                        x(sec.bpm).clamp(rect.left() + 16.0, rect.right() - 16.0),
                        rect.top() + 2.0,
                    ),
                    Align2::CENTER_TOP,
                    format!("{:.1}", sec.bpm),
                    small(10.0),
                    theme.sections[s % theme.sections.len()],
                );
            }
        }
        resp
    }
}
