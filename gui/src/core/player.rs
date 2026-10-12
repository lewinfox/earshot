//! The decks' side of the app: the audio engine, the [`DeckSession`] of each player,
//! loading tracks and editing their beatgrids. Shared between the UI thread, the
//! loaders and the analysis threads behind one mutex. MIDI doesn't take it: it has its
//! own ring into the engine ([`Inner::midi`]). The UI reads playback state from the
//! engine's snapshots ([`Monitor`]) without taking it either.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use earshot_engine::beatgrid::Beatgrid;
use earshot_engine::decode;
use earshot_engine::{
    Command, CueSet, DeckId, Engine, EngineSnapshot, EngineState, LoadedTrack, MAX_DECKS,
    MixerState, Monitor, OutputInfo, OutputOptions, PreviewCmd, Routing, Set, energy, loudness,
    waveform,
};
use serde::{Deserialize, Serialize};

use super::analysis::Mode;
use super::command::Lane;
use super::library::Library;
use super::session::{self, Change, DeckSession, PreviewSession, SLOTS};
use super::store::{GridSource, Store, StoreCmd};

/// How many decks the engine runs (it can run up to `earshot_engine::MAX_DECKS`).
pub const DECKS: usize = 2;

/// The engine's id for session `index` (a deck below [`DECKS`], or [`MAX_DECKS`] for
/// Prepare's player).
pub fn deck_id(index: usize) -> Result<DeckId, String> {
    match index {
        MAX_DECKS => Ok(DeckId::PREPARE),
        _ => DeckId::new(index)
            .filter(|_| index < DECKS)
            .ok_or_else(|| format!("no deck {index}")),
    }
}

pub struct Inner {
    pub engine: Option<Engine>,
    /// Every player's session, indexed by `DeckId::index()`: the decks, then Prepare's.
    pub decks: [DeckSession; SLOTS],
    pub preview: PreviewSession,
    pub mixer: MixerState,
    pub sync_leader: Option<DeckId>,
    pub generation: u64,
    /// Output routing; `None` means the default for the device's channel count.
    pub routing: Option<Routing>,
    /// MIDI's own ring into the engine, refilled whenever audio (re)starts.
    pub midi: Lane,
}

impl Inner {
    pub fn send(&mut self, cmd: Command) -> Result<(), String> {
        self.engine
            .as_mut()
            .ok_or("audio is not running")?
            .send(cmd)
    }

    pub fn monitor(&self) -> Option<Monitor> {
        self.engine.as_ref().map(|e| e.monitor().clone())
    }

    pub fn snapshot(&self) -> Option<EngineSnapshot> {
        self.engine.as_ref().map(|e| e.snapshot())
    }

    pub fn session(&self, deck: DeckId) -> &DeckSession {
        &self.decks[deck.index()]
    }

    pub fn session_mut(&mut self, deck: DeckId) -> &mut DeckSession {
        &mut self.decks[deck.index()]
    }

    /// Catch up with the engine: take its latest snapshot and fold in its events.
    /// Returns the snapshot and the decks whose hot cues changed.
    fn catch_up(&mut self) -> (Option<EngineSnapshot>, Vec<DeckId>) {
        let mut changed = vec![];
        let Some(engine) = self.engine.as_mut() else {
            return (None, changed);
        };
        let snap = engine.snapshot();
        for (i, s) in self.decks.iter_mut().enumerate() {
            s.observe(snap.decks.get(i).unwrap_or(&snap.prepare));
        }
        self.mixer = session::observe_mixer(&snap.mixer);
        self.sync_leader = snap.mixer.sync_leader;
        self.preview.observe(&snap.preview);
        while let Some(e) = engine.next_event() {
            let deck = e.deck();
            if self.decks[deck.index()].apply(&e) == Some(Change::Hotcues)
                && !changed.contains(&deck)
            {
                changed.push(deck);
            }
        }
        (Some(snap), changed)
    }

    /// Everything to put back after an audio restart.
    fn engine_state(&self) -> EngineState {
        let mut st = EngineState {
            mixer: self.mixer,
            sync_leader: self.sync_leader,
            ..EngineState::default()
        };
        for id in session::players(MAX_DECKS) {
            *st.deck_mut(id) = self.session(id).restore_state();
        }
        st
    }
}

pub type Player = Arc<Mutex<Inner>>;

pub fn new_player() -> Player {
    Arc::new(Mutex::new(Inner {
        engine: None,
        decks: std::array::from_fn(|i| DeckSession::new(session::slot(i))),
        preview: PreviewSession::default(),
        mixer: MixerState::default(),
        sync_leader: None,
        generation: 0,
        routing: None,
        midi: Lane::default(),
    }))
}

/// Once a frame: bring the sessions up to date with the engine, and save any hot cues
/// that changed. Returns the engine's latest state.
pub fn poll(player: &Player, store: &Store) -> Option<EngineSnapshot> {
    let mut inner = player.lock().unwrap();
    let (snap, changed) = inner.catch_up();
    for deck in changed {
        let s = inner.session(deck);
        if let Some(track) = s.track_id {
            store.send(StoreCmd::SaveHotcues {
                track,
                cues: s.state.cues.hot,
            });
        }
    }
    snap
}

/// Audio settings kept between runs. Environment variables override them.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct AudioSettings {
    /// Device id (or a substring of its name); `None` = the system default.
    pub device: Option<String>,
    pub buffer_frames: Option<u32>,
    pub routing: Option<Routing>,
}

/// Output options from saved settings, with environment variables taking precedence.
pub fn output_options(saved: &AudioSettings) -> OutputOptions {
    OutputOptions {
        device: std::env::var("EARSHOT_AUDIO_DEVICE")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| saved.device.clone()),
        buffer_frames: Some(
            std::env::var("EARSHOT_BUFFER")
                .ok()
                .and_then(|s| s.parse().ok())
                .or(saved.buffer_frames)
                .unwrap_or(256),
        ),
        null: std::env::var("EARSHOT_NULL_AUDIO").is_ok_and(|v| v == "1"),
    }
}

/// Change where master and headphones go, without restarting audio.
pub fn set_routing(player: &Player, routing: Routing) -> Result<(), String> {
    let mut inner = player.lock().unwrap();
    inner.routing = Some(routing);
    inner.send(Command::SetRouting(routing))
}

/// The routing in use: the chosen one, or the default for this device.
pub fn routing(player: &Player, channels: u16) -> Routing {
    player
        .lock()
        .unwrap()
        .routing
        .unwrap_or(Routing::default_for(channels))
}

/// Play a library track on the headphones only, from `start` (0..1 of the way in).
/// Blocking (decoding takes about a second, unless the track is on a deck): call it
/// from a background thread.
pub fn preview(player: &Player, store: &Store, id: i64, start: f64) -> Result<(), String> {
    let (seq, on_deck) = {
        let mut inner = player.lock().unwrap();
        inner.preview.seq += 1;
        let on_deck = inner
            .decks
            .iter()
            .find(|d| d.track_id == Some(id))
            .and_then(|d| d.audio.clone());
        (inner.preview.seq, on_deck)
    };
    let audio = match on_deck {
        Some(a) => a,
        None => {
            let path = store.reader.track_path(id).map_err(|e| e.to_string())?;
            Arc::new(decode::decode_file(&path)?.0)
        }
    };
    // At the same auto-gain as on a deck.
    let gain = match store.reader.loudness(id).map_err(|e| e.to_string())? {
        Some(l) => l.auto_gain(),
        None => loudness::measure(&audio).auto_gain(),
    };
    let mut inner = player.lock().unwrap();
    if inner.preview.seq != seq {
        return Ok(()); // another preview was asked for meanwhile
    }
    let start = start * audio.duration();
    inner.preview = PreviewSession {
        seq,
        track_id: Some(id),
        audio: Some(audio.clone()),
        gain,
        position: start,
        playing: true,
    };
    inner.send(Command::Preview(PreviewCmd::Start { audio, start, gain }))
}

/// Pause or resume the preview.
pub fn play_preview(player: &Player, on: bool) -> Result<(), String> {
    player
        .lock()
        .unwrap()
        .send(Command::Preview(PreviewCmd::Play(Set::from_bool(on))))
}

/// Move the preview to `at` (0..1 of the way in).
pub fn seek_preview(player: &Player, at: f64) -> Result<(), String> {
    let mut inner = player.lock().unwrap();
    let d = inner.snapshot().map_or(0.0, |s| s.preview.duration);
    inner.send(Command::Preview(PreviewCmd::Seek(at * d)))
}

pub fn stop_preview(player: &Player) -> Result<(), String> {
    let mut inner = player.lock().unwrap();
    inner.preview.stop();
    inner.send(Command::Preview(PreviewCmd::Stop))
}

/// Start (or restart) audio. On a restart every player and the mixer come back as
/// their sessions have them: tracks, position, cues, loops, tempo, sync, EQ and faders,
/// and the preview where it was.
pub fn start_engine(player: &Player, opts: &OutputOptions) -> Result<OutputInfo, String> {
    let mut inner = player.lock().unwrap();
    // Catch up with the old engine before it goes, then stop it so the device is free.
    inner.catch_up();
    inner.midi.set(None);
    inner.engine = None;
    let mut engine = Engine::start(opts, DECKS)?;
    engine.send(Command::Restore(Box::new(inner.engine_state())))?;
    let p = &inner.preview;
    if let Some(audio) = p.audio.clone() {
        let (start, gain) = (p.position, p.gain);
        engine.send(Command::Preview(PreviewCmd::Start { audio, start, gain }))?;
        if !p.playing {
            engine.send(Command::Preview(PreviewCmd::Play(Set::Off)))?;
        }
    }
    let info = engine.info().clone();
    engine.send(Command::SetRouting(
        inner.routing.unwrap_or(Routing::default_for(info.channels)),
    ))?;
    inner.midi.set(engine.sender());
    inner.engine = Some(engine);
    Ok(info)
}

/// Decode a library track and put it on a deck. Blocking (about a second): call it
/// from a background thread.
pub fn load_track(player: &Player, lib: &Library, deck: DeckId, id: i64) -> Result<(), String> {
    let reader = &lib.store.reader;
    let track = reader.track(id).map_err(|e| e.to_string())?;
    let path = reader.track_path(id).map_err(|e| e.to_string())?;
    let (audio, tags) = decode::decode_file(&path)?;
    let stored_level = reader.loudness(id).map_err(|e| e.to_string())?;
    let stored = reader.grid(id).map_err(|e| e.to_string())?;
    let cached_wave = reader
        .waveform(id, waveform::BINS_PER_SECOND)
        .map_err(|e| e.to_string())?;
    // Measured once (it's quick) if analysis hasn't yet: auto-gain needs it.
    let level = match stored_level {
        Some(l) => l,
        None => {
            let l = loudness::measure(&audio);
            lib.store.send(StoreCmd::SaveFeatures {
                track: id,
                features: vec![
                    ("energy", energy::energy(&audio)),
                    ("lufs", l.lufs),
                    ("peak", l.peak),
                ],
            });
            l
        }
    };
    let analysed = stored.is_some();
    let (grid, source) = match stored {
        Some((g, src)) => (Some(g), src.label().to_string()),
        None => grid_for_file(&path, &tags, audio.duration()),
    };
    // The library's title and artist, so the deck agrees with the track list.
    let (title, artist) = (track.title, track.artist);

    let audio = Arc::new(audio);
    let wave = match cached_wave {
        Some(w) => w,
        None => {
            // Saved, so the track list can show its thumbnail from now on.
            let w = waveform::summarise(&audio);
            lib.store.send(StoreCmd::SaveWaveform {
                track: id,
                data: w.clone(),
            });
            w
        }
    };
    let detail = waveform::detail(&audio);
    // No analysed or hand-fixed grid yet: analyse in the background. The deck plays
    // with the placeholder grid meanwhile and is updated when the result arrives.
    if !analysed && source != GridSource::Sidecar.label() {
        lib.analyser
            .queue(id, Some(audio.clone()), false, Mode::Static);
    }
    let hotcues = reader.hotcues(id).map_err(|e| e.to_string())?;

    let mut inner = player.lock().unwrap();
    inner.generation += 1;
    let track = Arc::new(LoadedTrack {
        track_id: id,
        generation: inner.generation,
        audio,
        grid: grid.map(Arc::new),
        cues: CueSet {
            main: None,
            hot: hotcues,
        },
        auto_gain: level.auto_gain(),
    });
    // The session first: with no audio running, it's still there for a restart.
    let session = inner.session_mut(deck);
    session.load(&track);
    session.title = title;
    session.artist = artist;
    session.grid_source = source;
    session.waveform = Arc::new(wave);
    session.detail = Arc::new(detail);
    inner.send(Command::Load { deck, track })
}

/// Whole-grid edits from the grid editor.
#[derive(Debug, Clone, Copy)]
pub enum GridOp {
    SetBpm(f64),
    /// Milliseconds; negative is earlier.
    Nudge(f64),
    /// Shift the grid so a beat lands exactly on the playhead, and make it the 1.
    AnchorHere,
    Halve,
    Double,
}

/// Edit a deck's grid. The deck gets it at once; the Store saves it afterwards (as a
/// new grid row, with the track's BPM), outside the player lock.
pub fn edit_grid(player: &Player, store: &Store, deck: DeckId, op: GridOp) -> Result<(), String> {
    let mut inner = player.lock().unwrap();
    let playhead = inner.snapshot().map_or(0.0, |s| s.deck(deck).position);
    let slot = inner.session(deck);
    let duration = slot.audio.as_ref().ok_or("deck is empty")?.duration();
    let grid = match &slot.grid {
        Some(g) => g.clone(),
        // No grid yet: start a 120 BPM one with its 1 at the playhead.
        None => Arc::new(Beatgrid::constant(120.0, playhead, duration).map_err(|e| e.to_string())?),
    };
    let nearest = grid
        .beat_at(playhead)
        .round()
        .clamp(0.0, (grid.len() - 1) as f64) as usize;
    let edited = match op {
        GridOp::SetBpm(bpm) if bpm > 20.0 && bpm < 400.0 => grid.with_bpm(bpm, nearest, duration),
        GridOp::SetBpm(_) => return Err("BPM out of range".into()),
        GridOp::Nudge(ms) => grid.nudged(ms / 1000.0, duration),
        GridOp::AnchorHere => grid
            .nudged(playhead - grid.times()[nearest], duration)
            .map(|g| {
                // Re-fitting may add beats at the start, so find the beat again.
                let i = g.beat_at(playhead).round().clamp(0.0, (g.len() - 1) as f64) as usize;
                g.with_downbeat(i)
            }),
        GridOp::Halve => grid.halved(duration),
        GridOp::Double => grid.doubled(duration),
    }
    .map_err(|e| e.to_string())?;
    let edited = Some(Arc::new(edited));
    let generation = slot.generation;
    inner.send(Command::SetGrid {
        deck,
        generation,
        grid: edited.clone(),
    })?;
    let session = inner.session_mut(deck);
    session.grid = edited.clone();
    session.grid_source = GridSource::User.label().into();
    let track = session.track_id;
    drop(inner);
    // Saved, so the fix survives a restart and re-analysis on load doesn't undo it.
    if let (Some(track), Some(grid)) = (track, edited) {
        store.send(StoreCmd::SaveUserGrid { track, grid });
    }
    Ok(())
}

/// tempo-lock's `analyse` JSON, saved next to the audio as `<file>.beats.json`.
#[derive(Deserialize)]
struct TempoLockAnalysis {
    beats: Vec<f64>,
    beat_index: Vec<i64>,
    is_downbeat: Vec<bool>,
}

fn sidecar_path(path: &Path) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(".beats.json");
    PathBuf::from(p)
}

fn grid_for_file(
    path: &Path,
    tags: &decode::TrackTags,
    duration: f64,
) -> (Option<Beatgrid>, String) {
    if let Ok(text) = std::fs::read_to_string(sidecar_path(path)) {
        match serde_json::from_str::<TempoLockAnalysis>(&text)
            .map_err(|e| e.to_string())
            .and_then(|a| {
                Beatgrid::from_indexed(&a.beats, &a.beat_index, &a.is_downbeat)
                    .map_err(|e| e.to_string())
            }) {
            Ok(g) => return (Some(g), GridSource::Sidecar.label().into()),
            Err(e) => eprintln!("{}: bad beats sidecar: {e}", path.display()),
        }
    }
    match tags.bpm.filter(|b| *b > 40.0) {
        Some(bpm) => (
            Beatgrid::constant(bpm, 0.0, duration).ok(),
            "BPM tag (placeholder)".into(),
        ),
        None => (None, "none".into()),
    }
}

/// A null-output engine and helpers for tests that need one running.
#[cfg(test)]
pub(crate) mod testing {
    use std::time::{Duration, Instant};

    use earshot_engine::decode::TrackAudio;

    use super::*;

    pub fn null() -> OutputOptions {
        OutputOptions {
            device: None,
            buffer_frames: Some(256),
            null: true,
        }
    }

    /// A player with audio running on the null output.
    pub fn running() -> Player {
        let p = new_player();
        start_engine(&p, &null()).unwrap();
        p
    }

    /// Put 30 s of silence on `deck` as track `track_id`, as `load_track` would.
    pub fn put_on(p: &Player, deck: DeckId, track_id: i64) {
        let mut inner = p.lock().unwrap();
        inner.generation += 1;
        let track = Arc::new(LoadedTrack {
            track_id,
            generation: inner.generation,
            audio: Arc::new(TrackAudio {
                samples: vec![0.0; 2 * 44100 * 30],
                sample_rate: 44100,
            }),
            grid: None,
            cues: CueSet::default(),
            auto_gain: 0.5,
        });
        inner.session_mut(deck).load(&track);
        inner.send(Command::Load { deck, track }).unwrap();
    }

    /// Catch up with the engine until `ok` holds (or fail after a few seconds).
    pub fn wait(p: &Player, ok: impl Fn(&EngineSnapshot) -> bool) -> EngineSnapshot {
        let t = Instant::now();
        loop {
            let snap = p.lock().unwrap().catch_up().0.expect("audio running");
            if ok(&snap) {
                return snap;
            }
            assert!(t.elapsed() < Duration::from_secs(5), "timed out waiting");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[cfg(test)]
mod tests {
    use earshot_engine::{ChannelParam, DeckCmd, EQ_LOW, LoopState, MasterParam, ParamId, Press};

    use super::testing::*;
    use super::*;

    #[test]
    fn a_restart_puts_every_player_back() {
        let p = running();
        let a = DeckId::new(0).unwrap();
        let b = DeckId::new(1).unwrap();
        let prep = DeckId::PREPARE;
        put_on(&p, a, 11);
        put_on(&p, prep, 12);
        {
            let mut inner = p.lock().unwrap();
            for c in [
                Command::Deck(a, DeckCmd::Seek(10.0)),
                Command::Deck(
                    a,
                    DeckCmd::Hotcue {
                        slot: 2,
                        press: Press::Down,
                    },
                ),
                Command::Deck(
                    a,
                    DeckCmd::Hotcue {
                        slot: 2,
                        press: Press::Up,
                    },
                ),
                Command::Deck(
                    a,
                    DeckCmd::SetLoop {
                        start: 4.0,
                        end: 6.0,
                    },
                ),
                Command::Deck(a, DeckCmd::Keylock(Set::On)),
                Command::Param(ParamId::Channel(a, ChannelParam::Volume), 0.3),
                Command::Param(ParamId::Channel(b, ChannelParam::Eq(EQ_LOW)), 0.0),
                Command::Param(ParamId::Master(MasterParam::Crossfader), 0.5),
                Command::Deck(prep, DeckCmd::SetCue(Some(7.0))),
                Command::SetSyncLeader(Some(b)),
            ] {
                inner.send(c).unwrap();
            }
            inner.session_mut(a).jump_size = 16.0;
        }
        let before = wait(&p, |s| {
            s.decks[0].loop_active
                && s.decks[0].hotcues[2].is_some()
                && s.prepare.cue_point == 7.0
                && s.mixer.sync_leader == Some(b)
        });
        // The sessions heard about the cues and the loop from the engine's events.
        {
            let inner = p.lock().unwrap();
            let s = inner.session(a);
            assert_eq!(s.state.cues.hot, before.decks[0].hotcues);
            assert_eq!(
                s.state.loop_,
                LoopState {
                    start: Some(4.0),
                    end: Some(6.0),
                    active: true
                }
            );
            assert_eq!(inner.session(prep).state.cues.main, Some(7.0));
        }

        start_engine(&p, &null()).unwrap();
        let after = wait(&p, |s| {
            s.decks[0].track_id == Some(11) && s.prepare.track_id == Some(12)
        });
        let (was, now) = (&before.decks[0], &after.decks[0]);
        assert_eq!(now.generation, was.generation);
        assert_eq!(now.hotcues, was.hotcues);
        assert_eq!(now.cue_point, was.cue_point);
        assert_eq!(
            (now.loop_in, now.loop_out, now.loop_active),
            (Some(4.0), Some(6.0), true)
        );
        assert!((now.position - was.position).abs() < 0.1);
        assert!(now.keylock);
        assert_eq!(now.volume, 0.3);
        assert_eq!(now.auto_gain, 0.5);
        assert_eq!(after.decks[1].eq[EQ_LOW], 0.0);
        assert_eq!(after.mixer.crossfader, 0.5);
        assert_eq!(after.mixer.sync_leader, Some(b));
        assert_eq!(after.prepare.cue_point, 7.0);
        assert_eq!(after.prepare.generation, before.prepare.generation);
        // The crossfader sides are as they were, not reset to "thru".
        assert_eq!(now.crossfader, was.crossfader);
        assert_eq!(after.decks[1].crossfader, before.decks[1].crossfader);
        assert_eq!(p.lock().unwrap().session(a).jump_size, 16.0);
    }

    #[test]
    fn deck_ids_for_sessions() {
        assert_eq!(deck_id(0), Ok(DeckId::new(0).unwrap()));
        assert_eq!(deck_id(MAX_DECKS), Ok(DeckId::PREPARE));
        // Decks the engine isn't running.
        assert!(deck_id(DECKS).is_err());
        let inner = new_player();
        assert_eq!(inner.lock().unwrap().decks.len(), MAX_DECKS + 1);
    }
}
