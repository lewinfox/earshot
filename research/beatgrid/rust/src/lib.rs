//! Candidate A: a straight beat grid from plain signal processing, no model.
//! A line-for-line port of ../fastgrid.py (see there for the reasoning).
//!
//! 1. Onset envelopes at 1 kHz: kick band (low-pass 150 Hz) log-energy slope, and the
//!    full-band loudness rise (for the attack).
//! 2. Tempo: autocorrelation (coarse), then a scan of +-3% for the tempo whose
//!    frequency and 2nd/4th harmonics carry the most of the kick envelope.
//! 3. Fold the whole track at the beat period into a one-beat profile; fold 16 s
//!    windows too and line each up with the whole: a steady creep means the period is
//!    a little off; fit a line, correct it, repeat.
//! 4. Phase: the profile's peak, moved back to the sharpest full-band rise up to 30 ms
//!    before it (the start of the hit).
//! 5. What's left of the window offsets after the line fit says how far the track is
//!    from one straight grid.

pub const FPS: f64 = 1000.0;
const WIN_S: f64 = 16.0;
const HOP_S: f64 = 8.0;
const ATTACK_BEFORE: i64 = 30;
const ATTACK_AFTER: i64 = 8;

#[derive(Debug, Clone, Copy)]
pub struct Grid {
    pub bpm: f64,
    /// A beat at this time (s); the others every 60/bpm either side.
    pub phase: f64,
    /// 90th percentile / max of the 16 s windows' distance from the fitted line (ms).
    pub win_resid_p90_ms: f64,
    pub win_resid_max_ms: f64,
    /// Share of windows with a clear beat that lines up with the whole track's.
    pub good_frac: f64,
    pub coarse_bpm: f64,
}

// ---- filters and envelopes -----------------------------------------------------------

struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    /// RBJ low-pass. Two in series with Q 0.5412 and 1.3066 make a 4th-order Butterworth.
    fn lowpass(sr: f64, f0: f64, q: f64) -> Biquad {
        let w0 = 2.0 * std::f64::consts::PI * f0 / sr;
        let (s, c) = w0.sin_cos();
        let alpha = s / (2.0 * q);
        let a0 = 1.0 + alpha;
        Biquad {
            b: [(1.0 - c) / 2.0 / a0, (1.0 - c) / a0, (1.0 - c) / 2.0 / a0],
            a: [-2.0 * c / a0, (1.0 - alpha) / a0],
            z: [0.0; 2],
        }
    }
    #[inline]
    fn run(&mut self, x: f64) -> f64 {
        // transposed direct form II
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// Mean energy per 1 ms frame of the full signal and of its kick band, in one pass.
fn energies(y: &[f32], sr: u32) -> (Vec<f64>, Vec<f64>) {
    let sr_f = sr as f64;
    let n = (y.len() as f64 * FPS / sr_f) as usize;
    let mut f1 = Biquad::lowpass(sr_f, 150.0, 0.541_196_100_146_197);
    let mut f2 = Biquad::lowpass(sr_f, 150.0, 1.306_562_964_876_376_7);
    let mut full = Vec::with_capacity(n);
    let mut low = Vec::with_capacity(n);
    let mut start = 0usize;
    for i in 0..n {
        let end = ((i + 1) as f64 * sr_f / FPS).round() as usize;
        let (mut ef, mut el) = (0.0f64, 0.0f64);
        for &s in &y[start..end] {
            let x = s as f64;
            ef += x * x;
            let l = f2.run(f1.run(x));
            el += l * l;
        }
        let m = (end - start).max(1) as f64;
        full.push(ef / m);
        low.push(el / m);
        start = end;
    }
    (full, low)
}

/// numpy-style linear-interpolated percentile.
fn percentile(x: &[f64], q: f64) -> f64 {
    if x.is_empty() {
        return 0.0;
    }
    let mut v = x.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pos = q / 100.0 * (v.len() - 1) as f64;
    let i = pos.floor() as usize;
    let f = pos - i as f64;
    if i + 1 < v.len() {
        v[i] * (1.0 - f) + v[i + 1] * f
    } else {
        v[i]
    }
}

fn median(x: &[f64]) -> f64 {
    percentile(x, 50.0)
}

/// Centred moving average, like numpy.convolve(x, ones(k)/k, "same") (zeros outside).
fn moving_avg(x: &[f64], k: usize) -> Vec<f64> {
    let n = x.len();
    let mut c = vec![0.0; n + 1];
    for i in 0..n {
        c[i + 1] = c[i] + x[i];
    }
    let back = k / 2; // window x[i-back .. i-back+k]
    (0..n)
        .map(|i| {
            let a = i.saturating_sub(back);
            let b = (i + k - back).min(n);
            (c[b] - c[a]) / k as f64
        })
        .collect()
}

/// Kick-band onset strength: log energy slope over 4 ms, rises only, slow trend removed.
fn onset(e: &[f64]) -> Vec<f64> {
    let e = moving_avg(e, 8);
    let r = percentile(&e, 90.0) + 1e-12;
    let l: Vec<f64> = e.iter().map(|v| (v / r * 100.0).ln_1p()).collect();
    let n = l.len();
    let mut d = vec![0.0; n];
    for i in 2..n.saturating_sub(2) {
        d[i] = (l[i + 2] - l[i - 2]).max(0.0);
    }
    let m = moving_avg(&d, 500);
    d.iter().zip(&m).map(|(a, b)| (a - b).max(0.0)).collect()
}

/// Full-band loudness rise per frame (log energy, rises only): sharp at a hit's start.
fn attack_env(e: &[f64]) -> Vec<f64> {
    let l: Vec<f64> = e.iter().map(|v| (v + 1e-10).log10()).collect();
    let mut d = vec![0.0; l.len()];
    for i in 1..l.len() {
        d[i] = (l[i] - l[i - 1]).max(0.0);
    }
    d
}

// ---- tempo -------------------------------------------------------------------------

fn interp(ac: &[f64], x: f64) -> f64 {
    let i = x.floor() as usize;
    if i + 1 >= ac.len() {
        return *ac.last().unwrap_or(&0.0);
    }
    let f = x - i as f64;
    ac[i] * (1.0 - f) + ac[i + 1] * f
}

fn coarse_tempo(env: &[f64]) -> f64 {
    let fps = 100.0;
    let e: Vec<f64> = env.chunks_exact(10).map(|c| c.iter().sum()).collect();
    let mean = e.iter().sum::<f64>() / e.len() as f64;
    let e: Vec<f64> = e.iter().map(|v| v - mean).collect();
    let (lag_lo, lag_hi) = ((fps * 60.0 / 200.0) as usize, (fps * 60.0 / 60.0) as usize);
    let maxlag = (4 * lag_hi + 2).min(e.len().saturating_sub(1));
    let ac: Vec<f64> = (0..=maxlag)
        .map(|l| {
            e[..e.len() - l]
                .iter()
                .zip(&e[l..])
                .map(|(a, b)| a * b)
                .sum()
        })
        .collect();
    let mut best = (f64::MIN, 125.0);
    let mut lag = lag_lo as f64;
    while lag < lag_hi as f64 {
        let score = interp(&ac, lag) + 0.5 * interp(&ac, 2.0 * lag) + 0.25 * interp(&ac, 4.0 * lag);
        let bpm = 60.0 * fps / lag;
        let prior = (-0.5 * ((bpm / 125.0).log2() / 0.6).powi(2)).exp();
        if score * prior > best.0 {
            best = (score * prior, bpm);
        }
        lag += 0.05;
    }
    best.1
}

/// Scan bpm0 +-3% for the tempo whose beat frequency and 2nd/4th harmonics carry the
/// most of the envelope.
fn fine_tempo(env: &[f64], bpm0: f64, span: f64, step: f64) -> f64 {
    let fps = 200.0;
    let e: Vec<f64> = env.chunks_exact(5).map(|c| c.iter().sum()).collect();
    let mean = e.iter().sum::<f64>() / e.len() as f64;
    let e: Vec<f64> = e.iter().map(|v| v - mean).collect();
    let nb = ((2.0 * span * bpm0) / step).ceil() as usize;
    let bpms: Vec<f64> = (0..nb)
        .map(|i| bpm0 * (1.0 - span) + i as f64 * step)
        .collect();
    let mag: Vec<f64> = bpms
        .iter()
        .map(|&bpm| {
            let mut total = 0.0;
            for (h, w) in [(1.0, 1.0), (2.0, 0.5), (4.0, 0.25)] {
                // a rotating phasor instead of a sin/cos per sample
                let th = -2.0 * std::f64::consts::PI * bpm / 60.0 * h / fps;
                let (ws, wc) = th.sin_cos();
                let (mut zr, mut zi) = (1.0f64, 0.0f64);
                let (mut ar, mut ai) = (0.0f64, 0.0f64);
                for (i, &v) in e.iter().enumerate() {
                    ar += v * zr;
                    ai += v * zi;
                    let nr = zr * wc - zi * ws;
                    zi = zr * ws + zi * wc;
                    zr = nr;
                    if i % 4096 == 4095 {
                        let m = (zr * zr + zi * zi).sqrt();
                        zr /= m;
                        zi /= m;
                    }
                }
                total += w * (ar * ar + ai * ai).sqrt();
            }
            total
        })
        .collect();
    let j = (0..mag.len())
        .max_by(|&a, &b| mag[a].partial_cmp(&mag[b]).unwrap())
        .unwrap_or(0);
    let mut bpm = bpms[j];
    if j > 0 && j + 1 < mag.len() {
        let (a, b, c) = (mag[j - 1], mag[j], mag[j + 1]);
        let den = a - 2.0 * b + c;
        if den < 0.0 {
            bpm += step * 0.5 * (a - c) / den;
        }
    }
    bpm
}

// ---- folding -----------------------------------------------------------------------

/// Frames t0..t1 of env folded at `period` frames into round(period) bins.
fn fold(env: &[f64], t0: usize, t1: usize, period: f64) -> Vec<f64> {
    let nb = period.round() as usize;
    let mut prof = vec![0.0; nb];
    for i in t0..t1.min(env.len()) {
        let pos = (i as f64 % period) / period * nb as f64;
        let b0 = pos.floor();
        let f = pos - b0;
        let b0 = b0 as usize % nb;
        prof[b0] += env[i] * (1.0 - f);
        prof[(b0 + 1) % nb] += env[i] * f;
    }
    prof
}

/// Circular argmax with parabolic refinement.
fn peak(x: &[f64]) -> f64 {
    let n = x.len();
    let j = (0..n)
        .max_by(|&a, &b| x[a].partial_cmp(&x[b]).unwrap())
        .unwrap_or(0);
    let (a, b, c) = (x[(j + n - 1) % n], x[j], x[(j + 1) % n]);
    let den = a - 2.0 * b + c;
    j as f64 + if den < 0.0 { 0.5 * (a - c) / den } else { 0.0 }
}

fn smooth_circ(x: &[f64], k: usize) -> Vec<f64> {
    let n = x.len() as i64;
    let h = (k / 2) as i64;
    (0..n)
        .map(|i| {
            (-h..=h)
                .map(|d| x[((i + d).rem_euclid(n)) as usize])
                .sum::<f64>()
                / k as f64
        })
        .collect()
}

struct Window {
    centre: f64,
    offset: f64, // frames, -period/2..period/2
    corr: f64,
    sum: f64,
}

fn windows(env: &[f64], period: f64, template: &[f64]) -> Vec<Window> {
    let (w, h) = ((WIN_S * FPS) as usize, (HOP_S * FPS) as usize);
    let nb = template.len();
    let tm = template.iter().sum::<f64>() / nb as f64;
    let tz: Vec<f64> = template.iter().map(|v| v - tm).collect();
    let tn = tz.iter().map(|v| v * v).sum::<f64>().sqrt();
    let mut out = vec![];
    let last = if env.len() > w { env.len() - w + 1 } else { 1 };
    let mut t0 = 0;
    while t0 < last {
        let p = fold(env, t0, t0 + w, period);
        let pm = p.iter().sum::<f64>() / nb as f64;
        let pz: Vec<f64> = p.iter().map(|v| v - pm).collect();
        // r[k] = sum_j pz[j] tz[j-k]
        let r: Vec<f64> = (0..nb)
            .map(|k| (0..nb).map(|j| pz[j] * tz[(j + nb - k) % nb]).sum())
            .collect();
        let mut k = peak(&r);
        if k > nb as f64 / 2.0 {
            k -= nb as f64;
        }
        let den = pz.iter().map(|v| v * v).sum::<f64>().sqrt() * tn;
        let rmax = r.iter().cloned().fold(f64::MIN, f64::max);
        out.push(Window {
            centre: (t0 as f64 + w as f64 / 2.0) / FPS,
            offset: k * period / nb as f64,
            corr: if den > 0.0 { rmax / den } else { 0.0 },
            sum: p.iter().sum(),
        });
        t0 += h;
    }
    out
}

/// Huber-weighted straight line y = a + b x (weights w). Returns (a, b, residuals).
fn weighted_line(x: &[f64], y: &[f64], w: &[f64]) -> (f64, f64, Vec<f64>) {
    let mut ww = w.to_vec();
    let (mut a, mut b) = (0.0, 0.0);
    let mut r = vec![0.0; x.len()];
    for _ in 0..8 {
        let sw: f64 = ww.iter().sum();
        let mx = x.iter().zip(&ww).map(|(x, w)| x * w).sum::<f64>() / sw;
        let my = y.iter().zip(&ww).map(|(y, w)| y * w).sum::<f64>() / sw;
        let sxy: f64 = (0..x.len())
            .map(|i| ww[i] * (x[i] - mx) * (y[i] - my))
            .sum();
        let sxx: f64 = (0..x.len()).map(|i| ww[i] * (x[i] - mx).powi(2)).sum();
        b = if sxx > 0.0 { sxy / sxx } else { 0.0 };
        a = my - b * mx;
        for i in 0..x.len() {
            r[i] = y[i] - (a + b * x[i]);
        }
        let active: Vec<f64> = (0..x.len())
            .filter(|&i| ww[i] > 0.0)
            .map(|i| r[i].abs())
            .collect();
        let s = 1.4826 * median(&active) + 0.5;
        let c = 1.345 * s;
        for i in 0..x.len() {
            ww[i] = w[i]
                * if r[i].abs() <= c {
                    1.0
                } else {
                    c / r[i].abs().max(1e-9)
                };
        }
    }
    (a, b, r)
}

fn unwrap(x: &[f64], period: f64) -> Vec<f64> {
    let mut out: Vec<f64> = Vec::with_capacity(x.len());
    let mut shift = 0.0f64;
    for (i, &v) in x.iter().enumerate() {
        if i > 0 {
            let d = v + shift - out[i - 1];
            if d > period / 2.0 {
                shift -= period * (d / period).round().max(1.0);
            } else if d < -period / 2.0 {
                shift += period * (-d / period).round().max(1.0);
            }
        }
        out.push(v + shift);
    }
    out
}

/// Windows that count (clear beat, not quiet), their fitted line residuals (frames).
fn window_fit(ws: &[Window], period: f64) -> Option<(f64, Vec<f64>, f64)> {
    let sums: Vec<f64> = ws.iter().map(|w| w.sum).collect();
    let msum = median(&sums);
    let conf: Vec<f64> = ws
        .iter()
        .map(|w| {
            if w.sum > 0.2 * msum {
                w.corr.clamp(0.0, 1.0).powi(2)
            } else {
                0.0
            }
        })
        .collect();
    let good: Vec<usize> = (0..ws.len()).filter(|&i| conf[i] > 0.1).collect();
    let good_frac = good.len() as f64 / ws.len().max(1) as f64;
    if good.len() < 3 {
        return None;
    }
    let x: Vec<f64> = good.iter().map(|&i| ws[i].centre).collect();
    let off = unwrap(
        &good.iter().map(|&i| ws[i].offset).collect::<Vec<_>>(),
        period,
    );
    let w: Vec<f64> = good.iter().map(|&i| conf[i]).collect();
    let (_, b, r) = weighted_line(&x, &off, &w);
    Some((b, r, good_frac))
}

pub fn analyse(y: &[f32], sr: u32) -> Grid {
    let (full, low) = energies(y, sr);
    let env = onset(&low);
    let coarse = coarse_tempo(&env);
    let bpm = fine_tempo(&env, coarse, 0.03, 0.02);
    let mut period = 60.0 / bpm * FPS;
    for _ in 0..4 {
        let template = fold(&env, 0, env.len(), period);
        let ws = windows(&env, period, &template);
        match window_fit(&ws, period) {
            Some((b, _, _)) => period *= 1.0 + b / FPS,
            None => break,
        }
    }
    let template = fold(&env, 0, env.len(), period);
    let nb = template.len() as f64;
    let mut ph = peak(&smooth_circ(&template, 5)) * period / nb;
    let prof = fold(&attack_env(&full), 0, env.len(), period);
    let centre = (ph * nb / period).round() as i64;
    let best = (-ATTACK_BEFORE..=ATTACK_AFTER)
        .max_by(|&a, &b| {
            let pa = prof[(centre + a).rem_euclid(nb as i64) as usize];
            let pb = prof[(centre + b).rem_euclid(nb as i64) as usize];
            pa.partial_cmp(&pb).unwrap()
        })
        .unwrap();
    ph = (ph + best as f64 * period / nb).rem_euclid(period);

    let ws = windows(&env, period, &template);
    let (resid, good_frac) = match window_fit(&ws, period) {
        Some((_, r, g)) => (r, g),
        None => (vec![0.0], 0.0),
    };
    let abs: Vec<f64> = resid.iter().map(|v| v.abs()).collect();
    Grid {
        bpm: 60.0 * FPS / period,
        phase: ph / FPS,
        win_resid_p90_ms: percentile(&abs, 90.0),
        win_resid_max_ms: abs.iter().cloned().fold(0.0, f64::max),
        good_frac,
        coarse_bpm: coarse,
    }
}

/// Read a 32-bit float mono WAV as written by the app (and bin/decode.rs).
pub fn read_wav(path: &std::path::Path) -> std::io::Result<(Vec<f32>, u32)> {
    let b = std::fs::read(path)?;
    let sr = u32::from_le_bytes(b[24..28].try_into().unwrap());
    let y = b[44..]
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    Ok((y, sr))
}
