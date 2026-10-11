//! How loud a track is, for auto-gain: integrated loudness in LUFS (ITU-R BS.1770-4 /
//! EBU R128) and the sample peak.
//!
//! The signal is K-weighted (a high shelf for how the head hears, and a high-pass to
//! ignore rumble), cut into 400 ms blocks overlapping by 75 %, and the mean square of
//! the blocks averaged. Silent blocks (below -70 LUFS) are ignored, then any block
//! 10 LU quieter than that average, so intros and breakdowns don't drag a track's
//! figure down.

use crate::decode::TrackAudio;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Loudness {
    /// Integrated loudness, LUFS. `-inf` for silence.
    pub lufs: f64,
    /// Largest sample, linear (1.0 = full scale).
    pub peak: f64,
}

/// Auto-gain brings tracks to this loudness: a little under a typical club master, so
/// loud tracks (whose decoded peaks often go over full scale) come down to about 0 dB.
pub const TARGET_LUFS: f64 = -11.0;

impl Loudness {
    /// The gain that brings the track to [`TARGET_LUFS`], linear. Turning down is
    /// unlimited; turning up stops where the peak would reach full scale.
    pub fn auto_gain(&self) -> f64 {
        if !self.lufs.is_finite() || self.peak <= 0.0 {
            return 1.0;
        }
        let g = 10f64.powf((TARGET_LUFS - self.lufs) / 20.0);
        if g > 1.0 {
            g.min(1.0 / self.peak).max(1.0)
        } else {
            g
        }
    }
}

/// A biquad, direct form I, one per channel.
#[derive(Clone, Copy, Default)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    x: [f64; 2],
    y: [f64; 2],
}

impl Biquad {
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.b[1] * self.x[0] + self.b[2] * self.x[1]
            - self.a[0] * self.y[0]
            - self.a[1] * self.y[1];
        self.x = [x, self.x[0]];
        self.y = [y, self.y[0]];
        y
    }
}

/// BS.1770's two K-weighting stages for `sr` (its 48 kHz coefficients, re-derived for
/// other rates from the filters' analogue prototypes, as libebur128 does).
fn k_weighting(sr: f64) -> [Biquad; 2] {
    use std::f64::consts::PI;
    // Stage 1: high shelf, +4 dB above ~1.7 kHz.
    let (f0, g, q) = (1681.974450955533, 3.999843853973347, 0.7071752369554196);
    let k = (PI * f0 / sr).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.4996667741545416);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad {
        b: [
            (vh + vb * k / q + k * k) / a0,
            2.0 * (k * k - vh) / a0,
            (vh - vb * k / q + k * k) / a0,
        ],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        ..Default::default()
    };
    // Stage 2: high-pass at ~38 Hz.
    let (f0, q) = (38.13547087602444, 0.5003270373238773);
    let k = (PI * f0 / sr).tan();
    let a0 = 1.0 + k / q + k * k;
    let hp = Biquad {
        b: [1.0, -2.0, 1.0],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        ..Default::default()
    };
    [shelf, hp]
}

fn lufs_of(mean_square: f64) -> f64 {
    -0.691 + 10.0 * mean_square.log10()
}

pub fn measure(audio: &TrackAudio) -> Loudness {
    let sr = audio.sample_rate as f64;
    let mut filters = [k_weighting(sr), k_weighting(sr)];
    // Sums of squares per 100 ms step; a block is four steps.
    let step = (sr * 0.1).round() as usize;
    let mut steps = Vec::with_capacity(audio.frames() / step + 1);
    let (mut sum, mut n, mut peak) = (0.0f64, 0usize, 0f32);
    for f in audio.samples.chunks_exact(2) {
        for (c, &s) in f.iter().enumerate() {
            peak = peak.max(s.abs());
            let [shelf, hp] = &mut filters[c];
            let y = hp.run(shelf.run(s as f64));
            sum += y * y;
        }
        n += 1;
        if n == step {
            steps.push(sum);
            (sum, n) = (0.0, 0);
        }
    }
    let blocks: Vec<f64> = steps
        .windows(4)
        .map(|w| w.iter().sum::<f64>() / (4 * step) as f64)
        .collect();
    let gated = |threshold: f64| {
        let kept: Vec<f64> = blocks
            .iter()
            .copied()
            .filter(|&z| lufs_of(z) > threshold)
            .collect();
        (!kept.is_empty()).then(|| kept.iter().sum::<f64>() / kept.len() as f64)
    };
    let lufs = gated(-70.0)
        .and_then(|z| gated(lufs_of(z) - 10.0))
        .map_or(f64::NEG_INFINITY, lufs_of);
    Loudness {
        lufs,
        peak: peak as f64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(amplitude: f32, hz: f64, secs: f64) -> TrackAudio {
        let sr = 48_000;
        let n = (secs * sr as f64) as usize;
        let samples = (0..n)
            .flat_map(|i| {
                let v =
                    amplitude * (std::f64::consts::TAU * hz * i as f64 / sr as f64).sin() as f32;
                [v, v]
            })
            .collect();
        TrackAudio {
            samples,
            sample_rate: sr,
        }
    }

    #[test]
    fn a_1k_sine_reads_its_level() {
        // The standard's check: a 1 kHz sine at -23 dBFS in both channels is -23 LUFS.
        let l = measure(&sine(10f32.powf(-23.0 / 20.0), 997.0, 10.0));
        assert!((l.lufs + 23.0).abs() < 0.1, "{}", l.lufs);
        assert!((20.0 * l.peak.log10() + 23.0).abs() < 0.01);
    }

    #[test]
    fn quiet_parts_dont_drag_it_down() {
        // 10 s loud then 10 s 30 dB quieter: the relative gate drops the quiet part.
        let mut a = sine(0.5, 997.0, 10.0);
        a.samples
            .extend(sine(0.5 * 10f32.powf(-1.5), 997.0, 10.0).samples);
        let loud = measure(&sine(0.5, 997.0, 10.0)).lufs;
        assert!((measure(&a).lufs - loud).abs() < 0.2);
        assert_eq!(measure(&sine(0.0, 997.0, 2.0)).lufs, f64::NEG_INFINITY);
    }

    #[test]
    fn auto_gain_turns_down_freely_but_boosts_only_to_full_scale() {
        let loud = Loudness {
            lufs: -8.0,
            peak: 1.0,
        };
        assert!((20.0 * loud.auto_gain().log10() + 3.0).abs() < 1e-9);
        let quiet = Loudness {
            lufs: -17.0,
            peak: 0.5,
        };
        // Wants +6 dB; the peak allows +6.02: fine.
        assert!((20.0 * quiet.auto_gain().log10() - 6.0).abs() < 1e-9);
        let peaky = Loudness {
            lufs: -17.0,
            peak: 0.9,
        };
        assert!((peaky.auto_gain() - 1.0 / 0.9).abs() < 1e-9);
    }
}
