//! The real-time part: decks, mixer and the render loop that runs on the audio thread.
//!
//! Rules for everything reachable from [`Renderer::process`]: no allocation, no locks,
//! no frees. Commands arrive on an `rtrb` ring buffer; replaced track data leaves on
//! another ring buffer to be dropped elsewhere; events go out on a third; state is
//! published as one [`EngineSnapshot`] per callback through a triple buffer.

use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Instant;

use crate::beatgrid::Beatgrid;
use crate::command::{
    ChannelParam, Command, DeckCmd, DeckId, DeckState, EngineState, HOTCUES, LoadedTrack,
    LoopState, MAX_DECKS, MasterParam, ParamId, PreviewCmd, Press, SLOTS, XfSide,
};
use crate::decode::TrackAudio;
use crate::dsp::{self, Limiter, Smoothed};
use crate::processor::{Channel, KNOB_SECS, MasterChain};
use crate::snapshot::{
    DeckSnap, EngineEvent, EngineSnapshot, MixerSnap, Monitor, PreviewSnap, Stats,
};
use crate::tempo::TempoRange;
use crate::transport::{JumpReason, LEAD_IN, Player};

/// Largest block rendered in one go; bigger device buffers are rendered in chunks.
pub const MAX_BLOCK: usize = 4096;
/// Seconds for a level meter to fall back by a factor of e.
const VU_FALL_SECS: f64 = 0.25;
const COMMAND_SLOTS: usize = 1024;
const EVENT_SLOTS: usize = 256;
const GARBAGE_SLOTS: usize = 64;

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

// ---- wiring --------------------------------------------------------------------------

/// Things the audio thread must not free itself.
pub enum Garbage {
    Track(Arc<LoadedTrack>),
    Grid(Arc<Beatgrid>),
    Audio(Arc<TrackAudio>),
    State(Box<EngineState>),
}

/// The app's ends of the engine's rings.
pub(crate) struct AppEnds {
    pub commands: rtrb::Producer<Command>,
    pub events: rtrb::Consumer<EngineEvent>,
    pub garbage: rtrb::Consumer<Garbage>,
    pub monitor: Monitor,
}

/// The audio thread's ends, handed to [`Renderer::new`].
pub(crate) struct RendererEnds {
    commands: rtrb::Consumer<Command>,
    events: rtrb::Producer<EngineEvent>,
    garbage: rtrb::Producer<Garbage>,
    snapshots: triple_buffer::Input<EngineSnapshot>,
    stats: Arc<Stats>,
}

impl RendererEnds {
    pub fn stats(&self) -> &Arc<Stats> {
        &self.stats
    }
}

/// Make the rings and the snapshot buffer between the app and a renderer.
pub(crate) fn wire() -> (AppEnds, RendererEnds) {
    let (commands, command_rx) = rtrb::RingBuffer::new(COMMAND_SLOTS);
    let (event_tx, events) = rtrb::RingBuffer::new(EVENT_SLOTS);
    let (garbage_tx, garbage) = rtrb::RingBuffer::new(GARBAGE_SLOTS);
    let (snap_in, snap_out) = triple_buffer::triple_buffer(&EngineSnapshot::default());
    let stats = Arc::new(Stats::default());
    (
        AppEnds {
            commands,
            events,
            garbage,
            monitor: Monitor::new(snap_out, stats.clone()),
        },
        RendererEnds {
            commands: command_rx,
            events: event_tx,
            garbage: garbage_tx,
            snapshots: snap_in,
            stats,
        },
    )
}

/// Events on their way out. When the app falls behind they're counted and dropped.
struct Outbox {
    tx: rtrb::Producer<EngineEvent>,
    stats: Arc<Stats>,
}

impl Outbox {
    fn send(&mut self, e: EngineEvent) {
        if self.tx.push(e).is_err() {
            self.stats.dropped_events.fetch_add(1, Relaxed);
        }
    }
}

// ---- deck --------------------------------------------------------------------------

/// Holding cue or a hot cue to play from it, and back on release.
#[derive(Clone, Copy, PartialEq)]
enum Preview {
    None,
    Cue,
    Hotcue(usize),
}

struct Deck {
    id: DeckId,
    track: Option<Arc<LoadedTrack>>,
    /// The loaded track's grid, or an edited one ([`Command::SetGrid`]).
    grid: Option<Arc<Beatgrid>>,
    player: Player,
    rate: f64,
    range: TempoRange,
    /// The tempo fader's position and whether it's in control: see
    /// [`DeckSnap::fader`].
    fader: f64,
    fader_live: bool,
    nudge: f64,
    sync: bool,
    quantize: bool,
    cue_point: f64,
    preview: Preview,
    hotcues: [Option<f64>; HOTCUES],
    /// While synced: the smoothed tempo ratio to the leader, and the smoothed phase
    /// correction on top of it. 0 = not following yet.
    sync_ratio: f64,
    sync_correction: f64,
    /// The deck it's following this block.
    following: Option<DeckId>,
    /// See [`DeckSnap::tempo`].
    tempo: f64,
    /// Playing at the last publish, to send Started/Stopped.
    was_playing: bool,
}

/// Sync follows the leader's tempo averaged over this many beats either side, so
/// beat-to-beat wobble in an analysed grid doesn't shake the follower.
const SYNC_TEMPO_BEATS: usize = 16;
/// Seconds for the followed tempo and the phase correction to settle.
const SYNC_TEMPO_SECS: f64 = 0.5;
const SYNC_PHASE_SECS: f64 = 0.25;
/// Phase errors smaller than this (beats; ~4 ms at 120 BPM) are left alone.
const SYNC_DEADBAND: f64 = 0.008;

/// The tempo fader picks the tempo up when it comes this close (in fader travel,
/// -1..1) or moves past it.
const PICKUP: f64 = 0.01;

/// Without a grid, beat maths falls back to 120 BPM from zero.
const FALLBACK_BEAT: f64 = 0.5;

impl Deck {
    /// `keylock`: give it a pitch shifter (sample rate, largest block).
    fn new(id: DeckId, keylock: Option<(u32, usize)>) -> Self {
        Deck {
            id,
            track: None,
            grid: None,
            player: Player::new(LEAD_IN, keylock),
            rate: 0.0,
            range: TempoRange::default(),
            fader: 0.0,
            fader_live: true,
            nudge: 0.0,
            sync: false,
            quantize: true,
            cue_point: 0.0,
            preview: Preview::None,
            hotcues: [None; HOTCUES],
            sync_ratio: 0.0,
            sync_correction: 0.0,
            following: None,
            tempo: 1.0,
            was_playing: false,
        }
    }

    fn generation(&self) -> u64 {
        self.track.as_ref().map_or(0, |t| t.generation)
    }

    fn pos(&self) -> f64 {
        self.player.transport.pos()
    }

    fn playing(&self) -> bool {
        self.player.transport.playing
    }

    fn jump(&mut self, to: f64, why: JumpReason) {
        self.player.transport.jump(to, why);
    }

    fn loop_(&mut self) -> &mut LoopState {
        &mut self.player.transport.loop_
    }

    fn duration(&self) -> f64 {
        self.player.transport.duration()
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
            .map(|g| g.bpm_at(self.pos(), SYNC_TEMPO_BEATS))
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
        self.time_at((self.beat_at(self.pos()) + 0.05).floor())
    }

    /// After the tempo moved without the fader: the fader is in control again only
    /// if it's where the tempo now is.
    fn check_pickup(&mut self) {
        self.fader_live = (self.range.fader(self.rate) - self.fader).abs() < PICKUP;
    }

    fn base_speed(&self) -> f64 {
        (1.0 + self.rate) * (1.0 + self.nudge)
    }

    /// Put `track` on the deck, stopped at its first beat. Tempo, sync, quantize and
    /// keylock stay as they were. Returns what it replaced, for the garbage ring.
    fn load(&mut self, track: Arc<LoadedTrack>) -> (Option<Arc<LoadedTrack>>, Option<Arc<Beatgrid>>) {
        let old_grid = std::mem::replace(&mut self.grid, track.grid.clone());
        let first_beat = self.grid.as_ref().map_or(0.0, |g| g.time_at(0.0).max(0.0));
        let t = &mut self.player.transport;
        t.set_duration(track.audio.duration());
        t.playing = false;
        t.loop_ = LoopState::default();
        t.jump(first_beat, JumpReason::Load);
        self.player.speed = 1.0;
        self.cue_point = track.cues.main.unwrap_or(first_beat);
        self.hotcues = track.cues.hot;
        self.nudge = 0.0;
        self.preview = Preview::None;
        self.sync_ratio = 0.0;
        self.sync_correction = 0.0;
        self.tempo = 1.0;
        (self.track.replace(track), old_grid)
    }

    /// Everything but the track and the channel from `s`.
    fn restore(&mut self, s: &DeckState) {
        let loaded = self.track.is_some();
        self.jump(s.position, JumpReason::Restore);
        self.player.transport.playing = loaded && s.playing;
        self.player.transport.loop_ = s.loop_;
        self.player.keylock = s.keylock;
        if let Some(c) = s.cues.main {
            self.cue_point = c;
        }
        self.hotcues = s.cues.hot;
        self.range = s.range;
        let (lo, hi) = s.range.bounds();
        self.rate = s.rate.clamp(lo, hi);
        self.fader = s.fader.clamp(-1.0, 1.0);
        self.fader_live = s.fader_live;
        self.sync = s.sync;
        self.quantize = s.quantize;
    }

    fn event_loop(&mut self, out: &mut Outbox) {
        out.send(EngineEvent::LoopSet {
            deck: self.id,
            generation: self.generation(),
            loop_: self.player.transport.loop_,
        });
    }

    /// The commands that only need this deck. (Play and Sync need the others; see
    /// [`Renderer::deck_cmd`].)
    fn apply(&mut self, cmd: DeckCmd, out: &mut Outbox) {
        let generation = self.generation();
        match cmd {
            DeckCmd::Cue(press) => {
                if self.track.is_none() {
                    return;
                }
                let playing = self.playing();
                match press {
                    Press::Down if playing && self.preview == Preview::None => {
                        self.player.transport.playing = false;
                        self.jump(self.cue_point, JumpReason::Cue);
                    }
                    Press::Down if !playing => {
                        if (self.pos() - self.cue_point).abs() < 0.005 {
                            self.preview = Preview::Cue;
                            self.player.transport.playing = true;
                        } else {
                            self.cue_point = self.snap(self.pos());
                            self.jump(self.cue_point, JumpReason::Cue);
                            out.send(EngineEvent::CueSet {
                                deck: self.id,
                                generation,
                                position: self.cue_point,
                            });
                        }
                    }
                    Press::Up if self.preview == Preview::Cue => {
                        self.player.transport.playing = false;
                        self.preview = Preview::None;
                        self.jump(self.cue_point, JumpReason::Cue);
                    }
                    _ => {}
                }
            }
            DeckCmd::Seek(t) => self.jump(t, JumpReason::Seek),
            DeckCmd::SetCue(at) => {
                self.cue_point = at.unwrap_or_else(|| self.snap(self.pos())).max(-LEAD_IN);
                out.send(EngineEvent::CueSet {
                    deck: self.id,
                    generation,
                    position: self.cue_point,
                });
            }
            DeckCmd::SetLoop { start, end } => {
                if end > start {
                    *self.loop_() = LoopState {
                        start: Some(start),
                        end: Some(end),
                        active: true,
                    };
                    self.jump(start, JumpReason::Loop);
                    self.event_loop(out);
                }
            }
            DeckCmd::BeatJump(beats) => {
                let target = self.time_at(self.beat_at(self.pos()) + beats);
                let l = self.player.transport.loop_;
                if l.active
                    && let (Some(i), Some(o)) = (l.start, l.end)
                {
                    let start = Some(self.time_at(self.beat_at(i) + beats));
                    let end = Some(self.time_at(self.beat_at(o) + beats));
                    *self.loop_() = LoopState { start, end, ..l };
                    self.event_loop(out);
                }
                self.jump(target, JumpReason::BeatJump);
            }
            DeckCmd::BeatLoop(beats) => {
                let l = self.player.transport.loop_;
                if l.active
                    && let (Some(lin), Some(lout)) = (l.start, l.end)
                {
                    // The same size again ends the loop; another size resizes it from
                    // the same start.
                    let size = self.beat_at(lout) - self.beat_at(lin);
                    if (size - beats).abs() < 1e-6 {
                        self.loop_().active = false;
                    } else {
                        let end = self.time_at(self.beat_at(lin) + beats);
                        self.loop_().end = Some(end);
                        if self.pos() >= end {
                            self.jump(lin, JumpReason::Loop);
                        }
                    }
                } else if l.active {
                    self.loop_().active = false;
                } else {
                    let start = if self.quantize {
                        self.current_beat_start()
                    } else {
                        self.pos()
                    };
                    let end = self.time_at(self.beat_at(start) + beats);
                    *self.loop_() = LoopState {
                        start: Some(start),
                        end: Some(end),
                        active: true,
                    };
                }
                self.event_loop(out);
            }
            DeckCmd::LoopIn => {
                let start = Some(self.snap(self.pos()));
                *self.loop_() = LoopState {
                    start,
                    end: None,
                    active: false,
                };
                self.event_loop(out);
            }
            DeckCmd::LoopOut => {
                let end = self.snap(self.pos());
                if let Some(i) = self.player.transport.loop_.start
                    && end > i
                {
                    let l = self.loop_();
                    l.end = Some(end);
                    l.active = true;
                    self.event_loop(out);
                }
            }
            DeckCmd::Reloop(set) => {
                let l = self.player.transport.loop_;
                let on = set.apply(l.active);
                if on == l.active {
                    return;
                }
                if !on {
                    self.loop_().active = false;
                } else if let (Some(i), Some(o)) = (l.start, l.end) {
                    self.loop_().active = true;
                    if self.pos() > o {
                        self.jump(i, JumpReason::Loop);
                    }
                } else {
                    return;
                }
                self.event_loop(out);
            }
            DeckCmd::LoopScale(factor) => {
                let l = self.player.transport.loop_;
                if let (Some(i), Some(o)) = (l.start, l.end) {
                    let bi = self.beat_at(i);
                    let len = (self.beat_at(o) - bi) * factor;
                    if len > 1.0 / 64.0 {
                        let new_out = self.time_at(bi + len);
                        self.loop_().end = Some(new_out);
                        let pos = self.pos();
                        if l.active && pos >= new_out {
                            self.jump(i + (pos - i) % (new_out - i), JumpReason::Loop);
                        }
                        self.event_loop(out);
                    }
                }
            }
            DeckCmd::Hotcue { slot, press } => {
                let slot = slot as usize;
                if self.track.is_none() || slot >= HOTCUES {
                    return;
                }
                match (press, self.hotcues[slot]) {
                    (Press::Down, None) => {
                        let t = self.snap(self.pos());
                        self.hotcues[slot] = Some(t);
                        out.send(EngineEvent::HotcueSet {
                            deck: self.id,
                            generation,
                            slot: slot as u8,
                            position: Some(t),
                        });
                    }
                    (Press::Down, Some(t)) => {
                        self.jump(t, JumpReason::Hotcue);
                        if !self.playing() {
                            self.player.transport.playing = true;
                            self.preview = Preview::Hotcue(slot);
                        }
                    }
                    (Press::Up, Some(t)) if self.preview == Preview::Hotcue(slot) => {
                        self.player.transport.playing = false;
                        self.preview = Preview::None;
                        self.jump(t, JumpReason::Hotcue);
                    }
                    _ => {}
                }
            }
            DeckCmd::HotcueClear(slot) => {
                if let Some(h) = self.hotcues.get_mut(slot as usize)
                    && h.take().is_some()
                {
                    out.send(EngineEvent::HotcueSet {
                        deck: self.id,
                        generation,
                        slot,
                        position: None,
                    });
                }
            }
            DeckCmd::Rate(value) => {
                let (lo, hi) = self.range.bounds();
                self.rate = value.clamp(lo, hi);
                self.check_pickup();
            }
            DeckCmd::TempoFader(position) => {
                let target = self.range.fader(self.rate);
                let was = self.fader;
                self.fader = position.clamp(-1.0, 1.0);
                if self.sync {
                    self.check_pickup();
                } else {
                    let crossed = (was - target) * (self.fader - target) <= 0.0;
                    if crossed || (self.fader - target).abs() < PICKUP {
                        self.fader_live = true;
                    }
                    if self.fader_live {
                        self.rate = self.range.rate(self.fader);
                    }
                }
            }
            DeckCmd::TempoRange(range) => self.set_range(range),
            DeckCmd::TempoRangeNext => self.set_range(self.range.next()),
            DeckCmd::Nudge(amount) => self.nudge = amount,
            DeckCmd::Quantize(set) => self.quantize = set.apply(self.quantize),
            DeckCmd::Keylock(set) => self.player.keylock = set.apply(self.player.keylock),
            // Handled by the renderer.
            DeckCmd::Play(_)
            | DeckCmd::Sync(_)
            | DeckCmd::Pfl(_)
            | DeckCmd::Crossfader(_) => {}
        }
    }

    fn set_range(&mut self, range: TempoRange) {
        let (lo, hi) = range.bounds();
        self.range = range;
        self.rate = self.rate.clamp(lo, hi);
        self.check_pickup();
    }

    fn snapshot(&self, ch: &Channel) -> DeckSnap {
        let l = self.player.transport.loop_;
        DeckSnap {
            track_id: self.track.as_ref().map(|t| t.track_id),
            generation: self.generation(),
            position: self.pos(),
            duration: self.duration(),
            playing: self.playing(),
            speed: self.player.speed,
            tempo: self.tempo,
            rate: self.rate,
            range: self.range,
            fader: self.fader,
            fader_live: self.fader_live,
            // The track's one tempo times the tempo fader: steady while playing, and
            // unmoved by nudges and sync's small phase corrections. A synced deck's
            // `rate` is set to match the leader, so it shows the leader's tempo.
            bpm: self.grid.as_ref().map(|g| g.mean_bpm() * (1.0 + self.rate)),
            sync: self.sync,
            following: self.following,
            quantize: self.quantize,
            keylock: self.player.keylock,
            loop_in: l.start,
            loop_out: l.end,
            loop_active: l.active,
            cue_point: self.cue_point,
            hotcues: self.hotcues,
            volume: ch.get(ChannelParam::Volume),
            pfl: ch.pfl,
            pregain: ch.get(ChannelParam::Trim),
            auto_gain: ch.auto_gain(),
            eq: [0, 1, 2].map(|b| ch.get(ChannelParam::Eq(b))),
            filter: ch.get(ChannelParam::Filter),
            crossfader: ch.crossfader,
            vu: ch.vu as f64,
        }
    }
}

// ---- renderer (audio thread) -------------------------------------------------------

pub struct Renderer {
    commands: rtrb::Consumer<Command>,
    garbage: rtrb::Producer<Garbage>,
    outbox: Outbox,
    snapshots: triple_buffer::Input<EngineSnapshot>,
    stats: Arc<Stats>,
    callbacks: u64,
    last_block_frames: u32,
    /// All [`MAX_DECKS`] exist, then Prepare's player ([`DeckId::PREPARE`]), so a
    /// [`DeckId`] can always index them. Only the first `live` decks and Prepare's
    /// player are rendered and take commands.
    decks: [Deck; SLOTS],
    channels: [Channel; SLOTS],
    live: usize,
    /// The deck synced decks follow, if the app picked one.
    sync_leader: Option<DeckId>,
    sample_rate: f64,
    /// Per-deck stereo scratch, master and headphone buses (interleaved stereo).
    deck_buf: [Vec<f32>; SLOTS],
    master: Vec<f32>,
    phones: Vec<f32>,
    preview_buf: Vec<f32>,
    routing: Routing,
    preview: Option<Arc<TrackAudio>>,
    preview_player: Player,
    preview_gain: f32,
    master_chain: MasterChain,
    crossfader: f64,
    /// Left and right crossfader gains, eased.
    xf_gain: [Smoothed; 2],
    head_gain: Smoothed,
    head_mix: Smoothed,
    limiters: [Limiter; 2],
    limiting: f64,
    vu_main: [f32; 2],
    /// Per-sample easing for faders; see [`KNOB_SECS`].
    knob_ease: f64,
}

/// Prepare mode's slot, after the decks.
const PREP: usize = MAX_DECKS;

/// The slots in use: the first `live` decks, then Prepare's player.
fn slots(live: usize) -> impl Iterator<Item = usize> {
    (0..live).chain(std::iter::once(PREP))
}

/// A meter that jumps up to a block's peak and falls back slowly.
fn meter(level: f32, peak: f32, secs: f64) -> f32 {
    peak.max(level * (-secs / VU_FALL_SECS).exp() as f32)
}

impl Renderer {
    /// `decks`: how many decks are live (at most [`MAX_DECKS`]).
    pub(crate) fn new(ends: RendererEnds, sample_rate: u32, decks: usize) -> Self {
        let live = decks.min(MAX_DECKS);
        let sr = sample_rate as f64;
        let id = |i: usize| DeckId::new(i).unwrap_or(DeckId::PREPARE);
        let mut r = Renderer {
            commands: ends.commands,
            garbage: ends.garbage,
            outbox: Outbox {
                tx: ends.events,
                stats: ends.stats.clone(),
            },
            snapshots: ends.snapshots,
            stats: ends.stats,
            callbacks: 0,
            last_block_frames: 0,
            decks: std::array::from_fn(|i| {
                let used = i < live || i == PREP;
                Deck::new(id(i), used.then_some((sample_rate, MAX_BLOCK)))
            }),
            channels: std::array::from_fn(|i| {
                let mut ch = Channel::new(sr, XfSide::default_for(id(i)));
                ch.direct = i == PREP;
                ch
            }),
            live,
            sync_leader: None,
            sample_rate: sr,
            deck_buf: std::array::from_fn(|_| vec![0.0; MAX_BLOCK * 2]),
            master: vec![0.0; MAX_BLOCK * 2],
            phones: vec![0.0; MAX_BLOCK * 2],
            preview_buf: vec![0.0; MAX_BLOCK * 2],
            routing: Routing::default(),
            preview: None,
            preview_player: Player::new(0.0, None),
            preview_gain: 1.0,
            master_chain: MasterChain::new(sr),
            crossfader: 0.0,
            xf_gain: [Smoothed::new(0.0); 2],
            head_gain: Smoothed::new(1.0),
            head_mix: Smoothed::new(-1.0),
            limiters: [Limiter::new(sr), Limiter::new(sr)],
            limiting: 0.0,
            vu_main: [0.0; 2],
            knob_ease: dsp::ease_per_sample(KNOB_SECS, sr),
        };
        r.set_crossfader(0.0);
        for g in &mut r.xf_gain {
            g.current = g.target;
        }
        r
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
        self.callbacks += 1;
        self.last_block_frames = total as u32;
        self.publish();
        let elapsed = started.elapsed().as_nanos() as u64;
        let budget_ns = (total as f64 / self.sample_rate * 1e9) as u64;
        let s = &self.stats;
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
        let live = self.live;
        for d in slots(live) {
            let deck = &mut self.decks[d];
            let buf = &mut self.deck_buf[d][..n * 2];
            buf.fill(0.0);
            let Some(track) = &deck.track else {
                deck.player.idle();
                continue;
            };
            if deck.player.render(&track.audio, buf, sr) {
                deck.preview = Preview::None;
                self.outbox.send(EngineEvent::TrackEnded {
                    deck: deck.id,
                    generation: track.generation,
                });
            }
        }

        // Channel chains: gain, EQ, filter. Meters read here, before the faders.
        let secs = n as f64 / sr;
        for d in slots(live) {
            let ch = &mut self.channels[d];
            let peak = ch.process(&mut self.deck_buf[d][..n * 2]);
            ch.vu = meter(ch.vu, peak, secs);
        }

        // Faders, crossfader and the cue bus. Prepare's player skips all three.
        let k = self.knob_ease;
        let any_pfl = self.channels[..live].iter().any(|c| c.pfl);
        for f in 0..n {
            let xf = [self.xf_gain[0].next(k) as f32, self.xf_gain[1].next(k) as f32];
            let head = self.head_gain.next(k) as f32;
            // Head mix: how much of the main mix the headphones hear, against the cue bus.
            let mix = ((self.head_mix.next(k).clamp(-1.0, 1.0) + 1.0) / 2.0) as f32;
            let mut m = [0f32; 2];
            let mut p = [0f32; 2];
            for d in slots(live) {
                let (ch, buf) = (&mut self.channels[d], &self.deck_buf[d]);
                if ch.direct {
                    m[0] += buf[2 * f];
                    m[1] += buf[2 * f + 1];
                    continue;
                }
                let v = ch.fader.next(k) as f32;
                let g = v
                    * v
                    * match ch.crossfader {
                        XfSide::Left => xf[0],
                        XfSide::Right => xf[1],
                        XfSide::Thru => 1.0,
                    };
                for c in 0..2 {
                    let s = buf[2 * f + c];
                    m[c] += s * g;
                    if ch.pfl {
                        p[c] += s;
                    }
                }
            }
            for c in 0..2 {
                let m = 0.8 * m[c];
                self.master[2 * f + c] = m;
                // With nothing cued the cue bus carries the main mix.
                let cue = if any_pfl { 0.8 * p[c] } else { m };
                self.phones[2 * f + c] = head * ((1.0 - mix) * cue + mix * m);
            }
        }

        // Main level and balance, metered before the limiter so over 0 dB shows when
        // it's working.
        let master = &mut self.master[..n * 2];
        self.master_chain.process(master);
        let mut peaks = [0f32; 2];
        for frame in master.chunks_exact(2) {
            peaks[0] = peaks[0].max(frame[0].abs());
            peaks[1] = peaks[1].max(frame[1].abs());
        }
        for (v, p) in self.vu_main.iter_mut().zip(peaks) {
            *v = meter(*v, p, secs);
        }
        let lowest = self.limiters[0].process(master);
        self.limiting = -20.0 * (lowest as f64).log10();

        // The preview player plays at its own pace on the headphone bus only.
        if let Some(audio) = self.preview.as_deref() {
            let buf = &mut self.preview_buf[..n * 2];
            buf.fill(0.0);
            self.preview_player.render(audio, buf, sr);
            let g = 0.8 * self.head_gain.current as f32 * self.preview_gain;
            for (o, s) in self.phones.iter_mut().zip(buf.iter()) {
                *o += g * s;
            }
        }
        // The headphones get their own limiter, after the preview is mixed in.
        self.limiters[1].process(&mut self.phones[..n * 2]);
    }

    /// The deck `d` follows when synced: the app's chosen leader, or else the best
    /// loaded deck (one playing without sync, then one stopped without sync, then
    /// a playing synced one). `None` for the chosen leader itself.
    fn leader_for(&self, d: usize) -> Option<usize> {
        let ok = |l: usize| {
            l != d && l < self.live && self.decks[l].track.is_some() && !self.channels[l].direct
        };
        if let Some(l) = self.sync_leader.map(DeckId::index) {
            if l == d {
                return None;
            }
            if ok(l) {
                return Some(l);
            }
        }
        (0..self.live).filter(|&l| ok(l)).min_by_key(|&l| {
            let dk = &self.decks[l];
            let rank = match (dk.playing(), dk.sync) {
                (true, false) => 0,
                (false, false) => 1,
                (true, true) => 2,
                (false, true) => 3,
            };
            (rank, l)
        })
    }

    /// Synced decks follow their leader's tempo and beat phase, block by block, so
    /// they keep up when the leader's tempo drifts. Both are smoothed: the tempo is
    /// averaged over many beats and eased in, and small phase errors are ignored, so
    /// the follower (and its BPM readout) stays steady when the grids wobble a little.
    ///
    /// A paused follower matches the leader's tempo (so its readout and waveform zoom
    /// are right when it starts) but doesn't correct phase: it isn't moving, so the
    /// error would just sweep round with every one of the leader's beats.
    fn update_speeds(&mut self, n: usize) {
        for d in slots(self.live) {
            let dk = &mut self.decks[d];
            dk.player.speed = dk.base_speed();
            dk.tempo = 1.0 + dk.rate;
        }
        let secs = n as f64 / self.sample_rate;
        let ease = |tau: f64| 1.0 - (-secs / tau).exp();
        for d in 0..self.live {
            let leader = if self.decks[d].sync {
                self.leader_for(d)
            } else {
                None
            };
            self.decks[d].following = leader.and_then(DeckId::new);
            let Some(o) = leader else {
                self.decks[d].sync_ratio = 0.0;
                self.decks[d].sync_correction = 0.0;
                continue;
            };
            let (me, other) = (&self.decks[d], &self.decks[o]);
            let (Some(my_bpm), Some(leader_bpm)) = (me.sync_bpm(), other.sync_bpm()) else {
                continue;
            };
            // The leader's own sync correction is left out, so two synced decks don't
            // chase each other's corrections.
            let leader_speed = other.player.speed / (1.0 + other.sync_correction);
            let target = leader_bpm * leader_speed / my_bpm;
            // The readout shows the leader's readout: both tracks' one tempo, not the
            // local tempo the speed follows (they are the same for straight grids).
            let shown = match (&me.grid, &other.grid) {
                (Some(m), Some(l)) => l.mean_bpm() * (1.0 + other.rate) / m.mean_bpm(),
                _ => target,
            };
            let wanted = if me.playing() && other.playing() {
                let err = phase_error(other.beat_at(other.pos()), me.beat_at(me.pos()));
                if err.abs() < SYNC_DEADBAND {
                    0.0
                } else {
                    (err * 0.1).clamp(-0.02, 0.02)
                }
            } else {
                0.0
            };
            let me = &mut self.decks[d];
            let playing = me.playing();
            me.sync_ratio = if me.sync_ratio == 0.0 || !playing {
                target
            } else {
                me.sync_ratio + (target - me.sync_ratio) * ease(SYNC_TEMPO_SECS)
            };
            me.sync_correction = if playing {
                me.sync_correction + (wanted - me.sync_correction) * ease(SYNC_PHASE_SECS)
            } else {
                0.0
            };
            me.player.speed = me.sync_ratio * (1.0 + me.sync_correction) * (1.0 + me.nudge);
            me.tempo = me.sync_ratio;
            me.rate = shown - 1.0;
            me.check_pickup();
        }
    }

    /// Jump deck `d` to the nearest position whose beat phase matches its leader.
    fn snap_phase(&mut self, d: usize) {
        let Some(o) = self.leader_for(d) else { return };
        let other = &self.decks[o];
        if self.decks[d].grid.is_none() {
            return;
        }
        let err = phase_error(
            other.beat_at(other.pos()),
            self.decks[d].beat_at(self.decks[d].pos()),
        );
        let me = &mut self.decks[d];
        let beat = me.beat_at(me.pos()) + err;
        let target = me.time_at(beat);
        me.jump(target, JumpReason::Sync);
    }

    fn trash(&mut self, g: Garbage) {
        if let Err(rtrb::PushError::Full(g)) = self.garbage.push(g) {
            // Leaking is better than freeing on the audio thread.
            std::mem::forget(g);
        }
    }

    fn trash_load(&mut self, old: (Option<Arc<LoadedTrack>>, Option<Arc<Beatgrid>>)) {
        if let Some(t) = old.0 {
            self.trash(Garbage::Track(t));
        }
        if let Some(g) = old.1 {
            self.trash(Garbage::Grid(g));
        }
    }

    /// The slot behind `id`, if it's in use.
    fn live(&self, id: DeckId) -> Option<usize> {
        (id.index() < self.live || id.is_prepare()).then_some(id.index())
    }

    fn load(&mut self, d: usize, track: Arc<LoadedTrack>) {
        let generation = track.generation;
        self.channels[d].set_auto_gain(track.auto_gain);
        let old = self.decks[d].load(track);
        self.trash_load(old);
        self.outbox.send(EngineEvent::Loaded {
            deck: self.decks[d].id,
            generation,
        });
    }

    fn apply(&mut self, cmd: Command) {
        match cmd {
            Command::Deck(id, c) => self.deck_cmd(id, c),
            Command::Param(p, v) => self.set_param(p, v),
            Command::Load { deck, track } => match self.live(deck) {
                Some(d) => self.load(d, track),
                None => self.trash(Garbage::Track(track)),
            },
            Command::SetGrid {
                deck,
                generation,
                grid,
            } => {
                let old = match self.live(deck) {
                    Some(d) if self.decks[d].generation() == generation => {
                        std::mem::replace(&mut self.decks[d].grid, grid)
                    }
                    // Stale or for no deck: the new grid is the garbage.
                    _ => grid,
                };
                if let Some(g) = old {
                    self.trash(Garbage::Grid(g));
                }
            }
            Command::SetSyncLeader(l) => {
                self.sync_leader = l.filter(|l| l.index() < self.live);
            }
            Command::SetRouting(r) => self.routing = r,
            Command::Preview(p) => self.preview_cmd(p),
            Command::Restore(st) => self.restore(st),
        }
    }

    fn deck_cmd(&mut self, id: DeckId, cmd: DeckCmd) {
        let Some(d) = self.live(id) else { return };
        match cmd {
            DeckCmd::Play(set) => {
                let dk = &mut self.decks[d];
                if dk.track.is_none() {
                    return;
                }
                dk.preview = Preview::None;
                let was = dk.playing();
                let on = set.apply(was);
                dk.player.transport.playing = on;
                if on && !was && dk.sync {
                    self.snap_phase(d);
                }
            }
            DeckCmd::Sync(set) => {
                // Prepare's player doesn't sync.
                let on = set.apply(self.decks[d].sync) && !self.channels[d].direct;
                self.decks[d].sync = on;
                // Line the beats up now if both are playing; the tempo follows in
                // `update_speeds` (only a loaded track is needed for that).
                let leader_playing = self
                    .leader_for(d)
                    .is_some_and(|l| self.decks[l].playing());
                if on && self.decks[d].playing() && leader_playing {
                    self.snap_phase(d);
                }
            }
            DeckCmd::Pfl(set) => {
                let ch = &mut self.channels[d];
                ch.pfl = set.apply(ch.pfl);
            }
            DeckCmd::Crossfader(side) => self.channels[d].crossfader = side,
            other => self.decks[d].apply(other, &mut self.outbox),
        }
    }

    fn set_crossfader(&mut self, v: f64) {
        self.crossfader = v.clamp(-1.0, 1.0);
        // Constant power.
        let x = (self.crossfader + 1.0) * std::f64::consts::FRAC_PI_4;
        self.xf_gain[0].target = x.cos();
        self.xf_gain[1].target = x.sin();
    }

    fn set_param(&mut self, p: ParamId, v: f64) {
        match p {
            ParamId::Channel(id, cp) => {
                if let Some(d) = self.live(id) {
                    self.channels[d].set(cp, v);
                }
            }
            ParamId::Master(mp) => {
                if self.master_chain.set(mp, v) {
                    return;
                }
                match mp {
                    MasterParam::Crossfader => self.set_crossfader(v),
                    MasterParam::HeadGain => self.head_gain.target = v.clamp(0.0, 5.0),
                    MasterParam::HeadMix => self.head_mix.target = v.clamp(-1.0, 1.0),
                    MasterParam::Gain | MasterParam::Balance => {}
                }
            }
        }
    }

    fn preview_cmd(&mut self, p: PreviewCmd) {
        let t = &mut self.preview_player.transport;
        match p {
            PreviewCmd::Start { audio, start, gain } => {
                self.preview_gain = gain.clamp(0.05, 8.0) as f32;
                t.set_duration(audio.duration());
                t.jump(start, JumpReason::Seek);
                t.playing = true;
                if let Some(old) = self.preview.replace(audio) {
                    self.trash(Garbage::Audio(old));
                }
            }
            PreviewCmd::Stop => {
                t.playing = false;
                if let Some(old) = self.preview.take() {
                    self.trash(Garbage::Audio(old));
                }
            }
            PreviewCmd::Play(set) => {
                if self.preview.is_some() {
                    let on = set.apply(t.playing);
                    // Resuming at the very end starts again.
                    if on && t.pos() >= t.duration() {
                        t.jump(0.0, JumpReason::Seek);
                    }
                    t.playing = on;
                }
            }
            PreviewCmd::Seek(at) => {
                if self.preview.is_some() {
                    t.jump(at, JumpReason::Seek);
                }
            }
        }
    }

    fn restore(&mut self, mut st: Box<EngineState>) {
        for id in DeckId::first(MAX_DECKS).chain([DeckId::PREPARE]) {
            let Some(track) = st.deck_mut(id).track.take() else {
                continue;
            };
            match self.live(id) {
                Some(d) => self.load(d, track),
                None => self.trash(Garbage::Track(track)),
            }
        }
        for id in DeckId::first(self.live).chain([DeckId::PREPARE]) {
            let (d, ds) = (id.index(), st.deck_mut(id));
            self.decks[d].restore(ds);
            self.channels[d].restore(&ds.channel);
        }
        let m = st.mixer;
        self.set_crossfader(m.crossfader);
        self.master_chain.set(MasterParam::Gain, m.main_gain);
        self.master_chain.set(MasterParam::Balance, m.balance);
        self.set_param(ParamId::Master(MasterParam::HeadGain), m.head_gain);
        self.set_param(ParamId::Master(MasterParam::HeadMix), m.head_mix);
        self.apply(Command::SetSyncLeader(st.sync_leader));
        // Its tracks are out; the box itself is freed elsewhere.
        self.trash(Garbage::State(st));
    }

    fn publish(&mut self) {
        let mut decks = [DeckSnap::default(); MAX_DECKS];
        let mut prepare = DeckSnap::default();
        for d in slots(self.live) {
            let dk = &mut self.decks[d];
            *decks.get_mut(d).unwrap_or(&mut prepare) = dk.snapshot(&self.channels[d]);
            let playing = dk.playing();
            if playing != dk.was_playing {
                dk.was_playing = playing;
                let (deck, generation) = (dk.id, dk.generation());
                self.outbox.send(if playing {
                    EngineEvent::Started { deck, generation }
                } else {
                    EngineEvent::Stopped { deck, generation }
                });
            }
        }
        let pt = &self.preview_player.transport;
        let snap = EngineSnapshot {
            decks,
            decks_live: self.live,
            prepare,
            mixer: MixerSnap {
                crossfader: self.crossfader,
                main_gain: self.master_chain.get(MasterParam::Gain),
                balance: self.master_chain.get(MasterParam::Balance),
                head_gain: self.head_gain.target,
                head_mix: self.head_mix.target,
                vu_main: self.vu_main.map(|v| v as f64),
                limiting: self.limiting,
                sync_leader: self.sync_leader,
            },
            preview: PreviewSnap {
                playing: self.preview.is_some() && pt.playing,
                position: pt.pos(),
                duration: self.preview.as_ref().map_or(0.0, |a| a.duration()),
            },
            sample_rate: self.sample_rate as u32,
            last_block_frames: self.last_block_frames,
            callbacks: self.callbacks,
        };
        self.snapshots.write(snap);
    }
}

/// Beat-phase difference `leader - follower`, wrapped to -0.5..0.5 beats.
fn phase_error(leader_beat: f64, follower_beat: f64) -> f64 {
    let e = leader_beat.rem_euclid(1.0) - follower_beat.rem_euclid(1.0);
    e - e.round()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{CueSet, EQ_HIGH, EQ_LOW, EQ_MID, Set};
    use crate::demo::{DemoSpec, generate};

    const SR: u32 = 44_100;

    fn id(d: usize) -> DeckId {
        DeckId::new(d).unwrap()
    }

    fn track(audio: TrackAudio, grid: Option<Arc<Beatgrid>>, generation: u64) -> Arc<LoadedTrack> {
        Arc::new(LoadedTrack {
            track_id: generation as i64 * 10,
            generation,
            audio: Arc::new(audio),
            grid,
            cues: CueSet::default(),
            auto_gain: 1.0,
        })
    }

    fn demo(bpm0: f64, bpm1: f64, seconds: f64, seed: u64) -> (TrackAudio, Beatgrid) {
        generate(
            &DemoSpec {
                start_bpm: bpm0,
                end_bpm: bpm1,
                key: "8A".into(),
                seconds,
                seed,
            },
            SR,
        )
    }

    struct Rig {
        tx: rtrb::Producer<Command>,
        events: rtrb::Consumer<EngineEvent>,
        garbage: rtrb::Consumer<Garbage>,
        monitor: Monitor,
        r: Renderer,
        out: Vec<f32>,
    }

    impl Rig {
        fn new() -> Self {
            Rig::with_decks(2)
        }
        fn with_decks(n: usize) -> Self {
            let (app, ends) = wire();
            Rig {
                tx: app.commands,
                events: app.events,
                garbage: app.garbage,
                monitor: app.monitor,
                r: Renderer::new(ends, SR, n),
                out: vec![0.0; 256 * 4],
            }
        }
        fn send(&mut self, c: Command) {
            assert!(self.tx.push(c).is_ok());
        }
        fn deck(&mut self, d: usize, c: DeckCmd) {
            self.send(Command::Deck(id(d), c));
        }
        fn param(&mut self, p: ParamId, v: f64) {
            self.send(Command::Param(p, v));
        }
        fn master(&mut self, p: MasterParam, v: f64) {
            self.param(ParamId::Master(p), v);
        }
        fn channel(&mut self, d: usize, p: ChannelParam, v: f64) {
            self.param(ParamId::Channel(id(d), p), v);
        }
        fn load(&mut self, deck: usize, bpm0: f64, bpm1: f64) -> Arc<Beatgrid> {
            let (audio, grid) = demo(bpm0, bpm1, 60.0, 3);
            let grid = Arc::new(grid);
            self.send(Command::Load {
                deck: id(deck),
                track: track(audio, Some(grid.clone()), 1),
            });
            grid
        }
        fn run(&mut self, seconds: f64) {
            let blocks = (seconds * SR as f64 / 256.0) as usize;
            for _ in 0..blocks.max(1) {
                self.r.process(&mut self.out, 4);
            }
        }
        fn snap(&self) -> EngineSnapshot {
            self.monitor.snapshot()
        }
        fn pos(&self, d: usize) -> f64 {
            self.snap().decks[d].position
        }
        fn events(&mut self) -> Vec<EngineEvent> {
            std::iter::from_fn(|| self.events.pop().ok()).collect()
        }
    }

    /// Load a made-up track (stereo, `secs` long, sample `f(t)` at time t) without a grid.
    fn load_fn(rig: &mut Rig, deck: usize, secs: f64, f: impl Fn(f64) -> f32) {
        let n = (secs * SR as f64) as usize;
        let samples = (0..n)
            .flat_map(|i| {
                let v = f(i as f64 / SR as f64);
                [v, v]
            })
            .collect();
        let audio = TrackAudio {
            samples,
            sample_rate: SR,
        };
        rig.send(Command::Load {
            deck: id(deck),
            track: track(audio, None, 1),
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

    fn sine(hz: f64) -> impl Fn(f64) -> f32 {
        move |t| 0.5 * (std::f64::consts::TAU * hz * t).sin() as f32
    }

    #[test]
    fn keylock_keeps_the_key_at_another_tempo() {
        for (keylock, want) in [(false, 440.0 * 1.08), (true, 440.0)] {
            let mut rig = Rig::new();
            load_fn(&mut rig, 0, 20.0, sine(440.0));
            rig.deck(0, DeckCmd::Keylock(Set::from_bool(keylock)));
            rig.deck(0, DeckCmd::Rate(0.08));
            rig.deck(0, DeckCmd::Play(Set::On));
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
        load_fn(&mut rig, 0, 30.0, click_track);
        rig.deck(0, DeckCmd::Keylock(Set::On));
        rig.deck(0, DeckCmd::Rate(0.08));
        rig.deck(0, DeckCmd::Seek(0.25));
        rig.deck(0, DeckCmd::Play(Set::On));
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
        rig.deck(0, DeckCmd::Seek(10.4));
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
        rig.deck(1, DeckCmd::Sync(Set::On));
        rig.run(0.05);
        let rate = rig.snap().decks[1].rate;
        assert!((126.0 * (1.0 + rate) - 120.0).abs() < 0.01, "rate {rate}");
        assert_eq!(rig.snap().decks[1].following, Some(id(0)));
        // Off again: the tempo stays matched, the fader (still at 0) isn't in control.
        rig.deck(1, DeckCmd::Sync(Set::Off));
        rig.run(0.05);
        assert!((rig.snap().decks[1].rate - rate).abs() < 1e-9);
        assert!(!rig.snap().decks[1].fader_live);
        assert_eq!(rig.snap().decks[1].following, None);

        // The tempo (-4.8 %) is below the fader (0); moving the fader up but not past
        // it does nothing.
        let at = TempoRange::Ten.fader(rate);
        rig.deck(1, DeckCmd::TempoFader(0.2));
        rig.deck(1, DeckCmd::TempoFader(at + 0.05));
        rig.run(0.05);
        assert!(
            (rig.snap().decks[1].rate - rate).abs() < 1e-9,
            "not picked up yet"
        );
        // Past it: picked up, and the fader moves the tempo from then on.
        rig.deck(1, DeckCmd::TempoFader(at - 0.05));
        rig.run(0.05);
        assert!(rig.snap().decks[1].fader_live);
        let want = TempoRange::Ten.rate(at - 0.05);
        assert!((rig.snap().decks[1].rate - want).abs() < 1e-9);
        rig.deck(1, DeckCmd::TempoFader(0.3));
        rig.run(0.05);
        assert!((rig.snap().decks[1].rate - 0.03).abs() < 1e-9);
    }

    #[test]
    fn plays_from_before_the_start_on_the_grid() {
        let mut rig = Rig::new();
        let grid = rig.load(0, 120.0, 120.0);
        rig.run(0.01);
        // Two seconds early, snapped to the beat (cue points can go there too).
        rig.deck(0, DeckCmd::Seek(-2.1));
        rig.deck(0, DeckCmd::Cue(Press::Down));
        rig.deck(0, DeckCmd::Cue(Press::Up));
        rig.run(0.01);
        let cue = rig.snap().decks[0].cue_point;
        assert!(cue < -1.9, "cue at {cue}");
        assert!(
            (grid.beat_at(cue) - grid.beat_at(cue).round()).abs() < 1e-6,
            "on a beat"
        );
        rig.deck(0, DeckCmd::Play(Set::On));
        rig.run(0.5);
        assert!(
            rig.out.iter().all(|&s| s == 0.0),
            "silence before the track"
        );
        rig.run(2.0);
        assert!(rig.pos(0) > 0.0);
        assert!(rig.out.iter().any(|&s| s.abs() > 0.01), "then the track");
        // Never further back than the lead-in.
        rig.deck(0, DeckCmd::Seek(-100.0));
        rig.run(0.01);
        assert!(rig.pos(0) >= -LEAD_IN - 0.02);
    }

    #[test]
    fn loops_set_resize_and_end_while_playing() {
        let mut rig = Rig::new();
        let grid = rig.load(0, 120.0, 120.0);
        rig.deck(0, DeckCmd::Seek(10.2));
        rig.deck(0, DeckCmd::Play(Set::On));
        rig.run(0.3);
        // A 4-beat (2 s) loop while playing: the playhead stays inside it.
        rig.deck(0, DeckCmd::BeatLoop(4.0));
        rig.run(0.05);
        let lin = rig.snap().decks[0].loop_in.unwrap();
        assert!(rig.snap().decks[0].loop_active);
        rig.run(5.0);
        let pos = rig.pos(0);
        assert!(
            pos >= lin - 0.01 && pos < lin + 2.0 + 0.01,
            "pos {pos} loop from {lin}"
        );
        // Another size resizes it from the same start.
        rig.deck(0, DeckCmd::BeatLoop(2.0));
        rig.run(3.0);
        let want = grid.time_at(grid.beat_at(lin) + 2.0);
        assert!((rig.snap().decks[0].loop_out.unwrap() - want).abs() < 1e-6);
        assert!(rig.pos(0) < lin + 1.0 + 0.01);
        // The same size again ends it, and play carries on past the end.
        rig.deck(0, DeckCmd::BeatLoop(2.0));
        rig.run(2.0);
        assert!(!rig.snap().decks[0].loop_active);
        assert!(rig.pos(0) > lin + 1.0);
        // IN then OUT while playing makes a loop; reloop/exit ends it.
        rig.deck(0, DeckCmd::LoopIn);
        rig.run(1.0);
        rig.deck(0, DeckCmd::LoopOut);
        rig.run(0.05);
        assert!(rig.snap().decks[0].loop_active);
        rig.deck(0, DeckCmd::Reloop(Set::Toggle));
        rig.run(0.05);
        assert!(!rig.snap().decks[0].loop_active);
        // A loop set outright jumps to its start; a cue set outright stays put.
        rig.deck(0, DeckCmd::SetLoop {
            start: 20.0,
            end: 22.0,
        });
        rig.deck(0, DeckCmd::SetCue(Some(3.0)));
        rig.run(0.05);
        let s = rig.snap().decks[0];
        assert!(s.loop_active && (s.position - 20.05).abs() < 0.01, "{s:?}");
        assert_eq!(s.cue_point, 3.0);
    }

    /// Prepare mode's player sits beside all four decks: straight to the main mix
    /// whatever the fader, crossfader and headphone cue say, and never synced.
    #[test]
    fn prepare_plays_beside_four_decks_and_skips_the_mixer() {
        let mut rig = Rig::with_decks(MAX_DECKS);
        let p = DeckId::PREPARE;
        assert!(p.is_prepare() && DeckId::first(MAX_DECKS).all(|d| d != p));
        let (audio, _) = demo(120.0, 120.0, 5.0, 2);
        rig.send(Command::Load {
            deck: p,
            track: track(audio, None, 3),
        });
        rig.send(Command::Param(ParamId::Channel(p, ChannelParam::Volume), 0.0));
        rig.send(Command::Deck(p, DeckCmd::Pfl(Set::On)));
        rig.send(Command::Deck(p, DeckCmd::Sync(Set::On)));
        rig.deck(1, DeckCmd::Pfl(Set::On)); // headphones on the (silent) cue bus
        rig.master(MasterParam::Crossfader, 1.0);
        rig.send(Command::Deck(p, DeckCmd::Play(Set::On)));
        let level = level(&mut rig);
        assert!(level > 0.01, "{level}");
        assert_eq!(peaks(&rig.out, 4)[2], 0.0, "not on the cue bus");
        let s = rig.snap();
        assert!(s.prepare.playing && !s.prepare.sync);
        assert_eq!(s.prepare.generation, 3);
        assert_eq!(s.deck(p), &s.prepare);
        assert!(s.decks.iter().all(|d| d.track_id.is_none()), "no deck slot used");
        // All four decks still play alongside it, and none follows it.
        for d in 0..MAX_DECKS {
            rig.load(d, 120.0, 120.0);
            rig.deck(d, DeckCmd::Play(Set::On));
        }
        rig.deck(3, DeckCmd::Sync(Set::On));
        rig.send(Command::SetSyncLeader(Some(p)));
        rig.run(0.05);
        let s = rig.snap();
        assert!(s.live().iter().all(|d| d.playing));
        assert_eq!(s.mixer.sync_leader, None);
        assert_ne!(s.decks[3].following, Some(p));
        // Its events carry its own id.
        assert!(rig.events().iter().any(|e| *e
            == EngineEvent::Started {
                deck: p,
                generation: 3
            }));
    }

    #[test]
    fn plays_and_advances_in_real_time() {
        let mut rig = Rig::new();
        rig.load(0, 124.0, 124.0);
        rig.deck(0, DeckCmd::Play(Set::On));
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
        rig.deck(0, DeckCmd::BeatJump(16.0));
        rig.run(0.01);
        assert!((grid.beat_at(rig.pos(0)) - b0 - 16.0).abs() < 1e-6);
    }

    #[test]
    fn beat_loop_stays_inside_loop() {
        let mut rig = Rig::new();
        let grid = rig.load(0, 128.0, 128.0);
        rig.deck(0, DeckCmd::Seek(10.1));
        rig.deck(0, DeckCmd::Play(Set::On));
        rig.deck(0, DeckCmd::BeatLoop(4.0));
        rig.run(0.01);
        let s = rig.snap().decks[0];
        let (lin, lout) = (s.loop_in.unwrap(), s.loop_out.unwrap());
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
        rig.deck(0, DeckCmd::Play(Set::On));
        rig.run(5.0);
        rig.deck(1, DeckCmd::Sync(Set::On));
        rig.deck(1, DeckCmd::Play(Set::On));
        rig.run(40.0);
        let s = rig.snap();
        // Playing at the leader's local tempo…
        let lb = leader.bpm_at(s.decks[0].position, 4) * s.decks[0].speed;
        let fb = follower.bpm_at(s.decks[1].position, 4) * s.decks[1].speed;
        assert!((lb - fb).abs() < 0.3, "tempo {lb} vs {fb}");
        // …while showing the leader's one steady tempo.
        let (lr, fr) = (s.decks[0].bpm.unwrap(), s.decks[1].bpm.unwrap());
        assert!((lr - fr).abs() < 1e-9, "readout {lr} vs {fr}");
        let err = phase_error(
            leader.beat_at(s.decks[0].position),
            follower.beat_at(s.decks[1].position),
        );
        assert!(err.abs() < 0.02, "phase error {err} beats");
    }

    #[test]
    fn sync_from_the_first_beat_snaps_forward() {
        let mut rig = Rig::new();
        let leader = rig.load(0, 124.0, 124.0);
        let follower = rig.load(1, 126.0, 126.0);
        rig.deck(0, DeckCmd::Seek(10.5)); // leader 0.7 of the way through a beat
        rig.deck(0, DeckCmd::Play(Set::On));
        rig.deck(1, DeckCmd::Sync(Set::On));
        rig.deck(1, DeckCmd::Play(Set::On)); // follower still at its first beat
        rig.run(0.05);
        let err = phase_error(leader.beat_at(rig.pos(0)), follower.beat_at(rig.pos(1)));
        assert!(err.abs() < 0.01, "phase error {err} beats right after play");
    }

    #[test]
    fn cue_sets_returns_and_previews() {
        let mut rig = Rig::new();
        rig.load(0, 120.0, 120.0);
        rig.deck(0, DeckCmd::Seek(5.1));
        rig.deck(0, DeckCmd::Cue(Press::Down)); // sets cue (quantised to 5.0)
        rig.deck(0, DeckCmd::Cue(Press::Up));
        rig.run(0.01);
        assert!((rig.snap().decks[0].cue_point - 5.0).abs() < 1e-3);
        rig.deck(0, DeckCmd::Cue(Press::Down)); // preview while held
        rig.run(1.0);
        assert!(rig.snap().decks[0].playing);
        rig.deck(0, DeckCmd::Cue(Press::Up));
        rig.run(0.01);
        assert!(!rig.snap().decks[0].playing);
        assert!((rig.pos(0) - 5.0).abs() < 1e-3);
    }

    #[test]
    fn headphones_get_only_cued_decks() {
        let mut rig = Rig::new();
        rig.load(0, 120.0, 120.0);
        rig.deck(0, DeckCmd::Play(Set::On));
        rig.deck(1, DeckCmd::Pfl(Set::On));
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
        let (audio, _) = demo(124.0, 124.0, 60.0, 9);
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
            deck: id(0),
            track: track(audio, Some(leader.clone()), 2),
        });
        rig.deck(0, DeckCmd::Play(Set::On));
        rig.run(2.0);
        rig.deck(1, DeckCmd::Sync(Set::On));
        rig.deck(1, DeckCmd::Play(Set::On));
        rig.run(5.0); // settle
        let (mut lo, mut hi) = (f64::MAX, f64::MIN);
        let (mut slo, mut shi) = (f64::MAX, f64::MIN);
        for _ in 0..400 {
            rig.run(0.05);
            let s = rig.snap().decks[1];
            let bpm = s.bpm.unwrap();
            (lo, hi) = (lo.min(bpm), hi.max(bpm));
            (slo, shi) = (slo.min(s.speed), shi.max(s.speed));
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
        rig.deck(1, DeckCmd::Seek(10.3)); // off the leader's phase, so playing needs correcting
        rig.deck(0, DeckCmd::Play(Set::On));
        rig.deck(1, DeckCmd::Sync(Set::On));
        let range = |rig: &mut Rig, f: fn(&DeckSnap) -> f64| {
            let (mut lo, mut hi) = (f64::MAX, f64::MIN);
            for _ in 0..100 {
                rig.run(0.01);
                let v = f(&rig.snap().decks[1]);
                (lo, hi) = (lo.min(v), hi.max(v));
            }
            hi - lo
        };
        rig.run(0.5);
        let r = range(&mut rig, |d| d.speed);
        // (It used to swing by up to 4 %; what's left is beat times rounded to samples.)
        assert!(r < 1e-5, "paused speed moves by {r}");
        assert!(range(&mut rig, |d| d.tempo) < 1e-5, "paused tempo moves");
        assert!((rig.snap().decks[1].tempo - 124.0 / 126.0).abs() < 1e-5);
        rig.deck(1, DeckCmd::Play(Set::On));
        rig.deck(1, DeckCmd::Nudge(0.01));
        rig.run(0.2);
        assert!(range(&mut rig, |d| d.tempo) < 1e-5, "playing tempo moves");
    }

    fn peaks(out: &[f32], channels: usize) -> Vec<f32> {
        (0..channels)
            .map(|c| out.chunks(channels).map(|f| f[c].abs()).fold(0.0, f32::max))
            .collect()
    }

    #[test]
    fn preview_plays_on_headphones_only() {
        let mut rig = Rig::new();
        let (audio, _) = demo(124.0, 124.0, 10.0, 5);
        rig.send(Command::Preview(PreviewCmd::Start {
            audio: Arc::new(audio),
            start: 1.0,
            gain: 1.0,
        }));
        rig.run(0.5);
        let p = peaks(&rig.out, 4);
        assert!(p[0] == 0.0 && p[1] == 0.0, "master silent: {p:?}");
        assert!(p[2] > 0.01 && p[3] > 0.01, "phones play: {p:?}");
        assert!(rig.snap().preview.playing);
        assert!((rig.snap().preview.position - 1.5).abs() < 0.02);
        rig.send(Command::Preview(PreviewCmd::Play(Set::Off)));
        rig.run(0.1);
        assert_eq!(peaks(&rig.out, 4)[2], 0.0, "paused");
        assert!(
            (rig.snap().preview.position - 1.5).abs() < 0.02,
            "keeps its place"
        );
        rig.send(Command::Preview(PreviewCmd::Seek(0.25)));
        rig.send(Command::Preview(PreviewCmd::Play(Set::On)));
        rig.run(0.25);
        assert!(peaks(&rig.out, 4)[2] > 0.01, "resumed");
        assert!(
            (rig.snap().preview.position - 0.5).abs() < 0.02,
            "from the seek"
        );
        rig.send(Command::Preview(PreviewCmd::Stop));
        rig.run(0.1);
        assert_eq!(peaks(&rig.out, 4)[2], 0.0);
        assert!(!rig.snap().preview.playing);
    }

    #[test]
    fn meters_follow_gain_and_eq() {
        let mut rig = Rig::new();
        rig.load(0, 120.0, 120.0);
        rig.deck(0, DeckCmd::Play(Set::On));
        rig.run(0.5);
        let flat = rig.snap().decks[0].vu;
        assert!(flat > 0.01, "meter moves: {flat}");
        assert!(rig.snap().mixer.vu_main[0] > 0.01);
        // Killing all three bands silences the channel; its meter falls away.
        for band in [EQ_LOW, EQ_MID, EQ_HIGH] {
            rig.channel(0, ChannelParam::Eq(band), 0.0);
        }
        rig.run(3.0);
        assert!(rig.snap().decks[0].vu < flat * 0.01);
        assert!(peaks(&rig.out, 4)[0] < 0.001);
        assert_eq!(rig.snap().decks[0].eq[EQ_MID], 0.0);
    }

    #[test]
    fn main_gain_balance_and_head_mix() {
        let mut rig = Rig::new();
        rig.load(0, 120.0, 120.0);
        rig.deck(0, DeckCmd::Play(Set::On));
        rig.deck(1, DeckCmd::Pfl(Set::On)); // cue bus silent
        rig.master(MasterParam::Balance, 1.0); // right only
        rig.run(0.5);
        let p = peaks(&rig.out, 4);
        assert!(p[0] == 0.0 && p[1] > 0.01, "balance: {p:?}");
        assert_eq!(p[2], 0.0, "headphones hear only the (silent) cue bus");
        // Head mix fully to main: the headphones hear the main mix.
        rig.master(MasterParam::HeadMix, 1.0);
        rig.run(0.2);
        assert!(peaks(&rig.out, 4)[2] > 0.01);
        // Main gain 0 silences the main output but not the headphones.
        rig.master(MasterParam::Gain, 0.0);
        rig.run(0.2);
        let p = peaks(&rig.out, 4);
        assert!(p[0] == 0.0 && p[1] == 0.0 && p[2] > 0.01, "{p:?}");
        assert_eq!(rig.snap().mixer.main_gain, 0.0);
    }

    #[test]
    fn routing_moves_and_splits_buses() {
        let mut rig = Rig::new();
        rig.load(0, 120.0, 120.0);
        rig.deck(0, DeckCmd::Play(Set::On));
        rig.deck(1, DeckCmd::Pfl(Set::On)); // headphones hear nothing
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

    // ---- N decks, ids, events, snapshots, restore ---------------------------------

    /// Master level (left) of a steady tone once it has settled.
    fn level(rig: &mut Rig) -> f32 {
        rig.run(0.2);
        peaks(&rig.out, 4)[0]
    }

    #[test]
    fn four_decks_mix_through_their_crossfader_sides() {
        let mut rig = Rig::with_decks(4);
        // Tones on decks 3 (on the left by default) and 4 (set to thru).
        load_fn(&mut rig, 2, 5.0, sine(330.0));
        load_fn(&mut rig, 3, 5.0, sine(550.0));
        rig.deck(3, DeckCmd::Crossfader(XfSide::Thru));
        for d in [2, 3] {
            rig.channel(d, ChannelParam::Volume, 0.0);
            rig.deck(d, DeckCmd::Play(Set::On));
        }
        assert_eq!(level(&mut rig), 0.0, "faders down");
        assert_eq!(rig.snap().decks[2].crossfader, XfSide::Left);
        assert_eq!(rig.snap().decks[3].crossfader, XfSide::Thru);

        // Deck 3 alone: heard in the middle, gone with the crossfader hard right.
        rig.channel(2, ChannelParam::Volume, 1.0);
        let middle = level(&mut rig);
        assert!(middle > 0.1, "{middle}");
        rig.master(MasterParam::Crossfader, 1.0);
        assert!(level(&mut rig) < 1e-4);
        // Deck 4 on thru ignores the crossfader, and is louder than deck 3 was in the
        // middle (constant power: each side is at -3 dB there).
        rig.channel(2, ChannelParam::Volume, 0.0);
        rig.channel(3, ChannelParam::Volume, 1.0);
        let thru = level(&mut rig);
        assert!(thru > middle * 1.3, "{thru} vs {middle}");
        rig.master(MasterParam::Crossfader, -1.0);
        assert!((level(&mut rig) - thru).abs() < 0.01);
    }

    #[test]
    fn decks_past_the_live_count_are_ignored() {
        assert!(DeckId::new(MAX_DECKS).is_none());
        assert_eq!(DeckId::first(9).count(), MAX_DECKS);
        let mut rig = Rig::new(); // decks 3 and 4 exist but aren't live
        let (audio, _) = demo(120.0, 120.0, 5.0, 1);
        rig.send(Command::Load {
            deck: id(3),
            track: track(audio, None, 5),
        });
        rig.deck(3, DeckCmd::Play(Set::On));
        rig.deck(
            3,
            DeckCmd::Hotcue {
                slot: 200,
                press: Press::Down,
            },
        );
        rig.deck(2, DeckCmd::BeatJump(4.0));
        rig.channel(3, ChannelParam::Eq(9), 0.0);
        rig.send(Command::SetSyncLeader(Some(id(3))));
        rig.run(0.1);
        let s = rig.snap();
        assert_eq!(s.decks_live, 2);
        assert_eq!(s.live().len(), 2);
        assert_eq!(s.decks[3], DeckSnap::default());
        assert_eq!(s.mixer.sync_leader, None);
        // The track went to the garbage ring, not freed on the audio thread.
        assert!(matches!(rig.garbage.pop(), Ok(Garbage::Track(_))));
        // A bad hot cue slot or EQ band on a live deck is ignored too.
        rig.load(0, 120.0, 120.0);
        rig.deck(
            0,
            DeckCmd::Hotcue {
                slot: HOTCUES as u8,
                press: Press::Down,
            },
        );
        rig.deck(0, DeckCmd::HotcueClear(250));
        rig.channel(0, ChannelParam::Eq(3), 0.0);
        rig.run(0.01);
        assert_eq!(rig.snap().decks[0].hotcues, [None; HOTCUES]);
        assert_eq!(rig.snap().decks[0].eq, [1.0; 3]);
    }

    #[test]
    fn sync_follows_the_chosen_leader() {
        let mut rig = Rig::with_decks(3);
        rig.load(0, 120.0, 120.0);
        rig.load(1, 126.0, 126.0);
        rig.load(2, 130.0, 130.0);
        rig.deck(0, DeckCmd::Play(Set::On));
        rig.deck(2, DeckCmd::Sync(Set::On));
        rig.run(0.1);
        // Left to itself it follows the deck that's playing without sync.
        assert_eq!(rig.snap().decks[2].following, Some(id(0)));
        assert!((rig.snap().decks[2].bpm.unwrap() - 120.0).abs() < 0.01);
        // Choose deck 2 (stopped) as leader: deck 3 follows it instead.
        rig.send(Command::SetSyncLeader(Some(id(1))));
        rig.run(0.1);
        let s = rig.snap();
        assert_eq!(s.mixer.sync_leader, Some(id(1)));
        assert_eq!(s.decks[2].following, Some(id(1)));
        assert!((s.decks[2].bpm.unwrap() - 126.0).abs() < 0.01);
        // The leader itself follows nobody, even with sync on.
        rig.deck(1, DeckCmd::Sync(Set::On));
        rig.run(0.1);
        assert_eq!(rig.snap().decks[1].following, None);
        assert!((rig.snap().decks[1].bpm.unwrap() - 126.0).abs() < 0.01);
    }

    #[test]
    fn toggles_resolve_on_the_audio_thread() {
        let mut rig = Rig::new();
        rig.load(0, 120.0, 120.0);
        // Two quick presses land in the same block: back where it started.
        for _ in 0..2 {
            rig.deck(0, DeckCmd::Keylock(Set::Toggle));
            rig.deck(0, DeckCmd::Pfl(Set::Toggle));
        }
        rig.deck(0, DeckCmd::Quantize(Set::Toggle));
        rig.deck(0, DeckCmd::Play(Set::Toggle));
        rig.run(0.01);
        let s = rig.snap().decks[0];
        assert!(!s.keylock && !s.pfl && !s.quantize && s.playing);
        rig.deck(0, DeckCmd::TempoRangeNext);
        rig.deck(0, DeckCmd::TempoRangeNext);
        rig.run(0.01);
        assert_eq!(
            rig.snap().decks[0].range,
            TempoRange::default().next().next()
        );
    }

    #[test]
    fn events_carry_the_tracks_generation() {
        let mut rig = Rig::new();
        load_fn(&mut rig, 0, 0.5, sine(440.0));
        rig.deck(0, DeckCmd::Seek(0.1));
        rig.deck(
            0,
            DeckCmd::Hotcue {
                slot: 2,
                press: Press::Down,
            },
        );
        rig.deck(0, DeckCmd::BeatLoop(1.0));
        rig.deck(0, DeckCmd::BeatLoop(1.0)); // and off again
        rig.deck(0, DeckCmd::Play(Set::On));
        rig.run(1.0);
        let ev = rig.events();
        assert!(
            ev.iter().all(|e| e.deck() == id(0) && e.generation() == 1),
            "{ev:?}"
        );
        assert_eq!(
            ev[0],
            EngineEvent::Loaded {
                deck: id(0),
                generation: 1
            }
        );
        assert!(ev.iter().any(|e| matches!(
            e,
            EngineEvent::HotcueSet { slot: 2, position: Some(p), .. } if (p - 0.1).abs() < 1e-9
        )));
        assert!(ev.iter().any(|e| matches!(e, EngineEvent::LoopSet { .. })));
        let at = |want: fn(&EngineEvent) -> bool| ev.iter().position(want).unwrap();
        let started = at(|e| matches!(e, EngineEvent::Started { .. }));
        let ended = at(|e| matches!(e, EngineEvent::TrackEnded { .. }));
        let stopped = at(|e| matches!(e, EngineEvent::Stopped { .. }));
        assert!(started < ended && ended < stopped, "{ev:?}");
    }

    #[test]
    fn stale_grids_are_ignored() {
        let mut rig = Rig::new();
        rig.load(0, 120.0, 120.0); // generation 1
        let other = Arc::new(Beatgrid::constant(90.0, 0.0, 60.0).unwrap());
        rig.send(Command::SetGrid {
            deck: id(0),
            generation: 0,
            grid: Some(other.clone()),
        });
        rig.run(0.01);
        assert!((rig.snap().decks[0].bpm.unwrap() - 120.0).abs() < 1e-6);
        rig.send(Command::SetGrid {
            deck: id(0),
            generation: 1,
            grid: Some(other),
        });
        rig.run(0.01);
        assert!((rig.snap().decks[0].bpm.unwrap() - 90.0).abs() < 1e-6);
    }

    /// Each snapshot describes one track: its generation, id and length agree, even
    /// while loads keep landing.
    #[test]
    fn snapshots_never_mix_two_tracks() {
        let mut rig = Rig::new();
        let silent = |secs: f64, generation: u64| {
            let audio = TrackAudio {
                samples: vec![0.0; (secs * SR as f64) as usize * 2],
                sample_rate: SR,
            };
            track(audio, None, generation)
        };
        let reader = rig.monitor.clone();
        for generation in 1..100u64 {
            let secs = generation as f64 / 10.0;
            rig.send(Command::Load {
                deck: id(0),
                track: silent(secs, generation),
            });
            rig.r.process(&mut rig.out, 4);
            while rig.garbage.pop().is_ok() {}
            let s = reader.snapshot().decks[0];
            assert_eq!(s.generation, generation);
            assert_eq!(s.track_id, Some(generation as i64 * 10));
            assert!((s.duration - secs).abs() < 1e-4);
        }
    }

    #[test]
    fn restore_puts_everything_back() {
        let mut rig = Rig::with_decks(3);
        let (audio, grid) = demo(120.0, 120.0, 60.0, 4);
        let loaded = Arc::new(LoadedTrack {
            track_id: 77,
            generation: 9,
            audio: Arc::new(audio),
            grid: Some(Arc::new(grid)),
            cues: CueSet {
                main: Some(2.0),
                hot: [Some(1.0), None, None, Some(8.0), None, None, None, None],
            },
            auto_gain: 0.5,
        });
        rig.send(Command::Load {
            deck: id(1),
            track: loaded.clone(),
        });
        rig.load(0, 124.0, 124.0);
        rig.load(2, 126.0, 126.0);
        rig.deck(1, DeckCmd::Seek(20.0));
        rig.deck(
            1,
            DeckCmd::Hotcue {
                slot: 5,
                press: Press::Down,
            },
        );
        rig.deck(1, DeckCmd::BeatLoop(8.0));
        rig.deck(1, DeckCmd::Keylock(Set::On));
        rig.deck(1, DeckCmd::TempoRange(TempoRange::Sixteen));
        rig.deck(1, DeckCmd::Rate(0.05));
        rig.deck(1, DeckCmd::Pfl(Set::On));
        rig.deck(1, DeckCmd::Crossfader(XfSide::Thru));
        rig.channel(1, ChannelParam::Eq(EQ_LOW), 0.0);
        rig.channel(1, ChannelParam::Filter, 0.4);
        rig.channel(1, ChannelParam::Trim, 1.5);
        rig.channel(1, ChannelParam::Volume, 0.7);
        rig.deck(2, DeckCmd::Quantize(Set::Off));
        rig.deck(2, DeckCmd::Sync(Set::On));
        rig.master(MasterParam::Crossfader, 0.3);
        rig.master(MasterParam::HeadMix, 0.2);
        rig.send(Command::SetSyncLeader(Some(id(1))));
        rig.run(0.1);
        let before = rig.snap();
        assert_eq!(before.decks[1].hotcues[0], Some(1.0), "the track's cues");
        assert!(before.decks[1].hotcues[5].is_some());

        // A new renderer (as after an audio device restart) gets it all back, except
        // deck 1, whose track isn't sent.
        let mut state = before.state();
        state.decks[1].track = Some(loaded);
        let (audio, grid) = demo(126.0, 126.0, 60.0, 3);
        state.decks[2].track = Some(track(audio, Some(Arc::new(grid)), 1));
        let (audio, _) = demo(124.0, 124.0, 30.0, 6);
        state.prepare.track = Some(track(audio, None, 4));
        state.prepare.position = 7.0;
        let mut fresh = Rig::with_decks(3);
        fresh.send(Command::Restore(Box::new(state)));
        fresh.run(0.1);
        let after = fresh.snap();
        let same = |s: &DeckSnap| DeckSnap { vu: 0.0, ..*s };
        for d in 1..3 {
            assert_eq!(same(&after.decks[d]), same(&before.decks[d]), "deck {d}");
        }
        assert_eq!(after.decks[1].auto_gain, 0.5);
        assert_eq!(after.decks[0].track_id, None);
        assert_eq!(after.prepare.generation, 4);
        assert!((after.prepare.position - 7.0).abs() < 1e-9);
        let mixer = |m: MixerSnap| MixerSnap {
            vu_main: [0.0; 2],
            limiting: 0.0,
            ..m
        };
        assert_eq!(mixer(after.mixer), mixer(before.mixer));
        // The state box went to the garbage ring.
        assert!(
            std::iter::from_fn(|| fresh.garbage.pop().ok())
                .any(|g| matches!(g, Garbage::State(_)))
        );
    }
}
