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
//! The shifter works on a new slice of audio every 30 ms ([`INTERVAL_SECS`]), and
//! does a whole slice's transforms in the one block where the slice starts: about one
//! block in six costs many times the others, and a block can't borrow time from its
//! neighbours. Left alone, decks started together have their heavy blocks together,
//! and the worst block grows with every deck. So the decks are staggered: each
//! deck's slices start at its own offset in the cycle ([`Keylock::warm_frames`]), a
//! quarter of it apart, and the worst block is one deck's heavy block however many
//! play (lewinfox/earshot#32).
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

/// The shifter's cycle: it starts a new slice this often (Signalsmith Stretch's
/// default preset), and does that slice's heavy work at once.
const INTERVAL_SECS: f32 = 0.03;

/// Decks the cycle is shared between: each has its own quarter.
const SLOTS: usize = 4;

pub struct Keylock {
    /// Frames between the starts of two slices: the shifter's cycle.
    interval: usize,
    /// Where in the cycle (in frames of the engine's clock) this deck's slices start;
    /// and another deck's place, for the next restart only (an instant double starts
    /// in step with its original, so the two play the same samples).
    phase: usize,
    phase_once: Option<usize>,
    /// Audio run through the shifter and thrown away at a restart, to move its cycle
    /// to its place (interleaved stereo, in and out).
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
    /// which deck's this is (0-based), for its place in the cycle.
    pub fn new(sample_rate: u32, max_block: usize, slot: usize) -> Keylock {
        let stretch = Stretch::preset_default(2, sample_rate);
        let latency = stretch.input_latency() + stretch.output_latency();
        let interval = (sample_rate as f32 * INTERVAL_SECS) as usize;
        Keylock {
            interval,
            phase: (slot % SLOTS) * interval / SLOTS,
            phase_once: None,
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

    /// Where in the cycle this deck's slices start, in frames.
    pub fn phase(&self) -> usize {
        self.phase
    }

    /// Start afresh at the next block, in `phase` (another deck's [`Keylock::phase`])
    /// that once: restarted in the same block, the two then play the same samples.
    pub fn restart_in_phase(&mut self, phase: usize) {
        self.live = false;
        self.phase_once = Some(phase);
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
    /// through the shifter and throws away after the pre-roll. The shifter starts a
    /// slice on its first frame and every interval after: this many frames earlier,
    /// its slices fall on this deck's place in the cycle.
    pub fn warm_frames(&self, now: u64) -> usize {
        let i = self.interval as u64;
        let phase = self.phase_once.unwrap_or(self.phase) as u64 % i;
        ((now % i + i - phase) % i) as usize
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
        self.phase_once = None;
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

    /// Each deck's slices start at its own quarter of the cycle, whenever it's
    /// restarted: a quarter of 30 ms apart, so no two decks' heavy work falls in the
    /// same block of 256 frames.
    #[test]
    fn decks_have_their_own_place_in_the_cycle() {
        const SR: u32 = 48_000;
        let decks: Vec<Keylock> = (0..4).map(|d| Keylock::new(SR, 256, d)).collect();
        let interval = decks[0].interval as u64;
        assert_eq!(interval, 1440);
        for now in [0u64, 1, 255, 256, 1439, 1440, 99_999, 12_345_678] {
            // Where each deck's slices fall after a restart at `now`.
            let starts: Vec<u64> = decks
                .iter()
                .map(|k| (now + interval - k.warm_frames(now) as u64) % interval)
                .collect();
            assert_eq!(starts, [0, 360, 720, 1080], "restarted at {now}");
            for k in &decks {
                assert!(k.warm_frames(now) < k.interval);
            }
        }
        // An instant double takes its original's place, once.
        let mut double = Keylock::new(SR, 256, 1);
        double.restart_in_phase(decks[0].phase());
        assert_eq!(double.warm_frames(1000), decks[0].warm_frames(1000));
        double.restart(0, 1.0, |_| (0.0, 0.0));
        assert_eq!(double.warm_frames(1000), decks[1].warm_frames(1000));
    }
}
