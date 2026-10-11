//! Channel strip DSP: biquad filters, a 3-band isolator EQ and a DJ filter (low-pass
//! one way, high-pass the other). Everything here is allocation-free and runs on the
//! audio thread.

use std::f64::consts::PI;

/// Low/mid and mid/high crossovers of the isolator EQ (Mixxx's defaults, rounded).
pub const EQ_LOW_HZ: f64 = 250.0;
pub const EQ_HIGH_HZ: f64 = 2500.0;
const BUTTERWORTH_Q: f64 = std::f64::consts::FRAC_1_SQRT_2;

/// Biquad coefficients (RBJ cookbook), normalised so a0 = 1.
#[derive(Clone, Copy, Debug)]
pub struct Coeffs {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Coeffs {
    fn rbj(fc: f64, q: f64, sr: f64, high: bool) -> Coeffs {
        let w = 2.0 * PI * (fc / sr).clamp(1e-5, 0.49);
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        let (b0, b1) = if high {
            ((1.0 + cos) / 2.0, -(1.0 + cos))
        } else {
            ((1.0 - cos) / 2.0, 1.0 - cos)
        };
        Coeffs {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b0 / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
        }
    }

    pub fn lowpass(fc: f64, q: f64, sr: f64) -> Coeffs {
        Coeffs::rbj(fc, q, sr, false)
    }

    pub fn highpass(fc: f64, q: f64, sr: f64) -> Coeffs {
        Coeffs::rbj(fc, q, sr, true)
    }
}

/// One stereo biquad (transposed direct form II).
#[derive(Clone, Copy, Debug)]
pub struct Biquad {
    c: Coeffs,
    /// (z1, z2) per channel.
    z: [[f64; 2]; 2],
}

impl Biquad {
    pub fn new(c: Coeffs) -> Biquad {
        Biquad {
            c,
            z: [[0.0; 2]; 2],
        }
    }

    pub fn set(&mut self, c: Coeffs) {
        self.c = c;
    }

    pub fn reset(&mut self) {
        self.z = [[0.0; 2]; 2];
    }

    #[inline]
    pub fn run(&mut self, ch: usize, x: f64) -> f64 {
        let c = &self.c;
        let z = &mut self.z[ch];
        let y = c.b0 * x + z[0];
        z[0] = c.b1 * x - c.a1 * y + z[1];
        z[1] = c.b2 * x - c.a2 * y;
        y
    }
}

/// Linkwitz-Riley 4th order: two Butterworth biquads in a row. Its low and high
/// halves at one frequency add up to an all-pass, so splitting doesn't colour the sound.
#[derive(Clone, Copy, Debug)]
struct Lr4([Biquad; 2]);

impl Lr4 {
    fn new(c: Coeffs) -> Lr4 {
        Lr4([Biquad::new(c); 2])
    }

    #[inline]
    fn run(&mut self, ch: usize, x: f64) -> f64 {
        let y = self.0[0].run(ch, x);
        self.0[1].run(ch, y)
    }
}

/// A 3-band isolator, like a DJ mixer's EQ: each band's gain runs from 0 (killed) up,
/// and all three at 1 leave the level unchanged (the phase moves, as on hardware).
#[derive(Clone, Copy, Debug)]
pub struct Eq3 {
    low: Lr4,
    rest: Lr4,
    mid: Lr4,
    high: Lr4,
    /// The low band goes through the same all-pass as mid + high, to stay in phase.
    low_ap_lo: Lr4,
    low_ap_hi: Lr4,
}

impl Eq3 {
    pub fn new(sr: f64) -> Eq3 {
        let lp1 = Coeffs::lowpass(EQ_LOW_HZ, BUTTERWORTH_Q, sr);
        let hp1 = Coeffs::highpass(EQ_LOW_HZ, BUTTERWORTH_Q, sr);
        let lp2 = Coeffs::lowpass(EQ_HIGH_HZ, BUTTERWORTH_Q, sr);
        let hp2 = Coeffs::highpass(EQ_HIGH_HZ, BUTTERWORTH_Q, sr);
        Eq3 {
            low: Lr4::new(lp1),
            rest: Lr4::new(hp1),
            mid: Lr4::new(lp2),
            high: Lr4::new(hp2),
            low_ap_lo: Lr4::new(lp2),
            low_ap_hi: Lr4::new(hp2),
        }
    }

    /// One sample of channel `ch` with band gains `[low, mid, high]`.
    #[inline]
    pub fn run(&mut self, ch: usize, x: f64, g: [f64; 3]) -> f64 {
        let low = self.low.run(ch, x);
        let low = self.low_ap_lo.run(ch, low) + self.low_ap_hi.run(ch, low);
        let rest = self.rest.run(ch, x);
        let mid = self.mid.run(ch, rest);
        let high = self.high.run(ch, rest);
        g[0] * low + g[1] * mid + g[2] * high
    }
}

/// Filter knob positions this close to the centre leave the sound alone.
const FILTER_DEADZONE: f64 = 0.02;
const FILTER_Q: f64 = 0.9;

/// A one-knob DJ filter: -1..0 sweeps a low-pass down, 0..1 sweeps a high-pass up.
#[derive(Clone, Copy, Debug)]
pub struct DjFilter {
    stages: [Biquad; 2],
    /// -1 low-pass, 0 off, 1 high-pass: the mode the filter state belongs to.
    mode: i8,
    sr: f64,
}

impl DjFilter {
    pub fn new(sr: f64) -> DjFilter {
        DjFilter {
            stages: [Biquad::new(Coeffs::lowpass(20_000.0, FILTER_Q, sr)); 2],
            mode: 0,
            sr,
        }
    }

    /// Cut-off for knob position `v` (-1..1): low-pass from ~20 kHz down to 50 Hz,
    /// high-pass from 20 Hz up to 10 kHz, both swept exponentially.
    pub fn cutoff(v: f64) -> f64 {
        let a = v.abs().min(1.0);
        if v < 0.0 {
            20_000.0 * (50.0f64 / 20_000.0).powf(a)
        } else {
            20.0 * (10_000.0f64 / 20.0).powf(a)
        }
    }

    /// Set the knob position. Call once per block.
    pub fn set(&mut self, v: f64) {
        let mode = if v.abs() < FILTER_DEADZONE {
            0
        } else if v < 0.0 {
            -1
        } else {
            1
        };
        if mode != self.mode {
            for s in &mut self.stages {
                s.reset();
            }
            self.mode = mode;
        }
        if mode == 0 {
            return;
        }
        let fc = DjFilter::cutoff(v);
        let c = if mode < 0 {
            Coeffs::lowpass(fc, FILTER_Q, self.sr)
        } else {
            Coeffs::highpass(fc, FILTER_Q, self.sr)
        };
        for s in &mut self.stages {
            s.set(c);
        }
    }

    pub fn active(&self) -> bool {
        self.mode != 0
    }

    #[inline]
    pub fn run(&mut self, ch: usize, x: f64) -> f64 {
        let y = self.stages[0].run(ch, x);
        self.stages[1].run(ch, y)
    }
}

/// A peak limiter for a stereo bus, in place of hard clipping: when a frame would go
/// over the ceiling the gain drops at once to just fit it, then recovers over
/// [`LIMITER_RELEASE_SECS`]. Peaks are rounded off rather than chopped.
pub struct Limiter {
    gain: f32,
    release: f32,
}

/// Just under full scale.
pub const LIMITER_CEILING: f32 = 0.98;
pub const LIMITER_RELEASE_SECS: f64 = 0.15;

impl Limiter {
    pub fn new(sample_rate: f64) -> Limiter {
        Limiter {
            gain: 1.0,
            release: ease_per_sample(LIMITER_RELEASE_SECS, sample_rate) as f32,
        }
    }

    /// Limit interleaved stereo `buf` in place. Returns the lowest gain used (1.0 =
    /// untouched).
    pub fn process(&mut self, buf: &mut [f32]) -> f32 {
        let mut lowest = 1f32;
        for f in buf.chunks_exact_mut(2) {
            let peak = f[0].abs().max(f[1].abs());
            let fits = if peak > LIMITER_CEILING {
                LIMITER_CEILING / peak
            } else {
                1.0
            };
            self.gain = if fits < self.gain {
                fits
            } else {
                self.gain + (fits - self.gain) * self.release
            };
            f[0] *= self.gain;
            f[1] *= self.gain;
            lowest = lowest.min(self.gain);
        }
        lowest
    }
}

/// A value eased towards its target sample by sample, so knob moves don't click.
#[derive(Clone, Copy, Debug)]
pub struct Smoothed {
    pub current: f64,
    pub target: f64,
}

impl Smoothed {
    pub fn new(v: f64) -> Smoothed {
        Smoothed {
            current: v,
            target: v,
        }
    }

    /// `k`: the fraction of the remaining distance to move per sample.
    #[inline]
    pub fn next(&mut self, k: f64) -> f64 {
        self.current += (self.target - self.current) * k;
        self.current
    }
}

/// Per-sample easing factor for a time constant of `secs`.
pub fn ease_per_sample(secs: f64, sr: f64) -> f64 {
    1.0 - (-1.0 / (secs * sr)).exp()
}

#[cfg(test)]
mod tests {
    #[test]
    fn limiter_keeps_peaks_under_the_ceiling_and_recovers() {
        let sr = 48_000.0;
        let mut lim = super::Limiter::new(sr);
        // A 2x-over sine for a second, then quiet.
        let mut buf: Vec<f32> = (0..48_000)
            .flat_map(|i| {
                let v = 2.0 * (std::f64::consts::TAU * 100.0 * i as f64 / sr).sin() as f32;
                [v, v]
            })
            .collect();
        let lowest = lim.process(&mut buf);
        assert!(buf.iter().all(|s| s.abs() <= super::LIMITER_CEILING + 1e-6));
        assert!(lowest < 0.5);
        // Quiet afterwards: the gain comes back to 1 within a second.
        let mut quiet = vec![0.1f32; 96_000];
        lim.process(&mut quiet);
        assert!((quiet[quiet.len() - 1] - 0.1).abs() < 1e-3);
    }

    use super::*;

    const SR: f64 = 44_100.0;

    /// RMS of the last half of a sine at `hz` passed through `f`.
    fn rms_through(hz: f64, mut f: impl FnMut(f64) -> f64) -> f64 {
        let n = (SR * 0.5) as usize;
        let mut sum = 0.0;
        for i in 0..n {
            let x = (2.0 * PI * hz * i as f64 / SR).sin();
            let y = f(x);
            if i >= n / 2 {
                sum += y * y;
            }
        }
        (sum / (n / 2) as f64).sqrt()
    }

    fn db(gain: f64) -> f64 {
        20.0 * gain.log10()
    }

    const SINE_RMS: f64 = std::f64::consts::FRAC_1_SQRT_2;

    #[test]
    fn eq_at_unity_keeps_the_level() {
        for hz in [40.0, 250.0, 1000.0, 2500.0, 8000.0] {
            let mut eq = Eq3::new(SR);
            let r = rms_through(hz, |x| eq.run(0, x, [1.0; 3]));
            assert!(
                db(r / SINE_RMS).abs() < 0.1,
                "{hz} Hz: {:.2} dB",
                db(r / SINE_RMS)
            );
        }
    }

    #[test]
    fn eq_kills_each_band() {
        for (band, hz) in [(0, 50.0), (1, 800.0), (2, 10_000.0)] {
            let mut g = [1.0; 3];
            g[band] = 0.0;
            let mut eq = Eq3::new(SR);
            let r = rms_through(hz, |x| eq.run(0, x, g));
            assert!(
                db(r / SINE_RMS) < -24.0,
                "band {band} at {hz} Hz: {:.1} dB",
                db(r / SINE_RMS)
            );
        }
        // Killing the bass leaves the highs alone.
        let mut eq = Eq3::new(SR);
        let r = rms_through(8000.0, |x| eq.run(0, x, [0.0, 1.0, 1.0]));
        assert!(db(r / SINE_RMS).abs() < 0.5);
    }

    #[test]
    fn eq_boosts() {
        let mut eq = Eq3::new(SR);
        let r = rms_through(50.0, |x| eq.run(0, x, [2.0, 1.0, 1.0]));
        assert!(
            (db(r / SINE_RMS) - 6.0).abs() < 0.5,
            "{:.2} dB",
            db(r / SINE_RMS)
        );
    }

    #[test]
    fn filter_sweeps_both_ways() {
        let mut f = DjFilter::new(SR);
        f.set(0.0);
        assert!(!f.active());
        f.set(-0.7); // low-pass well down
        let lo = rms_through(100.0, |x| f.run(0, x));
        let mut f = DjFilter::new(SR);
        f.set(-0.7);
        let hi = rms_through(8000.0, |x| f.run(0, x));
        assert!(
            db(lo / SINE_RMS) > -3.0 && db(hi / SINE_RMS) < -30.0,
            "{lo} {hi}"
        );
        let mut f = DjFilter::new(SR);
        f.set(0.7); // high-pass well up
        let lo = rms_through(100.0, |x| f.run(0, x));
        let mut f = DjFilter::new(SR);
        f.set(0.7);
        let hi = rms_through(8000.0, |x| f.run(0, x));
        assert!(
            db(lo / SINE_RMS) < -30.0 && db(hi / SINE_RMS) > -3.0,
            "{lo} {hi}"
        );
    }
}
