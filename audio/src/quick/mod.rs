//! Quick analysis: a steady beat grid from signal processing alone, in a fraction of
//! a second per track, in Rust (no worker, no model).
//!
//! It's for the moments analysis isn't there yet: a track loaded before the worker has
//! analysed it plays on this grid instead of one made from its BPM tag. It assumes one
//! tempo for the whole track and a 1 on the first beat with music in it, which suits
//! steady dance music; the worker's Beat This! analysis (static or dynamic) stays the
//! real one and replaces it.
//!
//! [`grid`] says how it works. It's ported from ClubCraft
//! (<https://github.com/beneaze/ClubCraft>, `crates/analysis`), Copyright (c) 2026 The
//! ClubCraft contributors, used under the MIT licence (see `LICENSE-ClubCraft` in this
//! folder). See docs/quick-analysis.md for why and how it's evaluated.

mod dsp;
mod grid;

use grid::GridOptions;
pub use grid::QuickGrid;

use crate::decode::TrackAudio;

/// The track's left and right channels summed to mono.
fn mono(audio: &TrackAudio) -> Vec<f32> {
    audio
        .samples
        .chunks_exact(2)
        .map(|f| 0.5 * (f[0] + f[1]))
        .collect()
}

/// The steady grid of a decoded track (see [`grid`]), or `None` without a periodic
/// beat. `hint`: a tempo it's thought to have (its BPM tag), searched around instead
/// of the dance-music default.
pub fn grid(audio: &TrackAudio, hint: Option<f64>) -> Option<QuickGrid> {
    let opts = match hint.filter(|b| (50.0..=220.0).contains(b)) {
        Some(bpm) => GridOptions::around(bpm),
        None => GridOptions::default(),
    };
    grid::detect(&mono(audio), audio.sample_rate as f64, &opts)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

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
    fn silence_has_no_grid() {
        let audio = TrackAudio {
            samples: vec![0.0; 44_100 * 2 * 30],
            sample_rate: 44_100,
        };
        assert!(grid(&audio, None).is_none());
    }

    #[test]
    fn a_five_minute_track_takes_well_under_a_second() {
        let audio = demo(124.0, 300.0, 44_100, 0.0);
        let t = Instant::now();
        assert!(grid(&audio, None).is_some());
        let took = t.elapsed();
        // Generous: debug builds and busy CI machines.
        assert!(took < Duration::from_secs(10), "{took:?}");
        eprintln!("5-minute track: {took:?}");
    }
}
