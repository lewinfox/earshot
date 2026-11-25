//! The musical key of a track: its tonic and whether it's major or minor.
//!
//! The usual method, in three steps:
//!
//! 1. A **chromagram**: how much energy the track has in each pitch class (C, C♯ …
//!    B), summed over the whole of it. The audio is mixed to mono and brought down
//!    to about 11 kHz (keys are in the notes, which are all below that), cut into
//!    overlapping windows long enough to tell bass notes a semitone apart, and each
//!    window's spectrum between [`LOW_HZ`] and [`HIGH_HZ`] is shared out by pitch.
//! 2. **Tuning**: a track needn't be at concert pitch (a record pitched up, a
//!    detuned synth), so pitch is counted in thirds of a semitone, and the third
//!    that holds the most energy is taken as where its notes are.
//! 3. **Key profiles**: the twelve numbers are compared (correlated) with what a
//!    piece in each of the 24 keys typically looks like, and the best match wins.
//!    The profiles are Sha'ath's (KeyFinder), made for dance music.
//!
//! How sure it is ([`Key::confidence`]) is how far the best match is ahead of the
//! best key that isn't the same notes (its relative major or minor shares them all,
//! and telling those two apart is the hard part).

use realfft::RealFftPlanner;

use crate::decode::TrackAudio;

/// The rate the audio is brought down to, about: the highest note counted is well
/// under half of it.
const RATE: f64 = 11_025.0;
/// Samples in a window (at about [`RATE`]: three quarters of a second, so bins are
/// 1.3 Hz apart, enough to tell semitones apart down to [`LOW_HZ`]) and between the
/// starts of windows.
const WINDOW: usize = 8192;
const HOP: usize = 4096;
/// The notes counted: from about A1 (below it a semitone is narrower than a bin
/// and a half) to about B6 (above it there's mostly hats and noise).
const LOW_HZ: f64 = 55.0;
const HIGH_HZ: f64 = 2000.0;
/// Parts of a semitone pitch is counted in, to find the tuning.
const PARTS: usize = 3;

/// How strongly each scale degree (semitones above the tonic) is heard in a major
/// and in a minor key: Sha'ath's profiles (KeyFinder), from dance music.
const MAJOR: [f64; 12] = [6.6, 2.0, 3.5, 2.3, 4.6, 4.0, 2.5, 5.2, 2.4, 3.7, 2.3, 3.4];
const MINOR: [f64; 12] = [6.5, 2.7, 3.5, 5.4, 2.6, 3.5, 2.5, 5.2, 4.0, 2.7, 4.3, 3.2];

/// A track's key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Key {
    /// The tonic's pitch class: 0 (C) to 11 (B).
    pub tonic: u8,
    pub minor: bool,
    /// How far ahead of any other set of notes it is: 0 (a toss-up) upwards; above
    /// about 0.1 is a clear answer.
    pub confidence: f64,
}

/// The track's key, or `None` if it has no notes to speak of (silence, drums only,
/// or too short to tell).
pub fn key(audio: &TrackAudio) -> Option<Key> {
    let chroma = chromagram(audio)?;
    from_chroma(&chroma)
}

/// The key a chromagram (energy by pitch class, C first) matches best.
pub fn from_chroma(chroma: &[f64; 12]) -> Option<Key> {
    if chroma.iter().sum::<f64>() <= 0.0 {
        return None;
    }
    // How well it matches each of the 24 keys.
    let mut scores = [(0.0, 0u8, false); 24];
    for tonic in 0..12 {
        for (minor, profile) in [(false, &MAJOR), (true, &MINOR)] {
            let mut rotated = [0.0; 12];
            for (degree, weight) in profile.iter().enumerate() {
                rotated[(tonic + degree) % 12] = *weight;
            }
            scores[tonic * 2 + minor as usize] =
                (correlation(chroma, &rotated), tonic as u8, minor);
        }
    }
    scores.sort_by(|a, b| b.0.total_cmp(&a.0));
    let (best, tonic, minor) = scores[0];
    // The same notes under another name: a minor key's relative major is three
    // semitones up, a major key's relative minor three down.
    let relative = |t: u8, m: bool| ((t + if m { 3 } else { 9 }) % 12, !m);
    let next = scores[1..]
        .iter()
        .find(|s| (s.1, s.2) != relative(tonic, minor))
        .map_or(0.0, |s| s.0);
    Some(Key {
        tonic,
        minor,
        confidence: best - next,
    })
}

/// Pearson's correlation of two sets of twelve numbers: 1 the same shape, 0 nothing
/// alike.
fn correlation(a: &[f64; 12], b: &[f64; 12]) -> f64 {
    let mean = |v: &[f64; 12]| v.iter().sum::<f64>() / 12.0;
    let (ma, mb) = (mean(a), mean(b));
    let (mut ab, mut aa, mut bb) = (0.0, 0.0, 0.0);
    for i in 0..12 {
        let (x, y) = (a[i] - ma, b[i] - mb);
        ab += x * y;
        aa += x * x;
        bb += y * y;
    }
    if aa <= 0.0 || bb <= 0.0 {
        return 0.0;
    }
    ab / (aa * bb).sqrt()
}

/// The track's energy by pitch class (C first), at its own tuning. `None` if it's
/// shorter than a window, or silent.
pub fn chromagram(audio: &TrackAudio) -> Option<[f64; 12]> {
    let (mono, rate) = downsample(audio);
    if mono.len() < WINDOW {
        return None;
    }
    // Each bin's place in the fine chroma: which third of which semitone it's in.
    let bin_hz = rate / WINDOW as f64;
    let (first, last) = (
        (LOW_HZ / bin_hz).ceil() as usize,
        ((HIGH_HZ / bin_hz).floor() as usize).min(WINDOW / 2),
    );
    let fine_of: Vec<usize> = (first..=last)
        .map(|bin| {
            // MIDI's numbering: 69 is A at 440 Hz, and 0 a C.
            let pitch = 69.0 + 12.0 * (bin as f64 * bin_hz / 440.0).log2();
            ((pitch * PARTS as f64).round() as i64).rem_euclid(12 * PARTS as i64) as usize
        })
        .collect();
    let hann: Vec<f32> = (0..WINDOW)
        .map(|i| {
            let x = std::f32::consts::TAU * i as f32 / WINDOW as f32;
            0.5 - 0.5 * x.cos()
        })
        .collect();
    let fft = RealFftPlanner::<f32>::new().plan_fft_forward(WINDOW);
    let (mut frame, mut spectrum) = (fft.make_input_vec(), fft.make_output_vec());
    let mut fine = [0.0f64; 12 * PARTS];
    for start in (0..=mono.len() - WINDOW).step_by(HOP) {
        for (out, (x, w)) in frame
            .iter_mut()
            .zip(mono[start..start + WINDOW].iter().zip(&hann))
        {
            *out = x * w;
        }
        if fft.process(&mut frame, &mut spectrum).is_err() {
            return None;
        }
        // Each window counts by its own loudness (a quiet intro for less), which
        // adding magnitudes does.
        for (bin, &part) in (first..=last).zip(&fine_of) {
            fine[part] += spectrum[bin].norm() as f64;
        }
    }
    // The tuning: the third of a semitone the notes are in is the one with the most.
    let tuning = (0..PARTS)
        .max_by(|&a, &b| {
            let energy = |t: usize| (0..12).map(|pc| fine[pc * PARTS + t]).sum::<f64>();
            energy(a).total_cmp(&energy(b))
        })
        .unwrap_or(0);
    // How far off concert pitch that is, in thirds of a semitone: the last third
    // is nearer the semitone above, whose notes it is, played flat.
    let off = if tuning * 2 > PARTS {
        tuning as i64 - PARTS as i64
    } else {
        tuning as i64
    };
    // Each pitch class: its third, and half of the thirds either side (a note's
    // energy spills a little).
    let n = (12 * PARTS) as i64;
    let at = |i: i64| fine[i.rem_euclid(n) as usize];
    let mut chroma = [0.0; 12];
    for (pc, out) in chroma.iter_mut().enumerate() {
        let centre = (pc * PARTS) as i64 + off;
        *out = at(centre) + 0.5 * (at(centre - 1) + at(centre + 1));
    }
    let total: f64 = chroma.iter().sum();
    (total > 0.0).then_some(chroma)
}

/// The track as mono at about [`RATE`], with the rate it came to: each sample the
/// average of a whole number of the track's (which also takes off what's above the
/// new rate's range, well enough for this).
fn downsample(audio: &TrackAudio) -> (Vec<f32>, f64) {
    let rate = audio.sample_rate as f64;
    let every = ((rate / RATE).round() as usize).max(1);
    let mono: Vec<f32> = audio
        .samples
        .chunks_exact(2 * every)
        .map(|frames| frames.iter().sum::<f32>() / (2 * every) as f32)
        .collect();
    (mono, rate / every as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const C: i32 = 0;
    const D: i32 = 2;
    const E: i32 = 4;
    const F: i32 = 5;
    const G: i32 = 7;
    const A: i32 = 9;
    const B: i32 = 11;

    /// Chords, one after another (each a second and a half), as a track: every note
    /// (semitones above the C below middle C) a tone with a few harmonics, over its
    /// root an octave down. `cents`: how far off concert pitch it all is.
    fn track(chords: &[&[i32]], cents: f64, rate: u32) -> TrackAudio {
        let each = (rate as f64 * 1.5) as usize;
        let mut samples = Vec::with_capacity(chords.len() * each * 2);
        for chord in chords {
            let mut notes: Vec<i32> = chord.to_vec();
            notes.push(chord[0] - 12);
            for i in 0..each {
                let t = i as f64 / rate as f64;
                let mut x = 0.0;
                for n in &notes {
                    // (Note 0 here is MIDI's 48.)
                    let hz = 440.0 * 2f64.powf((*n as f64 + 48.0 - 69.0 + cents / 100.0) / 12.0);
                    for (h, level) in [(1.0, 1.0), (2.0, 0.5), (3.0, 0.3), (4.0, 0.2)] {
                        x += level * (std::f64::consts::TAU * hz * h * t).sin();
                    }
                }
                let x = (x * 0.05) as f32;
                samples.extend([x, x]);
            }
        }
        TrackAudio {
            samples,
            sample_rate: rate,
        }
    }

    /// I, IV, V, I in a major key, or i, iv, v, VI in a minor one, twice over.
    fn progression(tonic: i32, minor: bool) -> Vec<Vec<i32>> {
        let third = if minor { 3 } else { 4 };
        let triad = |root: i32, third: i32| vec![root, root + third, root + 7];
        let once = if minor {
            vec![
                triad(tonic, 3),
                triad(tonic + 5, 3),
                triad(tonic + 7, 3),
                triad(tonic + 8, 4),
            ]
        } else {
            vec![
                triad(tonic, third),
                triad(tonic + 5, 4),
                triad(tonic + 7, 4),
                triad(tonic, third),
            ]
        };
        [once.clone(), once].concat()
    }

    fn found(tonic: i32, minor: bool, cents: f64, rate: u32) -> Key {
        let chords = progression(tonic, minor);
        let chords: Vec<&[i32]> = chords.iter().map(Vec::as_slice).collect();
        key(&track(&chords, cents, rate)).expect("a key")
    }

    #[test]
    fn progressions_are_heard_in_their_keys() {
        for (tonic, minor) in [
            (C, false),
            (G, false),
            (F, false),
            (A, true),
            (E, true),
            (D, true),
        ] {
            let k = found(tonic, minor, 0.0, 44_100);
            assert_eq!(
                (k.tonic as i32, k.minor),
                (tonic, minor),
                "tonic {tonic}, minor {minor}: {k:?}"
            );
            assert!(k.confidence > 0.0, "{k:?}");
        }
        // Every tonic, both modes.
        for tonic in 0..12 {
            for minor in [false, true] {
                let k = found(tonic, minor, 0.0, 44_100);
                assert_eq!((k.tonic as i32, k.minor), (tonic, minor), "{k:?}");
            }
        }
    }

    #[test]
    fn a_track_off_concert_pitch_is_still_in_its_key() {
        // A third of a semitone sharp and flat (a record pitched 2 %), at another
        // sample rate.
        for cents in [-33.0, -20.0, 20.0, 33.0] {
            let k = found(A, true, cents, 48_000);
            assert_eq!((k.tonic as i32, k.minor), (A, true), "{cents} cents: {k:?}");
            let k = found(B, false, cents, 48_000);
            assert_eq!(
                (k.tonic as i32, k.minor),
                (B, false),
                "{cents} cents: {k:?}"
            );
        }
    }

    #[test]
    fn silence_and_scraps_have_no_key() {
        let silent = TrackAudio {
            samples: vec![0.0; 44_100 * 2 * 3],
            sample_rate: 44_100,
        };
        assert_eq!(key(&silent), None);
        let scrap = TrackAudio {
            samples: vec![0.1; 2000],
            sample_rate: 44_100,
        };
        assert_eq!(key(&scrap), None);
    }

    #[test]
    fn a_chromagram_of_one_key_matches_it_and_its_relative_is_no_rival() {
        // A minor's own profile: A minor, and sure of it (C major has the same
        // notes, and doesn't count against it).
        let mut chroma = [0.0; 12];
        for (degree, w) in MINOR.iter().enumerate() {
            chroma[(9 + degree) % 12] = *w;
        }
        let k = from_chroma(&chroma).unwrap();
        assert_eq!((k.tonic, k.minor), (9, true));
        assert!(k.confidence > 0.1, "{k:?}");
        assert_eq!(from_chroma(&[0.0; 12]), None);
    }
}
