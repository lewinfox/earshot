//! Pad effects: one momentary effect slot per deck, for a controller's PAD FX pads.
//! An effect is on while its pad is held and off when it's let go
//! ([`crate::DeckCmd::PadFx`]).
//!
//! The slot sits straight after the deck's channel (gain, EQ, filter), before its
//! fader, and has nothing to do with the effect units ([`crate::fx`]), which come
//! after the fader. It runs in time with its own deck.
//!
//! - Echo and Trans are the effect units' own.
//! - Roll catches the sound from the press, for its length, and repeats it.
//! - The filter sweeps close a low-pass (or open a high-pass) and bring it back, once
//!   every length, starting from wide open at the press.
//! - Reverb: its length is how long the tail takes to die away.
//! - Vinyl brake slows the sound to a stop over its length, like a turntable switched
//!   off; back spin plays it backwards, fast, slowing to a stop. Both work on a
//!   recording of the channel, not on the deck: the track plays on underneath, and is
//!   where it would have been when the pad is let go.
//!
//! Echo and Reverb ring out after the pad is let go; the others stop with it.
//!
//! Everything is allocated in [`PadFxSlot::new`]; nothing allocates on the audio
//! thread.

use crate::dsp::{self, Smoothed};
use crate::fx::{BeatClock, Controls, Echo, MAX_BEATS, MIN_BEATS, SILENT, SWITCH_SECS, Trans};

/// The longest pad echo, seconds. Longer ones are cut to this.
const ECHO_MAX_SECS: f64 = 4.0;
/// How loud a pad echo's repeats are (the effect units' level).
const ECHO_LEVEL: f64 = 0.7;

/// Seconds of the channel kept for roll, brake and spin.
const TAPE_SECS: f64 = 5.0;
/// Seconds for a roll's repeat to fade in and out, so the join doesn't click.
const ROLL_EDGE_SECS: f64 = 0.002;
/// How fast a back spin starts, times normal speed (backwards).
const SPIN_SPEED: f64 = 4.0;
/// A brake or spin fades out below this speed, to end in silence, not a click.
const TAPE_FADE_SPEED: f64 = 0.125;

/// A filter sweep's far end: the low-pass comes down to here, the high-pass up to
/// here (Hz).
const SWEEP_LOW_HZ: f64 = 150.0;
const SWEEP_HIGH_HZ: f64 = 6000.0;
const SWEEP_Q: f64 = 1.0;

/// The reverb's comb and all-pass lengths, in samples at 44.1 kHz, and how far the
/// right channel's are from the left's (Freeverb's tuning).
const COMBS: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASSES: [usize; 4] = [556, 441, 341, 225];
const STEREO_SPREAD: usize = 23;
/// How much of the sound goes into the reverb, and how much its highs are damped
/// each time round.
const REVERB_SEND: f64 = 0.015;
const REVERB_DAMP: f32 = 0.2;
const REVERB_ALLPASS: f32 = 0.5;
/// The shortest and longest reverb tail, seconds (to -60 dB).
const REVERB_SECS: std::ops::RangeInclusive<f64> = 0.2..=8.0;
/// Seconds of silence before a reverb tail counts as over.
const REVERB_QUIET_SECS: f64 = 0.1;

/// Which effect a pad plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PadFxKind {
    /// Repeats in time with the beat. Rings out after the pad is let go.
    Echo,
    /// Cuts the sound in and out in time with the beat.
    Trans,
    /// Catches the sound from the press and repeats it.
    Roll,
    /// A low-pass filter that closes and opens again, once every length.
    LowPass,
    /// A high-pass filter, likewise.
    HighPass,
    /// Rings out after the pad is let go.
    Reverb,
    /// Slows to a stop, like a turntable switched off.
    Brake,
    /// Spins backwards, slowing to a stop.
    Spin,
}

impl PadFxKind {
    pub const ALL: [PadFxKind; 8] = [
        PadFxKind::Echo,
        PadFxKind::Trans,
        PadFxKind::Roll,
        PadFxKind::LowPass,
        PadFxKind::HighPass,
        PadFxKind::Reverb,
        PadFxKind::Brake,
        PadFxKind::Spin,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            PadFxKind::Echo => "Echo",
            PadFxKind::Trans => "Trans",
            PadFxKind::Roll => "Roll",
            PadFxKind::LowPass => "LPF",
            PadFxKind::HighPass => "HPF",
            PadFxKind::Reverb => "Reverb",
            PadFxKind::Brake => "V.Brake",
            PadFxKind::Spin => "Back Spin",
        }
    }
}

/// A pad's effect: which, and its length in beats (the echo time, the Trans and
/// sweep cycle, the roll, how long a brake or spin takes to stop, how long the
/// reverb's tail is). Kept within [`MIN_BEATS`]..[`MAX_BEATS`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PadFx {
    pub kind: PadFxKind,
    pub beats: f64,
}

impl PadFx {
    pub const fn new(kind: PadFxKind, beats: f64) -> PadFx {
        PadFx { kind, beats }
    }
}

// ---- tape: roll, brake, spin --------------------------------------------------------

/// What the tape is playing in place of the channel.
#[derive(Clone, Copy, PartialEq)]
enum TapeFx {
    None,
    /// `len` frames from the press, caught (`t` frames in) and then repeated.
    Roll {
        len: usize,
        t: usize,
    },
    /// Reading `lag` frames behind the newest, at `speed`, which falls by `decel`
    /// each frame.
    Brake {
        lag: f64,
        speed: f64,
        decel: f64,
    },
    /// The same, backwards from the press, with the tape stopped.
    Spin {
        lag: f64,
        speed: f64,
        decel: f64,
    },
}

/// The last few seconds of the channel, always recording (a back spin needs what
/// came before the press) except while a roll or a spin is playing from it.
struct Tape {
    /// Interleaved stereo, a power of two of frames long.
    buf: Vec<f32>,
    mask: usize,
    /// The next frame to write.
    write: usize,
    fx: TapeFx,
    /// 0: the channel as it is. 1: the tape in its place.
    wet: Smoothed,
    k_switch: f64,
    edge: usize,
}

impl Tape {
    fn new(sr: f64) -> Tape {
        let frames = ((TAPE_SECS * sr) as usize).next_power_of_two();
        Tape {
            buf: vec![0.0; frames * 2],
            mask: frames - 1,
            write: 0,
            fx: TapeFx::None,
            wet: Smoothed::new(0.0),
            k_switch: dsp::ease_per_sample(SWITCH_SECS, sr),
            edge: ((ROLL_EDGE_SECS * sr) as usize).max(1),
        }
    }

    fn frames(&self) -> usize {
        self.mask + 1
    }

    /// Start `kind` (roll, brake or spin), `len` frames long.
    fn start(&mut self, kind: PadFxKind, len: f64) {
        let frames = self.frames();
        self.fx = match kind {
            PadFxKind::Roll => TapeFx::Roll {
                len: (len as usize).clamp(4 * self.edge, frames / 2),
                t: 0,
            },
            PadFxKind::Brake => TapeFx::Brake {
                lag: 0.0,
                speed: 1.0,
                // It falls behind by half its length: never more than the tape.
                decel: 1.0 / len.clamp(1.0, (frames - 4) as f64),
            },
            PadFxKind::Spin => TapeFx::Spin {
                lag: 0.0,
                speed: SPIN_SPEED,
                decel: SPIN_SPEED / len.max(1.0),
            },
            _ => return,
        };
        self.wet.target = 1.0;
    }

    /// Back to the channel as it is.
    fn release(&mut self) {
        self.wet.target = 0.0;
    }

    fn active(&self) -> bool {
        self.fx != TapeFx::None
    }

    #[inline]
    fn push(&mut self, x: [f32; 2]) {
        let w = 2 * (self.write & self.mask);
        self.buf[w] = x[0];
        self.buf[w + 1] = x[1];
        self.write = self.write.wrapping_add(1);
    }

    /// The frame `lag` frames (fractional) before the newest.
    #[inline]
    fn read(&self, lag: f64) -> [f32; 2] {
        let whole = lag.floor();
        let frac = (lag - whole) as f32;
        let i0 = self.write.wrapping_sub(1 + whole as usize) & self.mask;
        let i1 = i0.wrapping_sub(1) & self.mask;
        [0, 1].map(|ch| {
            let (a, b) = (self.buf[2 * i0 + ch], self.buf[2 * i1 + ch]);
            a + (b - a) * frac
        })
    }

    fn process(&mut self, buf: &mut [f32]) {
        let mut fx = self.fx;
        if fx == TapeFx::None {
            for frame in buf.chunks_exact(2) {
                self.push([frame[0], frame[1]]);
            }
            return;
        }
        // The furthest back there's anything to read.
        let max_lag = (self.frames() - 3) as f64;
        // A brake or spin's sound at `speed`, read `lag` behind: quieter as it stops.
        let slowed = |tape: &Tape, lag: f64, speed: f64| {
            if speed <= 0.0 || lag > max_lag {
                return [0.0; 2];
            }
            let g = (speed / TAPE_FADE_SPEED).min(1.0) as f32;
            tape.read(lag).map(|s| s * g)
        };
        for frame in buf.chunks_exact_mut(2) {
            let x = [frame[0], frame[1]];
            let y = match &mut fx {
                TapeFx::None => x,
                TapeFx::Roll { len, t } => {
                    let y = if *t < *len {
                        // Catching it: it's heard as it is, the first time.
                        self.push(x);
                        x
                    } else {
                        let i = *t % *len;
                        let at = self.write.wrapping_sub(*len - i) & self.mask;
                        let from_edge = i.min(*len - 1 - i) as f32;
                        let g = (from_edge / self.edge as f32).min(1.0);
                        [self.buf[2 * at] * g, self.buf[2 * at + 1] * g]
                    };
                    *t += 1;
                    if *t >= 2 * *len {
                        *t -= *len;
                    }
                    y
                }
                TapeFx::Brake { lag, speed, decel } => {
                    self.push(x);
                    let y = slowed(self, *lag, *speed);
                    *lag += 1.0 - *speed;
                    *speed = (*speed - *decel).max(0.0);
                    y
                }
                TapeFx::Spin { lag, speed, decel } => {
                    let y = slowed(self, *lag, *speed);
                    *lag += *speed;
                    *speed = (*speed - *decel).max(0.0);
                    y
                }
            };
            let w = self.wet.next(self.k_switch) as f32;
            frame[0] = x[0] + (y[0] - x[0]) * w;
            frame[1] = x[1] + (y[1] - x[1]) * w;
        }
        // Let go and faded back: recording again.
        if self.wet.target == 0.0 && self.wet.current == 0.0 {
            fx = TapeFx::None;
        }
        self.fx = fx;
    }
}

// ---- filter sweep -------------------------------------------------------------------

/// A two-pole filter whose cut-off swings out and back once a cycle.
struct Sweep {
    high: bool,
    beats: f64,
    /// Cycles since the press.
    phase: f64,
    on: Smoothed,
    k_switch: f64,
    /// The filter's two states, per channel.
    ic: [[f64; 2]; 2],
    sr: f64,
}

impl Sweep {
    fn new(sr: f64) -> Sweep {
        Sweep {
            high: false,
            beats: 1.0,
            phase: 0.0,
            on: Smoothed::new(0.0),
            k_switch: dsp::ease_per_sample(SWITCH_SECS, sr),
            ic: [[0.0; 2]; 2],
            sr,
        }
    }

    fn start(&mut self, high: bool, beats: f64) {
        self.high = high;
        self.beats = beats;
        self.phase = 0.0;
        self.ic = [[0.0; 2]; 2];
        self.on.target = 1.0;
    }

    fn release(&mut self) {
        self.on.target = 0.0;
    }

    fn active(&self) -> bool {
        self.on.target != 0.0 || self.on.current != 0.0
    }

    /// The cut-off (Hz) `depth` (0..1) of the way out.
    fn cutoff(high: bool, depth: f64) -> f64 {
        if high {
            20.0 * (SWEEP_HIGH_HZ / 20.0).powf(depth)
        } else {
            20_000.0 * (SWEEP_LOW_HZ / 20_000.0).powf(depth)
        }
    }

    fn process(&mut self, buf: &mut [f32], secs_per_beat: f64) {
        if !self.active() {
            return;
        }
        let step = 1.0 / (self.beats * secs_per_beat * self.sr).max(1.0);
        let k = 1.0 / SWEEP_Q;
        for frame in buf.chunks_exact_mut(2) {
            let depth = 0.5 - 0.5 * (self.phase * std::f64::consts::TAU).cos();
            self.phase = (self.phase + step).fract();
            let fc = Sweep::cutoff(self.high, depth).min(0.45 * self.sr);
            // A state-variable filter (trapezoidal): steady however fast it's swept.
            let g = (std::f64::consts::PI * fc / self.sr).tan();
            let a1 = 1.0 / (1.0 + g * (g + k));
            let (a2, a3) = (g * a1, g * g * a1);
            let on = self.on.next(self.k_switch);
            for (ch, s) in frame.iter_mut().enumerate() {
                let x = *s as f64;
                let [ic1, ic2] = &mut self.ic[ch];
                let v3 = x - *ic2;
                let v1 = a1 * *ic1 + a2 * v3;
                let v2 = *ic2 + a2 * *ic1 + a3 * v3;
                *ic1 = 2.0 * v1 - *ic1;
                *ic2 = 2.0 * v2 - *ic2;
                let y = if self.high { x - k * v1 - v2 } else { v2 };
                *s = (x + (y - x) * on) as f32;
            }
        }
    }
}

// ---- reverb -------------------------------------------------------------------------

struct Comb {
    buf: Vec<f32>,
    pos: usize,
    feedback: f32,
    /// The damping filter's state.
    low: f32,
}

impl Comb {
    #[inline]
    fn run(&mut self, x: f32) -> f32 {
        let out = self.buf[self.pos];
        self.low = out + (self.low - out) * REVERB_DAMP;
        self.buf[self.pos] = x + self.low * self.feedback;
        self.pos = (self.pos + 1) % self.buf.len();
        out
    }
}

struct Allpass {
    buf: Vec<f32>,
    pos: usize,
}

impl Allpass {
    #[inline]
    fn run(&mut self, x: f32) -> f32 {
        let delayed = self.buf[self.pos];
        self.buf[self.pos] = x + delayed * REVERB_ALLPASS;
        self.pos = (self.pos + 1) % self.buf.len();
        delayed - x
    }
}

/// Freeverb: eight combs side by side into four all-passes in a row, per channel,
/// the right channel's a little longer than the left's.
struct Reverb {
    combs: [Vec<Comb>; 2],
    allpasses: [Vec<Allpass>; 2],
    /// How much of the channel is going in.
    send: Smoothed,
    k_switch: f64,
    running: bool,
    /// Frames in a row the tail has been silent with nothing going in.
    quiet: usize,
    sr: f64,
}

impl Reverb {
    fn new(sr: f64) -> Reverb {
        let len = |n: usize, ch: usize| {
            (((n + ch * STEREO_SPREAD) as f64 * sr / 44_100.0) as usize).max(1)
        };
        Reverb {
            combs: [0, 1].map(|ch| {
                COMBS
                    .iter()
                    .map(|&n| Comb {
                        buf: vec![0.0; len(n, ch)],
                        pos: 0,
                        feedback: 0.0,
                        low: 0.0,
                    })
                    .collect()
            }),
            allpasses: [0, 1].map(|ch| {
                ALLPASSES
                    .iter()
                    .map(|&n| Allpass {
                        buf: vec![0.0; len(n, ch)],
                        pos: 0,
                    })
                    .collect()
            }),
            send: Smoothed::new(0.0),
            k_switch: dsp::ease_per_sample(SWITCH_SECS, sr),
            running: false,
            quiet: 0,
            sr,
        }
    }

    /// Take the channel in, with a tail `secs` long (to -60 dB).
    fn start(&mut self, secs: f64) {
        let secs = secs.clamp(*REVERB_SECS.start(), *REVERB_SECS.end());
        for comb in self.combs.iter_mut().flatten() {
            // Once round a comb is this much of the way down the tail.
            let round = comb.buf.len() as f64 / (secs * self.sr);
            comb.feedback = 10f64.powf(-3.0 * round) as f32;
        }
        self.send.target = 1.0;
    }

    /// Take no more in: what's there rings out.
    fn release(&mut self) {
        self.send.target = 0.0;
    }

    fn process(&mut self, buf: &mut [f32]) {
        let sending = self.send.target != 0.0 || self.send.current != 0.0;
        if !self.running && !sending {
            return;
        }
        self.running = true;
        let mut peak = 0f32;
        for frame in buf.chunks_exact_mut(2) {
            let send = self.send.next(self.k_switch);
            let x = ((frame[0] + frame[1]) as f64 * REVERB_SEND * send) as f32;
            for (ch, s) in frame.iter_mut().enumerate() {
                let mut wet = 0.0;
                for comb in &mut self.combs[ch] {
                    wet += comb.run(x);
                }
                for allpass in &mut self.allpasses[ch] {
                    wet = allpass.run(wet);
                }
                peak = peak.max(wet.abs());
                *s += wet;
            }
        }
        if !sending && peak < SILENT {
            self.quiet += buf.len() / 2;
            if self.quiet as f64 > REVERB_QUIET_SECS * self.sr {
                self.stop();
            }
        } else {
            self.quiet = 0;
        }
    }

    /// The tail is over: empty, for the next one.
    fn stop(&mut self) {
        self.running = false;
        self.quiet = 0;
        for comb in self.combs.iter_mut().flatten() {
            comb.buf.fill(0.0);
            comb.low = 0.0;
        }
        for allpass in self.allpasses.iter_mut().flatten() {
            allpass.buf.fill(0.0);
        }
    }
}

// ---- the slot -----------------------------------------------------------------------

/// A deck's pad effect slot: the effect held now, if any, and every effect's state
/// (an echo's tail rings on under the next pad's effect).
pub struct PadFxSlot {
    sr: f64,
    held: Option<PadFx>,
    /// The held effect hasn't started yet: it does at the next block, which knows
    /// the tempo.
    starting: bool,
    echo: Echo,
    echo_c: Controls,
    echo_beats: f64,
    trans: Trans,
    trans_c: Controls,
    trans_beats: f64,
    sweep: Sweep,
    reverb: Reverb,
    tape: Tape,
}

impl PadFxSlot {
    pub fn new(sr: f64) -> PadFxSlot {
        PadFxSlot {
            sr,
            held: None,
            starting: false,
            echo: Echo::with_max_secs(ECHO_MAX_SECS, sr),
            echo_c: Controls::new(ECHO_LEVEL, sr),
            echo_beats: 1.0,
            trans: Trans::new(sr),
            trans_c: Controls::new(1.0, sr),
            trans_beats: 1.0,
            sweep: Sweep::new(sr),
            reverb: Reverb::new(sr),
            tape: Tape::new(sr),
        }
    }

    /// The effect held now.
    pub fn held(&self) -> Option<PadFx> {
        self.held
    }

    /// A pad pressed (its effect), or the held one let go (`None`). A press while
    /// another effect is held takes its place.
    pub fn set(&mut self, fx: Option<PadFx>) {
        // Whatever was on goes off; echo and reverb ring out.
        self.echo_c.on.target = 0.0;
        self.trans_c.on.target = 0.0;
        self.sweep.release();
        self.reverb.release();
        self.tape.release();
        self.held = fx.filter(|fx| fx.beats.is_finite()).map(|fx| PadFx {
            beats: fx.beats.clamp(MIN_BEATS, MAX_BEATS),
            ..fx
        });
        self.starting = self.held.is_some();
    }

    /// Whether it's doing anything: an effect held, or a tail still ringing.
    pub fn busy(&self) -> bool {
        self.held.is_some()
            || self.echo.running
            || self.reverb.running
            || self.tape.active()
            || self.sweep.active()
            || self.trans.gain.current != 1.0
    }

    fn start(&mut self, fx: PadFx, secs_per_beat: f64) {
        let secs = fx.beats * secs_per_beat;
        match fx.kind {
            PadFxKind::Echo => {
                self.echo_beats = fx.beats;
                self.echo.start_at(secs * self.sr);
                self.echo_c.on.target = 1.0;
            }
            PadFxKind::Trans => {
                self.trans_beats = fx.beats;
                self.trans_c.on.target = 1.0;
            }
            PadFxKind::LowPass => self.sweep.start(false, fx.beats),
            PadFxKind::HighPass => self.sweep.start(true, fx.beats),
            PadFxKind::Reverb => self.reverb.start(secs),
            PadFxKind::Roll | PadFxKind::Brake | PadFxKind::Spin => {
                self.tape.start(fx.kind, secs * self.sr)
            }
        }
    }

    /// Run on one block of the deck's channel (interleaved stereo), in place. Call
    /// it every block, busy or not: it keeps the last few seconds for a back spin.
    pub fn process(&mut self, buf: &mut [f32], clock: &BeatClock) {
        if let (true, Some(fx)) = (self.starting, self.held) {
            self.starting = false;
            self.start(fx, clock.secs_per_beat);
        }
        self.tape.process(buf);
        self.sweep.process(buf, clock.secs_per_beat);
        self.trans
            .process(buf, clock, self.trans_beats, &mut self.trans_c);
        let delay = self.echo_beats * clock.secs_per_beat * self.sr;
        self.echo.process(buf, delay, &mut self.echo_c);
        self.reverb.process(buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;
    /// A beat at 120 BPM, in frames.
    const BEAT: usize = 24_000;

    /// Run `frames` of `input(frame)` (counted from `from`) through `slot` at a
    /// steady 120 BPM, in 256-frame blocks; the left channel out.
    fn run_from(
        slot: &mut PadFxSlot,
        from: usize,
        frames: usize,
        input: impl Fn(usize) -> f32,
    ) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames);
        let bpf = 120.0 / 60.0 / SR;
        for start in (0..frames).step_by(256) {
            let n = 256.min(frames - start);
            let mut buf: Vec<f32> = (0..n).flat_map(|f| [input(from + start + f); 2]).collect();
            let beat_end = (from + start + n) as f64 * bpf;
            slot.process(&mut buf, &BeatClock::steady(120.0, beat_end, SR));
            out.extend(buf.chunks(2).map(|c| c[0]));
        }
        out
    }

    fn run(slot: &mut PadFxSlot, frames: usize, input: impl Fn(usize) -> f32) -> Vec<f32> {
        run_from(slot, 0, frames, input)
    }

    fn peak(v: &[f32]) -> f32 {
        v.iter().fold(0f32, |m, s| m.max(s.abs()))
    }

    fn tone(f: usize) -> f32 {
        (f as f32 * 0.05).sin() * 0.5
    }

    /// Frequency (Hz) of a steady tone from its upward zero crossings.
    fn frequency(x: &[f32]) -> f64 {
        let ups: Vec<usize> = (1..x.len())
            .filter(|&i| x[i - 1] < 0.0 && x[i] >= 0.0)
            .collect();
        (ups.len() - 1) as f64 * SR / (ups[ups.len() - 1] - ups[0]) as f64
    }

    fn press(kind: PadFxKind, beats: f64) -> PadFxSlot {
        let mut slot = PadFxSlot::new(SR);
        slot.set(Some(PadFx::new(kind, beats)));
        slot
    }

    #[test]
    fn nothing_held_is_untouched() {
        let mut slot = PadFxSlot::new(SR);
        let out = run(&mut slot, 10_000, tone);
        for (f, s) in out.iter().enumerate() {
            assert_eq!(*s, tone(f), "frame {f}");
        }
        assert!(!slot.busy());
    }

    /// Every effect but echo and reverb is gone a moment after its pad is let go:
    /// the channel is as it was, and the slot is idle.
    #[test]
    fn let_go_is_back_to_the_channel() {
        for kind in PadFxKind::ALL {
            if matches!(kind, PadFxKind::Echo | PadFxKind::Reverb) {
                continue;
            }
            let mut slot = press(kind, 0.5);
            run(&mut slot, BEAT, tone);
            assert!(slot.busy(), "{kind:?} held");
            slot.set(None);
            assert_eq!(slot.held(), None);
            run_from(&mut slot, BEAT, 4800, tone);
            let out = run_from(&mut slot, BEAT + 4800, 4800, tone);
            for (f, s) in out.iter().enumerate() {
                assert_eq!(*s, tone(BEAT + 4800 + f), "{kind:?} frame {f}");
            }
            assert!(!slot.busy(), "{kind:?} idle");
        }
    }

    /// Echo and reverb ring out after the pad is let go, then stop running.
    #[test]
    fn echo_and_reverb_ring_out_after_the_pad_is_let_go() {
        for kind in [PadFxKind::Echo, PadFxKind::Reverb] {
            let mut slot = press(kind, 1.0);
            run(&mut slot, BEAT, tone);
            slot.set(None);
            // Silence in from the release: all that's heard is the tail.
            let tail = run_from(&mut slot, BEAT, BEAT / 2, |_| 0.0);
            assert!(
                peak(&tail[2400..]) > 0.02,
                "{kind:?} rang on: {}",
                peak(&tail[2400..])
            );
            assert!(slot.busy(), "{kind:?} still ringing");
            run(&mut slot, (SR * 20.0) as usize, |_| 0.0);
            assert!(!slot.busy(), "{kind:?}'s tail ended");
        }
    }

    #[test]
    fn echo_repeats_on_the_beat() {
        // A click at the start, half-beat echo: the first repeat a quarter second on.
        let mut slot = press(PadFxKind::Echo, 0.5);
        slot.echo_c.on.current = 1.0;
        let out = run(&mut slot, BEAT, |f| (f == 0) as u8 as f32);
        assert!(peak(&out[11_900..12_100]) > 0.3, "a repeat");
        assert!(peak(&out[1000..11_800]) < 1e-3, "nothing before it");
    }

    #[test]
    fn trans_cuts_in_time() {
        // Half-beat cycles: open for the first 125 ms of each, cut for the rest.
        let mut slot = press(PadFxKind::Trans, 0.5);
        let out = run(&mut slot, BEAT, |_| 1.0);
        let ms = |t: f64| out[(t / 1000.0 * SR) as usize];
        for cycle in 0..2 {
            let t0 = cycle as f64 * 250.0;
            assert!(ms(t0 + 60.0) > 0.99, "open in cycle {cycle}");
            assert!(ms(t0 + 190.0) < 0.01, "cut in cycle {cycle}");
        }
    }

    /// A roll repeats the first half beat after the press, whatever comes after.
    #[test]
    fn roll_repeats_what_it_caught() {
        let len = BEAT / 2;
        // A ramp through the first half beat, then silence.
        let ramp = move |f: usize| if f < len { f as f32 / len as f32 } else { 0.0 };
        let mut slot = press(PadFxKind::Roll, 0.5);
        let out = run(&mut slot, 3 * len, ramp);
        for at in [len / 4, len / 2, 3 * len / 4] {
            assert_eq!(out[at], ramp(at), "the first time through, as it is");
            assert!(
                (out[len + at] - ramp(at)).abs() < 1e-3,
                "the second, at {at}"
            );
            assert!(
                (out[2 * len + at] - ramp(at)).abs() < 1e-3,
                "the third, at {at}"
            );
        }
    }

    /// A low-pass sweep takes the highs out half-way through its cycle and has them
    /// back at its end; a high-pass sweep does the same to the lows.
    #[test]
    fn filter_sweeps_close_and_open_again() {
        let sine = |hz: f64| {
            move |f: usize| (f as f64 * hz * std::f64::consts::TAU / SR).sin() as f32 * 0.5
        };
        let level = |out: &[f32], at: usize| peak(&out[at - 1000..at + 1000]);
        for (kind, cut_hz, kept_hz) in [
            (PadFxKind::LowPass, 8000.0, 60.0),
            (PadFxKind::HighPass, 60.0, 8000.0),
        ] {
            // A one-beat cycle: furthest out half a beat in.
            let mut slot = press(kind, 1.0);
            let out = run(&mut slot, BEAT, sine(cut_hz));
            assert!(
                level(&out, BEAT / 2) < 0.02,
                "{kind:?} cuts {cut_hz} Hz: {}",
                level(&out, BEAT / 2)
            );
            assert!(level(&out, BEAT - 1000) > 0.4, "{kind:?} lets it back");
            let mut slot = press(kind, 1.0);
            let out = run(&mut slot, BEAT, sine(kept_hz));
            assert!(level(&out, BEAT / 2) > 0.4, "{kind:?} keeps {kept_hz} Hz");
        }
    }

    #[test]
    fn reverb_tail_lasts_about_its_length() {
        // Short and long tails after the same burst: the long one is still there a
        // second on, the short one isn't.
        let after = |beats: f64| {
            let mut slot = press(PadFxKind::Reverb, beats);
            run(&mut slot, BEAT, tone);
            slot.set(None);
            let tail = run_from(&mut slot, BEAT, 2 * BEAT + 4800, |_| 0.0);
            peak(&tail[2 * BEAT..])
        };
        // 1/2 beat is a quarter second; 8 beats is four.
        let (short, long) = (after(0.5), after(8.0));
        assert!(short < 1e-4, "short tail gone: {short}");
        assert!(long > 0.01, "long tail still there: {long}");
    }

    /// A brake drops the pitch on its way to silence, over its length.
    #[test]
    fn brake_slows_to_a_stop() {
        let hz = 1000.0;
        let sine = move |f: usize| (f as f64 * hz * std::f64::consts::TAU / SR).sin() as f32 * 0.5;
        let mut slot = press(PadFxKind::Brake, 1.0);
        let out = run(&mut slot, 2 * BEAT, sine);
        // At the start it's the tone; half-way it's at about half speed.
        let early = frequency(&out[..2400]);
        assert!((early - hz * 0.95).abs() < hz * 0.05, "{early}");
        let mid = frequency(&out[BEAT / 2 - 1200..BEAT / 2 + 1200]);
        assert!((mid - hz * 0.5).abs() < hz * 0.06, "{mid}");
        // Past its length: stopped, silent, for as long as it's held.
        assert!(peak(&out[BEAT + 100..]) < 1e-4);
    }

    /// A back spin plays what came before the press, backwards and fast.
    #[test]
    fn back_spin_plays_backwards() {
        // A rising ramp before the press: spun back, it falls, four times as fast.
        let ramp = |f: usize| f as f32 / (4 * BEAT) as f32;
        let mut slot = PadFxSlot::new(SR);
        run(&mut slot, 2 * BEAT, ramp);
        slot.set(Some(PadFx::new(PadFxKind::Spin, 2.0)));
        let out = run_from(&mut slot, 2 * BEAT, 3 * BEAT, ramp);
        // From 50 ms in to 60 ms, it goes back a little under 4 × 10 ms.
        let (a, b) = (out[2400], out[2880]);
        assert!(b < a, "falling: {a} then {b}");
        let back = (a - b) * (4 * BEAT) as f32 / 480.0;
        assert!((3.5..4.0).contains(&back), "at {back} times speed");
        // Past its length: stopped, silent.
        assert!(peak(&out[2 * BEAT + 100..]) < 1e-4);
    }

    /// A second pad takes over from the first; lengths out of range are brought in.
    #[test]
    fn a_press_replaces_the_held_effect() {
        let mut slot = press(PadFxKind::Trans, 0.5);
        run(&mut slot, BEAT, tone);
        slot.set(Some(PadFx::new(PadFxKind::Brake, 1000.0)));
        assert_eq!(slot.held(), Some(PadFx::new(PadFxKind::Brake, MAX_BEATS)));
        run_from(&mut slot, BEAT, BEAT, tone);
        assert_eq!(slot.trans_c.on.current, 0.0, "trans is off");
        assert!(slot.tape.active());
        slot.set(Some(PadFx::new(PadFxKind::Echo, f64::NAN)));
        assert_eq!(slot.held(), None, "no such length: nothing held");
    }
}
