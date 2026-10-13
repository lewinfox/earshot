//! What the app tells the engine: deck ids, commands, and the state it can restore.
//!
//! Everything here crosses the command ring to the audio thread, so it is `Send`,
//! needs no allocation to apply, and can't name a deck that doesn't exist.

use std::sync::Arc;

use crate::beatgrid::Beatgrid;
use crate::decode::TrackAudio;
use crate::engine::Routing;
use crate::tempo::TempoRange;

/// The most decks the engine can run. The app picks how many are live when it starts
/// the engine ([`crate::Engine::start`]); the rest stay silent and ignore commands.
pub const MAX_DECKS: usize = 4;
pub const HOTCUES: usize = 8;
/// EQ band indices for [`ChannelParam::Eq`] and [`crate::DeckSnap::eq`].
pub const EQ_LOW: usize = 0;
pub const EQ_MID: usize = 1;
pub const EQ_HIGH: usize = 2;

/// Deck slots inside the engine: the decks, then Prepare mode's player.
pub(crate) const SLOTS: usize = MAX_DECKS + 1;

/// A deck, or Prepare mode's player ([`DeckId::PREPARE`]). Checked when it's made, so
/// indexing with it can never go out of bounds on the audio thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeckId(u8);

impl DeckId {
    /// Prepare mode's player: a full deck (cues, loops, grid, keylock) that isn't one
    /// of the [`MAX_DECKS`]. It plays straight to the main mix, past the fader,
    /// crossfader and headphone cue, and never syncs or leads.
    pub const PREPARE: DeckId = DeckId(MAX_DECKS as u8);

    pub const fn is_prepare(self) -> bool {
        self.0 as usize == MAX_DECKS
    }

    pub const fn new(index: usize) -> Option<DeckId> {
        if index < MAX_DECKS {
            Some(DeckId(index as u8))
        } else {
            None
        }
    }

    /// 0.. for decks; [`MAX_DECKS`] for [`DeckId::PREPARE`].
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// The first `n` decks (at most [`MAX_DECKS`]).
    pub fn first(n: usize) -> impl Iterator<Item = DeckId> {
        (0..n.min(MAX_DECKS)).map(|i| DeckId(i as u8))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Press {
    Down,
    Up,
}

/// An on/off control. `Toggle` is resolved on the audio thread against the live
/// state, so two quick presses can't both read the same stale value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Set {
    On,
    Off,
    Toggle,
}

impl Set {
    pub fn from_bool(on: bool) -> Set {
        if on { Set::On } else { Set::Off }
    }

    pub fn apply(self, current: bool) -> bool {
        match self {
            Set::On => true,
            Set::Off => false,
            Set::Toggle => !current,
        }
    }
}

/// Which side of the crossfader a channel is on. `Thru` ignores the crossfader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum XfSide {
    Left,
    #[default]
    Thru,
    Right,
}

impl XfSide {
    /// Decks 1 and 3 on the left, 2 and 4 on the right, as on a four-deck mixer.
    pub fn default_for(deck: DeckId) -> XfSide {
        if deck.index() % 2 == 0 {
            XfSide::Left
        } else {
            XfSide::Right
        }
    }
}

/// Things a deck does. Continuous mixer controls go through [`Command::Param`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DeckCmd {
    Play(Set),
    /// CDJ-style cue: set / return / preview while held.
    Cue(Press),
    Seek(f64),
    BeatJump(f64),
    /// Toggle an auto-loop of this many beats starting at the current beat.
    BeatLoop(f64),
    LoopIn,
    LoopOut,
    /// Turn the stored loop on or off.
    Reloop(Set),
    LoopScale(f64),
    Hotcue {
        slot: u8,
        press: Press,
    },
    HotcueClear(u8),
    /// Tempo change, as a fraction (within the deck's [`TempoRange`]).
    Rate(f64),
    /// The tempo fader moved to this position (-1..1 across its range). It sets the
    /// tempo only once it has been moved past the current tempo ("pickup"), and never
    /// while sync is on.
    TempoFader(f64),
    /// The tempo fader's range. The tempo stays put (clamped into the new range).
    TempoRange(TempoRange),
    /// Step to the next range, like a CDJ's tempo range button.
    TempoRangeNext,
    /// Temporary speed offset while a nudge button is held; 0 releases it.
    Nudge(f64),
    Sync(Set),
    Quantize(Set),
    Keylock(Set),
    /// Headphone cue for this deck's channel.
    Pfl(Set),
    Crossfader(XfSide),
    /// Put the cue point here (seconds), or at the playhead (snapped to the beat when
    /// quantising) with `None`.
    SetCue(Option<f64>),
    /// Loop `start..end` (seconds) and jump to its start.
    SetLoop {
        start: f64,
        end: f64,
    },
}

/// A continuous control on a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelParam {
    /// Channel fader, 0..1.
    Volume,
    /// Gain (trim) before the EQ, linear, 0..4.
    Trim,
    /// One EQ band's gain ([`EQ_LOW`] etc.), linear: 0 kills it, 1 is flat.
    Eq(usize),
    /// -1 (low-pass) .. 0 (off) .. 1 (high-pass).
    Filter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MasterParam {
    /// -1 (left) .. 1 (right).
    Crossfader,
    /// Main output level, linear, 0..5.
    Gain,
    /// -1 (left) .. 1 (right).
    Balance,
    /// Headphone level, linear, 0..5.
    HeadGain,
    /// What the headphones hear: -1 the cue bus only .. 1 the main mix only.
    HeadMix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamId {
    Channel(DeckId, ChannelParam),
    Master(MasterParam),
}

/// Cue points that travel with a track: the main cue and the hot cues, in seconds.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CueSet {
    /// `None`: the grid's first beat (or the start). The playhead starts here on load.
    pub main: Option<f64>,
    pub hot: [Option<f64>; HOTCUES],
}

/// Everything about a track the engine needs, loaded in one go so a deck never plays
/// one track with another's grid or gain.
pub struct LoadedTrack {
    /// The app's id for the track (the engine just hands it back in snapshots).
    pub track_id: i64,
    /// The app's load counter; snapshots and events carry it so stale ones can be told
    /// apart.
    pub generation: u64,
    pub audio: Arc<TrackAudio>,
    pub grid: Option<Arc<Beatgrid>>,
    pub cues: CueSet,
    /// Linear (see `loudness::Loudness::auto_gain`).
    pub auto_gain: f64,
}

/// A loop: where it starts and ends (seconds), and whether it's playing.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LoopState {
    pub start: Option<f64>,
    pub end: Option<f64>,
    pub active: bool,
}

impl LoopState {
    /// Start and end, when the loop is on and makes sense.
    pub fn bounds(&self) -> Option<(f64, f64)> {
        match (self.active, self.start, self.end) {
            (true, Some(s), Some(e)) if e > s => Some((s, e)),
            _ => None,
        }
    }
}

/// A channel's mixer settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChannelState {
    pub volume: f64,
    pub trim: f64,
    pub eq: [f64; 3],
    pub filter: f64,
    pub pfl: bool,
    pub crossfader: XfSide,
}

impl Default for ChannelState {
    fn default() -> ChannelState {
        ChannelState {
            volume: 1.0,
            trim: 1.0,
            eq: [1.0; 3],
            filter: 0.0,
            pfl: false,
            crossfader: XfSide::Thru,
        }
    }
}

/// One deck's full state, for [`Command::Restore`].
#[derive(Clone)]
pub struct DeckState {
    /// What's loaded. Its grid is used as is (send the edited grid, if any).
    pub track: Option<Arc<LoadedTrack>>,
    pub position: f64,
    pub playing: bool,
    /// Cues as they are now (they replace the track's).
    pub cues: CueSet,
    pub loop_: LoopState,
    pub rate: f64,
    pub range: TempoRange,
    pub fader: f64,
    pub fader_live: bool,
    pub sync: bool,
    pub quantize: bool,
    pub keylock: bool,
    pub channel: ChannelState,
}

impl Default for DeckState {
    fn default() -> DeckState {
        DeckState {
            track: None,
            position: 0.0,
            playing: false,
            cues: CueSet::default(),
            loop_: LoopState::default(),
            rate: 0.0,
            range: TempoRange::default(),
            fader: 0.0,
            fader_live: true,
            sync: false,
            quantize: true,
            keylock: false,
            channel: ChannelState::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MixerState {
    pub crossfader: f64,
    pub main_gain: f64,
    pub balance: f64,
    pub head_gain: f64,
    pub head_mix: f64,
}

impl Default for MixerState {
    fn default() -> MixerState {
        MixerState {
            crossfader: 0.0,
            main_gain: 1.0,
            balance: 0.0,
            head_gain: 1.0,
            head_mix: -1.0,
        }
    }
}

/// Everything the engine holds that the app would want back after the audio device
/// restarts. Build it from the last [`crate::EngineSnapshot`] with
/// [`crate::EngineSnapshot::state`] and put the tracks back in.
#[derive(Clone, Default)]
pub struct EngineState {
    pub decks: [DeckState; MAX_DECKS],
    /// Prepare mode's player.
    pub prepare: DeckState,
    pub mixer: MixerState,
    pub sync_leader: Option<DeckId>,
    pub hotcue_mode: HotcueMode,
}

/// What pressing a hot cue does on a stopped deck (a playing one just jumps there).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum HotcueMode {
    /// Jump there and play on (Rekordbox's default, and earshot's).
    #[default]
    PlayOnPress,
    /// Play from there while held; letting go stops and goes back (Mixxx's).
    PlayWhileHeld,
}

pub enum PreviewCmd {
    /// Play a track on the headphone bus only, from `start` seconds, at `gain`
    /// (its auto-gain, linear).
    Start {
        audio: Arc<TrackAudio>,
        start: f64,
        gain: f64,
    },
    /// Pause or resume, keeping its place.
    Play(Set),
    Seek(f64),
    Stop,
}

pub enum Command {
    Deck(DeckId, DeckCmd),
    Param(ParamId, f64),
    Load {
        deck: DeckId,
        track: Arc<LoadedTrack>,
    },
    /// Replace the grid of the track loaded as `generation` (ignored if the deck has
    /// moved on to another track since).
    SetGrid {
        deck: DeckId,
        generation: u64,
        grid: Option<Arc<Beatgrid>>,
    },
    /// Move the cue point and the playhead to `at` (seconds) on the track loaded as
    /// `generation`, but only if nothing has played it, moved the playhead or set the
    /// cue since it was loaded (ignored otherwise, or if the deck has moved on to
    /// another track): the initial cue, once analysis finds where the beats start.
    /// Sends [`crate::EngineEvent::CueSet`] when it moves it.
    InitialCue {
        deck: DeckId,
        generation: u64,
        at: f64,
    },
    /// The deck synced decks follow. `None`: each synced deck picks one (a playing
    /// deck that isn't synced itself, if there is one).
    SetSyncLeader(Option<DeckId>),
    /// What hot cues do on a stopped deck, on every player.
    SetHotcueMode(HotcueMode),
    SetRouting(Routing),
    Preview(PreviewCmd),
    /// Put every deck and the mixer back as they were (after an audio restart).
    Restore(Box<EngineState>),
}

impl EngineState {
    pub fn deck_mut(&mut self, id: DeckId) -> &mut DeckState {
        match self.decks.get_mut(id.index()) {
            Some(d) => d,
            None => &mut self.prepare,
        }
    }
}
