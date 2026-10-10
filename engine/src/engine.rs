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

pub const DECKS: usize = 2;
pub const HOTCUES: usize = 8;
/// Largest block rendered in one go; bigger device buffers are rendered in chunks.
pub const MAX_BLOCK: usize = 4096;
/// Pitch fader range, as a fraction (±8 %, the CDJ default).
pub const RATE_RANGE: f64 = 0.08;

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
    /// Current playback speed ratio (1.0 = original tempo), including sync and nudge.
    pub speed: AtomicF64,
    /// Pitch fader position as a fraction, e.g. 0.02 = +2 %.
    pub rate: AtomicF64,
    /// The track's tempo (its grid's mean) × the tempo fader. NaN without a grid.
    pub bpm: AtomicF64,
    pub sync: AtomicBool,
    pub loop_in: AtomicF64,
    pub loop_out: AtomicF64,
    pub loop_active: AtomicBool,
    pub cue_point: AtomicF64,
    pub hotcues: [AtomicF64; HOTCUES],
    pub volume: AtomicF64,
    pub pfl: AtomicBool,
    /// Bumped on every load so readers can tell tracks apart.
    pub generation: AtomicU64,
}

#[derive(Default)]
pub struct Shared {
    pub decks: [DeckShared; DECKS],
    pub crossfader: AtomicF64,
    pub quantize: AtomicBool,
    pub callbacks: AtomicU64,
    /// Callbacks that took more than 80 % of their time budget.
    pub slow_callbacks: AtomicU64,
    /// Xruns reported by the audio backend.
    pub xruns: AtomicU64,
    /// Worst callback duration since the last [`Shared::take_max_callback_ns`].
    pub max_callback_ns: AtomicU64,
    pub last_block_frames: AtomicU32,
    pub sample_rate: AtomicU32,
}

impl Shared {
    pub fn take_max_callback_ns(&self) -> u64 {
        self.max_callback_ns.swap(0, Relaxed)
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
    /// Pitch fader, as a fraction (-RATE_RANGE..=RATE_RANGE).
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
    Crossfader {
        value: f64,
    },
    Quantize {
        on: bool,
    },
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
    nudge: f64,
    sync: bool,
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
}

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
            nudge: 0.0,
            sync: false,
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

    fn local_bpm(&self) -> Option<f64> {
        self.grid.as_ref().map(|g| g.bpm_at(self.pos, 4))
    }

    fn snap(&self, t: f64, quantize: bool) -> f64 {
        match (&self.grid, quantize) {
            (Some(g), true) => g.quantize(t),
            _ => t,
        }
    }

    /// Start of the beat the playhead is in (with a little tolerance for "just before").
    fn current_beat_start(&self) -> f64 {
        self.time_at((self.beat_at(self.pos) + 0.05).floor())
    }

    fn seek(&mut self, t: f64) {
        self.pos = t.clamp(0.0, self.duration());
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
    decks: [Deck; DECKS],
    crossfader: f64,
    quantize: bool,
    sample_rate: f64,
    /// Per-deck stereo scratch, master and headphone buses (interleaved stereo).
    deck_buf: [Vec<f32>; DECKS],
    master: Vec<f32>,
    phones: Vec<f32>,
}

impl Renderer {
    pub fn new(
        commands: rtrb::Consumer<Command>,
        garbage: rtrb::Producer<Garbage>,
        shared: Arc<Shared>,
        sample_rate: u32,
    ) -> Self {
        shared.sample_rate.store(sample_rate, Relaxed);
        shared.quantize.store(true, Relaxed);
        Renderer {
            commands,
            garbage,
            shared,
            decks: [Deck::new(), Deck::new()],
            crossfader: 0.0,
            quantize: true,
            sample_rate: sample_rate as f64,
            deck_buf: [vec![0.0; MAX_BLOCK * 2], vec![0.0; MAX_BLOCK * 2]],
            master: vec![0.0; MAX_BLOCK * 2],
            phones: vec![0.0; MAX_BLOCK * 2],
        }
    }

    /// Render into an interleaved device buffer with `channels` channels. Master goes to
    /// channels 1–2 and headphones to 3–4 when the device has them (DDJ-400 layout).
    pub fn process(&mut self, out: &mut [f32], channels: usize) {
        let started = Instant::now();
        let channels = channels.max(1);
        let total = out.len() / channels;
        let mut done = 0;
        while done < total {
            let n = (total - done).min(MAX_BLOCK);
            self.render_block(n);
            let chunk = &mut out[done * channels..(done + n) * channels];
            for (f, frame) in chunk.chunks_exact_mut(channels).enumerate() {
                let (ml, mr) = (self.master[2 * f], self.master[2 * f + 1]);
                match channels {
                    1 => frame[0] = 0.5 * (ml + mr),
                    2 | 3 => {
                        frame[0] = ml;
                        frame[1] = mr;
                    }
                    _ => {
                        frame[0] = ml;
                        frame[1] = mr;
                        frame[2] = self.phones[2 * f];
                        frame[3] = self.phones[2 * f + 1];
                    }
                }
                for s in frame.iter_mut().skip(4) {
                    *s = 0.0;
                }
                if channels == 3 {
                    frame[2] = 0.0;
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
        self.update_speeds();

        let sr = self.sample_rate;
        for d in 0..DECKS {
            let buf = &mut self.deck_buf[d][..n * 2];
            buf.fill(0.0);
            let deck = &mut self.decks[d];
            let Some(audio) = deck.audio.as_deref() else {
                continue;
            };
            if !deck.playing {
                continue;
            }
            let src_rate = audio.sample_rate as f64;
            let step = deck.speed / sr;
            let duration = audio.duration();
            let looping = deck.loop_active && deck.loop_in.is_some() && deck.loop_out.is_some();
            let (lin, lout) = (
                deck.loop_in.unwrap_or(0.0),
                deck.loop_out.unwrap_or(f64::MAX),
            );
            for f in 0..n {
                let (l, r) = sample_at(audio, deck.pos * src_rate);
                buf[2 * f] = l;
                buf[2 * f + 1] = r;
                deck.pos += step;
                if looping && deck.pos >= lout && lout > lin {
                    deck.pos -= lout - lin;
                }
                if deck.pos >= duration {
                    deck.pos = duration;
                    deck.playing = false;
                    deck.preview = Preview::None;
                    break;
                }
            }
        }

        // Constant-power crossfader.
        let x = (self.crossfader.clamp(-1.0, 1.0) + 1.0) * std::f64::consts::FRAC_PI_4;
        let xf = [x.cos() as f32, x.sin() as f32];
        let any_pfl = self.decks.iter().any(|d| d.pfl);
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
            let m = (0.8 * m).clamp(-1.0, 1.0);
            self.master[f] = m;
            self.phones[f] = if any_pfl {
                (0.8 * p).clamp(-1.0, 1.0)
            } else {
                m
            };
        }
    }

    /// Synced decks follow the other playing deck's tempo and beat phase, block by block,
    /// so they keep up when the leader's tempo drifts.
    fn update_speeds(&mut self) {
        for d in 0..DECKS {
            self.decks[d].speed = self.decks[d].base_speed();
        }
        for d in 0..DECKS {
            let o = 1 - d;
            let (me, other) = (&self.decks[d], &self.decks[o]);
            if !me.sync || other.audio.is_none() || !other.playing {
                continue;
            }
            let (Some(my_bpm), Some(leader_bpm)) = (me.local_bpm(), other.local_bpm()) else {
                continue;
            };
            let target = leader_bpm * other.speed / my_bpm;
            let err = phase_error(other.beat_at(other.pos), me.beat_at(me.pos));
            let correction = (err * 0.1).clamp(-0.02, 0.02);
            self.decks[d].speed = target * (1.0 + correction) * (1.0 + me.nudge);
            self.decks[d].rate = target - 1.0;
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
        let mut beat = me.beat_at(me.pos) + err;
        // At the very start of a track the matching phase may lie before 0:00; take
        // the next matching beat instead.
        if me.time_at(beat) < 0.0 {
            beat += 1.0;
        }
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
        let q = self.quantize;
        match cmd {
            Command::Load {
                deck,
                audio,
                grid,
                generation,
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
                    sync: dk.sync,
                    volume: dk.volume,
                    pfl: dk.pfl,
                    generation,
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
                            dk.cue_point = dk.snap(dk.pos, q);
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
            Command::BeatLoop { deck, beats } => {
                let dk = &mut self.decks[deck];
                if dk.loop_active {
                    dk.loop_active = false;
                    return;
                }
                let start = if q { dk.current_beat_start() } else { dk.pos };
                dk.loop_in = Some(start);
                dk.loop_out = Some(dk.time_at(dk.beat_at(start) + beats));
                dk.loop_active = true;
            }
            Command::LoopIn { deck } => {
                let dk = &mut self.decks[deck];
                dk.loop_in = Some(dk.snap(dk.pos, q));
                dk.loop_out = None;
                dk.loop_active = false;
            }
            Command::LoopOut { deck } => {
                let dk = &mut self.decks[deck];
                let out = dk.snap(dk.pos, q);
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
                    (Press::Down, None) => dk.hotcues[slot] = Some(dk.snap(dk.pos, q)),
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
                dk.rate = value.clamp(-1.0, 1.0);
            }
            Command::Nudge { deck, amount } => self.decks[deck].nudge = amount,
            Command::Sync { deck, on } => {
                self.decks[deck].sync = on;
                if on && self.decks[deck].playing {
                    self.snap_phase(deck);
                }
            }
            Command::Volume { deck, value } => {
                self.decks[deck].volume = value.clamp(0.0, 1.0) as f32
            }
            Command::Pfl { deck, on } => self.decks[deck].pfl = on,
            Command::Crossfader { value } => self.crossfader = value,
            Command::Quantize { on } => self.quantize = on,
        }
    }

    fn publish(&self) {
        let s = &self.shared;
        s.crossfader.set(self.crossfader);
        s.quantize.store(self.quantize, Relaxed);
        for (dk, sh) in self.decks.iter().zip(&s.decks) {
            sh.position.set(dk.pos);
            sh.duration.set(dk.duration());
            sh.playing.store(dk.playing, Relaxed);
            sh.speed.set(dk.speed);
            sh.rate.set(dk.rate);
            // The track's one tempo times the tempo fader: steady while playing, and
            // unmoved by nudges and sync's small phase corrections. A synced deck's
            // `rate` is set to match the leader, so it shows the leader's tempo.
            sh.bpm.set(
                dk.grid
                    .as_ref()
                    .map_or(f64::NAN, |g| g.mean_bpm() * (1.0 + dk.rate)),
            );
            sh.sync.store(dk.sync, Relaxed);
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
        let lb = leader.bpm_at(rig.pos(0), 4) * rig.shared.decks[0].speed.get();
        let fb = rig.shared.decks[1].bpm.get();
        assert!((lb - fb).abs() < 0.2, "tempo {lb} vs {fb}");
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
}
