//! The small DSP toolbox the quick analysis shares: a windowed STFT, a mel filterbank,
//! autocorrelation and a few helpers.

use realfft::RealFftPlanner;
use realfft::num_complex::Complex;

/// Periodic Hann window.
fn hann(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos()) as f32)
        .collect()
}

/// Centred STFT frames for `len` samples (frame `i` is centred on sample `i * hop`).
fn n_frames(len: usize, hop: usize) -> usize {
    len.div_ceil(hop)
}

/// Threads to split the STFT over.
fn threads() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get().min(8))
}

/// A centred, Hann-windowed STFT, frames spread over a few threads. Frame `i` is
/// centred on sample `i * hop` (zero-padded at the edges); `f` maps each spectrum
/// (`n_fft / 2 + 1` bins) to a result. Scaled so a full-scale sine peaks at about 0.5.
pub fn stft_map<T, F>(x: &[f32], n_fft: usize, hop: usize, f: F) -> Vec<T>
where
    T: Send,
    F: Fn(&[Complex<f32>]) -> T + Sync,
{
    let frames = n_frames(x.len(), hop);
    let plan = RealFftPlanner::<f32>::new().plan_fft_forward(n_fft);
    let win = hann(n_fft);
    let scale = 1.0 / win.iter().sum::<f32>();
    let win: Vec<f32> = win.iter().map(|w| w * scale).collect();
    let half = (n_fft / 2) as isize;
    let per = frames.div_ceil(threads()).max(1);
    std::thread::scope(|s| {
        let parts: Vec<_> = (0..frames)
            .step_by(per)
            .map(|from| {
                let (plan, win, f) = (&plan, &win, &f);
                s.spawn(move || {
                    let (mut inp, mut out, mut scratch) = (
                        plan.make_input_vec(),
                        plan.make_output_vec(),
                        plan.make_scratch_vec(),
                    );
                    (from..(from + per).min(frames))
                        .map(|i| {
                            let start = (i * hop) as isize - half;
                            for (j, o) in inp.iter_mut().enumerate() {
                                let idx = start + j as isize;
                                *o = if idx >= 0 && (idx as usize) < x.len() {
                                    x[idx as usize] * win[j]
                                } else {
                                    0.0
                                };
                            }
                            if plan
                                .process_with_scratch(&mut inp, &mut out, &mut scratch)
                                .is_err()
                            {
                                out.fill(Complex::default());
                            }
                            f(&out)
                        })
                        .collect::<Vec<T>>()
                })
            })
            .collect();
        parts
            .into_iter()
            .flat_map(|p| p.join().expect("an STFT thread panicked"))
            .collect()
    })
}

/// Triangular mel filterbank (HTK mel scale) on magnitude spectra.
pub struct MelBank {
    filters: Vec<(usize, Vec<f32>)>,
    pub centers: Vec<f32>,
}

fn hz_to_mel(f: f64) -> f64 {
    2595.0 * (1.0 + f / 700.0).log10()
}

fn mel_to_hz(m: f64) -> f64 {
    700.0 * (10f64.powf(m / 2595.0) - 1.0)
}

impl MelBank {
    pub fn new(n_fft: usize, sr: f64, n_mels: usize, fmin: f64, fmax: f64) -> MelBank {
        let bins = n_fft / 2 + 1;
        let bin_hz = sr / n_fft as f64;
        let (m0, m1) = (hz_to_mel(fmin), hz_to_mel(fmax.min(sr / 2.0)));
        let pts: Vec<f64> = (0..n_mels + 2)
            .map(|i| mel_to_hz(m0 + (m1 - m0) * i as f64 / (n_mels + 1) as f64))
            .collect();
        let mut filters = Vec::with_capacity(n_mels);
        let mut centers = Vec::with_capacity(n_mels);
        for m in 0..n_mels {
            let (lo, c, hi) = (pts[m], pts[m + 1], pts[m + 2]);
            // Every filter covers at least one bin (low mel bands are narrower than one).
            let lo_b = ((lo / bin_hz).floor() as usize).min(bins - 1);
            let hi_b = ((hi / bin_hz).ceil() as usize).min(bins - 1);
            let mut w: Vec<f32> = (lo_b..=hi_b)
                .map(|b| {
                    let f = b as f64 * bin_hz;
                    let v = if f <= c {
                        (f - lo) / (c - lo)
                    } else {
                        (hi - f) / (hi - c)
                    };
                    v.clamp(0.0, 1.0) as f32
                })
                .collect();
            if w.iter().all(|v| *v == 0.0) {
                let nb = ((c / bin_hz).round() as usize).clamp(lo_b, hi_b);
                w[nb - lo_b] = 1.0;
            }
            let s: f32 = w.iter().sum();
            for v in &mut w {
                *v /= s.max(1e-9);
            }
            filters.push((lo_b, w));
            centers.push(c as f32);
        }
        MelBank { filters, centers }
    }

    pub fn apply(&self, spec: &[Complex<f32>], out: &mut [f32]) {
        for ((start, w), o) in self.filters.iter().zip(out.iter_mut()) {
            *o = w
                .iter()
                .enumerate()
                .map(|(j, wv)| wv * spec[start + j].norm())
                .sum();
        }
    }
}

/// Centred moving average over `w` samples.
pub fn moving_average(x: &[f32], w: usize) -> Vec<f32> {
    let n = x.len();
    let h = w / 2;
    let mut pre = vec![0.0f64; n + 1];
    for i in 0..n {
        pre[i + 1] = pre[i] + x[i] as f64;
    }
    (0..n)
        .map(|i| {
            let a = i.saturating_sub(h);
            let b = (i + h + 1).min(n);
            ((pre[b] - pre[a]) / (b - a) as f64) as f32
        })
        .collect()
}

/// `x` at fractional index `pos`, linearly interpolated (0 outside).
pub fn lerp_at(x: &[f32], pos: f64) -> f32 {
    if pos < 0.0 {
        return 0.0;
    }
    let i = pos.floor() as usize;
    if i + 1 >= x.len() {
        return x.get(i).copied().unwrap_or(0.0);
    }
    let f = (pos - i as f64) as f32;
    x[i] * (1.0 - f) + x[i + 1] * f
}

/// The peak's fractional offset (-0.5..0.5) from the middle of three samples.
pub fn parabolic(ym: f64, y0: f64, yp: f64) -> f64 {
    let d = ym - 2.0 * y0 + yp;
    if d.abs() < 1e-12 {
        0.0
    } else {
        (0.5 * (ym - yp) / d).clamp(-0.5, 0.5)
    }
}

/// Autocorrelation up to `max_lag` (zero-padded, via FFT), normalised so r[0] = 1.
pub fn autocorr(x: &[f32], max_lag: usize) -> Vec<f32> {
    let n = x.len();
    let size = (2 * n).next_power_of_two();
    let mut planner = RealFftPlanner::<f32>::new();
    let fwd = planner.plan_fft_forward(size);
    let inv = planner.plan_fft_inverse(size);
    let mut inp = fwd.make_input_vec();
    inp[..n].copy_from_slice(x);
    let mut spec = fwd.make_output_vec();
    if fwd.process(&mut inp, &mut spec).is_err() {
        spec.fill(Complex::default());
    }
    for c in spec.iter_mut() {
        *c = Complex::new(c.norm_sqr(), 0.0);
    }
    let mut out = inv.make_output_vec();
    if inv.process(&mut spec, &mut out).is_err() {
        out.fill(0.0);
    }
    let r0 = out[0].max(1e-12);
    out.truncate(max_lag.min(n));
    for v in out.iter_mut() {
        *v /= r0;
    }
    out
}

/// Median of a small buffer (reorders it).
pub fn median_inplace(v: &mut [f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let m = v.len() / 2;
    *v.select_nth_unstable_by(m, |a, b| a.total_cmp(b)).1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sine_peaks_in_its_bin_at_about_half() {
        let sr = 44_100.0;
        let n_fft = 1024;
        let bin = 40;
        let hz = bin as f64 * sr / n_fft as f64;
        let x: Vec<f32> = (0..8192)
            .map(|i| (std::f64::consts::TAU * hz * i as f64 / sr).sin() as f32)
            .collect();
        let mags = stft_map(&x, n_fft, 256, |s| s[bin].norm());
        // Frames in the middle (the edges are zero-padded).
        let mid = mags[mags.len() / 2];
        assert!((mid - 0.5).abs() < 0.05, "{mid}");
        assert_eq!(mags.len(), 8192 / 256);
    }

    #[test]
    fn autocorrelation_finds_the_period() {
        let x: Vec<f32> = (0..1000).map(|i| (i % 50 == 0) as u8 as f32).collect();
        let r = autocorr(&x, 200);
        assert_eq!(r[0], 1.0);
        let best = (10..200).max_by(|a, b| r[*a].total_cmp(&r[*b])).unwrap();
        assert_eq!(best % 50, 0);
    }
}
