//! Beat effects: two effect units, each running one effect (Echo or Trans) in time
//! with the deck it's on (or with the master tempo on the main mix).
//!
//! A unit sits after its channel's fader and crossfader, so an echo's repeats go on
//! after the fader comes down (an "echo out"); on the main mix it runs after the
//! channels are summed, before the main level and limiter.
//!
//! Each block, the renderer hands a unit a [`BeatClock`]: where the target is in its
//! beat grid and how fast it's going. Echo times its repeats from it, Trans its cuts.
//!
//! Everything is allocated in [`FxUnit::new`]; nothing allocates on the audio thread.

use crate::command::{DeckId, Set};
use crate::dsp::{self, Smoothed};

/// How many effect units there are.
pub const FX_UNITS: usize = 2;

/// The shortest and longest beat length a unit takes. The ◀ / ▶ buttons halve and
/// double the length within this range, as on Pioneer's gear: 1/16, 1/8 … 8, 16.
pub const MIN_BEATS: f64 = 1.0 / 16.0;
pub const MAX_BEATS: f64 = 16.0;

/// The longest echo, seconds: 16 beats down to 80 BPM. Longer ones are cut to this.
pub const MAX_ECHO_SECS: f64 = 12.0;

/// Each echo repeat is this much quieter than the one before (-6 dB).
const ECHO_FEEDBACK: f64 = 0.5;
/// The repeats lose their lows and highs as they go, like a tape echo.
const ECHO_LOW_CUT_HZ: f64 = 120.0;
const ECHO_HIGH_CUT_HZ: f64 = 7000.0;
/// Seconds for the echo time to follow a new length or tempo. It glides, so a change
/// bends the repeats' pitch a little rather than clicking.
const ECHO_GLIDE_SECS: f64 = 0.08;
/// Below this the echo's tail counts as gone.
const SILENT: f32 = 1e-5;

/// The part of each Trans cycle that's open.
const TRANS_DUTY: f64 = 0.5;
/// Seconds for a Trans cut to open or close, so the edges don't click.
const TRANS_EDGE_SECS: f64 = 0.0015;

/// Seconds for an effect to come in or go out when switched on or off.
const SWITCH_SECS: f64 = 0.005;

/// Tempo used when there's nothing to follow (no grid, nothing playing).
pub const FALLBACK_BPM: f64 = 120.0;

/// Which effect a unit runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum FxKind {
    /// Repeats in time with the beat, each quieter and duller than the last.
    #[default]
    Echo,
    /// Cuts the sound in and out in time with the beat.
    Trans,
}

impl FxKind {
    pub const ALL: [FxKind; 2] = [FxKind::Echo, FxKind::Trans];

    pub const fn name(self) -> &'static str {
        match self {
            FxKind::Echo => "Echo",
            FxKind::Trans => "Trans",
        }
    }
}

/// What a unit is applied to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FxTarget {
    /// One deck's channel, after its fader and crossfader.
    Deck(DeckId),
    /// The main mix.
    Master,
}

/// A unit's settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FxUnitState {
    pub kind: FxKind,
    pub enabled: bool,
    /// The effect's length in beats: the echo time, the Trans cycle.
    pub beats: f64,
    /// 0..1: how loud the repeats are (Echo), how deep the cuts go (Trans).
    pub level: f64,
    pub target: FxTarget,
}

impl FxUnitState {
    /// Unit 1 on deck A, unit 2 on deck B, both off.
    pub fn default_for(unit: usize) -> FxUnitState {
        FxUnitState {
            kind: FxKind::Echo,
            enabled: false,
            beats: 1.0,
            level: 0.5,
            target: DeckId::new(unit).map_or(FxTarget::Master, FxTarget::Deck),
        }
    }
}

impl Default for FxUnitState {
    fn default() -> FxUnitState {
        FxUnitState::default_for(0)
    }
}

/// Things an effect unit does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FxCmd {
    Kind(FxKind),
    Enabled(Set),
    /// Clamped to [`MIN_BEATS`]..[`MAX_BEATS`].
    Beats(f64),
    /// Clamped to 0..1.
    Level(f64),
    Target(FxTarget),
    /// Halve (-1) or double (1) the length, within [`MIN_BEATS`]..[`MAX_BEATS`].
    BeatsStep(i32),
    /// The next effect in [`FxKind::ALL`].
    NextKind,
    /// The next target: deck 1, deck 2, … the main mix, then deck 1 again. The engine
    /// resolves it against the decks it has.
    NextTarget,
}

/// `beats` halved (`dir` -1) or doubled (1), `dir` times over, kept within
/// [`MIN_BEATS`]..[`MAX_BEATS`].
pub fn step_beats(beats: f64, dir: i32) -> f64 {
    (beats * 2f64.powi(dir)).clamp(MIN_BEATS, MAX_BEATS)
}

impl FxTarget {
    /// The one after this, with `decks` live decks.
    pub fn next(self, decks: usize) -> FxTarget {
        match self {
            FxTarget::Deck(id) if id.index() + 1 < decks => {
                DeckId::new(id.index() + 1).map_or(FxTarget::Master, FxTarget::Deck)
            }
            FxTarget::Deck(_) => FxTarget::Master,
            FxTarget::Master => DeckId::new(0).map_or(FxTarget::Master, FxTarget::Deck),
        }
    }
}

/// Where a unit's target is in its beat grid, for one block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BeatClock {
    /// The beat (fractional, from the grid) at the end of the block.
    pub beat_end: f64,
    /// Beats per output frame (0 when stopped).
    pub beats_per_frame: f64,
    /// Seconds of output per beat, at the current tempo and speed. Held at the last
    /// moving tempo while the target is stopped, so an echo's tail keeps its timing.
    pub secs_per_beat: f64,
}

impl BeatClock {
    /// A steady `bpm`, at `beat_end`.
    pub fn steady(bpm: f64, beat_end: f64, sr: f64) -> BeatClock {
        BeatClock {
            beat_end,
            beats_per_frame: bpm / 60.0 / sr,
            secs_per_beat: 60.0 / bpm,
        }
    }

    /// The beat at frame `f` of an `n`-frame block.
    #[inline]
    fn beat(&self, f: usize, n: usize) -> f64 {
        self.beat_end - (n - f) as f64 * self.beats_per_frame
    }
}

// ---- Echo -------------------------------------------------------------------------

/// An effect's on/off and level, each eased.
struct Controls {
    on: Smoothed,
    level: Smoothed,
    k_switch: f64,
    k_level: f64,
}

impl Controls {
    fn new(level: f64, sr: f64) -> Controls {
        Controls {
            on: Smoothed::new(0.0),
            level: Smoothed::new(level),
            k_switch: dsp::ease_per_sample(SWITCH_SECS, sr),
            k_level: dsp::ease_per_sample(crate::processor::KNOB_SECS, sr),
        }
    }

    /// Off, and done fading out.
    fn off(&self) -> bool {
        self.on.target == 0.0 && self.on.current == 0.0
    }

    /// The next frame's (on, level).
    #[inline]
    fn next(&mut self) -> (f64, f64) {
        (self.on.next(self.k_switch), self.level.next(self.k_level))
    }
}

/// A one-pole low-pass, per channel.
#[derive(Clone, Copy, Default)]
struct OnePole {
    a: f64,
    z: [f64; 2],
}

impl OnePole {
    fn new(hz: f64, sr: f64) -> OnePole {
        OnePole {
            a: 1.0 - (-2.0 * std::f64::consts::PI * hz / sr).exp(),
            z: [0.0; 2],
        }
    }

    #[inline]
    fn run(&mut self, ch: usize, x: f64) -> f64 {
        self.z[ch] += self.a * (x - self.z[ch]);
        self.z[ch]
    }
}

/// A stereo delay line with feedback, whose time follows the beat.
struct Echo {
    /// Interleaved stereo, a power of two of frames long.
    line: Vec<f32>,
    mask: usize,
    write: usize,
    /// The delay in frames, gliding to its target.
    delay: Smoothed,
    glide: f64,
    /// Feedback filters: a low-pass, and a low-pass whose output is taken away (a
    /// high-pass).
    high_cut: OnePole,
    low_cut: OnePole,
    /// Frames in a row the output has been silent: once that's a whole delay with
    /// nothing going in, the tail is over.
    quiet: usize,
    running: bool,
}

impl Echo {
    fn new(sr: f64) -> Echo {
        let frames = ((MAX_ECHO_SECS * sr) as usize + 2).next_power_of_two();
        Echo {
            line: vec![0.0; frames * 2],
            mask: frames - 1,
            write: 0,
            delay: Smoothed::new(sr * 0.5),
            glide: dsp::ease_per_sample(ECHO_GLIDE_SECS, sr),
            high_cut: OnePole::new(ECHO_HIGH_CUT_HZ, sr),
            low_cut: OnePole::new(ECHO_LOW_CUT_HZ, sr),
            quiet: 0,
            running: false,
        }
    }

    fn max_delay(&self) -> f64 {
        (self.mask - 1) as f64
    }

    /// The delay line's sample `back` frames (fractional) before the write head.
    #[inline]
    fn read(&self, back: f64, ch: usize) -> f64 {
        let whole = back.floor();
        let frac = back - whole;
        let i0 = self.write.wrapping_sub(whole as usize) & self.mask;
        let i1 = i0.wrapping_sub(1) & self.mask;
        let a = self.line[2 * i0 + ch] as f64;
        let b = self.line[2 * i1 + ch] as f64;
        a + (b - a) * frac
    }

    /// `c`: how much of the input goes in (on), and how loud the repeats are (level).
    /// Skipped once the tail has died away and nothing is going in.
    fn process(&mut self, buf: &mut [f32], delay_frames: f64, c: &mut Controls) {
        let n = buf.len() / 2;
        if !self.running && c.off() {
            // Off and silent: just keep the level where it's going.
            c.level.current = c.level.target;
            return;
        }
        self.running = true;
        self.delay.target = delay_frames.clamp(1.0, self.max_delay());
        let mut peak = 0f32;
        for f in 0..n {
            let (s, g) = c.next();
            let d = self.delay.next(self.glide);
            for ch in 0..2 {
                let x = buf[2 * f + ch] as f64;
                let wet = self.read(d, ch);
                // The repeat goes back in, duller each time round.
                let dull = self.high_cut.run(ch, wet);
                let fb = dull - self.low_cut.run(ch, dull);
                let w = 2 * (self.write & self.mask) + ch;
                self.line[w] = (x * s + fb * ECHO_FEEDBACK) as f32;
                let y = (wet * g) as f32;
                peak = peak.max(y.abs());
                buf[2 * f + ch] = (x + wet * g) as f32;
            }
            self.write = self.write.wrapping_add(1);
        }
        // Off, and a whole delay's worth of silence out: the tail is over.
        if c.off() && peak < SILENT {
            self.quiet += n;
            if self.quiet as f64 > self.delay.current + n as f64 {
                self.stop();
            }
        } else {
            self.quiet = 0;
        }
    }

    /// Stop running. What's left in the line is silent, so it needn't be cleared.
    fn stop(&mut self) {
        self.running = false;
        self.quiet = 0;
        self.high_cut.z = [0.0; 2];
        self.low_cut.z = [0.0; 2];
    }
}

// ---- Trans ------------------------------------------------------------------------

/// A gate on the beat: open for the first half of each cycle, cut for the second.
struct Trans {
    /// The gain, easing between open and cut.
    gain: Smoothed,
    edge: f64,
}

impl Trans {
    fn new(sr: f64) -> Trans {
        Trans {
            gain: Smoothed::new(1.0),
            edge: dsp::ease_per_sample(TRANS_EDGE_SECS, sr),
        }
    }

    fn process(&mut self, buf: &mut [f32], clock: &BeatClock, beats: f64, c: &mut Controls) {
        let n = buf.len() / 2;
        if c.off() && self.gain.current == 1.0 {
            c.level.current = c.level.target;
            return;
        }
        for f in 0..n {
            let phase = (clock.beat(f, n) / beats).rem_euclid(1.0);
            let (on, level) = c.next();
            let depth = on * level;
            self.gain.target = if phase < TRANS_DUTY { 1.0 } else { 1.0 - depth };
            let g = self.gain.next(self.edge) as f32;
            buf[2 * f] *= g;
            buf[2 * f + 1] *= g;
        }
    }
}

// ---- the unit ---------------------------------------------------------------------

/// One effect unit: its settings, and the state of both its effects (an echo's tail
/// rings on after the unit switches to Trans).
pub struct FxUnit {
    state: FxUnitState,
    sr: f64,
    /// Each effect's on/off, eased: Echo's is how much goes into the line.
    /// Each effect's own (an echo's tail can run beside Trans). Echo's on is how much
    /// goes into the line.
    echo_c: Controls,
    trans_c: Controls,
    echo: Echo,
    trans: Trans,
}

impl FxUnit {
    pub fn new(sr: f64, state: FxUnitState) -> FxUnit {
        let mut u = FxUnit {
            state,
            sr,
            echo_c: Controls::new(state.level, sr),
            trans_c: Controls::new(state.level, sr),
            echo: Echo::new(sr),
            trans: Trans::new(sr),
        };
        u.set(state);
        u
    }

    pub fn state(&self) -> FxUnitState {
        self.state
    }

    /// Take on all of `s` (a restore).
    pub fn set(&mut self, s: FxUnitState) {
        self.apply(FxCmd::Kind(s.kind));
        self.apply(FxCmd::Beats(s.beats));
        self.apply(FxCmd::Level(s.level));
        self.apply(FxCmd::Target(s.target));
        self.apply(FxCmd::Enabled(Set::from_bool(s.enabled)));
    }

    pub fn apply(&mut self, cmd: FxCmd) {
        let s = &mut self.state;
        match cmd {
            FxCmd::Kind(k) => s.kind = k,
            FxCmd::Enabled(set) => s.enabled = set.apply(s.enabled),
            FxCmd::Beats(b) if b.is_finite() => s.beats = b.clamp(MIN_BEATS, MAX_BEATS),
            FxCmd::Level(l) if l.is_finite() => s.level = l.clamp(0.0, 1.0),
            FxCmd::Beats(_) | FxCmd::Level(_) => {}
            FxCmd::BeatsStep(dir) => s.beats = step_beats(s.beats, dir),
            FxCmd::NextKind => {
                let i = FxKind::ALL.iter().position(|k| *k == s.kind).unwrap_or(0);
                s.kind = FxKind::ALL[(i + 1) % FxKind::ALL.len()];
            }
            // Resolved by the renderer, which knows the decks.
            FxCmd::NextTarget => {}
            FxCmd::Target(t) => {
                // A tail doesn't follow the unit to another channel.
                if t != s.target {
                    self.echo.stop();
                    self.echo_c.on.current = 0.0;
                    self.trans.gain.current = 1.0;
                }
                s.target = t;
            }
        }
        let s = self.state;
        self.echo_c.level.target = s.level;
        self.trans_c.level.target = s.level;
        self.echo_c.on.target = (s.enabled && s.kind == FxKind::Echo) as u8 as f64;
        self.trans_c.on.target = (s.enabled && s.kind == FxKind::Trans) as u8 as f64;
    }

    /// Whether it's doing anything (on, or an echo still ringing).
    pub fn busy(&self) -> bool {
        self.state.enabled || self.echo.running || self.trans.gain.current != 1.0
    }

    /// Run on one block of interleaved stereo, in place.
    pub fn process(&mut self, buf: &mut [f32], clock: &BeatClock) {
        let beats = self.state.beats;
        let delay = beats * clock.secs_per_beat * self.sr;
        self.trans.process(buf, clock, beats, &mut self.trans_c);
        self.echo.process(buf, delay, &mut self.echo_c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    fn unit(kind: FxKind, beats: f64, level: f64) -> FxUnit {
        let mut u = FxUnit::new(
            SR,
            FxUnitState {
                kind,
                beats,
                level,
                ..FxUnitState::default()
            },
        );
        u.apply(FxCmd::Enabled(Set::On));
        // Skip the switch-on fade so the numbers below are exact.
        u.echo_c.on.current = u.echo_c.on.target;
        u.trans_c.on.current = u.trans_c.on.target;
        u
    }

    /// Run `frames` of `input(frame)` through `u` at a steady 120 BPM from beat 0, in
    /// 256-frame blocks; the left channel out.
    fn run(u: &mut FxUnit, frames: usize, input: impl Fn(usize) -> f32) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames);
        let mut beat = 0.0;
        let bpf = 120.0 / 60.0 / SR;
        for start in (0..frames).step_by(256) {
            let n = 256.min(frames - start);
            let mut buf: Vec<f32> = (0..n).flat_map(|f| [input(start + f); 2]).collect();
            beat += n as f64 * bpf;
            u.process(&mut buf, &BeatClock::steady(120.0, beat, SR));
            out.extend(buf.chunks(2).map(|c| c[0]));
        }
        out
    }

    #[test]
    fn echo_repeats_on_the_beat_and_fades() {
        // One click at the start; 1 beat at 120 BPM is half a second.
        let mut u = unit(FxKind::Echo, 1.0, 1.0);
        let out = run(&mut u, (SR * 2.2) as usize, |f| (f == 0) as u8 as f32);
        let beat = (SR * 0.5) as usize;
        let around = |at: usize| {
            out[at - 200..at + 200]
                .iter()
                .fold(0f32, |m, s| m.max(s.abs()))
        };
        // The dry click, then repeats a beat apart, each quieter than the last.
        assert_eq!(out[0], 1.0);
        let (r1, r2, r3) = (around(beat), around(2 * beat), around(3 * beat));
        assert!(r1 > 0.5, "first repeat {r1}");
        assert!(r2 < r1 && r3 < r2, "{r1} {r2} {r3}");
        // Nothing between them.
        assert!(around(beat / 2) < 1e-3);
    }

    #[test]
    fn echo_tail_rings_on_after_switching_off() {
        let mut u = unit(FxKind::Echo, 1.0, 1.0);
        let tone = |f: usize| (f as f32 * 0.05).sin() * 0.5;
        // A beat of tone with the echo on, then off with the input silent.
        run(&mut u, (SR * 0.5) as usize, tone);
        u.apply(FxCmd::Enabled(Set::Off));
        let tail = run(&mut u, (SR * 0.5) as usize, |_| 0.0);
        let peak = tail.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.1, "the tail went on: {peak}");
        assert!(u.busy());
        // Given long enough it dies away and stops running.
        run(&mut u, (SR * 12.0) as usize, |_| 0.0);
        assert!(!u.busy());
    }

    #[test]
    fn echo_at_zero_level_is_dry() {
        let mut u = unit(FxKind::Echo, 0.5, 0.0);
        u.echo_c.level.current = 0.0;
        let tone = |f: usize| (f as f32 * 0.01).sin();
        let out = run(&mut u, (SR * 1.0) as usize, tone);
        for (f, s) in out.iter().enumerate() {
            assert!((s - tone(f)).abs() < 1e-6, "frame {f}");
        }
    }

    #[test]
    fn trans_cuts_the_second_half_of_each_cycle() {
        // Half-beat cycles at 120 BPM: open 0..125 ms, cut 125..250 ms, and so on.
        let mut u = unit(FxKind::Trans, 0.5, 1.0);
        let out = run(&mut u, (SR * 1.0) as usize, |_| 1.0);
        let ms = |t: f64| out[(t / 1000.0 * SR) as usize];
        for cycle in 0..4 {
            let t0 = cycle as f64 * 250.0;
            assert!(ms(t0 + 60.0) > 0.99, "open in cycle {cycle}");
            assert!(ms(t0 + 190.0) < 0.01, "cut in cycle {cycle}");
        }
    }

    #[test]
    fn halving_and_doubling_retime_the_effects() {
        // Echo at 1 beat (0.5 s at 120 BPM), halved: once the time has glided over,
        // a click's first repeat comes a quarter second later.
        let mut u = unit(FxKind::Echo, 1.0, 1.0);
        u.apply(FxCmd::BeatsStep(-1));
        assert_eq!(u.state().beats, 0.5);
        run(&mut u, (SR * 1.0) as usize, |_| 0.0);
        let out = run(&mut u, (SR * 0.6) as usize, |f| (f == 0) as u8 as f32);
        let peak_at = |from: f64, to: f64| {
            out[(from * SR) as usize..(to * SR) as usize]
                .iter()
                .fold(0f32, |m, s| m.max(s.abs()))
        };
        assert!(peak_at(0.24, 0.26) > 0.5, "a repeat at 1/2 beat");
        assert!(peak_at(0.05, 0.23) < 1e-3, "nothing before it");

        // Trans at 1 beat, doubled to 2: open for a whole beat, cut for the next.
        let mut u = unit(FxKind::Trans, 1.0, 1.0);
        u.apply(FxCmd::BeatsStep(1));
        let out = run(&mut u, (SR * 1.0) as usize, |_| 1.0);
        let at = |t: f64| out[(t * SR) as usize];
        assert!(
            at(0.3) > 0.99 && at(0.45) > 0.99,
            "open through the first beat"
        );
        assert!(at(0.6) < 0.01 && at(0.9) < 0.01, "cut through the second");
    }

    #[test]
    fn trans_level_sets_the_depth() {
        let mut u = unit(FxKind::Trans, 1.0, 0.5);
        let out = run(&mut u, (SR * 0.5) as usize, |_| 1.0);
        // Cut from 0.25 s to 0.5 s, to half.
        assert!((out[(SR * 0.4) as usize] - 0.5).abs() < 0.01);
    }

    #[test]
    fn switched_off_is_untouched() {
        for kind in FxKind::ALL {
            let mut u = FxUnit::new(
                SR,
                FxUnitState {
                    kind,
                    ..FxUnitState::default()
                },
            );
            let tone = |f: usize| (f as f32 * 0.01).sin();
            let out = run(&mut u, 10_000, tone);
            for (f, s) in out.iter().enumerate() {
                assert_eq!(*s, tone(f), "{kind:?} frame {f}");
            }
            assert!(!u.busy());
        }
    }

    #[test]
    fn settings_are_clamped() {
        let mut u = FxUnit::new(SR, FxUnitState::default());
        u.apply(FxCmd::Beats(100.0));
        u.apply(FxCmd::Level(-1.0));
        assert_eq!(u.state().beats, MAX_BEATS);
        assert_eq!(u.state().level, 0.0);
        u.apply(FxCmd::Beats(f64::NAN));
        assert_eq!(u.state().beats, MAX_BEATS);
        u.apply(FxCmd::Enabled(Set::Toggle));
        assert!(u.state().enabled);
    }

    #[test]
    fn steps_and_cycles() {
        assert_eq!(step_beats(1.0, 1), 2.0);
        assert_eq!(step_beats(1.0, -1), 0.5);
        assert_eq!(step_beats(0.5, -1), 0.25);
        assert_eq!(step_beats(16.0, 1), 16.0);
        assert_eq!(step_beats(1.0 / 16.0, -1), 1.0 / 16.0);
        // Halving down from 1 beat and doubling back lands on 1 exactly.
        let mut b = 1.0;
        for _ in 0..4 {
            b = step_beats(b, -1);
        }
        assert_eq!(b, 1.0 / 16.0);
        for _ in 0..4 {
            b = step_beats(b, 1);
        }
        assert_eq!(b, 1.0);
        let mut u = FxUnit::new(SR, FxUnitState::default());
        u.apply(FxCmd::NextKind);
        assert_eq!(u.state().kind, FxKind::Trans);
        u.apply(FxCmd::NextKind);
        assert_eq!(u.state().kind, FxKind::Echo);
        let a = FxTarget::Deck(DeckId::new(0).unwrap());
        let b = FxTarget::Deck(DeckId::new(1).unwrap());
        assert_eq!(a.next(2), b);
        assert_eq!(b.next(2), FxTarget::Master);
        assert_eq!(FxTarget::Master.next(2), a);
    }
}
