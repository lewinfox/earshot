//! A stand-in "energy" number per track, until real energy analysis (Essentia in the
//! worker, see docs/design.md) exists: how loud the busy parts are, plus how bright.
//!
//! Only the order matters: the UI ranks tracks by it across the library.

use crate::decode::TrackAudio;

/// Loudness of the loudest half of the track (dBFS, so breakdowns and intros don't
/// drag it down) plus 20 × the share of energy above ~2 kHz (hats and claps).
pub fn energy(audio: &TrackAudio) -> f64 {
    let sr = audio.sample_rate as f64;
    let window = (sr * 0.4) as usize;
    let a = 1.0 - (-2.0 * std::f64::consts::PI * 2000.0 / sr).exp();
    let mut lp = 0.0f64;
    let (mut sum, mut high, mut all) = (0.0f64, 0.0f64, 0.0f64);
    let mut windows = Vec::new();
    for (i, f) in audio.samples.chunks_exact(2).enumerate() {
        let x = 0.5 * (f[0] + f[1]) as f64;
        lp += a * (x - lp);
        let h = x - lp;
        sum += x * x;
        high += h * h;
        all += x * x;
        if (i + 1) % window == 0 {
            windows.push(sum / window as f64);
            sum = 0.0;
        }
    }
    if windows.is_empty() || all <= 0.0 {
        return f64::NEG_INFINITY;
    }
    windows.sort_by(f64::total_cmp);
    let loud = &windows[windows.len() / 2..];
    let rms = (loud.iter().sum::<f64>() / loud.len() as f64).sqrt();
    let db = 20.0 * rms.max(1e-9).log10();
    db + 20.0 * (high / all)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freq: f32, amp: f32) -> TrackAudio {
        let sr = 44_100u32;
        let samples = (0..sr as usize * 4)
            .flat_map(|i| {
                let x = (2.0 * std::f32::consts::PI * freq * i as f32 / sr as f32).sin() * amp;
                [x, x]
            })
            .collect();
        TrackAudio {
            samples,
            sample_rate: sr,
        }
    }

    #[test]
    fn louder_and_brighter_score_higher() {
        let quiet = energy(&tone(100.0, 0.1));
        let loud = energy(&tone(100.0, 0.5));
        let bright = energy(&tone(6000.0, 0.5));
        assert!(quiet < loud && loud < bright, "{quiet} {loud} {bright}");
    }
}
