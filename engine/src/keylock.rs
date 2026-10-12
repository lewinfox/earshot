//! Keylock: keep a deck's key while its tempo changes.
//!
//! The deck still plays its samples faster or slower, which moves the pitch with
//! the tempo; this then shifts the pitch back by `1 / speed` (Signalsmith Stretch,
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

pub struct Keylock {
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
    /// `max_block`: the most frames [`Keylock::process`] is given at once.
    pub fn new(sample_rate: u32, max_block: usize) -> Keylock {
        let stretch = Stretch::preset_default(2, sample_rate);
        let latency = stretch.input_latency() + stretch.output_latency();
        Keylock {
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

    /// Clear the shifter and prime it so its very next output is from the playhead.
    /// `fill(i)` gives the stereo frame at offset `i` of the pre-roll, which ends at
    /// the playhead plus [`Keylock::lead`]: frames `-len..0` relative to that, in
    /// order, where `len` is [`Keylock::preroll_frames`].
    pub fn restart(&mut self, mut fill: impl FnMut(usize) -> (f32, f32)) {
        self.stretch.reset();
        for (i, f) in self.preroll.chunks_exact_mut(2).enumerate() {
            (f[0], f[1]) = fill(i);
        }
        self.stretch.seek(&self.preroll, 1.0);
    }

    pub fn preroll_frames(&self) -> usize {
        self.preroll.len() / 2
    }

    /// Shift `buf` (interleaved stereo, read at `speed`) back to its original key, in
    /// place. Remembers where the playhead ended up (`pos`) to spot jumps.
    pub fn process(&mut self, buf: &mut [f32], speed: f64, pos: f64) {
        self.stretch
            .set_transpose_factor((1.0 / speed) as f32, None);
        let out = &mut self.out[..buf.len()];
        self.stretch.process(&*buf, &mut *out);
        buf.copy_from_slice(out);
        self.live = true;
        self.next_pos = pos;
    }

    /// Not used for a block (keylock off, deck stopped, or out of range).
    pub fn idle(&mut self) {
        self.live = false;
    }
}
