//! Keylock and key shift: a deck's key, apart from its tempo.
//!
//! The deck still plays its samples faster or slower, which moves the pitch with
//! the tempo; with keylock this shifts the pitch back by `1 / speed`, and a key
//! shift moves it on by `2^(semitones / 12)` (Signalsmith Stretch,
//! same length in and out, so timing is untouched). The shifter's output lags its
//! input, so the deck reads [`Keylock::lead`] ahead of the playhead to make up for
//! it, and after a jump the shifter is cleared and primed at the new place
//! ([`Keylock::restart`]) so the old audio doesn't smear into the new.
//!
//! Everything is allocated in [`Keylock::new`]; nothing allocates on the audio thread.

use signalsmith_stretch::Stretch;

/// Keylock steps aside outside this speed range (scratching, a stopping deck), where
/// pitch-shifting would sound worse than not.
pub const SPEEDS: std::ops::RangeInclusive<f64> = 0.5..=2.0;

/// The most a key shift moves the key, in semitones, up or down.
pub const KEY_SHIFT_MAX: f64 = 24.0;

/// The pitch factor `semitones` make.
pub fn semitones(semitones: f64) -> f64 {
    (semitones / 12.0).exp2()
}

pub struct Keylock {
    /// The shifter at its full setting and at its cheaper one (smaller blocks, less
    /// often, the work spread out): only one is in use at a time.
    stretch: [Stretch; 2],
    /// Frames from input to output (the shifter's input plus output latency), each.
    latency: [usize; 2],
    /// The cheaper one is in use ([`Keylock::set_cheap`]).
    cheap: bool,
    /// Interleaved stereo: what's been played (`out`), and audio to prime it with.
    out: Vec<f32>,
    preroll: Vec<f32>,
    /// It was used for the last block and the playhead then ended here; anything
    /// else means a jump (or a fresh start) and it must be primed again.
    live: bool,
    next_pos: f64,
}

impl Keylock {
    /// `max_block`: the most frames [`Keylock::process`] is given at once.
    pub fn new(sample_rate: u32, max_block: usize) -> Keylock {
        let stretch = [
            Stretch::preset_default(2, sample_rate),
            Stretch::preset_cheaper(2, sample_rate),
        ];
        let latency =
            [0, 1].map(|i: usize| stretch[i].input_latency() + stretch[i].output_latency());
        Keylock {
            stretch,
            latency,
            cheap: false,
            out: vec![0.0; max_block * 2],
            // The library asks for at least a block plus an interval of pre-roll,
            // which is about its latency; twice that is plenty.
            preroll: vec![0.0; latency[0].max(latency[1]) * 2 * 2],
            live: false,
            next_pos: 0.0,
        }
    }

    /// Use the cheaper setting (`true`) or the full one: for when the machine can't
    /// keep up ([`crate::load`]). Their delays differ, so on a change the shifter is
    /// primed again before its next block ([`Keylock::needs_restart`]), once. Both
    /// were made in [`Keylock::new`]: nothing is allocated here.
    pub fn set_cheap(&mut self, cheap: bool) {
        if cheap != self.cheap {
            self.cheap = cheap;
            self.live = false;
        }
    }

    /// The cheaper setting is in use.
    pub fn cheap(&self) -> bool {
        self.cheap
    }

    /// The delay of the setting in use, frames.
    pub fn latency(&self) -> usize {
        self.latency[self.cheap as usize]
    }

    /// How far ahead of the playhead (in track seconds) to read, at `step` track
    /// seconds per output frame.
    pub fn lead(&self, step: f64) -> f64 {
        self.latency() as f64 * step
    }

    /// Whether the shifter must be primed before playing from `pos`.
    pub fn needs_restart(&self, pos: f64) -> bool {
        !self.live || (pos - self.next_pos).abs() > 1e-9
    }

    /// Clear the shifter and prime it so its very next output is from the playhead.
    /// `fill(i)` gives the stereo frame at offset `i` of the pre-roll, which ends at
    /// the playhead plus [`Keylock::lead`]: frames `-len..0` relative to that, in
    /// order, where `len` is [`Keylock::preroll_frames`].
    pub fn restart(&mut self, mut fill: impl FnMut(usize) -> (f32, f32)) {
        let preroll = &mut self.preroll[..self.latency[self.cheap as usize] * 2 * 2];
        let stretch = &mut self.stretch[self.cheap as usize];
        stretch.reset();
        for (i, f) in preroll.chunks_exact_mut(2).enumerate() {
            (f[0], f[1]) = fill(i);
        }
        stretch.seek(&*preroll, 1.0);
    }

    pub fn preroll_frames(&self) -> usize {
        self.latency() * 2
    }

    /// Shift `buf` (interleaved stereo) by the pitch factor `transpose`, in place.
    /// Remembers where the playhead ended up (`pos`) to spot jumps.
    pub fn process(&mut self, buf: &mut [f32], transpose: f64, pos: f64) {
        let stretch = &mut self.stretch[self.cheap as usize];
        stretch.set_transpose_factor(transpose as f32, None);
        let out = &mut self.out[..buf.len()];
        stretch.process(&*buf, &mut *out);
        buf.copy_from_slice(out);
        self.live = true;
        self.next_pos = pos;
    }

    /// Not used for a block (no keylock or key shift, deck stopped, or out of range).
    pub fn idle(&mut self) {
        self.live = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switching_setting_primes_the_shifter_again_once() {
        let mut k = Keylock::new(48_000, 256);
        let full = k.latency();
        let mut buf = [0.0f32; 512];
        let mut play = |k: &mut Keylock, pos: f64| {
            if k.needs_restart(pos) {
                k.restart(|_| (0.0, 0.0));
            }
            k.process(&mut buf, 1.0, pos + 1.0);
        };
        play(&mut k, 0.0);
        assert!(!k.needs_restart(1.0), "carrying on");

        k.set_cheap(true);
        assert!(
            k.cheap() && k.needs_restart(1.0),
            "primed again for the change"
        );
        assert_ne!(k.latency(), full, "the cheaper setting has its own delay");
        assert!(k.preroll_frames() * 2 <= k.preroll.len());
        play(&mut k, 1.0);
        assert!(!k.needs_restart(2.0));
        // Told again what it already is: nothing to do.
        k.set_cheap(true);
        assert!(!k.needs_restart(2.0), "only once");

        k.set_cheap(false);
        assert!(k.needs_restart(2.0));
        assert_eq!(k.latency(), full);
    }
}
