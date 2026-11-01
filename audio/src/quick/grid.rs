//! A steady beat grid from signal processing alone, in well under a second.
//!
//! 1. **Onset strength**: SuperFlux-style log-mel spectral flux (about 172 frames a
//!    second, lag 2, a max filter across neighbouring bands), with the band below
//!    200 Hz added at double weight so the kick dominates.
//! 2. **Tempo candidates**: autocorrelation of the envelope (its local mean taken
//!    away), scored as a comb over 1, 2, 4 and 8 beats and weighted by a log-Gaussian
//!    prior around 124 BPM. The four best peaks are kept, folded into 85..175 BPM.
//! 3. **One grid for the whole track**: the envelope is folded over the beat into a
//!    96-bin phase profile. The track is cut into 8- or 16-beat segments and each
//!    segment's peak phase is found. A line through them (slope: the tempo
//!    correction; intercept: the phase) is fitted by weighted least squares that
//!    drops outliers, after a coherence search with doubled angles, so a segment that
//!    locked onto the off-beat doesn't spoil the fit. Four rounds, then a fine tempo
//!    scan for the sharpest whole-track profile. Beatless stretches carry no weight.
//!    When the beat is weak the runner-up tempos are fitted too and the sharpest wins.
//! 4. **On the beat, not between**: onsets of the kick body (150-600 Hz), claps
//!    (600 Hz-4 kHz) and hats (above 4 kHz), folded at the beat and half a beat on.
//!    Kick and claps are on the beat, open hats between.
//! 5. **Round tempos**: the BPM snaps to a whole or half number within 0.03 BPM if the
//!    grid stays (nearly) as sharp.
//! 6. **Downbeat**: the first beat with music in it (louder than -65 dBFS) is a 1.
//!
//! Ported from ClubCraft (see the module above for the licence). Left out: its
//! bar-phase evidence features, used only when no beat has music in it.

use super::dsp::{self, MelBank};
use crate::beatgrid::Constant;

const N_FFT: usize = 1024;
const N_MELS: usize = 40;
/// Bins in a folded beat.
const FOLD_BINS: usize = 96;
/// Where the onset envelope peaks against where a beat is heard (s), as ClubCraft
/// calibrated it against DJ.Studio and Mixed In Key.
const BEAT_OFFSET_S: f64 = -0.003;
/// Quieter than this, a beat has no music in it.
const MUSIC_DBFS: f32 = -65.0;

/// Tempo search settings.
#[derive(Debug, Clone)]
pub struct GridOptions {
    /// The tempo range searched (BPM).
    pub min_bpm: f64,
    pub max_bpm: f64,
    /// Results are halved or doubled into this range.
    pub fold_min: f64,
    pub fold_max: f64,
    /// Centre (BPM) and width (octaves, one sigma) of the tempo prior.
    pub prior_bpm: f64,
    pub prior_octaves: f64,
    /// Snap to a whole or half BPM when the fit allows it.
    pub snap: bool,
}

impl GridOptions {
    /// Centred on a tempo the track is thought to have (its BPM tag): the prior is
    /// narrower and the results fold into a range around it, which keeps a drum & bass
    /// track off two thirds of its tempo.
    pub fn around(bpm: f64) -> Self {
        let lo = (bpm / 1.4).max(40.0);
        let hi = bpm * 1.4;
        GridOptions {
            min_bpm: lo,
            max_bpm: hi,
            fold_min: lo,
            fold_max: hi,
            prior_bpm: bpm,
            prior_octaves: 0.15,
            ..Default::default()
        }
    }
}

impl Default for GridOptions {
    fn default() -> Self {
        GridOptions {
            min_bpm: 70.0,
            max_bpm: 180.0,
            fold_min: 85.0,
            fold_max: 175.0,
            prior_bpm: 124.0,
            prior_octaves: 0.6,
            snap: true,
        }
    }
}

/// A grid found by [`detect`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QuickGrid {
    /// A 1 at the first beat with music in it.
    pub grid: Constant,
    /// How sharp the folded beat is (0: no periodicity). Above about 1.5 is a clear beat.
    pub confidence: f32,
}

/// Mel-band magnitudes, `n_frames × N_MELS`.
struct Spectral {
    fps: f64,
    n_frames: usize,
    mel: Vec<f32>,
    centers: Vec<f32>,
}

impl Spectral {
    fn compute(x: &[f32], sr: f64) -> Spectral {
        // About 23 ms frames, 5.8 ms apart, at any rate up to 48 kHz.
        let n_fft = if sr > 50_000.0 { 2 * N_FFT } else { N_FFT };
        let hop = n_fft / 4;
        let bank = MelBank::new(n_fft, sr, N_MELS, 30.0, 16_000.0);
        let frames: Vec<[f32; N_MELS]> = dsp::stft_map(x, n_fft, hop, |spec| {
            let mut m = [0.0f32; N_MELS];
            bank.apply(spec, &mut m);
            m
        });
        Spectral {
            fps: sr / hop as f64,
            n_frames: frames.len(),
            mel: frames.concat(),
            centers: bank.centers,
        }
    }

    /// The mel bands whose centres are in `lo..hi` Hz.
    fn bands(&self, lo: f32, hi: f32) -> std::ops::Range<usize> {
        let a = self.centers.iter().position(|c| *c >= lo).unwrap_or(N_MELS);
        let b = self.centers.iter().position(|c| *c >= hi).unwrap_or(N_MELS);
        a..b.max(a)
    }

    fn log_mel(&self) -> Vec<f32> {
        self.mel.iter().map(|m| (1.0 + 1000.0 * m).ln()).collect()
    }

    /// SuperFlux onset strength over `bands`: the rise in log magnitude against the
    /// frame two hops before, max-filtered across neighbouring bands.
    fn onset(&self, logmel: &[f32], bands: std::ops::Range<usize>) -> Vec<f32> {
        let lag = 2;
        let mut out = vec![0.0f32; self.n_frames];
        for (i, o) in out.iter_mut().enumerate().skip(lag) {
            let cur = &logmel[i * N_MELS..(i + 1) * N_MELS];
            let prev = &logmel[(i - lag) * N_MELS..(i - lag + 1) * N_MELS];
            *o = bands
                .clone()
                .map(|b| {
                    let lo = b.saturating_sub(1);
                    let hi = (b + 1).min(N_MELS - 1);
                    let p = prev[lo..=hi].iter().copied().fold(f32::MIN, f32::max);
                    (cur[b] - p).max(0.0)
                })
                .sum();
        }
        out
    }
}

/// The steady grid of a mono signal at `sr`, or `None` without a periodic beat
/// (silence, beatless ambient, under 8 s).
pub fn detect(mono: &[f32], sr: f64, opts: &GridOptions) -> Option<QuickGrid> {
    let spec = Spectral::compute(mono, sr);
    let logmel = spec.log_mel();
    let full = spec.onset(&logmel, 0..N_MELS);
    let low = spec.onset(&logmel, spec.bands(0.0, 200.0));
    let env: Vec<f32> = full.iter().zip(&low).map(|(a, b)| a + 2.0 * b).collect();
    let fps = spec.fps;
    if env.len() < (fps * 8.0) as usize {
        return None;
    }
    let cands = tempo_candidates(&env, fps, opts);
    let bpm0 = *cands.first()?;
    let (mut period, mut phase, mut conf) = refine(&env, 60.0 * fps / bpm0);
    // A weak beat: fit the runner-up tempos too and keep the sharpest grid.
    if conf < 2.0 {
        let prior = |b: f64| {
            let z = (b / opts.prior_bpm).log2() / (2.0 * opts.prior_octaves);
            (-0.5 * z * z).exp() as f32
        };
        let mut best = conf * prior(60.0 * fps / period);
        for &b in cands.iter().skip(1) {
            let (p2, ph2, c2) = refine(&env, 60.0 * fps / b);
            let score = c2 * prior(60.0 * fps / p2);
            if score > best {
                (best, period, phase, conf) = (score, p2, ph2, c2);
            }
        }
    }
    if conf <= 0.0 {
        return None;
    }
    if onbeat_score(&spec, &logmel, period, phase) < 0.0 {
        phase += 0.5 * period;
    }
    let mut bpm = 60.0 * fps / period;
    if opts.snap {
        for step in [1.0, 0.5] {
            let snapped = (bpm / step).round() * step;
            let off = (snapped - bpm).abs();
            if off <= 1e-9 {
                bpm = snapped;
                break;
            }
            if off < 0.03 {
                let p2 = 60.0 * fps / snapped;
                let (ph2, c2) = near_phase(&env, p2, phase);
                if c2 >= conf * 0.985 {
                    (period, phase, conf, bpm) = (p2, ph2, c2, snapped);
                }
                break;
            }
        }
    }
    // The first beat at or after 0 s.
    let beat_s = period / fps;
    let first_s = (phase.rem_euclid(period) / fps + BEAT_OFFSET_S).rem_euclid(beat_s);
    // The beat before it counts too if it's at most 30 ms before 0 s: a bar that starts
    // right at the start of the file, moved a little by encoder delay.
    let k = first_music_beat(mono, sr, first_s, beat_s).unwrap_or(0);
    Some(QuickGrid {
        grid: Constant::new(bpm, first_s + k as f64 * beat_s),
        confidence: conf,
    })
}

/// Tempo candidates (BPM, folded into the fold range), best first.
fn tempo_candidates(env: &[f32], fps: f64, opts: &GridOptions) -> Vec<f64> {
    let avg = dsp::moving_average(env, fps as usize);
    let e: Vec<f32> = env
        .iter()
        .zip(&avg)
        .map(|(a, m)| (a - m).max(0.0))
        .collect();
    let max_lag = (60.0 * fps / opts.min_bpm * 8.0) as usize + 4;
    let ac = dsp::autocorr(&e, max_lag);
    let score = |bpm: f64| -> f64 {
        let tau = 60.0 * fps / bpm;
        let a = |m: f64| dsp::lerp_at(&ac, m * tau) as f64;
        let s = a(1.0) + 0.5 * a(2.0) + 0.5 * a(4.0) + 0.25 * a(8.0);
        let z = (bpm / opts.prior_bpm).log2() / opts.prior_octaves;
        s * (-0.5 * z * z).exp()
    };
    let step = 0.02;
    let n = ((opts.max_bpm - opts.min_bpm) / step) as usize + 1;
    let scores: Vec<f64> = (0..n)
        .map(|i| score(opts.min_bpm + i as f64 * step))
        .collect();
    let mut peaks: Vec<(f64, f64)> = (1..n.saturating_sub(1))
        .filter(|&i| scores[i] > scores[i - 1] && scores[i] >= scores[i + 1] && scores[i] > 0.0)
        .map(|i| {
            let off = dsp::parabolic(scores[i - 1], scores[i], scores[i + 1]);
            (opts.min_bpm + (i as f64 + off) * step, scores[i])
        })
        .collect();
    peaks.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut out: Vec<f64> = Vec::new();
    for (mut bpm, _) in peaks {
        while bpm < opts.fold_min && bpm * 2.0 <= opts.fold_max * 1.02 {
            bpm *= 2.0;
        }
        while bpm > opts.fold_max && bpm / 2.0 >= opts.fold_min * 0.98 {
            bpm /= 2.0;
        }
        if out.iter().all(|b| (b / bpm - 1.0).abs() > 0.01) {
            out.push(bpm);
        }
        if out.len() >= 4 {
            break;
        }
    }
    out
}

/// The mean of `env` over `range` folded onto one beat of a grid (`period`, `phase`
/// in frames), in [`FOLD_BINS`] bins.
fn fold(
    env: &[f32],
    period: f64,
    phase: f64,
    range: std::ops::Range<usize>,
    out: &mut [f64; FOLD_BINS],
) {
    let mut sum = [0.0f64; FOLD_BINS];
    let mut cnt = [0.0f64; FOLD_BINS];
    let nb = FOLD_BINS as f64;
    for n in range {
        let th = ((n as f64 - phase) / period).rem_euclid(1.0) * nb;
        let i0 = th.floor() as usize % FOLD_BINS;
        let f = th - th.floor();
        let i1 = (i0 + 1) % FOLD_BINS;
        let v = env[n] as f64;
        sum[i0] += v * (1.0 - f);
        cnt[i0] += 1.0 - f;
        sum[i1] += v * f;
        cnt[i1] += f;
    }
    for i in 0..FOLD_BINS {
        out[i] = if cnt[i] > 0.0 { sum[i] / cnt[i] } else { 0.0 };
    }
}

/// A folded profile's peak phase (fraction of a beat, -0.5..0.5) and how much it
/// stands out from the mean.
fn profile_peak(p: &[f64; FOLD_BINS]) -> (f64, f32) {
    let s: [f64; FOLD_BINS] = std::array::from_fn(|i| {
        0.25 * p[(i + FOLD_BINS - 1) % FOLD_BINS] + 0.5 * p[i] + 0.25 * p[(i + 1) % FOLD_BINS]
    });
    let (im, &mx) = s
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .unwrap_or((0, &0.0));
    let mean = s.iter().sum::<f64>() / FOLD_BINS as f64;
    if mean <= 1e-9 {
        return (0.0, 0.0);
    }
    let off = dsp::parabolic(
        s[(im + FOLD_BINS - 1) % FOLD_BINS],
        mx,
        s[(im + 1) % FOLD_BINS],
    );
    let mut th = (im as f64 + off) / FOLD_BINS as f64;
    if th >= 0.5 {
        th -= 1.0;
    }
    (th, ((mx - mean) / mean) as f32)
}

/// The whole track's peak phase near `phase`, and its sharpness.
fn global_phase(env: &[f32], period: f64, phase: f64) -> (f64, f32) {
    let mut prof = [0.0; FOLD_BINS];
    fold(env, period, phase, 0..env.len(), &mut prof);
    let ph = phase + profile_peak(&prof).0 * period;
    // Again, centred on what it found, for a symmetric estimate.
    fold(env, period, ph, 0..env.len(), &mut prof);
    let (th, sal) = profile_peak(&prof);
    (ph + th * period, sal)
}

/// Like [`global_phase`], but only within a quarter beat of `phase` (keeps the
/// on-beat decision).
fn near_phase(env: &[f32], period: f64, phase: f64) -> (f64, f32) {
    let mut prof = [0.0; FOLD_BINS];
    fold(env, period, phase, 0..env.len(), &mut prof);
    let q = FOLD_BINS / 4;
    let i = (0..2 * q)
        .map(|d| (FOLD_BINS + d - q) % FOLD_BINS)
        .max_by(|a, b| prof[*a].total_cmp(&prof[*b]))
        .unwrap_or(0);
    let off = dsp::parabolic(
        prof[(i + FOLD_BINS - 1) % FOLD_BINS],
        prof[i],
        prof[(i + 1) % FOLD_BINS],
    );
    let mut th = (i as f64 + off) / FOLD_BINS as f64;
    if th >= 0.5 {
        th -= 1.0;
    }
    let mean = prof.iter().sum::<f64>() / FOLD_BINS as f64;
    let mx = prof.iter().copied().fold(f64::MIN, f64::max);
    let sal = if mean > 0.0 {
        ((mx - mean) / mean) as f32
    } else {
        0.0
    };
    (phase + th * period, sal)
}

/// Fit one steady grid to the whole envelope, from a starting period (frames).
/// Returns (period, phase, confidence), in frames.
fn refine(env: &[f32], period0: f64) -> (f64, f64, f32) {
    use std::f64::consts::PI;
    let mut period = period0;
    let (mut phase, _) = global_phase(env, period, 0.0);
    for iter in 0..4 {
        let seg_beats = if iter == 0 { 8.0 } else { 16.0 };
        let seg_len = (seg_beats * period).round() as usize;
        // (beat index, phase, weight) per segment with a clear beat.
        let mut pts: Vec<(f64, f64, f64)> = Vec::new();
        let mut prof = [0.0; FOLD_BINS];
        let mut start = 0usize;
        while start + seg_len / 2 < env.len() {
            let end = (start + seg_len).min(env.len());
            fold(env, period, phase, start..end, &mut prof);
            let (th, sal) = profile_peak(&prof);
            let c = ((start + end) as f64 * 0.5 - phase) / period;
            if sal > 0.3 {
                let w = (sal as f64).powi(2) * (end - start) as f64 / seg_len as f64;
                pts.push((c, th, w));
            }
            start = end;
        }
        if pts.len() < 3 {
            break;
        }
        // A first line from the tempo correction whose segment phases agree best.
        // Angles are doubled: a segment's strongest pulse may be the off-beat.
        let coherence = |eps: f64| -> (f64, f64) {
            let (mut re, mut im) = (0.0, 0.0);
            for &(x, y, w) in &pts {
                let a = 4.0 * PI * (y - eps * x);
                re += w * a.cos();
                im += w * a.sin();
            }
            (re * re + im * im, im.atan2(re) / (4.0 * PI))
        };
        let range = if iter == 0 { 0.004 } else { 0.0005 };
        let steps = 400;
        let mut best = (0.0, f64::MIN, 0.0);
        for i in 0..=steps {
            let eps = -range + 2.0 * range * i as f64 / steps as f64;
            let (c, ph) = coherence(eps);
            if c > best.1 {
                best = (eps, c, ph);
            }
        }
        // Each segment's phase to the half beat nearest that line.
        for p in pts.iter_mut() {
            let pred = best.2 + best.0 * p.0;
            p.1 += ((pred - p.1) * 2.0).round() * 0.5;
        }
        // Weighted least squares, dropping segments more than 5% of a beat off.
        let mut keep = vec![true; pts.len()];
        let mut fit = (best.2, best.0);
        for _ in 0..4 {
            let (mut sw, mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for (&(x, y, w), _) in pts.iter().zip(&keep).filter(|(_, k)| **k) {
                sw += w;
                sx += w * x;
                sy += w * y;
                sxx += w * x * x;
                sxy += w * x * y;
            }
            if sw <= 0.0 {
                break;
            }
            let den = sw * sxx - sx * sx;
            let slope = if den.abs() > 1e-12 {
                (sw * sxy - sx * sy) / den
            } else {
                0.0
            };
            let icpt = (sy - slope * sx) / sw;
            fit = (icpt, slope);
            let mut changed = false;
            for (&(x, y, _), k) in pts.iter().zip(keep.iter_mut()) {
                let ok = (y - (icpt + slope * x)).abs() < 0.05;
                changed |= ok != *k;
                *k = ok;
            }
            if !changed {
                break;
            }
        }
        phase += fit.0 * period;
        period *= 1.0 + fit.1;
    }
    // Last: the tempo with the sharpest whole-track profile, within ±0.06%.
    let (mut ph, mut conf) = global_phase(env, period, phase);
    let base = period;
    for i in -30..=30 {
        let p = base * (1.0 + i as f64 * 2e-5);
        let (ph2, c2) = global_phase(env, p, ph);
        if c2 > conf {
            (conf, ph, period) = (c2, ph2, p);
        }
    }
    (period, ph, conf)
}

/// Above 0 when the grid is on the beat: kick body and claps there, hats half a beat on.
fn onbeat_score(spec: &Spectral, logmel: &[f32], period: f64, phase: f64) -> f64 {
    let bands = [
        (spec.bands(150.0, 600.0), 1.0),
        (spec.bands(600.0, 4000.0), 1.0),
        (spec.bands(4000.0, 20_000.0), -1.0),
    ];
    let mut score = 0.0;
    for (b, w) in bands {
        let env = spec.onset(logmel, b);
        let mut prof = [0.0; FOLD_BINS];
        fold(&env, period, phase, 0..env.len(), &mut prof);
        let at = |c: usize| {
            (0..5)
                .map(|d| prof[(c + FOLD_BINS + d - 2) % FOLD_BINS])
                .fold(0.0, f64::max)
                .max(1e-9)
        };
        score += w * (at(0) / at(FOLD_BINS / 2)).ln();
    }
    score
}

/// The first beat (counting from the one at `first_s`; -1 if that's within 30 ms of
/// the start) whose beat-long stretch has music in it.
fn first_music_beat(mono: &[f32], sr: f64, first_s: f64, beat_s: f64) -> Option<i64> {
    let level = |t: f64| -> f32 {
        let a = (t.max(0.0) * sr) as usize;
        let b = (((t + beat_s) * sr) as usize).min(mono.len());
        if b <= a {
            return -200.0;
        }
        let ms = mono[a..b].iter().map(|v| (v * v) as f64).sum::<f64>() / (b - a) as f64;
        (10.0 * ms.max(1e-30).log10()) as f32
    };
    let start: i64 = if first_s - beat_s >= -0.03 { -1 } else { 0 };
    let n = ((mono.len() as f64 / sr - first_s) / beat_s).floor() as i64;
    (start..n).find(|&k| level(first_s + k as f64 * beat_s) > MUSIC_DBFS)
}
