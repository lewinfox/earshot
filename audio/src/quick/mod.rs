//! Quick analysis: a steady beat grid and a key from signal processing alone, in a
//! fraction of a second per track, in Rust (no worker, no model).
//!
//! It's for the moments analysis isn't there yet: a track loaded before the worker has
//! analysed it plays on this grid instead of one made from its BPM tag, and the
//! library can show a key. It assumes one tempo for the whole track and a 1 on the
//! first beat with music in it, which suits steady dance music; the worker's Beat
//! This! analysis (static or dynamic) stays the real one and replaces it.
//!
//! [`grid`] and [`key`] say how each works. Both are ported from ClubCraft
//! (<https://github.com/beneaze/ClubCraft>, `crates/analysis`), Copyright (c) 2026 The
//! ClubCraft contributors, used under the MIT licence (see `LICENSE-ClubCraft` in this
//! folder). See docs/quick-analysis.md for why and how it's evaluated.

mod dsp;
pub mod grid;
pub mod key;

use std::time::{Duration, Instant};

pub use grid::{GridOptions, QuickGrid};
pub use key::{Key, KeyEstimate};

use crate::decode::TrackAudio;

/// What [`analyse`] found, and how long it took.
#[derive(Debug, Clone)]
pub struct QuickAnalysis {
    /// `None` without a periodic beat.
    pub grid: Option<QuickGrid>,
    /// `None` without harmonic content.
    pub key: Option<KeyEstimate>,
    /// Wall time for the grid and the key (they run side by side).
    pub took: Duration,
}

/// The track's left and right channels summed to mono.
fn mono(audio: &TrackAudio) -> Vec<f32> {
    audio
        .samples
        .chunks_exact(2)
        .map(|f| 0.5 * (f[0] + f[1]))
        .collect()
}

/// The steady grid of a decoded track (see [`grid`]). `hint`: a tempo it's thought to
/// have (its BPM tag), searched around instead of the dance-music default.
pub fn grid(audio: &TrackAudio, hint: Option<f64>) -> Option<QuickGrid> {
    grid::detect(&mono(audio), audio.sample_rate as f64, &options(hint))
}

fn options(hint: Option<f64>) -> GridOptions {
    match hint.filter(|b| (50.0..=220.0).contains(b)) {
        Some(bpm) => GridOptions::around(bpm),
        None => GridOptions::default(),
    }
}

/// The grid and the key of a decoded track, worked out side by side. `hint` as for
/// [`grid()`].
pub fn analyse(audio: &TrackAudio, hint: Option<f64>) -> QuickAnalysis {
    let t = Instant::now();
    let m = mono(audio);
    let sr = audio.sample_rate as f64;
    let (grid, key) = std::thread::scope(|s| {
        let k = s.spawn(|| key::detect(&m, sr));
        let g = grid::detect(&m, sr, &options(hint));
        (g, k.join().expect("key detection panicked"))
    });
    QuickAnalysis {
        grid,
        key,
        took: t.elapsed(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::{DemoSpec, generate};

    fn demo(bpm: f64, seconds: f64, sr: u32, silence: f64) -> TrackAudio {
        let (mut audio, _) = generate(
            &DemoSpec {
                start_bpm: bpm,
                end_bpm: bpm,
                key: "8A".into(),
                seconds,
                seed: 7,
            },
            sr,
        );
        let pad = (silence * sr as f64) as usize * 2;
        audio.samples.splice(0..0, std::iter::repeat_n(0.0, pad));
        audio
    }

    #[test]
    fn finds_a_steady_tempo_and_the_first_downbeat() {
        for (bpm, sr) in [(126.0, 44_100), (122.0, 48_000), (100.0, 44_100)] {
            // Two seconds of silence first: the 1 is where the music starts.
            let audio = demo(bpm, 60.0, sr, 2.0);
            let g = grid(&audio, None).expect("a grid");
            assert!((g.grid.bpm - bpm).abs() < 0.02, "{bpm} at {sr}: {:?}", g);
            assert!(
                (g.grid.anchor - 2.0).abs() < 0.015,
                "{bpm} at {sr}: anchor {}",
                g.grid.anchor
            );
            assert!(g.confidence > 1.0, "{g:?}");
        }
    }

    #[test]
    fn half_time_reads_are_folded_into_range() {
        // 87 BPM is in range as it is; 87 doesn't become 174.
        let audio = demo(87.0, 60.0, 44_100, 0.0);
        let g = grid(&audio, None).expect("a grid");
        assert!((g.grid.bpm - 87.0).abs() < 0.02, "{g:?}");
    }

    #[test]
    fn drum_and_bass_needs_its_tag() {
        // With the dance-music default, 174 locks to two thirds of itself (a known
        // weakness, as in the worker's tempo pass); its BPM tag puts it right.
        let audio = demo(174.0, 60.0, 44_100, 0.0);
        let g = grid(&audio, None).expect("a grid");
        assert!(
            (g.grid.bpm - 116.0).abs() < 1.0 && g.confidence < 1.0,
            "{g:?}"
        );
        let g = grid(&audio, Some(174.0)).expect("a grid");
        assert!((g.grid.bpm - 174.0).abs() < 0.02, "{g:?}");
        // A tag that's a little off still finds the real tempo.
        let g = grid(&audio, Some(172.0)).expect("a grid");
        assert!((g.grid.bpm - 174.0).abs() < 0.02, "{g:?}");
    }

    #[test]
    fn silence_has_no_grid_or_key() {
        let audio = TrackAudio {
            samples: vec![0.0; 44_100 * 2 * 30],
            sample_rate: 44_100,
        };
        let a = analyse(&audio, None);
        assert!(a.grid.is_none() && a.key.is_none(), "{a:?}");
    }

    /// Chords as sustained sines with a few harmonics: `(seconds, MIDI notes)`.
    fn chords(sr: u32, parts: &[(f64, &[u8])]) -> TrackAudio {
        let sr_f = sr as f64;
        let mut samples = vec![];
        for (secs, notes) in parts {
            let n = (secs * sr_f) as usize;
            for i in 0..n {
                let t = i as f64 / sr_f;
                let v: f64 = notes
                    .iter()
                    .flat_map(|m| {
                        let f = 440.0 * 2f64.powf((*m as f64 - 69.0) / 12.0);
                        (1..=3).map(move |h| {
                            (std::f64::consts::TAU * f * h as f64 * t).sin() / h as f64
                        })
                    })
                    .sum();
                let v = (0.1 * v) as f32;
                samples.extend([v, v]);
            }
        }
        TrackAudio {
            samples,
            sample_rate: sr,
        }
    }

    #[test]
    fn finds_a_minor_key() {
        // Am, F, G, Am, four seconds each, twice round.
        let am: &[u8] = &[57, 60, 64];
        let f: &[u8] = &[53, 57, 60];
        let g: &[u8] = &[55, 59, 62];
        let p = [(4.0, am), (4.0, f), (4.0, g), (4.0, am)];
        let audio = chords(44_100, &[p, p].concat());
        let k = analyse(&audio, None).key.expect("a key");
        assert_eq!(k.key.camelot(), "8A", "{k:?}");
        assert!(k.tuning_cents.abs() < 5.0, "{k:?}");
    }

    #[test]
    fn a_five_minute_track_takes_well_under_a_second() {
        let audio = demo(124.0, 300.0, 44_100, 0.0);
        let a = analyse(&audio, None);
        assert!(a.grid.is_some());
        // Generous: debug builds and busy CI machines.
        assert!(a.took < Duration::from_secs(10), "{:?}", a.took);
        eprintln!("5-minute track: {:?}", a.took);
    }
}
