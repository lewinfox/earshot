//! Synthetic tracks so the spike runs without a music folder: a kick on every beat
//! (louder on the 1), off-beat hats and a bassline in the track's key. The tempo can
//! drift from `start_bpm` to `end_bpm`, and the exact beat times come back as a grid.

use crate::beatgrid::Beatgrid;
use crate::decode::TrackAudio;

pub struct DemoSpec {
    pub start_bpm: f64,
    pub end_bpm: f64,
    /// Camelot key, e.g. "8A".
    pub key: String,
    pub seconds: f64,
    pub seed: u64,
}

pub fn generate(spec: &DemoSpec, sample_rate: u32) -> (TrackAudio, Beatgrid) {
    let sr = sample_rate as f64;
    let n = (spec.seconds * sr) as usize;
    let mut out = vec![0f32; n * 2];
    let bass_hz = camelot_bass_hz(&spec.key);
    let mut rng = spec.seed | 1;

    // Integrate tempo to find beat starts; tempo is linear in time.
    let mut beats = Vec::new();
    let mut phase = 0.0f64; // in beats
    let mut last_beat = -1i64;
    let mut last_eighth = -1i64;
    let mut kick_start = usize::MAX;
    let mut kick_gain = 0.0f32;
    let mut hat_start = usize::MAX;
    let mut bass_start = usize::MAX;
    let mut bass_note = 1.0f64;
    let mut bass_phase = 0.0f64;
    for i in 0..n {
        let t = i as f64 / sr;
        let bpm = spec.start_bpm + (spec.end_bpm - spec.start_bpm) * (t / spec.seconds);
        let b = phase.floor() as i64;
        if b != last_beat {
            last_beat = b;
            beats.push(t);
            kick_start = i;
            kick_gain = if b % 4 == 0 { 1.0 } else { 0.7 };
        }
        let e = (phase * 2.0).floor() as i64;
        if e != last_eighth {
            last_eighth = e;
            if e % 2 == 1 {
                hat_start = i;
            }
            bass_start = i;
            // Root most of the time, a fifth or octave now and then.
            bass_note = match (e + spec.seed as i64) % 8 {
                3 => 1.5,
                6 => 2.0,
                _ => 1.0,
            };
        }
        phase += bpm / 60.0 / sr;

        let mut s = 0.0f32;
        if kick_start != usize::MAX {
            let kt = (i - kick_start) as f64 / sr;
            if kt < 0.3 {
                let f = 50.0 + 90.0 * (-kt * 30.0).exp();
                let env = (-kt * 12.0).exp() as f32;
                s += kick_gain * env * (2.0 * std::f64::consts::PI * f * kt).sin() as f32;
            }
        }
        if hat_start != usize::MAX {
            let ht = (i - hat_start) as f64 / sr;
            if ht < 0.05 {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                let noise = (rng as f64 / u64::MAX as f64) as f32 * 2.0 - 1.0;
                s += 0.25 * (-ht * 90.0).exp() as f32 * noise;
            }
        }
        if bass_start != usize::MAX {
            let bt = (i - bass_start) as f64 / sr;
            bass_phase = (bass_phase + bass_hz * bass_note / sr).fract();
            let saw = (2.0 * bass_phase - 1.0) as f32;
            let env = ((bt * 200.0).min(1.0) * (-bt * 5.0).exp()) as f32;
            s += 0.3 * env * saw;
        }
        out[2 * i] = 0.7 * s;
        out[2 * i + 1] = 0.7 * s;
    }

    let positions = (0..beats.len()).map(|i| (i % 4) as u8 + 1).collect();
    let grid = Beatgrid::new(beats, positions).expect("demo has beats");
    (
        TrackAudio {
            samples: out,
            sample_rate,
        },
        grid,
    )
}

/// Root of a Camelot key as a bass frequency between A1 (55 Hz) and G#2.
pub fn camelot_bass_hz(key: &str) -> f64 {
    let (num, letter) = key.split_at(key.len().saturating_sub(1));
    let num: i64 = num.parse().unwrap_or(8);
    // 8B is C major; each step round the wheel is a fifth. 'A' keys are the relative minor.
    let mut root_from_c = ((num - 8) * 7).rem_euclid(12);
    if letter.eq_ignore_ascii_case("A") {
        root_from_c = (root_from_c - 3).rem_euclid(12);
    }
    let from_a = (root_from_c + 3) % 12;
    55.0 * 2f64.powf(from_a as f64 / 12.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_grid_matches_requested_tempo() {
        let spec = DemoSpec {
            start_bpm: 120.0,
            end_bpm: 130.0,
            key: "8A".into(),
            seconds: 30.0,
            seed: 1,
        };
        let (audio, grid) = generate(&spec, 44_100);
        assert_eq!(audio.frames(), 30 * 44_100);
        assert!((grid.bpm_at(1.0, 2) - 120.0).abs() < 1.0);
        assert!((grid.bpm_at(29.0, 2) - 130.0).abs() < 1.0);
        assert_eq!(grid.positions()[0], 1);
    }

    #[test]
    fn camelot_roots() {
        assert!((camelot_bass_hz("8A") - 55.0).abs() < 0.01); // A minor
        assert!((camelot_bass_hz("8B") - 65.406).abs() < 0.01); // C major
    }
}
