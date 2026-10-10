//! Waveform drawing.
//!
//! The scrolling waveform is always a thin line through the real signal, built each
//! frame from the 2000-per-second min/max summary: for every pixel column, the line's
//! range across that slice of time, with the bins' full range drawn faintly behind it
//! (so hi-hats still show). Zoomed in, the line traces individual cycles; zoomed out it
//! fills in where the signal oscillates densely. Colour comes from the 150-per-second
//! band summary. A few thousand rectangles per frame, so cost doesn't depend on track
//! length.

use std::sync::Arc;

use earshot_engine::beatgrid::Beatgrid;
use egui::{
    Color32, ColorImage, Mesh, Painter, Pos2, Rect, Shape, Stroke, TextureHandle, TextureOptions,
    pos2, vec2,
};

use super::util::{self, band_colour};

/// Detail bins per bin of the coarse min/max copy.
const MIP: usize = 16;
/// Most detail bins read per column before switching to the coarse copy.
const MAX_FINE: usize = 24;
/// Half the line's thickness, in points.
const LINE_HALF: f32 = 0.6;

/// A deck's waveform data, rebuilt when a new track loads.
pub struct DeckWave {
    bands: Arc<Vec<u8>>,
    band_rate: f64,
    fine: Arc<Vec<u8>>,
    fine_rate: f64,
    /// Per MIP fine bins: line (bin middle) min/max, then full min/max, as -1..1.
    mip: Vec<[f32; 4]>,
    pub overview: Option<TextureHandle>,
}

fn signed(b: u8) -> f32 {
    (b as f32 - 128.0) / 127.0
}

impl DeckWave {
    pub fn new(
        ctx: &egui::Context,
        name: &str,
        bands: Arc<Vec<u8>>,
        fine: Arc<Vec<u8>>,
    ) -> DeckWave {
        let n = fine.len() / 2;
        let mip = (0..n.div_ceil(MIP))
            .map(|j| {
                let mut m = [1.0f32, -1.0, 1.0, -1.0];
                for i in j * MIP..((j + 1) * MIP).min(n) {
                    let (lo, hi) = (signed(fine[i * 2]), signed(fine[i * 2 + 1]));
                    let mid = 0.5 * (lo + hi);
                    m = [m[0].min(mid), m[1].max(mid), m[2].min(lo), m[3].max(hi)];
                }
                m
            })
            .collect();
        let overview = (!bands.is_empty()).then(|| {
            ctx.load_texture(
                name,
                overview_image(&bands, 1200, 64),
                TextureOptions::LINEAR,
            )
        });
        DeckWave {
            bands,
            band_rate: earshot_engine::waveform::BINS_PER_SECOND,
            fine,
            fine_rate: earshot_engine::waveform::DETAIL_PER_SECOND,
            mip,
            overview,
        }
    }

    fn fine_bins(&self) -> usize {
        self.fine.len() / 2
    }

    fn mid(&self, i: f64) -> f32 {
        let n = self.fine_bins();
        if n == 0 {
            return 0.0;
        }
        let i = (i.max(0.0) as usize).min(n - 1);
        0.5 * (signed(self.fine[i * 2]) + signed(self.fine[i * 2 + 1]))
    }

    /// The line at time t, interpolated between bin centres.
    fn line_at(&self, t: f64) -> f32 {
        let x = t * self.fine_rate - 0.5;
        let i = x.floor();
        let f = (x - i) as f32;
        self.mid(i) * (1.0 - f) + self.mid(i + 1.0) * f
    }

    fn colour_at(&self, t: f64) -> Color32 {
        let n = self.bands.len() / 4;
        if n == 0 {
            return util::MUTED;
        }
        let i = ((t * self.band_rate) as usize).min(n - 1);
        band_colour(
            self.bands[i * 4 + 1],
            self.bands[i * 4 + 2],
            self.bands[i * 4 + 3],
        )
    }
}

/// Everything drawn on top of the waveform, in track seconds.
pub struct Marks<'a> {
    pub grid: Option<&'a Beatgrid>,
    pub cue: f64,
    pub hotcues: &'a [Option<f64>],
    pub loop_range: Option<(f64, f64)>,
    pub loop_active: bool,
}

/// Draw the scrolling waveform: `pos` at the centre, `half` track seconds each side.
pub fn scrolling(
    painter: &Painter,
    rect: Rect,
    wave: Option<&DeckWave>,
    pos: f64,
    half: f64,
    marks: &Marks,
) {
    painter.rect_filled(rect, 4.0, util::WAVE_BG);
    let w = rect.width();
    let cx = rect.center().x;
    let spp = 2.0 * half / w as f64; // track seconds per point
    let x_of = |t: f64| cx + ((t - pos) / spp) as f32;
    let (t_lo, t_hi) = (pos - half, pos + half);

    if let Some((a, b)) = marks.loop_range {
        let r = Rect::from_x_y_ranges(
            x_of(a).max(rect.left())..=x_of(b).min(rect.right()),
            rect.y_range(),
        );
        if r.width() > 0.0 {
            painter.rect_filled(
                r,
                0.0,
                if marks.loop_active {
                    Color32::from_rgba_unmultiplied(255, 180, 40, 50)
                } else {
                    Color32::from_white_alpha(18)
                },
            );
        }
    }

    if let Some(wv) = wave
        && wv.fine_bins() > 0
    {
        let end = wv.fine_bins() as f64 / wv.fine_rate;
        let half_h = rect.height() / 2.0;
        let cy = rect.center().y;
        let mut mesh = Mesh::default();
        let cols = w.floor() as usize;
        for c in 0..cols {
            let x = rect.left() + c as f32;
            let t0 = pos + (x - cx) as f64 * spp;
            let t1 = t0 + spp;
            if t1 <= 0.0 || t0 >= end {
                continue;
            }
            let (t0, t1) = (t0.max(0.0), t1.min(end));
            let (e0, e1) = (wv.line_at(t0), wv.line_at(t1));
            let (mut lo, mut hi) = (e0.min(e1), e0.max(e1));
            let (mut slo, mut shi) = (lo, hi);
            let a = (t0 * wv.fine_rate - 0.5).ceil().max(0.0) as usize;
            let b = ((t1 * wv.fine_rate - 0.5).floor().max(0.0) as usize).min(wv.fine_bins() - 1);
            if b >= a && b - a < MAX_FINE {
                for i in a..=b {
                    let (l, h) = (signed(wv.fine[i * 2]), signed(wv.fine[i * 2 + 1]));
                    let m = 0.5 * (l + h);
                    lo = lo.min(m);
                    hi = hi.max(m);
                    slo = slo.min(l);
                    shi = shi.max(h);
                }
            } else if b >= a {
                for m in &wv.mip[a / MIP..=(b / MIP).min(wv.mip.len() - 1)] {
                    lo = lo.min(m[0]);
                    hi = hi.max(m[1]);
                    slo = slo.min(m[2]);
                    shi = shi.max(m[3]);
                }
            }
            let mut col = wv.colour_at(0.5 * (t0 + t1));
            if x < cx {
                col = util::dim(col, 0.55); // already played
            }
            // Screen y grows downwards: the signal's +1 is at the top.
            let y = |v: f32| cy - v * half_h;
            mesh.add_colored_rect(
                Rect::from_x_y_ranges(x..=x + 1.0, y(shi)..=y(slo)),
                col.gamma_multiply(0.25),
            );
            mesh.add_colored_rect(
                Rect::from_x_y_ranges(x..=x + 1.0, y(hi) - LINE_HALF..=y(lo) + LINE_HALF),
                col,
            );
        }
        painter.add(Shape::mesh(mesh));
    }

    if let Some(g) = marks.grid {
        let times = g.times();
        let start = times.partition_point(|&t| t < t_lo);
        for (i, &t) in times.iter().enumerate().skip(start) {
            if t > t_hi {
                break;
            }
            let x = x_of(t);
            if g.positions()[i] == 1 {
                painter.line_segment(
                    [pos2(x, rect.top()), pos2(x, rect.bottom())],
                    Stroke::new(1.5, Color32::from_rgba_unmultiplied(255, 70, 70, 230)),
                );
            } else {
                let s = Stroke::new(1.0, Color32::from_white_alpha(90));
                let tick = rect.height() * 0.18;
                painter.line_segment([pos2(x, rect.top()), pos2(x, rect.top() + tick)], s);
                painter.line_segment([pos2(x, rect.bottom() - tick), pos2(x, rect.bottom())], s);
            }
        }
    }
    let marker = |t: f64, c: Color32, label: &str| {
        if t < t_lo || t > t_hi {
            return;
        }
        let x = x_of(t);
        painter.line_segment(
            [pos2(x, rect.top()), pos2(x, rect.bottom())],
            Stroke::new(2.0, c),
        );
        painter.add(Shape::convex_polygon(
            vec![
                pos2(x - 6.0, rect.top()),
                pos2(x + 6.0, rect.top()),
                pos2(x, rect.top() + 8.0),
            ],
            c,
            Stroke::NONE,
        ));
        if !label.is_empty() {
            painter.text(
                pos2(x + 4.0, rect.bottom() - 3.0),
                egui::Align2::LEFT_BOTTOM,
                label,
                egui::FontId::proportional(10.0),
                c,
            );
        }
    };
    marker(marks.cue, util::ACCENT, "");
    for (i, h) in marks.hotcues.iter().enumerate() {
        if let Some(t) = h {
            marker(*t, util::HOTCUE[i % 8], &"ABCDEFGH"[i..i + 1]);
        }
    }
    painter.line_segment(
        [pos2(cx, rect.top()), pos2(cx, rect.bottom())],
        Stroke::new(2.0, Color32::WHITE),
    );
}

fn overview_image(bands: &[u8], w: usize, h: usize) -> ColorImage {
    let bins = bands.len() / 4;
    let mut img = ColorImage::new([w, h], vec![Color32::TRANSPARENT; w * h]);
    for x in 0..w {
        let a = x * bins / w;
        let b = ((x + 1) * bins / w).max(a + 1).min(bins);
        let (mut peak, mut lo, mut mid, mut hi) = (0u8, 0u32, 0u32, 0u32);
        for i in a..b {
            peak = peak.max(bands[i * 4]);
            lo += bands[i * 4 + 1] as u32;
            mid += bands[i * 4 + 2] as u32;
            hi += bands[i * 4 + 3] as u32;
        }
        let n = (b - a) as u32;
        let c = band_colour((lo / n) as u8, (mid / n) as u8, (hi / n) as u8);
        let amp = (peak as f32 / 255.0 * h as f32 / 2.0) as usize;
        for y in h / 2 - amp.min(h / 2)..(h / 2 + amp).min(h) {
            img.pixels[y * w + x] = c;
        }
    }
    img
}

/// The whole-track strip under the deck header.
pub fn overview(
    painter: &Painter,
    rect: Rect,
    wave: Option<&DeckWave>,
    pos: f64,
    duration: f64,
    marks: &Marks,
    analysing: Option<f64>,
) {
    painter.rect_filled(rect, 3.0, util::WAVE_BG);
    if let Some(tex) = wave.and_then(|w| w.overview.as_ref()) {
        painter.image(
            tex.id(),
            rect,
            Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }
    if duration <= 0.0 {
        return;
    }
    let x_of = |t: f64| rect.left() + (t / duration) as f32 * rect.width();
    if let Some((a, b)) = marks.loop_range {
        painter.rect_filled(
            Rect::from_x_y_ranges(x_of(a)..=x_of(b).max(x_of(a) + 2.0), rect.y_range()),
            0.0,
            Color32::from_rgba_unmultiplied(255, 180, 40, 80),
        );
    }
    painter.rect_filled(
        Rect::from_x_y_ranges(rect.left()..=x_of(pos), rect.y_range()),
        0.0,
        Color32::from_black_alpha(115),
    );
    let tri = |t: f64, c: Color32| {
        let x = x_of(t);
        painter.add(Shape::convex_polygon(
            vec![
                pos2(x - 5.0, rect.top()),
                pos2(x + 5.0, rect.top()),
                pos2(x, rect.top() + 7.0),
            ],
            c,
            Stroke::NONE,
        ));
    };
    tri(marks.cue, util::ACCENT);
    for (i, h) in marks.hotcues.iter().enumerate() {
        if let Some(t) = h {
            tri(*t, util::HOTCUE[i % 8]);
        }
    }
    painter.line_segment(
        [pos2(x_of(pos), rect.top()), pos2(x_of(pos), rect.bottom())],
        Stroke::new(2.0, Color32::WHITE),
    );
    if let Some(p) = analysing {
        let r = Rect::from_min_size(
            pos2(rect.left(), rect.bottom() - 3.0),
            vec2(rect.width() * p as f32, 3.0),
        );
        painter.rect_filled(r, 0.0, util::WARN);
    }
}

/// A track's thumbnail (128 bins of peak + bands) as a small texture.
pub fn thumbnail(ctx: &egui::Context, id: i64, preview: &[u8]) -> TextureHandle {
    let (w, h) = (128usize, 24usize);
    let mut img = ColorImage::new([w, h], vec![Color32::TRANSPARENT; w * h]);
    for x in 0..w.min(preview.len() / 4) {
        let c = band_colour(preview[x * 4 + 1], preview[x * 4 + 2], preview[x * 4 + 3]);
        let amp = ((preview[x * 4] as f32 / 255.0) * (h as f32 / 2.0 - 1.0)).max(0.5) as usize;
        for y in h / 2 - amp.min(h / 2)..(h / 2 + amp.max(1)).min(h) {
            img.pixels[y * w + x] = c;
        }
    }
    ctx.load_texture(format!("thumb-{id}"), img, TextureOptions::LINEAR)
}
