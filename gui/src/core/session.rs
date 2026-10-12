//! What each player (the decks, Prepare's player and the headphone preview) has on it,
//! held on the app's side so it outlives the engine: the loaded track, cues, loop,
//! tempo and channel settings, and the beat jump and loop sizes.
//!
//! The engine changes cues and loops itself (a pad press, a loop button) and says so
//! with an [`EngineEvent`]; [`DeckSession::apply`] folds those in, dropping any about
//! a track the deck has since replaced. If the engine had to drop events (the app fell
//! behind), [`DeckSession::resync`] takes them from a snapshot instead. Continuous state (position, tempo, faders)
//! comes from the snapshots ([`DeckSession::observe`]). After an audio restart
//! [`DeckSession::restore_state`] gives it all back to the new engine.

use std::sync::Arc;

use earshot_engine::beatgrid::Beatgrid;
use earshot_engine::decode::TrackAudio;
use earshot_engine::{
    DeckId, DeckSnap, DeckState, EngineEvent, LoadedTrack, LoopState, MAX_DECKS, MixerSnap,
    MixerState, PreviewSnap, XfSide,
};

/// The players the engine has: [`MAX_DECKS`] decks, then Prepare's player, so a
/// [`DeckId`]'s index picks its session.
pub const SLOTS: usize = MAX_DECKS + 1;

/// Every player the app uses: the first `decks` decks, then Prepare's.
pub fn players(decks: usize) -> impl Iterator<Item = DeckId> {
    DeckId::first(decks).chain([DeckId::PREPARE])
}

/// The player in slot `index` (below [`SLOTS`]).
pub fn slot(index: usize) -> DeckId {
    DeckId::new(index).unwrap_or(DeckId::PREPARE)
}

#[derive(Clone)]
pub struct DeckSession {
    // ---- what's loaded ------------------------------------------------------------------
    pub track_id: Option<i64>,
    pub title: String,
    pub artist: String,
    pub audio: Option<Arc<TrackAudio>>,
    pub grid: Option<Arc<Beatgrid>>,
    pub grid_source: String,
    /// 4 bytes per 1/150 s: peak, low, mid, high (`waveform::summarise`).
    pub waveform: Arc<Vec<u8>>,
    /// Min/max per 1/2000 s (`waveform::detail`).
    pub detail: Arc<Vec<u8>>,
    /// The load counter the track went to the engine with. Events and snapshots about
    /// any other are stale.
    pub generation: u64,
    /// Linear.
    pub auto_gain: f64,
    /// The engine has started playing this load (its `Loaded` event came back).
    pub in_engine: bool,

    // ---- performance state --------------------------------------------------------------
    /// Cues, loop, position, tempo and channel as the engine last had them. Cues and the
    /// loop follow the engine's events, the rest its snapshots. `track` is always
    /// `None` here (see [`DeckSession::restore_state`]).
    pub state: DeckState,
    /// Beats a beat jump moves.
    pub jump_size: f64,
    /// Beats the loop button loops.
    pub loop_size: f64,
}

/// Something about a deck that changed with an event and needs saving.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Change {
    Hotcues,
}

impl DeckSession {
    pub fn new(deck: DeckId) -> DeckSession {
        let mut state = DeckState::default();
        // As the engine starts its channels.
        state.channel.crossfader = XfSide::default_for(deck);
        DeckSession {
            track_id: None,
            title: String::new(),
            artist: String::new(),
            audio: None,
            grid: None,
            grid_source: String::new(),
            waveform: Arc::default(),
            detail: Arc::default(),
            generation: 0,
            auto_gain: 1.0,
            in_engine: false,
            state,
            jump_size: 4.0,
            loop_size: 4.0,
        }
    }

    /// Note a new track going to the engine. Tempo, sync, keylock and the channel stay
    /// as they were, as the engine keeps them.
    pub fn load(&mut self, track: &LoadedTrack) {
        self.track_id = Some(track.track_id);
        self.audio = Some(track.audio.clone());
        self.grid = track.grid.clone();
        self.generation = track.generation;
        self.auto_gain = track.auto_gain;
        self.in_engine = false;
        self.state.cues = track.cues;
        self.state.loop_ = LoopState::default();
        self.state.position = 0.0;
        self.state.playing = false;
    }

    /// What the engine needs to play this track again, with its grid and cues as they
    /// are now.
    pub fn loaded(&self) -> Option<Arc<LoadedTrack>> {
        Some(Arc::new(LoadedTrack {
            track_id: self.track_id?,
            generation: self.generation,
            audio: self.audio.clone()?,
            grid: self.grid.clone(),
            cues: self.state.cues,
            auto_gain: self.auto_gain,
        }))
    }

    /// Everything the engine should have for this player, track included.
    pub fn restore_state(&self) -> DeckState {
        DeckState {
            track: self.loaded(),
            ..self.state.clone()
        }
    }

    /// Fold in an event from the engine. Events about another load are dropped.
    /// Returns what changed that should be saved.
    pub fn apply(&mut self, e: &EngineEvent) -> Option<Change> {
        if e.generation() != self.generation || self.track_id.is_none() {
            return None;
        }
        let st = &mut self.state;
        match *e {
            EngineEvent::Loaded { .. } => self.in_engine = true,
            EngineEvent::Started { .. } => st.playing = true,
            EngineEvent::Stopped { .. } | EngineEvent::TrackEnded { .. } => st.playing = false,
            EngineEvent::CueSet { position, .. } => st.cues.main = Some(position),
            EngineEvent::HotcueSet { slot, position, .. } => {
                let pad = st.cues.hot.get_mut(slot as usize)?;
                if *pad == position {
                    return None;
                }
                *pad = position;
                return Some(Change::Hotcues);
            }
            EngineEvent::LoopSet { loop_, .. } => st.loop_ = loop_,
        }
        None
    }

    /// Events were lost: take what they'd have said (playing, cues, hot cues, the
    /// loop) from a snapshot of this load. Returns what changed that should be saved.
    pub fn resync(&mut self, d: &DeckSnap) -> Option<Change> {
        if self.track_id.is_none() || d.generation != self.generation {
            return None;
        }
        self.in_engine = true;
        let st = &mut self.state;
        st.playing = d.playing;
        st.cues.main = Some(d.cue_point);
        st.loop_ = LoopState {
            start: d.loop_in,
            end: d.loop_out,
            active: d.loop_active,
        };
        if st.cues.hot == d.hotcues {
            return None;
        }
        st.cues.hot = d.hotcues;
        Some(Change::Hotcues)
    }

    /// Take the continuous state from a snapshot: the channel and tempo always, the
    /// playhead only when the snapshot is of this load.
    pub fn observe(&mut self, d: &DeckSnap) {
        let st = &mut self.state;
        st.rate = d.rate;
        st.range = d.range;
        st.fader = d.fader;
        st.fader_live = d.fader_live;
        st.sync = d.sync;
        st.quantize = d.quantize;
        st.keylock = d.keylock;
        let ch = &mut st.channel;
        ch.volume = d.volume;
        ch.trim = d.pregain;
        ch.eq = d.eq;
        ch.filter = d.filter;
        ch.pfl = d.pfl;
        ch.crossfader = d.crossfader;
        if self.track_id.is_some() && d.generation == self.generation {
            st.position = d.position;
            st.playing = d.playing;
        }
    }
}

/// The mixer's settings, as the engine last had them.
pub fn observe_mixer(m: &MixerSnap) -> MixerState {
    MixerState {
        crossfader: m.crossfader,
        main_gain: m.main_gain,
        balance: m.balance,
        head_gain: m.head_gain,
        head_mix: m.head_mix,
    }
}

/// The headphone preview player.
#[derive(Default, Clone)]
pub struct PreviewSession {
    /// Bumped on every preview request, so a slow decode can't start a stale one.
    pub seq: u64,
    pub track_id: Option<i64>,
    pub audio: Option<Arc<TrackAudio>>,
    /// Linear.
    pub gain: f64,
    pub position: f64,
    pub playing: bool,
}

impl PreviewSession {
    pub fn observe(&mut self, p: &PreviewSnap) {
        if self.audio.is_some() {
            self.position = p.position;
            self.playing = p.playing;
        }
    }

    pub fn stop(&mut self) {
        self.seq += 1;
        self.track_id = None;
        self.audio = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use earshot_engine::CueSet;

    fn track(generation: u64, hot: [Option<f64>; 8]) -> LoadedTrack {
        LoadedTrack {
            track_id: 7,
            generation,
            audio: Arc::new(TrackAudio {
                samples: vec![0.0; 2 * 4410],
                sample_rate: 44100,
            }),
            grid: None,
            cues: CueSet { main: None, hot },
            auto_gain: 0.5,
        }
    }

    fn deck() -> DeckId {
        DeckId::new(1).unwrap()
    }

    #[test]
    fn a_resync_takes_cues_and_the_loop_from_a_snapshot_of_this_load() {
        let mut s = DeckSession::new(deck());
        s.load(&track(3, [None; 8]));
        let mut d = DeckSnap {
            track_id: Some(7),
            generation: 3,
            playing: true,
            cue_point: 2.0,
            loop_in: Some(4.0),
            loop_out: Some(6.0),
            loop_active: true,
            ..DeckSnap::default()
        };
        d.hotcues[1] = Some(8.0);
        assert_eq!(s.resync(&d), Some(Change::Hotcues));
        assert!(s.in_engine && s.state.playing);
        assert_eq!(s.state.cues.main, Some(2.0));
        assert_eq!(s.state.cues.hot[1], Some(8.0));
        assert_eq!(s.state.loop_.bounds(), Some((4.0, 6.0)));
        // Nothing new to save the second time; another load's snapshot is ignored.
        assert_eq!(s.resync(&d), None);
        d.generation = 2;
        d.hotcues[1] = None;
        assert_eq!(s.resync(&d), None);
        assert_eq!(s.state.cues.hot[1], Some(8.0));
    }

    #[test]
    fn events_update_cues_and_loops() {
        let mut s = DeckSession::new(deck());
        let mut hot = [None; 8];
        hot[0] = Some(1.0);
        s.load(&track(3, hot));
        assert_eq!(s.state.cues.hot[0], Some(1.0));
        let d = deck();
        let set = |slot, position| EngineEvent::HotcueSet {
            deck: d,
            generation: 3,
            slot,
            position,
        };
        assert_eq!(s.apply(&set(2, Some(9.5))), Some(Change::Hotcues));
        // The same again changes nothing worth saving.
        assert_eq!(s.apply(&set(2, Some(9.5))), None);
        assert_eq!(s.apply(&set(0, None)), Some(Change::Hotcues));
        assert_eq!(s.state.cues.hot[..3], [None, None, Some(9.5)]);
        // A slot the engine doesn't have is ignored, not a panic.
        assert_eq!(s.apply(&set(200, Some(1.0))), None);

        s.apply(&EngineEvent::CueSet {
            deck: d,
            generation: 3,
            position: 4.0,
        });
        let l = LoopState {
            start: Some(2.0),
            end: Some(4.0),
            active: true,
        };
        s.apply(&EngineEvent::LoopSet {
            deck: d,
            generation: 3,
            loop_: l,
        });
        s.apply(&EngineEvent::Loaded {
            deck: d,
            generation: 3,
        });
        assert_eq!(s.state.cues.main, Some(4.0));
        assert_eq!(s.state.loop_, l);
        assert!(s.in_engine);
    }

    #[test]
    fn stale_events_are_dropped() {
        let mut s = DeckSession::new(deck());
        let d = deck();
        // Nothing loaded: nothing to change.
        let ev = EngineEvent::HotcueSet {
            deck: d,
            generation: 0,
            slot: 0,
            position: Some(1.0),
        };
        assert_eq!(s.apply(&ev), None);
        s.load(&track(4, [None; 8]));
        // From the track that was on the deck before (generation 3).
        for e in [
            EngineEvent::HotcueSet {
                deck: d,
                generation: 3,
                slot: 0,
                position: Some(1.0),
            },
            EngineEvent::CueSet {
                deck: d,
                generation: 3,
                position: 8.0,
            },
            EngineEvent::Started {
                deck: d,
                generation: 3,
            },
            EngineEvent::Loaded {
                deck: d,
                generation: 3,
            },
        ] {
            assert_eq!(s.apply(&e), None);
        }
        assert_eq!(s.state.cues, CueSet::default());
        assert!(!s.state.playing && !s.in_engine);
    }

    #[test]
    fn snapshots_of_another_load_leave_the_playhead() {
        let mut s = DeckSession::new(deck());
        s.load(&track(5, [None; 8]));
        let mut snap = DeckSnap {
            generation: 4,
            position: 60.0,
            playing: true,
            volume: 0.5,
            ..DeckSnap::default()
        };
        s.observe(&snap);
        assert_eq!((s.state.position, s.state.playing), (0.0, false));
        assert_eq!(s.state.channel.volume, 0.5, "the channel isn't per track");
        snap.generation = 5;
        s.observe(&snap);
        assert_eq!((s.state.position, s.state.playing), (60.0, true));
    }

    #[test]
    fn restore_state_carries_the_track_and_its_cues() {
        let mut s = DeckSession::new(deck());
        let mut hot = [None; 8];
        hot[3] = Some(3.0);
        s.load(&track(9, hot));
        s.apply(&EngineEvent::HotcueSet {
            deck: deck(),
            generation: 9,
            slot: 1,
            position: Some(2.0),
        });
        s.jump_size = 16.0;
        let st = s.restore_state();
        let t = st.track.expect("the track");
        assert_eq!((t.track_id, t.generation, t.auto_gain), (7, 9, 0.5));
        assert_eq!(t.cues.hot[1], Some(2.0), "cues as they are now");
        assert_eq!(st.cues.hot[3], Some(3.0));
        assert_eq!(st.channel.crossfader, XfSide::Right);
    }
}
