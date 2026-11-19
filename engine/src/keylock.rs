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
//! The shifter works on a new slice of audio every 30 ms, and left to itself does a
//! whole slice's transforms in the one block where the slice starts: about one block in
//! six costs many times the others, and a block can't borrow time from its neighbours.
//! So it runs with the library's `splitComputation` on, which spreads a slice's work
//! evenly over the 30 ms until the next one. The sound is the same, 30 ms later
//! (`split_computation_only_delays_the_sound`), which the lead already allows for; a
//! change of tempo or key is heard that much later too. (lewinfox/earshot#32)
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

/// How the shifter's heavy work is spread (an experiment for lewinfox/earshot#32:
/// `EARSHOT_KEYLOCK=plain|stagger|split`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Spread {
    /// As the library does it: a slice's work in the block where the slice starts.
    Plain,
    /// The same, but each deck's slices start at its own offset in the 30 ms cycle,
    /// so no block carries two decks' heavy work.
    Stagger,
    /// The library's `splitComputation`: a slice's work spread over its 30 ms.
    Split,
}

impl Spread {
    pub fn chosen() -> Spread {
        match std::env::var("EARSHOT_KEYLOCK").as_deref() {
            Ok("plain") => Spread::Plain,
            Ok("stagger") => Spread::Stagger,
            _ => Spread::Split,
        }
    }
}

/// Decks a cycle is shared between when staggering.
const SLOTS: usize = 4;

pub struct Keylock {
    spread: Spread,
    /// Frames between the starts of two slices: the shifter's cycle.
    interval: usize,
    /// Staggering: where in the cycle (in frames of the engine's clock) this deck's
    /// slices start.
    phase: usize,
    /// Staggering: audio run through the shifter and thrown away at a restart, to
    /// move its cycle (interleaved stereo, in and out).
    warm: Vec<f32>,
    warm_out: Vec<f32>,
    stretch: Stretch,
    /// Frames from input to output (the shifter's input plus output latency).
    latency: usize,
    /// Interleaved stereo: what's been played (`out`), and audio to prime it with.
    out: Vec<f32>,
    preroll: Vec<f32>,
    /// It was used for the last block and the playhead then ended here; anything
    /// else means a jump (or a fresh start) and it must be primed again.
    live: bool,
    next_pos: f64,
}

impl Keylock {
    /// `max_block`: the most frames [`Keylock::process`] is given at once. `slot`:
    /// which deck's this is (0-based), for its place in the cycle when staggering.
    pub fn new(sample_rate: u32, max_block: usize, slot: usize) -> Keylock {
        let spread = Spread::chosen();
        let stretch = match spread {
            // Split: a slice's work spread over its interval (see the module's notes).
            Spread::Split => Stretch::preset_default_split(2, sample_rate),
            _ => Stretch::preset_default(2, sample_rate),
        };
        let latency = stretch.input_latency() + stretch.output_latency();
        // The default preset's: 30 ms.
        let interval = (sample_rate as f32 * 0.03) as usize;
        Keylock {
            spread,
            interval,
            phase: (slot % SLOTS) * interval / SLOTS,
            warm: vec![0.0; interval * 2],
            warm_out: vec![0.0; interval * 2],
            stretch,
            latency,
            out: vec![0.0; max_block * 2],
            // The library asks for at least a block plus an interval of pre-roll,
            // which is about its latency; twice that is plenty.
            preroll: vec![0.0; latency * 2 * 2],
            live: false,
            next_pos: 0.0,
        }
    }

    /// How far ahead of the playhead (in track seconds) to read, at `step` track
    /// seconds per output frame.
    pub fn lead(&self, step: f64) -> f64 {
        self.latency as f64 * step
    }

    /// Whether the shifter must be primed before playing from `pos`.
    pub fn needs_restart(&self, pos: f64) -> bool {
        !self.live || (pos - self.next_pos).abs() > 1e-9
    }

    /// Frames of audio a restart at `now` (the engine's clock, in frames) runs
    /// through the shifter and throws away after the pre-roll, to put this deck's
    /// slices at its own place in the cycle (staggering; else none).
    pub fn warm_frames(&self, now: u64) -> usize {
        if self.spread != Spread::Stagger {
            return 0;
        }
        let i = self.interval as u64;
        ((now % i + i - self.phase as u64) % i) as usize
    }

    /// Clear the shifter and prime it so its very next output is from the playhead.
    /// `fill(i)` gives the stereo frame at offset `i` of the audio that ends at the
    /// playhead plus [`Keylock::lead`]: [`Keylock::preroll_frames`] of pre-roll, then
    /// `warm` frames ([`Keylock::warm_frames`]) that are processed and thrown away.
    pub fn restart(
        &mut self,
        warm: usize,
        transpose: f64,
        mut fill: impl FnMut(usize) -> (f32, f32),
    ) {
        self.stretch.reset();
        for (i, f) in self.preroll.chunks_exact_mut(2).enumerate() {
            (f[0], f[1]) = fill(i);
        }
        self.stretch.seek(&self.preroll, 1.0);
        if warm > 0 {
            let len = self.preroll.len() / 2;
            let warm = warm.min(self.interval);
            for (i, f) in self.warm[..warm * 2].chunks_exact_mut(2).enumerate() {
                (f[0], f[1]) = fill(len + i);
            }
            self.stretch.set_transpose_factor(transpose as f32, None);
            self.stretch
                .process(&self.warm[..warm * 2], &mut self.warm_out[..warm * 2]);
        }
    }

    pub fn preroll_frames(&self) -> usize {
        self.preroll.len() / 2
    }

    /// Shift `buf` (interleaved stereo) by the pitch factor `transpose`, in place.
    /// Remembers where the playhead ended up (`pos`) to spot jumps.
    pub fn process(&mut self, buf: &mut [f32], transpose: f64, pos: f64) {
        self.stretch.set_transpose_factor(transpose as f32, None);
        let out = &mut self.out[..buf.len()];
        self.stretch.process(&*buf, &mut *out);
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

    /// Spreading the shifter's work over the interval changes when the sound comes
    /// out (one interval later) and nothing else: against the same shifter doing
    /// each slice at once, the output is the same, sample for sample.
    #[test]
    fn split_computation_only_delays_the_sound() {
        const SR: u32 = 48_000;
        const BLOCK: usize = 256;
        let frames = SR as usize * 2;
        // A tone and some noise, different in each ear.
        let input: Vec<f32> = (0..frames)
            .flat_map(|i| {
                let t = i as f32 / SR as f32;
                let noise = ((i as u64).wrapping_mul(2654435761) % 1000) as f32 / 1000.0 - 0.5;
                let tone = (t * 440.0 * std::f32::consts::TAU).sin();
                [0.4 * tone + 0.2 * noise, 0.4 * tone - 0.2 * noise]
            })
            .collect();
        let run = |mut stretch: Stretch| {
            stretch.set_transpose_factor(1.0 / 1.08, None);
            let mut out = vec![0.0; input.len()];
            for (i, o) in input.chunks(BLOCK * 2).zip(out.chunks_mut(BLOCK * 2)) {
                stretch.process(i, o);
            }
            (out, stretch.output_latency())
        };
        let (at_once, latency) = run(Stretch::preset_default(2, SR));
        let (split, split_latency) = run(Stretch::preset_default_split(2, SR));
        let delay = split_latency - latency;
        assert_eq!(delay, (SR as f32 * 0.03) as usize, "one interval later");
        // Past the start (the shifter filling up), the same sound, `delay` later.
        let from = SR as usize / 2;
        let worst = (from..frames - delay)
            .flat_map(|i| [2 * i, 2 * i + 1].map(|a| (at_once[a] - split[a + 2 * delay]).abs()))
            .fold(0.0_f32, f32::max);
        assert!(worst < 1e-5, "differs by up to {worst}");
        assert!(
            at_once[2 * from..].iter().any(|v| v.abs() > 0.1),
            "and there is sound"
        );
    }
}
