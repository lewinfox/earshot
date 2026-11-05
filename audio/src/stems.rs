//! A track's stems, as a deck holds them (issue #2).
//!
//! Three stems are stored: drums, bass and vocals. The fourth, "other", is never
//! stored: it's the track minus those three. So a deck plays
//! `g_other·track + Σ (g_s − g_other)·stem_s`, which at every level 1 is the track
//! itself, exactly.
//!
//! Held in half precision (2 bytes a sample, not 4): a 6-minute track's three stems
//! take about 190 MB, not 380. The rounding is far below the separation's own errors.

use half::f16;

use crate::decode::TrackAudio;

/// Every stem, in the order of the deck's stem controls. The first [`STORED`] are
/// held; the last is derived.
pub const NAMES: [&str; 4] = ["Drums", "Bass", "Vocals", "Other"];
pub const COUNT: usize = NAMES.len();
pub const STORED: usize = 3;
/// The derived stem's index.
pub const OTHER: usize = 3;

/// The stored stems, interleaved: per frame, drums L R, bass L R, vocals L R.
pub struct StemAudio {
    samples: Vec<f16>,
    frames: usize,
    pub sample_rate: u32,
}

impl StemAudio {
    /// From drums, bass and vocals as decoded (each stereo). Lengths are matched to
    /// `frames` (the track's): a shorter stem is padded with silence.
    pub fn new(stems: [&TrackAudio; STORED], frames: usize) -> StemAudio {
        let mut samples = vec![f16::ZERO; frames * STORED * 2];
        for (s, a) in stems.iter().enumerate() {
            for (f, lr) in a.samples.chunks_exact(2).take(frames).enumerate() {
                samples[f * 6 + s * 2] = f16::from_f32(lr[0]);
                samples[f * 6 + s * 2 + 1] = f16::from_f32(lr[1]);
            }
        }
        StemAudio {
            samples,
            frames,
            sample_rate: stems[0].sample_rate,
        }
    }

    /// The samples as held, for keeping them in RAM.
    pub fn raw(&self) -> &[f16] {
        &self.samples
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Stem `s`'s (left, right) at frame `k`; silence outside the track.
    #[inline]
    pub fn get(&self, k: i64, s: usize) -> (f32, f32) {
        if k < 0 || k as usize >= self.frames {
            return (0.0, 0.0);
        }
        let i = k as usize * 6 + s * 2;
        (self.samples[i].to_f32(), self.samples[i + 1].to_f32())
    }

    /// Stem `s` (any of [`COUNT`]) as plain stereo, for its waveform. "Other" is
    /// worked out from `track`.
    pub fn stem(&self, s: usize, track: &TrackAudio) -> TrackAudio {
        let mut samples = vec![0.0f32; self.frames * 2];
        for (f, out) in samples.chunks_exact_mut(2).enumerate() {
            let (l, r) = if s == OTHER {
                let mut l = track.samples.get(f * 2).copied().unwrap_or(0.0);
                let mut r = track.samples.get(f * 2 + 1).copied().unwrap_or(0.0);
                for k in 0..STORED {
                    let (sl, sr) = self.get(f as i64, k);
                    l -= sl;
                    r -= sr;
                }
                (l, r)
            } else {
                self.get(f as i64, s)
            };
            out[0] = l;
            out[1] = r;
        }
        TrackAudio {
            samples,
            sample_rate: self.sample_rate,
        }
    }
}
