//! The real-time part: decks, mixer and the render loop that runs on the audio thread.
//!
//! Rules for everything reachable from [`Renderer::process`]: no allocation, no locks,
//! no frees. Commands arrive on an `rtrb` ring buffer; replaced track data leaves on
//! another ring buffer to be dropped elsewhere; state is published through atomics.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::time::Instant;

use crate::beatgrid::Beatgrid;
use crate::decode::TrackAudio;
use crate::dsp::{self, DjFilter, Eq3, Limiter, Smoothed};
use crate::keylock::{self, Keylock};
use crate::tempo::TempoRange;

pub const DECKS: usize = 2;
/// The decks plus Prepare mode's player, which plays straight to the main output
/// (no channel fader, crossfader or sync).
pub const PLAYERS: usize = 3;
/// Prepare mode's player.
pub const PREP: usize = 2;
pub const HOTCUES: usize = 8;
/// Largest block rendered in one go; bigger device buffers are rendered in chunks.
pub const MAX_BLOCK: usize = 4096;
/// EQ band indices for [`Command::Eq`] and [`DeckShared::eq`].
pub const EQ_LOW: usize = 0;
pub const EQ_MID: usize = 1;
pub const EQ_HIGH: usize = 2;
/// Seconds for channel strip knobs (gain, EQ) to settle, so moves don't click.
const KNOB_SECS: f64 = 0.01;
/// Seconds for a level meter to fall back by a factor of e.
const VU_FALL_SECS: f64 = 0.25;

// ---- shared state (written by the audio thread, read anywhere) --------------------

#[derive(Default)]
pub struct AtomicF64(AtomicU64);

impl AtomicF64 {
    pub fn new(v: f64) -> Self {
        Self(AtomicU64::new(v.to_bits()))
    }
    pub fn get(&self) -> f64 {
        f64::from_bits(self.0.load(Relaxed))
    }
    pub fn set(&self, v: f64) {
        self.0.store(v.to_bits(), Relaxed)
    }
}

#[derive(Default)]
pub struct DeckShared {
    pub position: AtomicF64,
    pub duration: AtomicF64,
    pub playing: AtomicBool,
    /// How fast the audio is advancing right now (1.0 = original tempo), including
    /// nudges and sync's brief phase corrections. For moving the playhead.
    pub speed: AtomicF64,
    /// The deck's tempo as a ratio to the track's own: the pitch fader, or the followed
    /// tempo when synced. Steady: leaves out nudges and phase corrections. For anything
    /// that shows tempo (waveform zoom, effective key).
    pub tempo: AtomicF64,
    /// Pitch fader position as a fraction, e.g. 0.02 = +2 %.
    pub rate: AtomicF64,
    /// The tempo fader's range ([`TempoRange::index`]).
    pub rate_range: AtomicU32,
    /// Where the tempo fader is (-1..1), which differs from `rate` until it's picked
    /// up again after sync or a range change moved the tempo.
    pub fader: AtomicF64,
    /// The fader is in control (it has been moved past the tempo since then).
    pub fader_live: AtomicBool,
    /// The track's tempo (its grid's mean) × the tempo fader. NaN without a grid.
    pub bpm: AtomicF64,
    pub sync: AtomicBool,
    /// Cues, loops and hot cues snap to the beat grid.
    pub quantize: AtomicBool,
    /// The tempo fader leaves the key alone.
    pub keylock: AtomicBool,
    pub loop_in: AtomicF64,
    pub loop_out: AtomicF64,
    pub loop_active: AtomicBool,
    pub cue_point: AtomicF64,
    pub hotcues: [AtomicF64; HOTCUES],
    pub volume: AtomicF64,
    pub pfl: AtomicBool,
    /// Channel gain (trim) before the EQ, linear: 1 = unchanged.
    pub pregain: AtomicF64,
    /// Auto-gain for the loaded track, linear (see `loudness::Loudness::auto_gain`).
    pub auto_gain: AtomicF64,
    /// EQ band gains, linear, indexed by [`EQ_LOW`] etc.: 0 = killed, 1 = flat.
    pub eq: [AtomicF64; 3],
    /// Filter knob: -1 low-pass .. 0 off .. 1 high-pass.
    pub filter: AtomicF64,
    /// Channel level meter (peak, after gain, EQ and filter, before the fader).
    pub vu: AtomicF64,
    /// Bumped on every load so readers can tell tracks apart.
    pub generation: AtomicU64,
}

#[derive(Default)]
pub struct Shared {
    pub decks: [DeckShared; PLAYERS],
    pub crossfader: AtomicF64,
    /// Main output level, linear.
    pub main_gain: AtomicF64,
    /// Main output balance, -1 (left) .. 1 (right).
    pub balance: AtomicF64,
    /// Headphone level, linear.
    pub head_gain: AtomicF64,
    /// What the headphones hear: -1 the cue bus only .. 1 the main mix only.
    pub head_mix: AtomicF64,
    /// Main output meters, left and right (peak).
    pub vu_main: [AtomicF64; 2],
    /// How much the master limiter turned down in the last block, dB (0 = not at all).
    pub limiting: AtomicF64,
    pub callbacks: AtomicU64,
    /// Callbacks that took more than 80 % of their time budget.
    pub slow_callbacks: AtomicU64,
    /// Xruns reported by the audio backend.
    pub xruns: AtomicU64,
    /// Worst callback duration since the last [`Shared::take_max_callback_ns`].
    pub max_callback_ns: AtomicU64,
    pub last_block_frames: AtomicU32,
    pub sample_rate: AtomicU32,
    /// The preview player (headphones only).
    pub preview_playing: AtomicBool,
    pub preview_position: AtomicF64,
    pub preview_duration: AtomicF64,
}

impl Shared {
    pub fn take_max_callback_ns(&self) -> u64 {
        self.max_callback_ns.swap(0, Relaxed)
    }
}

// ---- output routing ----------------------------------------------------------------

/// Where a stereo bus goes on the device. Channels are 0-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Out {
    Off,
    /// Left on this channel, right on the next.
    Stereo(u16),
    /// Left and right summed onto one channel (e.g. split cueing on a laptop).
    Mono(u16),
}

/// Master and headphone buses to device channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Routing {
    pub master: Out,
    pub phones: Out,
}

impl Routing {
    /// Master on 1/2, headphones on 3/4 when the device has them (DDJ-400 layout).
    pub fn default_for(channels: u16) -> Routing {
        Routing {
            master: Out::Stereo(0),
            phones: if channels >= 4 {
                Out::Stereo(2)
            } else {
                Out::Off
            },
        }
    }
}

impl Default for Routing {
    fn default() -> Routing {
        Routing::default_for(4)
    }
}

/// Add a stereo frame to `frame` as `out` says. Channels the device lacks are skipped.
#[inline]
fn route(frame: &mut [f32], out: Out, l: f32, r: f32) {
    let ch = frame.len();
    match out {
        Out::Off => {}
        Out::Stereo(c) if ch == 1 => {
            if c == 0 {
                frame[0] += 0.5 * (l + r);
            }
        }
        Out::Stereo(c) => {
            let c = c as usize;
            if c + 1 < ch {
                frame[c] += l;
                frame[c + 1] += r;
            }
        }
        Out::Mono(c) => {
            if (c as usize) < ch {
                frame[c as usize] += 0.5 * (l + r);
            }
        }
    }
}

// ---- commands ----------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Press {
    Down,
    Up,
}

pub enum Command {
    Load {
        deck: usize,
        audio: Arc<TrackAudio>,
        grid: Option<Arc<Beatgrid>>,
        generation: u64,
        /// The track's saved hot cues (seconds).
        hotcues: [Option<f64>; HOTCUES],
    },
    /// Put the cue point at `at` (seconds), or at the playhead (snapped to the beat
    /// when quantising) with `None`.
    SetCue {
        deck: usize,
        at: Option<f64>,
    },
    /// Loop `start..end` (seconds) and jump to its start.
    SetLoop {
        deck: usize,
        start: f64,
        end: f64,
    },
    SetGrid {
        deck: usize,
        grid: Option<Arc<Beatgrid>>,
    },
    Play {
        deck: usize,
        on: bool,
    },
    /// CDJ-style cue: set / return / preview while held.
    Cue {
        deck: usize,
        press: Press,
    },
    Seek {
        deck: usize,
        seconds: f64,
    },
    BeatJump {
        deck: usize,
        beats: f64,
    },
    /// Toggle an auto-loop of `beats` beats starting at the current beat.
    BeatLoop {
        deck: usize,
        beats: f64,
    },
    LoopIn {
        deck: usize,
    },
    LoopOut {
        deck: usize,
    },
    ReloopToggle {
        deck: usize,
    },
    LoopScale {
        deck: usize,
        factor: f64,
    },
    Hotcue {
        deck: usize,
        slot: usize,
        press: Press,
    },
    HotcueClear {
        deck: usize,
        slot: usize,
    },
    /// Tempo change, as a fraction (within the deck's [`TempoRange`]).
    Rate {
        deck: usize,
        value: f64,
    },
    /// Temporary speed offset while a nudge button is held; 0 releases it.
    Nudge {
        deck: usize,
        amount: f64,
    },
    Sync {
        deck: usize,
        on: bool,
    },
    Volume {
        deck: usize,
        value: f64,
    },
    Pfl {
        deck: usize,
        on: bool,
    },
    /// Channel gain (trim), linear (0..4).
    Pregain {
        deck: usize,
        value: f64,
    },
    /// One EQ band's gain, linear (0 kills it, 1 is flat).
    Eq {
        deck: usize,
        band: usize,
        value: f64,
    },
    /// Filter knob, -1 (low-pass) .. 0 (off) .. 1 (high-pass).
    Filter {
        deck: usize,
        value: f64,
    },
    MainGain {
        value: f64,
    },
    Balance {
        value: f64,
    },
    HeadGain {
        value: f64,
    },
    HeadMix {
        value: f64,
    },
    Crossfader {
        value: f64,
    },
    Quantize {
        deck: usize,
        on: bool,
    },
    Keylock {
        deck: usize,
        on: bool,
    },
    /// The tempo fader moved to `position` (-1..1 across its range). It sets the
    /// tempo only once it has been moved past the current tempo ("pickup"), and never
    /// while sync is on.
    TempoFader {
        deck: usize,
        position: f64,
    },
    /// The tempo fader's range. The tempo stays put (clamped into the new range).
    TempoRange {
        deck: usize,
        range: TempoRange,
    },
    SetRouting(Routing),
    /// Play a track on the headphone bus only, from `start` seconds.
    Preview {
        audio: Arc<TrackAudio>,
        start: f64,
        /// Its auto-gain, linear.
        gain: f64,
    },
    /// Auto-gain for the track just loaded on `deck`, linear.
    AutoGain {
        deck: usize,
        gain: f64,
    },
    PreviewStop,
    /// Pause (`false`) or resume (`true`) the preview, keeping its place.
    PreviewPlay(bool),
    /// Move the preview to this many seconds in.
    PreviewSeek(f64),
}

/// Things the audio thread must not free itself.
pub enum Garbage {
    Audio(Arc<TrackAudio>),
    Grid(Arc<Beatgrid>),
}

// ---- deck --------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Preview {
    None,
    Cue,
    Hotcue(usize),
}

struct Deck {
    audio: Option<Arc<TrackAudio>>,
    grid: Option<Arc<Beatgrid>>,
    /// Playhead in seconds.
    pos: f64,
    playing: bool,
    rate: f64,
    range: TempoRange,
    /// The tempo fader's position and whether it's in control: see
    /// [`DeckShared::fader`].
    fader: f64,
    fader_live: bool,
    nudge: f64,
    sync: bool,
    quantize: bool,
    keylock: bool,
    speed: f64,
    cue_point: f64,
    preview: Preview,
    loop_in: Option<f64>,
    loop_out: Option<f64>,
    loop_active: bool,
    hotcues: [Option<f64>; HOTCUES],
    volume: f32,
    pfl: bool,
    generation: u64,
    /// While synced: the smoothed tempo ratio to the leader, and the smoothed phase
    /// correction on top of it. 0 = not following yet.
    sync_ratio: f64,
    sync_correction: f64,
    /// See [`DeckShared::tempo`].
    tempo: f64,
}

/// Sync follows the leader's tempo averaged over this many beats either side, so
/// beat-to-beat wobble in an analysed grid doesn't shake the follower.
const SYNC_TEMPO_BEATS: usize = 16;
/// Seconds for the followed tempo and the phase correction to settle.
const SYNC_TEMPO_SECS: f64 = 0.5;
const SYNC_PHASE_SECS: f64 = 0.25;
/// Phase errors smaller than this (beats; ~4 ms at 120 BPM) are left alone.
const SYNC_DEADBAND: f64 = 0.008;

/// How far before the start of a track the playhead may go (seconds).
pub const LEAD_IN: f64 = 30.0;

/// The tempo fader picks the tempo up when it comes this close (in fader travel,
/// -1..1) or moves past it.
const PICKUP: f64 = 0.01;

/// Without a grid, beat maths falls back to 120 BPM from zero.
const FALLBACK_BEAT: f64 = 0.5;

impl Deck {
    fn new() -> Self {
        Deck {
            audio: None,
            grid: None,
            pos: 0.0,
            playing: false,
            rate: 0.0,
            range: TempoRange::default(),
            fader: 0.0,
            fader_live: true,
            nudge: 0.0,
            sync: false,
            quantize: true,
            keylock: false,
            speed: 1.0,
            cue_point: 0.0,
            preview: Preview::None,
            loop_in: None,
            loop_out: None,
            loop_active: false,
            hotcues: [None; HOTCUES],
            volume: 1.0,
            pfl: false,
            generation: 0,
            sync_ratio: 0.0,
            sync_correction: 0.0,
            tempo: 1.0,
        }
    }

    fn duration(&self) -> f64 {
        self.audio.as_ref().map_or(0.0, |a| a.duration())
    }

    fn beat_at(&self, t: f64) -> f64 {
        match &self.grid {
            Some(g) => g.beat_at(t),
            None => t / FALLBACK_BEAT,
        }
    }

    fn time_at(&self, b: f64) -> f64 {
        match &self.grid {
            Some(g) => g.time_at(b),
            None => b * FALLBACK_BEAT,
        }
    }

    /// Tempo for sync: the grid's tempo around the playhead, averaged widely (the
    /// grid's one tempo when it is straight).
    fn sync_bpm(&self) -> Option<f64> {
        self.grid
            .as_ref()
            .map(|g| g.bpm_at(self.pos, SYNC_TEMPO_BEATS))
    }

    /// `t`, or the nearest beat when quantising.
    fn snap(&self, t: f64) -> f64 {
        match (&self.grid, self.quantize) {
            (Some(g), true) => g.quantize(t),
            _ => t,
        }
    }

    /// Start of the beat the playhead is in (with a little tolerance for "just before").
    fn current_beat_start(&self) -> f64 {
        self.time_at((self.beat_at(self.pos) + 0.05).floor())
    }

    /// Move the playhead. It may go up to [`LEAD_IN`] before the track starts (silence
    /// on the beat grid, for starting a track early so it comes in on time).
    fn seek(&mut self, t: f64) {
        self.pos = t.clamp(-LEAD_IN, self.duration());
    }

    /// After the tempo moved without the fader: the fader is in control again only
    /// if it's where the tempo now is.
    fn check_pickup(&mut self) {
        self.fader_live = (self.range.fader(self.rate) - self.fader).abs() < PICKUP;
    }

    fn base_speed(&self) -> f64 {
        (1.0 + self.rate) * (1.0 + self.nudge)
    }
}

// ---- renderer (audio thread) -------------------------------------------------------

pub struct Renderer {
    commands: rtrb::Consumer<Command>,
    garbage: rtrb::Producer<Garbage>,
    shared: Arc<Shared>,
    decks: [Deck; PLAYERS],
    crossfader: f64,
    sample_rate: f64,
    /// Per-deck stereo scratch, master and headphone buses (interleaved stereo).
    deck_buf: [Vec<f32>; PLAYERS],
    keylocks: [Keylock; PLAYERS],
    master: Vec<f32>,
    phones: Vec<f32>,
    routing: Routing,
    preview: Option<Arc<TrackAudio>>,
    preview_pos: f64,
    preview_playing: bool,
    preview_gain: f32,
    limiters: [Limiter; 2],
    limiting: f64,
    strips: [Strip; PLAYERS],
    main_gain: f64,
    balance: f64,
    head_gain: f64,
    head_mix: f64,
    vu_main: [f32; 2],
    /// Per-sample easing for knobs; see [`KNOB_SECS`].
    knob_ease: f64,
}

/// A mixer channel's gain, EQ and filter. Kept apart from [`Deck`] so loading a
/// track leaves the mixer as it is.
struct Strip {
    /// The loaded track's auto-gain, before the gain knob.
    auto_gain: f64,
    pregain: Smoothed,
    eq_gain: [Smoothed; 3],
    eq: Eq3,
    filter_value: f64,
    filter: DjFilter,
    vu: f32,
}

impl Strip {
    fn new(sr: f64) -> Strip {
        Strip {
            auto_gain: 1.0,
            pregain: Smoothed::new(1.0),
            eq_gain: [Smoothed::new(1.0); 3],
            eq: Eq3::new(sr),
            filter_value: 0.0,
            filter: DjFilter::new(sr),
            vu: 0.0,
        }
    }

    /// Gain, EQ and filter on one block of interleaved stereo, in place. Returns the
    /// block's peak.
    fn process(&mut self, buf: &mut [f32], k: f64) -> f32 {
        self.filter.set(self.filter_value);
        let filtering = self.filter.active();
        let mut peak = 0f32;
        for frame in buf.chunks_exact_mut(2) {
            let pre = self.auto_gain * self.pregain.next(k);
            let g = [
                self.eq_gain[0].next(k),
                self.eq_gain[1].next(k),
                self.eq_gain[2].next(k),
            ];
            for (ch, s) in frame.iter_mut().enumerate() {
                let mut y = self.eq.run(ch, *s as f64 * pre, g);
                if filtering {
                    y = self.filter.run(ch, y);
                }
                *s = y as f32;
                peak = peak.max(s.abs());
            }
        }
        peak
    }
}

/// A meter that jumps up to a block's peak and falls back slowly.
fn meter(level: f32, peak: f32, secs: f64) -> f32 {
    peak.max(level * (-secs / VU_FALL_SECS).exp() as f32)
}

impl Renderer {
    pub fn new(
        commands: rtrb::Consumer<Command>,
        garbage: rtrb::Producer<Garbage>,
        shared: Arc<Shared>,
        sample_rate: u32,
    ) -> Self {
        shared.sample_rate.store(sample_rate, Relaxed);
        Renderer {
            commands,
            garbage,
            shared,
            decks: std::array::from_fn(|_| Deck::new()),
            crossfader: 0.0,
            sample_rate: sample_rate as f64,
            deck_buf: std::array::from_fn(|_| vec![0.0; MAX_BLOCK * 2]),
            keylocks: std::array::from_fn(|_| Keylock::new(sample_rate, MAX_BLOCK)),
            master: vec![0.0; MAX_BLOCK * 2],
            phones: vec![0.0; MAX_BLOCK * 2],
            routing: Routing::default(),
            preview: None,
            preview_pos: 0.0,
            preview_playing: false,
            preview_gain: 1.0,
            limiters: [
                Limiter::new(sample_rate as f64),
                Limiter::new(sample_rate as f64),
            ],
            limiting: 0.0,
            strips: std::array::from_fn(|_| Strip::new(sample_rate as f64)),
            main_gain: 1.0,
            balance: 0.0,
            head_gain: 1.0,
            head_mix: -1.0,
            vu_main: [0.0; 2],
            knob_ease: dsp::ease_per_sample(KNOB_SECS, sample_rate as f64),
        }
    }

    /// Render into an interleaved device buffer with `channels` channels, routing the
    /// master and headphone buses as set by [`Command::SetRouting`].
    pub fn process(&mut self, out: &mut [f32], channels: usize) {
        let started = Instant::now();
        let channels = channels.max(1);
        let total = out.len() / channels;
        let mut done = 0;
        while done < total {
            let n = (total - done).min(MAX_BLOCK);
            self.render_block(n);
            let chunk = &mut out[done * channels..(done + n) * channels];
            let Routing { master, phones } = self.routing;
            for (f, frame) in chunk.chunks_exact_mut(channels).enumerate() {
                frame.fill(0.0);
                route(frame, master, self.master[2 * f], self.master[2 * f + 1]);
                route(frame, phones, self.phones[2 * f], self.phones[2 * f + 1]);
                if master != Out::Off && phones != Out::Off {
                    // Both buses may share channels (a laptop's only pair).
                    for s in frame.iter_mut() {
                        *s = s.clamp(-1.0, 1.0);
                    }
                }
            }
            done += n;
        }
        self.publish();
        let elapsed = started.elapsed().as_nanos() as u64;
        let budget_ns = (total as f64 / self.sample_rate * 1e9) as u64;
        let s = &self.shared;
        s.callbacks.fetch_add(1, Relaxed);
        s.last_block_frames.store(total as u32, Relaxed);
        s.max_callback_ns.fetch_max(elapsed, Relaxed);
        if elapsed * 10 > budget_ns * 8 {
            s.slow_callbacks.fetch_add(1, Relaxed);
        }
    }

    fn render_block(&mut self, n: usize) {
        while let Ok(cmd) = self.commands.pop() {
            self.apply(cmd);
        }
        self.update_speeds(n);

        let sr = self.sample_rate;
        for d in 0..PLAYERS {
            let buf = &mut self.deck_buf[d][..n * 2];
            buf.fill(0.0);
            let deck = &mut self.decks[d];
            let kl = &mut self.keylocks[d];
            let Some(audio) = deck.audio.as_deref() else {
                kl.idle();
                continue;
            };
            if !deck.playing {
                kl.idle();
                continue;
            }
            let src_rate = audio.sample_rate as f64;
            let step = deck.speed / sr;
            let duration = audio.duration();
            let looping = deck.loop_active
                && deck.loop_in.is_some()
                && deck.loop_out.is_some()
                && deck.loop_out > deck.loop_in;
            let (lin, lout) = (
                deck.loop_in.unwrap_or(0.0),
                deck.loop_out.unwrap_or(f64::MAX),
            );
            // Where the audio for a playhead position comes from: with keylock the
            // deck reads ahead by the shifter's delay, wrapping round a loop.
            let keylock = deck.keylock && keylock::SPEEDS.contains(&deck.speed);
            let lead = if keylock { kl.lead(step) } else { 0.0 };
            let source = |t: f64| {
                if looping && t >= lout {
                    lin + (t - lin) % (lout - lin)
                } else {
                    t
                }
            };
            if keylock && kl.needs_restart(deck.pos) {
                let len = kl.preroll_frames();
                let start = deck.pos + lead - len as f64 * step;
                kl.restart(|i| sample_at(audio, source(start + i as f64 * step) * src_rate));
            }
            let mut played = n;
            for f in 0..n {
                let (l, r) = sample_at(audio, source(deck.pos + lead) * src_rate);
                buf[2 * f] = l;
                buf[2 * f + 1] = r;
                deck.pos += step;
                if looping && deck.pos >= lout {
                    deck.pos -= lout - lin;
                }
                if deck.pos >= duration {
                    deck.pos = duration;
                    deck.playing = false;
                    deck.preview = Preview::None;
                    played = f + 1;
                    break;
                }
            }
            if keylock {
                kl.process(&mut buf[..played * 2], deck.speed, deck.pos);
            } else {
                kl.idle();
            }
        }

        // Channel strips: gain, EQ, filter. Meters read here, before the faders.
        let secs = n as f64 / sr;
        for d in 0..PLAYERS {
            let strip = &mut self.strips[d];
            let peak = strip.process(&mut self.deck_buf[d][..n * 2], self.knob_ease);
            strip.vu = meter(strip.vu, peak, secs);
        }

        // Constant-power crossfader.
        let x = (self.crossfader.clamp(-1.0, 1.0) + 1.0) * std::f64::consts::FRAC_PI_4;
        let xf = [x.cos() as f32, x.sin() as f32];
        let any_pfl = self.decks.iter().any(|d| d.pfl);
        let main = self.main_gain as f32;
        let bal = self.balance.clamp(-1.0, 1.0) as f32;
        let side = [(1.0 - bal).min(1.0), (1.0 + bal).min(1.0)];
        // Head mix: how much of the main mix the headphones hear, against the cue bus.
        let mix = ((self.head_mix.clamp(-1.0, 1.0) + 1.0) / 2.0) as f32;
        let head = self.head_gain as f32;
        let mut peaks = [0f32; 2];
        for f in 0..n * 2 {
            let mut m = 0.0;
            let mut p = 0.0;
            for d in 0..DECKS {
                let s = self.deck_buf[d][f];
                let deck = &self.decks[d];
                m += s * deck.volume * deck.volume * xf[d];
                if deck.pfl {
                    p += s;
                }
            }
            // Prepare's player goes straight to the main mix.
            let m = 0.8 * (m + self.deck_buf[PREP][f]);
            let out = m * main * side[f % 2];
            // Metered before the limiter, so over 0 dB shows when it's working.
            peaks[f % 2] = peaks[f % 2].max(out.abs());
            self.master[f] = out;
            // With nothing cued the cue bus carries the main mix.
            let cue = if any_pfl { 0.8 * p } else { m };
            self.phones[f] = head * ((1.0 - mix) * cue + mix * m);
        }
        for (v, p) in self.vu_main.iter_mut().zip(peaks) {
            *v = meter(*v, p, secs);
        }
        let lowest = self.limiters[0].process(&mut self.master[..n * 2]);
        self.limiting = -20.0 * (lowest as f64).log10();

        // The preview player plays at its own pace on the headphone bus only.
        if self.preview_playing
            && let Some(audio) = self.preview.as_deref()
        {
            let src_rate = audio.sample_rate as f64;
            let duration = audio.duration();
            for f in 0..n {
                if self.preview_pos >= duration {
                    self.preview_playing = false;
                    break;
                }
                let (l, r) = sample_at(audio, self.preview_pos * src_rate);
                let g = 0.8 * self.head_gain as f32 * self.preview_gain;
                self.phones[2 * f] += g * l;
                self.phones[2 * f + 1] += g * r;
                self.preview_pos += 1.0 / sr;
            }
        }
        // The headphones get their own limiter, after the preview is mixed in.
        self.limiters[1].process(&mut self.phones[..n * 2]);
    }

    /// Synced decks follow the other playing deck's tempo and beat phase, block by block,
    /// so they keep up when the leader's tempo drifts. Both are smoothed: the tempo is
    /// averaged over many beats and eased in, and small phase errors are ignored, so
    /// the follower (and its BPM readout) stays steady when the grids wobble a little.
    ///
    /// A paused follower matches the leader's tempo (so its readout and waveform zoom
    /// are right when it starts) but doesn't correct phase: it isn't moving, so the
    /// error would just sweep round with every one of the leader's beats.
    fn update_speeds(&mut self, n: usize) {
        for dk in &mut self.decks {
            dk.speed = dk.base_speed();
            dk.tempo = 1.0 + dk.rate;
        }
        let secs = n as f64 / self.sample_rate;
        let ease = |tau: f64| 1.0 - (-secs / tau).exp();
        for d in 0..DECKS {
            let o = 1 - d;
            let (me, other) = (&self.decks[d], &self.decks[o]);
            if !me.sync || other.audio.is_none() {
                self.decks[d].sync_ratio = 0.0;
                self.decks[d].sync_correction = 0.0;
                continue;
            }
            let (Some(my_bpm), Some(leader_bpm)) = (me.sync_bpm(), other.sync_bpm()) else {
                continue;
            };
            // The leader's own sync correction is left out, so two synced decks don't
            // chase each other's corrections.
            let leader_speed = other.speed / (1.0 + other.sync_correction);
            let target = leader_bpm * leader_speed / my_bpm;
            // The readout shows the leader's readout: both tracks' one tempo, not the
            // local tempo the speed follows (they are the same for straight grids).
            let shown = match (&me.grid, &other.grid) {
                (Some(m), Some(l)) => l.mean_bpm() * (1.0 + other.rate) / m.mean_bpm(),
                _ => target,
            };
            let wanted = if me.playing && other.playing {
                let err = phase_error(other.beat_at(other.pos), me.beat_at(me.pos));
                if err.abs() < SYNC_DEADBAND {
                    0.0
                } else {
                    (err * 0.1).clamp(-0.02, 0.02)
                }
            } else {
                0.0
            };
            let me = &mut self.decks[d];
            me.sync_ratio = if me.sync_ratio == 0.0 || !me.playing {
                target
            } else {
                me.sync_ratio + (target - me.sync_ratio) * ease(SYNC_TEMPO_SECS)
            };
            me.sync_correction = if me.playing {
                me.sync_correction + (wanted - me.sync_correction) * ease(SYNC_PHASE_SECS)
            } else {
                0.0
            };
            me.speed = me.sync_ratio * (1.0 + me.sync_correction) * (1.0 + me.nudge);
            me.tempo = me.sync_ratio;
            me.rate = shown - 1.0;
            me.check_pickup();
        }
    }

    /// Jump `deck` to the nearest position whose beat phase matches the other deck.
    fn snap_phase(&mut self, d: usize) {
        let other = &self.decks[1 - d];
        if other.audio.is_none() || self.decks[d].grid.is_none() {
            return;
        }
        let err = phase_error(
            other.beat_at(other.pos),
            self.decks[d].beat_at(self.decks[d].pos),
        );
        let me = &mut self.decks[d];
        let beat = me.beat_at(me.pos) + err;
        let target = me.time_at(beat);
        me.seek(target);
    }

    fn trash(&mut self, g: Garbage) {
        if let Err(rtrb::PushError::Full(g)) = self.garbage.push(g) {
            // Leaking is better than freeing on the audio thread.
            std::mem::forget(g);
        }
    }

    fn apply(&mut self, cmd: Command) {
        match cmd {
            Command::Load {
                deck,
                audio,
                grid,
                generation,
                hotcues,
            } => {
                let dk = &mut self.decks[deck];
                let old_audio = dk.audio.replace(audio);
                let old_grid = std::mem::replace(&mut dk.grid, grid);
                let first_beat = dk.grid.as_ref().map_or(0.0, |g| g.time_at(0.0).max(0.0));
                *dk = Deck {
                    audio: dk.audio.take(),
                    grid: dk.grid.take(),
                    pos: first_beat,
                    cue_point: first_beat,
                    rate: dk.rate,
                    range: dk.range,
                    fader: dk.fader,
                    fader_live: dk.fader_live,
                    sync: dk.sync,
                    quantize: dk.quantize,
                    keylock: dk.keylock,
                    volume: dk.volume,
                    pfl: dk.pfl,
                    generation,
                    hotcues,
                    ..Deck::new()
                };
                if let Some(a) = old_audio {
                    self.trash(Garbage::Audio(a));
                }
                if let Some(g) = old_grid {
                    self.trash(Garbage::Grid(g));
                }
            }
            Command::SetGrid { deck, grid } => {
                if let Some(g) = std::mem::replace(&mut self.decks[deck].grid, grid) {
                    self.trash(Garbage::Grid(g));
                }
            }
            Command::Play { deck, on } => {
                let dk = &mut self.decks[deck];
                if dk.audio.is_none() {
                    return;
                }
                dk.preview = Preview::None;
                if on && !dk.playing && dk.sync {
                    dk.playing = true;
                    self.snap_phase(deck);
                } else {
                    dk.playing = on;
                }
            }
            Command::Cue { deck, press } => {
                let dk = &mut self.decks[deck];
                if dk.audio.is_none() {
                    return;
                }
                match press {
                    Press::Down if dk.playing && dk.preview == Preview::None => {
                        dk.playing = false;
                        dk.pos = dk.cue_point;
                    }
                    Press::Down if !dk.playing => {
                        if (dk.pos - dk.cue_point).abs() < 0.005 {
                            dk.preview = Preview::Cue;
                            dk.playing = true;
                        } else {
                            dk.cue_point = dk.snap(dk.pos);
                            dk.pos = dk.cue_point;
                        }
                    }
                    Press::Up if dk.preview == Preview::Cue => {
                        dk.playing = false;
                        dk.preview = Preview::None;
                        dk.pos = dk.cue_point;
                    }
                    _ => {}
                }
            }
            Command::Seek { deck, seconds } => self.decks[deck].seek(seconds),
            Command::BeatJump { deck, beats } => {
                let dk = &mut self.decks[deck];
                let target = dk.time_at(dk.beat_at(dk.pos) + beats);
                let shift = target - dk.pos;
                if dk.loop_active {
                    if let (Some(i), Some(o)) = (dk.loop_in, dk.loop_out) {
                        dk.loop_in = Some(dk.time_at(dk.beat_at(i) + beats));
                        dk.loop_out = Some(dk.time_at(dk.beat_at(o) + beats));
                    }
                }
                dk.seek(dk.pos + shift);
            }
            Command::SetCue { deck, at } => {
                let dk = &mut self.decks[deck];
                dk.cue_point = at.unwrap_or_else(|| dk.snap(dk.pos)).max(-LEAD_IN);
            }
            Command::SetLoop { deck, start, end } => {
                let dk = &mut self.decks[deck];
                if end > start {
                    dk.loop_in = Some(start);
                    dk.loop_out = Some(end);
                    dk.loop_active = true;
                    dk.seek(start);
                }
            }
            Command::BeatLoop { deck, beats } => {
                let dk = &mut self.decks[deck];
                // The same size again ends the loop; another size resizes it from
                // the same start.
                if dk.loop_active
                    && let (Some(lin), Some(lout)) = (dk.loop_in, dk.loop_out)
                {
                    let size = dk.beat_at(lout) - dk.beat_at(lin);
                    if (size - beats).abs() < 1e-6 {
                        dk.loop_active = false;
                    } else {
                        dk.loop_out = Some(dk.time_at(dk.beat_at(lin) + beats));
                        if dk.pos >= dk.loop_out.unwrap_or(f64::MAX) {
                            dk.seek(lin);
                        }
                    }
                    return;
                }
                let start = if dk.quantize {
                    dk.current_beat_start()
                } else {
                    dk.pos
                };
                dk.loop_in = Some(start);
                dk.loop_out = Some(dk.time_at(dk.beat_at(start) + beats));
                dk.loop_active = true;
            }
            Command::LoopIn { deck } => {
                let dk = &mut self.decks[deck];
                dk.loop_in = Some(dk.snap(dk.pos));
                dk.loop_out = None;
                dk.loop_active = false;
            }
            Command::LoopOut { deck } => {
                let dk = &mut self.decks[deck];
                let out = dk.snap(dk.pos);
                if let Some(i) = dk.loop_in {
                    if out > i {
                        dk.loop_out = Some(out);
                        dk.loop_active = true;
                    }
                }
            }
            Command::ReloopToggle { deck } => {
                let dk = &mut self.decks[deck];
                if dk.loop_active {
                    dk.loop_active = false;
                } else if let (Some(i), Some(o)) = (dk.loop_in, dk.loop_out) {
                    dk.loop_active = true;
                    if dk.pos > o {
                        dk.pos = i;
                    }
                }
            }
            Command::LoopScale { deck, factor } => {
                let dk = &mut self.decks[deck];
                if let (Some(i), Some(o)) = (dk.loop_in, dk.loop_out) {
                    let bi = dk.beat_at(i);
                    let len = (dk.beat_at(o) - bi) * factor;
                    if len > 1.0 / 64.0 {
                        let new_out = dk.time_at(bi + len);
                        dk.loop_out = Some(new_out);
                        if dk.loop_active && dk.pos >= new_out {
                            dk.pos = i + (dk.pos - i) % (new_out - i);
                        }
                    }
                }
            }
            Command::Hotcue { deck, slot, press } => {
                let dk = &mut self.decks[deck];
                if dk.audio.is_none() || slot >= HOTCUES {
                    return;
                }
                match (press, dk.hotcues[slot]) {
                    (Press::Down, None) => dk.hotcues[slot] = Some(dk.snap(dk.pos)),
                    (Press::Down, Some(t)) => {
                        dk.pos = t;
                        if !dk.playing {
                            dk.playing = true;
                            dk.preview = Preview::Hotcue(slot);
                        }
                    }
                    (Press::Up, Some(t)) if dk.preview == Preview::Hotcue(slot) => {
                        dk.playing = false;
                        dk.preview = Preview::None;
                        dk.pos = t;
                    }
                    _ => {}
                }
            }
            Command::HotcueClear { deck, slot } => {
                if slot < HOTCUES {
                    self.decks[deck].hotcues[slot] = None;
                }
            }
            Command::Rate { deck, value } => {
                let dk = &mut self.decks[deck];
                let (lo, hi) = dk.range.bounds();
                dk.rate = value.clamp(lo, hi);
                dk.check_pickup();
            }
            Command::TempoFader { deck, position } => {
                let dk = &mut self.decks[deck];
                let target = dk.range.fader(dk.rate);
                let was = dk.fader;
                dk.fader = position.clamp(-1.0, 1.0);
                if dk.sync {
                    dk.check_pickup();
                } else {
                    let crossed = (was - target) * (dk.fader - target) <= 0.0;
                    if crossed || (dk.fader - target).abs() < PICKUP {
                        dk.fader_live = true;
                    }
                    if dk.fader_live {
                        dk.rate = dk.range.rate(dk.fader);
                    }
                }
            }
            Command::Nudge { deck, amount } => self.decks[deck].nudge = amount,
            Command::Sync { deck, on } => {
                if deck >= DECKS {
                    return; // Prepare's player doesn't sync
                }
                self.decks[deck].sync = on;
                // Line the beats up now if both are playing; the tempo follows below
                // (only a loaded track is needed for that).
                if on && self.decks[deck].playing && self.decks[1 - deck].playing {
                    self.snap_phase(deck);
                }
            }
            Command::Volume { deck, value } => {
                self.decks[deck].volume = value.clamp(0.0, 1.0) as f32
            }
            Command::Pfl { deck, on } => self.decks[deck].pfl = on,
            Command::Pregain { deck, value } => {
                self.strips[deck].pregain.target = value.clamp(0.0, 4.0)
            }
            Command::Eq { deck, band, value } => {
                if band < 3 {
                    self.strips[deck].eq_gain[band].target = value.clamp(0.0, 4.0);
                }
            }
            Command::Filter { deck, value } => {
                self.strips[deck].filter_value = value.clamp(-1.0, 1.0)
            }
            Command::MainGain { value } => self.main_gain = value.clamp(0.0, 5.0),
            Command::Balance { value } => self.balance = value.clamp(-1.0, 1.0),
            Command::HeadGain { value } => self.head_gain = value.clamp(0.0, 5.0),
            Command::HeadMix { value } => self.head_mix = value.clamp(-1.0, 1.0),
            Command::Crossfader { value } => self.crossfader = value,
            Command::Quantize { deck, on } => {
                if let Some(dk) = self.decks.get_mut(deck) {
                    dk.quantize = on;
                }
            }
            Command::TempoRange { deck, range } => {
                if let Some(dk) = self.decks.get_mut(deck) {
                    let (lo, hi) = range.bounds();
                    dk.range = range;
                    dk.rate = dk.rate.clamp(lo, hi);
                    dk.check_pickup();
                }
            }
            Command::Keylock { deck, on } => {
                if let Some(dk) = self.decks.get_mut(deck) {
                    dk.keylock = on;
                }
            }
            Command::SetRouting(r) => self.routing = r,
            Command::AutoGain { deck, gain } => {
                if let Some(st) = self.strips.get_mut(deck) {
                    st.auto_gain = gain.clamp(0.05, 8.0);
                }
            }
            Command::Preview { audio, start, gain } => {
                self.preview_gain = gain.clamp(0.05, 8.0) as f32;
                self.preview_pos = start.clamp(0.0, audio.duration());
                self.preview_playing = true;
                if let Some(old) = self.preview.replace(audio) {
                    self.trash(Garbage::Audio(old));
                }
            }
            Command::PreviewStop => {
                self.preview_playing = false;
                if let Some(old) = self.preview.take() {
                    self.trash(Garbage::Audio(old));
                }
            }
            Command::PreviewPlay(on) => {
                if let Some(audio) = &self.preview {
                    // Resuming at the very end starts again.
                    if on && self.preview_pos >= audio.duration() {
                        self.preview_pos = 0.0;
                    }
                    self.preview_playing = on;
                }
            }
            Command::PreviewSeek(t) => {
                if let Some(audio) = &self.preview {
                    self.preview_pos = t.clamp(0.0, audio.duration());
                }
            }
        }
    }

    fn publish(&self) {
        let s = &self.shared;
        s.crossfader.set(self.crossfader);
        s.preview_playing.store(self.preview_playing, Relaxed);
        s.preview_position.set(self.preview_pos);
        s.preview_duration
            .set(self.preview.as_ref().map_or(0.0, |a| a.duration()));
        s.main_gain.set(self.main_gain);
        s.limiting.set(self.limiting);
        s.balance.set(self.balance);
        s.head_gain.set(self.head_gain);
        s.head_mix.set(self.head_mix);
        for (v, m) in s.vu_main.iter().zip(self.vu_main) {
            v.set(m as f64);
        }
        for (st, sh) in self.strips.iter().zip(&s.decks) {
            sh.pregain.set(st.pregain.target);
            sh.auto_gain.set(st.auto_gain);
            for (e, g) in sh.eq.iter().zip(&st.eq_gain) {
                e.set(g.target);
            }
            sh.filter.set(st.filter_value);
            sh.vu.set(st.vu as f64);
        }
        for (dk, sh) in self.decks.iter().zip(&s.decks) {
            sh.position.set(dk.pos);
            sh.duration.set(dk.duration());
            sh.playing.store(dk.playing, Relaxed);
            sh.speed.set(dk.speed);
            sh.tempo.set(dk.tempo);
            sh.rate.set(dk.rate);
            sh.rate_range.store(dk.range.index(), Relaxed);
            sh.fader.set(dk.fader);
            sh.fader_live.store(dk.fader_live, Relaxed);
            // The track's one tempo times the tempo fader: steady while playing, and
            // unmoved by nudges and sync's small phase corrections. A synced deck's
            // `rate` is set to match the leader, so it shows the leader's tempo.
            sh.bpm.set(
                dk.grid
                    .as_ref()
                    .map_or(f64::NAN, |g| g.mean_bpm() * (1.0 + dk.rate)),
            );
            sh.sync.store(dk.sync, Relaxed);
            sh.quantize.store(dk.quantize, Relaxed);
            sh.keylock.store(dk.keylock, Relaxed);
            sh.loop_in.set(dk.loop_in.unwrap_or(f64::NAN));
            sh.loop_out.set(dk.loop_out.unwrap_or(f64::NAN));
            sh.loop_active.store(dk.loop_active, Relaxed);
            sh.cue_point.set(dk.cue_point);
            for (h, v) in sh.hotcues.iter().zip(&dk.hotcues) {
                h.set(v.unwrap_or(f64::NAN));
            }
            sh.volume.set(dk.volume as f64);
            sh.pfl.store(dk.pfl, Relaxed);
            sh.generation.store(dk.generation, Relaxed);
        }
    }
}

/// Beat-phase difference `leader - follower`, wrapped to -0.5..0.5 beats.
fn phase_error(leader_beat: f64, follower_beat: f64) -> f64 {
    let e = leader_beat.rem_euclid(1.0) - follower_beat.rem_euclid(1.0);
    e - e.round()
}

/// Cubic Hermite interpolation at fractional source frame `x`.
#[inline]
fn sample_at(audio: &TrackAudio, x: f64) -> (f32, f32) {
    let frames = audio.frames() as i64;
    let i = x.floor() as i64;
    let t = (x - i as f64) as f32;
    let get = |k: i64, c: usize| -> f32 {
        if k < 0 || k >= frames {
            0.0
        } else {
            audio.samples[(k as usize) * 2 + c]
        }
    };
    let mut out = [0f32; 2];
    for (c, o) in out.iter_mut().enumerate() {
        let (y0, y1, y2, y3) = (get(i - 1, c), get(i, c), get(i + 1, c), get(i + 2, c));
        let c1 = 0.5 * (y2 - y0);
        let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
        let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
        *o = ((c3 * t + c2) * t + c1) * t + y1;
    }
    (out[0], out[1])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::{DemoSpec, generate};

    const SR: u32 = 44_100;

    struct Rig {
        tx: rtrb::Producer<Command>,
        r: Renderer,
        shared: Arc<Shared>,
        out: Vec<f32>,
    }

    impl Rig {
        fn new() -> Self {
            let (tx, rx) = rtrb::RingBuffer::new(64);
            let (gtx, _grx) = rtrb::RingBuffer::new(64);
            let shared = Arc::new(Shared::default());
            let r = Renderer::new(rx, gtx, shared.clone(), SR);
            Rig {
                tx,
                r,
                shared,
                out: vec![0.0; 256 * 4],
            }
        }
        fn send(&mut self, c: Command) {
            assert!(self.tx.push(c).is_ok());
        }
        fn load(&mut self, deck: usize, bpm0: f64, bpm1: f64) -> Arc<Beatgrid> {
            let (audio, grid) = generate(
                &DemoSpec {
                    start_bpm: bpm0,
                    end_bpm: bpm1,
                    key: "8A".into(),
                    seconds: 60.0,
                    seed: 3,
                },
                SR,
            );
            let grid = Arc::new(grid);
            self.send(Command::Load {
                deck,
                audio: Arc::new(audio),
                grid: Some(grid.clone()),
                generation: 1,
                hotcues: [None; HOTCUES],
            });
            grid
        }
        fn run(&mut self, seconds: f64) {
            let blocks = (seconds * SR as f64 / 256.0) as usize;
            for _ in 0..blocks.max(1) {
                self.r.process(&mut self.out, 4);
            }
        }
        fn pos(&self, d: usize) -> f64 {
            self.shared.decks[d].position.get()
        }
    }

    /// Load a made-up track (stereo, `secs` long, sample `f(t)` at time t) without a grid.
    fn load_fn(rig: &mut Rig, secs: f64, f: impl Fn(f64) -> f32) {
        let n = (secs * SR as f64) as usize;
        let samples = (0..n)
            .flat_map(|i| {
                let v = f(i as f64 / SR as f64);
                [v, v]
            })
            .collect();
        rig.send(Command::Load {
            deck: 0,
            audio: Arc::new(TrackAudio {
                samples,
                sample_rate: SR,
            }),
            grid: None,
            generation: 1,
            hotcues: [None; HOTCUES],
        });
    }

    /// Run for `seconds`, keeping the master's left channel.
    fn record(rig: &mut Rig, seconds: f64) -> Vec<f32> {
        let blocks = (seconds * SR as f64 / 256.0) as usize;
        let mut out = vec![];
        for _ in 0..blocks {
            rig.r.process(&mut rig.out, 4);
            out.extend(rig.out.chunks_exact(4).map(|f| f[0]));
        }
        out
    }

    /// Frequency of a steady tone from its upward zero crossings.
    fn frequency(x: &[f32]) -> f64 {
        let ups: Vec<usize> = (1..x.len())
            .filter(|&i| x[i - 1] < 0.0 && x[i] >= 0.0)
            .collect();
        (ups.len() - 1) as f64 * SR as f64 / (ups[ups.len() - 1] - ups[0]) as f64
    }

    #[test]
    fn keylock_keeps_the_key_at_another_tempo() {
        for (keylock, want) in [(false, 440.0 * 1.08), (true, 440.0)] {
            let mut rig = Rig::new();
            load_fn(&mut rig, 20.0, |t| {
                0.5 * (std::f64::consts::TAU * 440.0 * t).sin() as f32
            });
            rig.send(Command::Keylock {
                deck: 0,
                on: keylock,
            });
            rig.send(Command::Rate {
                deck: 0,
                value: 0.08,
            });
            rig.send(Command::Play { deck: 0, on: true });
            rig.run(0.5);
            let start = rig.pos(0);
            let f = frequency(&record(&mut rig, 1.0));
            assert!(
                (f - want).abs() < 2.0,
                "keylock {keylock}: {f:.1} Hz, want {want:.1}"
            );
            // The tempo moves either way.
            assert!(
                (rig.pos(0) - start - 1.08).abs() < 0.02,
                "tempo with keylock {keylock}"
            );
        }
    }

    /// Times (s) where clicks start in `x`.
    fn clicks(x: &[f32]) -> Vec<f64> {
        let mut out = vec![];
        let mut quiet_until = 0;
        for (i, v) in x.iter().enumerate() {
            if i >= quiet_until && v.abs() > 0.1 {
                out.push(i as f64 / SR as f64);
                quiet_until = i + SR as usize / 10;
            }
        }
        out
    }

    /// A short burst every half second of track time.
    fn click_track(t: f64) -> f32 {
        let phase = t % 0.5;
        if phase < 0.01 {
            (0.8 * (std::f64::consts::TAU * 1000.0 * phase).sin()) as f32
        } else {
            0.0
        }
    }

    #[test]
    fn keylock_sounds_in_time_with_the_playhead() {
        let mut rig = Rig::new();
        load_fn(&mut rig, 30.0, click_track);
        rig.send(Command::Keylock { deck: 0, on: true });
        rig.send(Command::Rate {
            deck: 0,
            value: 0.08,
        });
        rig.send(Command::Seek {
            deck: 0,
            seconds: 0.25,
        });
        rig.send(Command::Play { deck: 0, on: true });
        let heard = clicks(&record(&mut rig, 3.0));
        // Click k (at 0.5 k s of track) should sound when the playhead gets there.
        assert!(heard.len() >= 5, "{heard:?}");
        for h in &heard {
            let track_time = 0.25 + h * 1.08;
            let off = (track_time / 0.5 - (track_time / 0.5).round()) * 0.5 / 1.08;
            assert!(
                off.abs() < 0.008,
                "click at {h:.3}s is {:.1} ms off the playhead",
                off * 1000.0
            );
        }

        // After a jump the new place sounds straight away: no smear of the old one.
        rig.send(Command::Seek {
            deck: 0,
            seconds: 10.4,
        });
        let heard = clicks(&record(&mut rig, 0.5));
        let first = heard.first().copied().unwrap_or(f64::NAN);
        assert!(
            (first - 0.1 / 1.08).abs() < 0.008,
            "first click after the jump at {first:.3}s"
        );
    }

    #[test]
    fn sync_matches_a_stopped_deck_and_the_fader_picks_up_after() {
        let mut rig = Rig::new();
        rig.load(0, 120.0, 120.0);
        rig.load(1, 126.0, 126.0);
        rig.run(0.05);
        // Neither deck is playing: sync still matches deck 1's tempo to deck 0's.
        rig.send(Command::Sync { deck: 1, on: true });
        rig.run(0.05);
        let rate = rig.shared.decks[1].rate.get();
        assert!((126.0 * (1.0 + rate) - 120.0).abs() < 0.01, "rate {rate}");
        // Off again: the tempo stays matched, the fader (still at 0) isn't in control.
        rig.send(Command::Sync { deck: 1, on: false });
        rig.run(0.05);
        assert!((rig.shared.decks[1].rate.get() - rate).abs() < 1e-9);
        assert!(!rig.shared.decks[1].fader_live.load(Relaxed));

        // The tempo (-4.8 %) is below the fader (0); moving the fader up but not past
        // it does nothing.
        let at = TempoRange::Ten.fader(rate);
        rig.send(Command::TempoFader {
            deck: 1,
            position: 0.2,
        });
        rig.send(Command::TempoFader {
            deck: 1,
            position: at + 0.05,
        });
        rig.run(0.05);
        assert!(
            (rig.shared.decks[1].rate.get() - rate).abs() < 1e-9,
            "not picked up yet"
        );
        // Past it: picked up, and the fader moves the tempo from then on.
        rig.send(Command::TempoFader {
            deck: 1,
            position: at - 0.05,
        });
        rig.run(0.05);
        assert!(rig.shared.decks[1].fader_live.load(Relaxed));
        let want = TempoRange::Ten.rate(at - 0.05);
        assert!((rig.shared.decks[1].rate.get() - want).abs() < 1e-9);
        rig.send(Command::TempoFader {
            deck: 1,
            position: 0.3,
        });
        rig.run(0.05);
        assert!((rig.shared.decks[1].rate.get() - 0.03).abs() < 1e-9);
    }

    #[test]
    fn plays_from_before_the_start_on_the_grid() {
        let mut rig = Rig::new();
        let grid = rig.load(0, 120.0, 120.0);
        rig.run(0.01);
        // Two seconds early, snapped to the beat (cue points can go there too).
        rig.send(Command::Seek {
            deck: 0,
            seconds: -2.1,
        });
        rig.send(Command::Cue {
            deck: 0,
            press: Press::Down,
        });
        rig.send(Command::Cue {
            deck: 0,
            press: Press::Up,
        });
        rig.run(0.01);
        let cue = rig.shared.decks[0].cue_point.get();
        assert!(cue < -1.9, "cue at {cue}");
        assert!(
            (grid.beat_at(cue) - grid.beat_at(cue).round()).abs() < 1e-6,
            "on a beat"
        );
        rig.send(Command::Play { deck: 0, on: true });
        rig.run(0.5);
        assert!(
            rig.out.iter().all(|&s| s == 0.0),
            "silence before the track"
        );
        rig.run(2.0);
        assert!(rig.pos(0) > 0.0);
        assert!(rig.out.iter().any(|&s| s.abs() > 0.01), "then the track");
        // Never further back than the lead-in.
        rig.send(Command::Seek {
            deck: 0,
            seconds: -100.0,
        });
        rig.run(0.01);
        assert!(rig.pos(0) >= -LEAD_IN - 0.02);
    }

    #[test]
    fn loops_set_resize_and_end_while_playing() {
        let mut rig = Rig::new();
        let grid = rig.load(0, 120.0, 120.0);
        rig.send(Command::Seek { deck: 0, seconds: 10.2 });
        rig.send(Command::Play { deck: 0, on: true });
        rig.run(0.3);
        // A 4-beat (2 s) loop while playing: the playhead stays inside it.
        rig.send(Command::BeatLoop { deck: 0, beats: 4.0 });
        rig.run(0.05);
        let lin = rig.shared.decks[0].loop_in.get();
        assert!(rig.shared.decks[0].loop_active.load(Relaxed));
        rig.run(5.0);
        let pos = rig.pos(0);
        assert!(pos >= lin - 0.01 && pos < lin + 2.0 + 0.01, "pos {pos} loop from {lin}");
        // Another size resizes it from the same start.
        rig.send(Command::BeatLoop { deck: 0, beats: 2.0 });
        rig.run(3.0);
        assert!((rig.shared.decks[0].loop_out.get() - grid.time_at(grid.beat_at(lin) + 2.0)).abs() < 1e-6);
        assert!(rig.pos(0) < lin + 1.0 + 0.01);
        // The same size again ends it, and play carries on past the end.
        rig.send(Command::BeatLoop { deck: 0, beats: 2.0 });
        rig.run(2.0);
        assert!(!rig.shared.decks[0].loop_active.load(Relaxed));
        assert!(rig.pos(0) > lin + 1.0);
        // IN then OUT while playing makes a loop; reloop/exit ends it.
        rig.send(Command::LoopIn { deck: 0 });
        rig.run(1.0);
        rig.send(Command::LoopOut { deck: 0 });
        rig.run(0.05);
        assert!(rig.shared.decks[0].loop_active.load(Relaxed));
        rig.send(Command::ReloopToggle { deck: 0 });
        rig.run(0.05);
        assert!(!rig.shared.decks[0].loop_active.load(Relaxed));
    }

    #[test]
    fn plays_and_advances_in_real_time() {
        let mut rig = Rig::new();
        rig.load(0, 124.0, 124.0);
        rig.send(Command::Play { deck: 0, on: true });
        let start = rig.pos(0);
        rig.run(2.0);
        assert!((rig.pos(0) - start - 2.0).abs() < 0.01);
        assert!(rig.out.iter().any(|&s| s.abs() > 0.01), "makes sound");
    }

    #[test]
    fn beat_jump_moves_whole_beats() {
        let mut rig = Rig::new();
        let grid = rig.load(0, 120.0, 120.0);
        rig.run(0.01);
        let b0 = grid.beat_at(rig.pos(0));
        rig.send(Command::BeatJump {
            deck: 0,
            beats: 16.0,
        });
        rig.run(0.01);
        assert!((grid.beat_at(rig.pos(0)) - b0 - 16.0).abs() < 1e-6);
    }

    #[test]
    fn beat_loop_stays_inside_loop() {
        let mut rig = Rig::new();
        let grid = rig.load(0, 128.0, 128.0);
        rig.send(Command::Seek {
            deck: 0,
            seconds: 10.1,
        });
        rig.send(Command::Play { deck: 0, on: true });
        rig.send(Command::BeatLoop {
            deck: 0,
            beats: 4.0,
        });
        rig.run(0.01);
        let lin = rig.shared.decks[0].loop_in.get();
        let lout = rig.shared.decks[0].loop_out.get();
        assert!((grid.beat_at(lout) - grid.beat_at(lin) - 4.0).abs() < 1e-6);
        assert!(
            (grid.beat_at(lin) - grid.beat_at(lin).round()).abs() < 1e-6,
            "starts on a beat"
        );
        for _ in 0..50 {
            rig.run(0.1);
            let p = rig.pos(0);
            assert!(
                p >= lin - 1e-9 && p < lout + 1e-9,
                "{p} outside {lin}..{lout}"
            );
        }
    }

    #[test]
    fn sync_follows_a_drifting_leader() {
        let mut rig = Rig::new();
        let leader = rig.load(0, 120.0, 130.0); // drifts by 10 BPM over a minute
        let follower = rig.load(1, 125.0, 125.0);
        rig.send(Command::Play { deck: 0, on: true });
        rig.run(5.0);
        rig.send(Command::Sync { deck: 1, on: true });
        rig.send(Command::Play { deck: 1, on: true });
        rig.run(40.0);
        // Playing at the leader's local tempo…
        let lb = leader.bpm_at(rig.pos(0), 4) * rig.shared.decks[0].speed.get();
        let fb = follower.bpm_at(rig.pos(1), 4) * rig.shared.decks[1].speed.get();
        assert!((lb - fb).abs() < 0.3, "tempo {lb} vs {fb}");
        // …while showing the leader's one steady tempo.
        let (lr, fr) = (rig.shared.decks[0].bpm.get(), rig.shared.decks[1].bpm.get());
        assert!((lr - fr).abs() < 1e-9, "readout {lr} vs {fr}");
        let err = phase_error(leader.beat_at(rig.pos(0)), follower.beat_at(rig.pos(1)));
        assert!(err.abs() < 0.02, "phase error {err} beats");
    }

    #[test]
    fn sync_from_the_first_beat_snaps_forward() {
        let mut rig = Rig::new();
        let leader = rig.load(0, 124.0, 124.0);
        let follower = rig.load(1, 126.0, 126.0);
        rig.send(Command::Seek {
            deck: 0,
            seconds: 10.5,
        }); // leader 0.7 of the way through a beat
        rig.send(Command::Play { deck: 0, on: true });
        rig.send(Command::Sync { deck: 1, on: true });
        rig.send(Command::Play { deck: 1, on: true }); // follower still at its first beat
        rig.run(0.05);
        let err = phase_error(leader.beat_at(rig.pos(0)), follower.beat_at(rig.pos(1)));
        assert!(err.abs() < 0.01, "phase error {err} beats right after play");
    }

    #[test]
    fn cue_sets_returns_and_previews() {
        let mut rig = Rig::new();
        rig.load(0, 120.0, 120.0);
        rig.send(Command::Seek {
            deck: 0,
            seconds: 5.1,
        });
        rig.send(Command::Cue {
            deck: 0,
            press: Press::Down,
        }); // sets cue (quantised to 5.0)
        rig.send(Command::Cue {
            deck: 0,
            press: Press::Up,
        });
        rig.run(0.01);
        assert!((rig.shared.decks[0].cue_point.get() - 5.0).abs() < 1e-3);
        rig.send(Command::Cue {
            deck: 0,
            press: Press::Down,
        }); // preview while held
        rig.run(1.0);
        assert!(rig.shared.decks[0].playing.load(Relaxed));
        rig.send(Command::Cue {
            deck: 0,
            press: Press::Up,
        });
        rig.run(0.01);
        assert!(!rig.shared.decks[0].playing.load(Relaxed));
        assert!((rig.pos(0) - 5.0).abs() < 1e-3);
    }

    #[test]
    fn headphones_get_only_cued_decks() {
        let mut rig = Rig::new();
        rig.load(0, 120.0, 120.0);
        rig.send(Command::Play { deck: 0, on: true });
        rig.send(Command::Pfl { deck: 1, on: true });
        rig.run(0.5);
        let phones_peak = rig.out.chunks(4).map(|f| f[2].abs()).fold(0.0, f32::max);
        let master_peak = rig.out.chunks(4).map(|f| f[0].abs()).fold(0.0, f32::max);
        assert!(master_peak > 0.01 && phones_peak == 0.0);
    }

    #[test]
    fn sync_ignores_wobble_in_the_leaders_grid() {
        let mut rig = Rig::new();
        let follower = rig.load(1, 125.0, 125.0);
        // A 124 BPM leader whose analysed beats are each up to 8 ms off.
        let (audio, _) = generate(
            &DemoSpec {
                start_bpm: 124.0,
                end_bpm: 124.0,
                key: "8A".into(),
                seconds: 60.0,
                seed: 9,
            },
            SR,
        );
        let mut seed = 12345u64;
        let times: Vec<f64> = (0..120)
            .map(|i| {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let jitter = ((seed >> 33) as f64 / (1u64 << 31) as f64 - 0.5) * 0.016;
                i as f64 * 60.0 / 124.0 + 0.1 + jitter
            })
            .collect();
        let positions = (0..120).map(|i| (i % 4 + 1) as u8).collect();
        let leader = Arc::new(Beatgrid::new(times, positions).unwrap());
        rig.send(Command::Load {
            deck: 0,
            audio: Arc::new(audio),
            grid: Some(leader.clone()),
            generation: 2,
            hotcues: [None; HOTCUES],
        });
        rig.send(Command::Play { deck: 0, on: true });
        rig.run(2.0);
        rig.send(Command::Sync { deck: 1, on: true });
        rig.send(Command::Play { deck: 1, on: true });
        rig.run(5.0); // settle
        let (mut lo, mut hi) = (f64::MAX, f64::MIN);
        let (mut slo, mut shi) = (f64::MAX, f64::MIN);
        for _ in 0..400 {
            rig.run(0.05);
            let bpm = rig.shared.decks[1].bpm.get();
            let speed = rig.shared.decks[1].speed.get();
            (lo, hi) = (lo.min(bpm), hi.max(bpm));
            (slo, shi) = (slo.min(speed), shi.max(speed));
        }
        assert!(hi - lo < 0.05, "BPM readout wanders {lo:.3}..{hi:.3}");
        assert!(shi / slo - 1.0 < 0.004, "speed wanders {slo:.4}..{shi:.4}");
        let err = phase_error(leader.beat_at(rig.pos(0)), follower.beat_at(rig.pos(1)));
        assert!(err.abs() < 0.03, "phase error {err} beats");
    }

    /// A paused synced deck must hold still while the leader plays (it used to swing
    /// its speed with every leader beat, and its waveform pulsed), and a playing one
    /// must show a steady tempo while it corrects phase.
    #[test]
    fn synced_follower_tempo_is_steady() {
        let mut rig = Rig::new();
        rig.load(0, 124.0, 124.0);
        rig.load(1, 126.0, 126.0);
        rig.send(Command::Seek {
            deck: 1,
            seconds: 10.3, // off the leader's phase, so playing needs correcting
        });
        rig.send(Command::Play { deck: 0, on: true });
        rig.send(Command::Sync { deck: 1, on: true });
        let range = |rig: &mut Rig, f: fn(&DeckShared) -> f64| {
            let (mut lo, mut hi) = (f64::MAX, f64::MIN);
            for _ in 0..100 {
                rig.run(0.01);
                let v = f(&rig.shared.decks[1]);
                (lo, hi) = (lo.min(v), hi.max(v));
            }
            hi - lo
        };
        rig.run(0.5);
        let r = range(&mut rig, |d| d.speed.get());
        // (It used to swing by up to 4 %; what's left is beat times rounded to samples.)
        assert!(r < 1e-5, "paused speed moves by {r}");
        assert!(
            range(&mut rig, |d| d.tempo.get()) < 1e-5,
            "paused tempo moves"
        );
        assert!((rig.shared.decks[1].tempo.get() - 124.0 / 126.0).abs() < 1e-5);
        rig.send(Command::Play { deck: 1, on: true });
        rig.send(Command::Nudge {
            deck: 1,
            amount: 0.01,
        });
        rig.run(0.2);
        assert!(
            range(&mut rig, |d| d.tempo.get()) < 1e-5,
            "playing tempo moves"
        );
    }

    fn peaks(out: &[f32], channels: usize) -> Vec<f32> {
        (0..channels)
            .map(|c| out.chunks(channels).map(|f| f[c].abs()).fold(0.0, f32::max))
            .collect()
    }

    #[test]
    fn preview_plays_on_headphones_only() {
        let mut rig = Rig::new();
        let (audio, _) = generate(
            &DemoSpec {
                start_bpm: 124.0,
                end_bpm: 124.0,
                key: "8A".into(),
                seconds: 10.0,
                seed: 5,
            },
            SR,
        );
        rig.send(Command::Preview {
            audio: Arc::new(audio),
            start: 1.0,
            gain: 1.0,
        });
        rig.run(0.5);
        let p = peaks(&rig.out, 4);
        assert!(p[0] == 0.0 && p[1] == 0.0, "master silent: {p:?}");
        assert!(p[2] > 0.01 && p[3] > 0.01, "phones play: {p:?}");
        assert!(rig.shared.preview_playing.load(Relaxed));
        assert!((rig.shared.preview_position.get() - 1.5).abs() < 0.02);
        rig.send(Command::PreviewPlay(false));
        rig.run(0.1);
        assert_eq!(peaks(&rig.out, 4)[2], 0.0, "paused");
        assert!(
            (rig.shared.preview_position.get() - 1.5).abs() < 0.02,
            "keeps its place"
        );
        rig.send(Command::PreviewSeek(0.25));
        rig.send(Command::PreviewPlay(true));
        rig.run(0.25);
        assert!(peaks(&rig.out, 4)[2] > 0.01, "resumed");
        assert!(
            (rig.shared.preview_position.get() - 0.5).abs() < 0.02,
            "from the seek"
        );
        rig.send(Command::PreviewStop);
        rig.run(0.1);
        assert_eq!(peaks(&rig.out, 4)[2], 0.0);
        assert!(!rig.shared.preview_playing.load(Relaxed));
    }

    #[test]
    fn meters_follow_gain_and_eq() {
        let mut rig = Rig::new();
        rig.load(0, 120.0, 120.0);
        rig.send(Command::Play { deck: 0, on: true });
        rig.run(0.5);
        let flat = rig.shared.decks[0].vu.get();
        assert!(flat > 0.01, "meter moves: {flat}");
        assert!(rig.shared.vu_main[0].get() > 0.01);
        // Killing all three bands silences the channel; its meter falls away.
        for band in [EQ_LOW, EQ_MID, EQ_HIGH] {
            rig.send(Command::Eq {
                deck: 0,
                band,
                value: 0.0,
            });
        }
        rig.run(3.0);
        assert!(rig.shared.decks[0].vu.get() < flat * 0.01);
        assert!(peaks(&rig.out, 4)[0] < 0.001);
        assert_eq!(rig.shared.decks[0].eq[EQ_MID].get(), 0.0);
    }

    #[test]
    fn main_gain_balance_and_head_mix() {
        let mut rig = Rig::new();
        rig.load(0, 120.0, 120.0);
        rig.send(Command::Play { deck: 0, on: true });
        rig.send(Command::Pfl { deck: 1, on: true }); // cue bus silent
        rig.send(Command::Balance { value: 1.0 }); // right only
        rig.run(0.5);
        let p = peaks(&rig.out, 4);
        assert!(p[0] == 0.0 && p[1] > 0.01, "balance: {p:?}");
        assert_eq!(p[2], 0.0, "headphones hear only the (silent) cue bus");
        // Head mix fully to main: the headphones hear the main mix.
        rig.send(Command::HeadMix { value: 1.0 });
        rig.run(0.2);
        assert!(peaks(&rig.out, 4)[2] > 0.01);
        // Main gain 0 silences the main output but not the headphones.
        rig.send(Command::MainGain { value: 0.0 });
        rig.run(0.2);
        let p = peaks(&rig.out, 4);
        assert!(p[0] == 0.0 && p[1] == 0.0 && p[2] > 0.01, "{p:?}");
        assert_eq!(rig.shared.main_gain.get(), 0.0);
    }

    #[test]
    fn routing_moves_and_splits_buses() {
        let mut rig = Rig::new();
        rig.load(0, 120.0, 120.0);
        rig.send(Command::Play { deck: 0, on: true });
        rig.send(Command::Pfl { deck: 1, on: true }); // headphones hear nothing
        // A laptop's one stereo pair, split: master on the right, headphones on the left.
        rig.send(Command::SetRouting(Routing {
            master: Out::Mono(1),
            phones: Out::Mono(0),
        }));
        let mut out = vec![0.0; 256 * 2];
        for _ in 0..80 {
            rig.r.process(&mut out, 2);
        }
        let p = peaks(&out, 2);
        assert!(p[0] == 0.0 && p[1] > 0.01, "{p:?}");
        // Off means silent; a pair the device lacks is skipped.
        rig.send(Command::SetRouting(Routing {
            master: Out::Stereo(2),
            phones: Out::Off,
        }));
        for _ in 0..4 {
            rig.r.process(&mut out, 2);
        }
        assert_eq!(peaks(&out, 2), vec![0.0, 0.0]);
    }
}
