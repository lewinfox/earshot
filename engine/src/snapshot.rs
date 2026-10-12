//! What the engine tells the app: one [`EngineSnapshot`] per audio callback (through a
//! triple buffer, so a reader always sees one whole callback's state), [`EngineEvent`]s
//! for things that happened, and a few [`Stats`] counters.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

use crate::command::{
    ChannelState, CueSet, DeckId, DeckState, EngineState, HOTCUES, LoopState, MAX_DECKS,
    MixerState, XfSide,
};
use crate::tempo::TempoRange;

/// One deck and its channel, as of the end of a callback.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DeckSnap {
    /// What's loaded (`None`: nothing), and the generation it was loaded as.
    pub track_id: Option<i64>,
    pub generation: u64,
    pub position: f64,
    pub duration: f64,
    pub playing: bool,
    /// How fast the audio is advancing right now (1.0 = original tempo), including
    /// nudges and sync's brief phase corrections. For moving the playhead.
    pub speed: f64,
    /// The deck's tempo as a ratio to the track's own: the pitch fader, or the followed
    /// tempo when synced. Steady: leaves out nudges and phase corrections. For anything
    /// that shows tempo (waveform zoom, effective key).
    pub tempo: f64,
    /// Pitch fader position as a fraction, e.g. 0.02 = +2 %.
    pub rate: f64,
    pub range: TempoRange,
    /// Where the tempo fader is (-1..1), which differs from `rate` until it's picked
    /// up again after sync or a range change moved the tempo.
    pub fader: f64,
    /// The fader is in control (it has been moved past the tempo since then).
    pub fader_live: bool,
    /// The track's tempo (its grid's mean) × the tempo fader. `None` without a grid.
    pub bpm: Option<f64>,
    pub sync: bool,
    /// The deck this one is following while synced.
    pub following: Option<DeckId>,
    /// Cues, loops and hot cues snap to the beat grid.
    pub quantize: bool,
    /// The tempo fader leaves the key alone.
    pub keylock: bool,
    pub loop_in: Option<f64>,
    pub loop_out: Option<f64>,
    pub loop_active: bool,
    pub cue_point: f64,
    pub hotcues: [Option<f64>; HOTCUES],
    pub volume: f64,
    pub pfl: bool,
    /// Channel gain (trim) before the EQ, linear: 1 = unchanged.
    pub pregain: f64,
    /// Auto-gain for the loaded track, linear.
    pub auto_gain: f64,
    /// EQ band gains, linear, indexed by [`crate::EQ_LOW`] etc.: 0 = killed, 1 = flat.
    pub eq: [f64; 3],
    /// Filter knob: -1 low-pass .. 0 off .. 1 high-pass.
    pub filter: f64,
    pub crossfader: XfSide,
    /// Channel level meter (peak, after gain, EQ and filter, before the fader).
    pub vu: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MixerSnap {
    pub crossfader: f64,
    pub main_gain: f64,
    pub balance: f64,
    pub head_gain: f64,
    pub head_mix: f64,
    /// Main output meters, left and right (peak).
    pub vu_main: [f64; 2],
    /// How much the master limiter turned down in the last block, dB (0 = not at all).
    pub limiting: f64,
    /// The leader the app chose ([`crate::Command::SetSyncLeader`]).
    pub sync_leader: Option<DeckId>,
}

/// The preview player (headphones only).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PreviewSnap {
    pub playing: bool,
    pub position: f64,
    pub duration: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct EngineSnapshot {
    /// Only the first [`EngineSnapshot::decks_live`] are in use.
    pub decks: [DeckSnap; MAX_DECKS],
    pub decks_live: usize,
    /// Prepare mode's player ([`DeckId::PREPARE`]).
    pub prepare: DeckSnap,
    pub mixer: MixerSnap,
    pub preview: PreviewSnap,
    pub sample_rate: u32,
    pub last_block_frames: u32,
    /// Callbacks so far (this snapshot is from the last one).
    pub callbacks: u64,
}

impl EngineSnapshot {
    pub fn deck(&self, deck: DeckId) -> &DeckSnap {
        self.decks.get(deck.index()).unwrap_or(&self.prepare)
    }

    /// The live decks.
    pub fn live(&self) -> &[DeckSnap] {
        &self.decks[..self.decks_live.min(MAX_DECKS)]
    }

    /// Everything in here that [`crate::Command::Restore`] can put back, without the
    /// tracks: put those back into `decks[i].track` before sending it.
    pub fn state(&self) -> EngineState {
        let mut st = EngineState {
            mixer: MixerState {
                crossfader: self.mixer.crossfader,
                main_gain: self.mixer.main_gain,
                balance: self.mixer.balance,
                head_gain: self.mixer.head_gain,
                head_mix: self.mixer.head_mix,
            },
            sync_leader: self.mixer.sync_leader,
            ..EngineState::default()
        };
        for (ds, d) in st.decks.iter_mut().zip(&self.decks) {
            *ds = DeckState {
                track: None,
                position: d.position,
                playing: d.playing,
                cues: CueSet {
                    main: Some(d.cue_point),
                    hot: d.hotcues,
                },
                loop_: LoopState {
                    start: d.loop_in,
                    end: d.loop_out,
                    active: d.loop_active,
                },
                rate: d.rate,
                range: d.range,
                fader: d.fader,
                fader_live: d.fader_live,
                sync: d.sync,
                quantize: d.quantize,
                keylock: d.keylock,
                channel: ChannelState {
                    volume: d.volume,
                    trim: d.pregain,
                    eq: d.eq,
                    filter: d.filter,
                    pfl: d.pfl,
                    crossfader: d.crossfader,
                },
            };
        }
        st
    }
}

/// Something that happened on the audio thread. Each carries the generation of the
/// track it happened to, so the app can drop events about a track it has since
/// replaced.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EngineEvent {
    Loaded {
        deck: DeckId,
        generation: u64,
    },
    Started {
        deck: DeckId,
        generation: u64,
    },
    Stopped {
        deck: DeckId,
        generation: u64,
    },
    /// Played to the end (and stopped).
    TrackEnded {
        deck: DeckId,
        generation: u64,
    },
    CueSet {
        deck: DeckId,
        generation: u64,
        position: f64,
    },
    /// A hot cue was set (`Some`) or cleared (`None`).
    HotcueSet {
        deck: DeckId,
        generation: u64,
        slot: u8,
        position: Option<f64>,
    },
    LoopSet {
        deck: DeckId,
        generation: u64,
        loop_: LoopState,
    },
}

impl EngineEvent {
    pub fn deck(&self) -> DeckId {
        match *self {
            EngineEvent::Loaded { deck, .. }
            | EngineEvent::Started { deck, .. }
            | EngineEvent::Stopped { deck, .. }
            | EngineEvent::TrackEnded { deck, .. }
            | EngineEvent::CueSet { deck, .. }
            | EngineEvent::HotcueSet { deck, .. }
            | EngineEvent::LoopSet { deck, .. } => deck,
        }
    }

    pub fn generation(&self) -> u64 {
        match *self {
            EngineEvent::Loaded { generation, .. }
            | EngineEvent::Started { generation, .. }
            | EngineEvent::Stopped { generation, .. }
            | EngineEvent::TrackEnded { generation, .. }
            | EngineEvent::CueSet { generation, .. }
            | EngineEvent::HotcueSet { generation, .. }
            | EngineEvent::LoopSet { generation, .. } => generation,
        }
    }
}

/// Counters that aren't part of one callback's state: some are bumped from outside the
/// render (xruns come from the backend's error callback), and the worst callback time
/// is read-and-reset.
#[derive(Default)]
pub struct Stats {
    /// Callbacks that took more than 80 % of their time budget.
    pub slow_callbacks: AtomicU64,
    /// Xruns reported by the audio backend.
    pub xruns: AtomicU64,
    /// Worst callback duration since the last [`Stats::take_max_callback_ns`].
    pub max_callback_ns: AtomicU64,
    /// Events the app didn't collect in time (the ring was full).
    pub dropped_events: AtomicU64,
}

impl Stats {
    pub fn take_max_callback_ns(&self) -> u64 {
        self.max_callback_ns.swap(0, Relaxed)
    }
}

/// The app's read side: the latest snapshot and the counters. Cheap to clone and safe
/// to read from any thread; the lock here is never taken by the audio thread.
#[derive(Clone)]
pub struct Monitor {
    snapshots: Arc<Mutex<triple_buffer::Output<EngineSnapshot>>>,
    stats: Arc<Stats>,
}

impl Monitor {
    pub(crate) fn new(snapshots: triple_buffer::Output<EngineSnapshot>, stats: Arc<Stats>) -> Self {
        Monitor {
            snapshots: Arc::new(Mutex::new(snapshots)),
            stats,
        }
    }

    /// The state as of the most recent callback.
    pub fn snapshot(&self) -> EngineSnapshot {
        *self.snapshots.lock().unwrap().read()
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }
}
