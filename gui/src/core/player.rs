//! The decks' side of the app: the audio engine, the [`DeckSession`] of each player,
//! loading tracks and editing their beatgrids. Shared between the UI thread, the
//! loaders and the analysis threads behind one mutex. MIDI doesn't take it: it has its
//! own ring into the engine ([`Inner::midi`]). The UI reads playback state from the
//! engine's snapshots ([`Monitor`]) without taking it either.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex};

use earshot_engine::beatgrid::{Beatgrid, GridModel};
use earshot_engine::decode;
use earshot_engine::decode::TrackAudio;
use earshot_engine::media::Location;
use earshot_engine::{
    Command, CueSet, DeckId, Engine, EngineSnapshot, EngineState, HotcueMode, LoadedTrack,
    MAX_DECKS, MixerState, Monitor, OutputInfo, OutputOptions, PreviewCmd, Routing, Set, energy,
    loudness, quick, waveform,
};
use serde::{Deserialize, Serialize};

use super::analysis::Mode;
use super::command::Lane;
use super::library::Library;
use super::session::{self, Change, DeckSession, PreviewSession, SLOTS};
use super::store::{GridSource, Reader, Store, StoreCmd, StoreEvent};
use super::{external, outside};

/// How many decks the engine runs: all it can. Perform shows two or four of them.
pub const DECKS: usize = MAX_DECKS;

/// The engine's id for session `index` (a deck below [`DECKS`]).
pub fn deck_id(index: usize) -> Result<DeckId, String> {
    DeckId::new(index)
        .filter(|_| index < DECKS)
        .ok_or_else(|| format!("no deck {index}"))
}

pub struct Inner {
    pub engine: Option<Engine>,
    /// Every player's session, indexed by `DeckId::index()`: the decks, then Prepare's.
    pub decks: [DeckSession; SLOTS],
    pub preview: PreviewSession,
    pub mixer: MixerState,
    pub sync_leader: Option<DeckId>,
    /// What hot cues do on a stopped deck (a setting; kept for audio restarts).
    pub hotcue_mode: HotcueMode,
    /// The decks effects can go on: the ones shown (kept for audio restarts).
    pub fx_decks: usize,
    /// Cue a newly loaded track on its first beat, else at its start (a setting).
    pub cue_first_beat: bool,
    pub generation: u64,
    /// The newest load asked for on each player (see [`request_load`]). Only that
    /// one may put its track on.
    pub load_tickets: [u64; SLOTS],
    /// Output routing; `None` means the default for the device's channel count.
    pub routing: Option<Routing>,
    /// MIDI's own ring into the engine, refilled whenever audio (re)starts.
    pub midi: Lane,
    /// The engine's count of events it had to drop, as last seen: when it grows, the
    /// sessions are brought up to date from a snapshot.
    dropped_seen: u64,
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

    /// The latest snapshot, if a sound card is playing (not the null output): what
    /// `core::audio_watch` watches.
    pub fn device_snapshot(&self) -> Option<EngineSnapshot> {
        self.engine
            .as_ref()
            .filter(|e| !e.info().null)
            .map(|e| e.snapshot())
    }

    pub fn session(&self, deck: DeckId) -> &DeckSession {
        &self.decks[deck.index()]
    }

    pub fn session_mut(&mut self, deck: DeckId) -> &mut DeckSession {
        &mut self.decks[deck.index()]
    }

    /// Catch up with the engine: take its latest snapshot and fold in its events.
    /// Returns the snapshot and what changed on which deck, to save.
    fn catch_up(&mut self) -> (Option<EngineSnapshot>, Vec<(DeckId, Change)>) {
        let mut changed = vec![];
        let Some(engine) = self.engine.as_mut() else {
            return (None, changed);
        };
        let snap = engine.snapshot();
        for (i, s) in self.decks.iter_mut().enumerate() {
            s.observe(slot_snap(&snap, i));
        }
        self.mixer = session::observe_mixer(&snap.mixer);
        self.sync_leader = snap.mixer.sync_leader;
        self.preview.observe(&snap.preview);
        while let Some(e) = engine.next_event() {
            let deck = e.deck();
            if let Some(c) = self.decks[deck.index()].apply(&e)
                && !changed.contains(&(deck, c))
            {
                changed.push((deck, c));
            }
        }
        // Events were dropped (the ring was full): a cue or loop change may be among
        // them. Take every player's from a snapshot newer than all the events.
        let dropped = engine.monitor().stats().dropped_events.load(Relaxed);
        if dropped == self.dropped_seen {
            return (Some(snap), changed);
        }
        eprintln!(
            "engine: {} events dropped; resyncing the players",
            dropped - self.dropped_seen
        );
        self.dropped_seen = dropped;
        let snap = engine.snapshot();
        for (i, s) in self.decks.iter_mut().enumerate() {
            let d = slot_snap(&snap, i);
            s.observe(d);
            let deck = session::slot(i);
            for c in s.resync(d) {
                if !changed.contains(&(deck, c)) {
                    changed.push((deck, c));
                }
            }
        }
        (Some(snap), changed)
    }

    /// Everything to put back after an audio restart.
    fn engine_state(&self) -> EngineState {
        let mut st = EngineState {
            mixer: self.mixer,
            sync_leader: self.sync_leader,
            hotcue_mode: self.hotcue_mode,
            ..EngineState::default()
        };
        for id in session::players(MAX_DECKS) {
            *st.deck_mut(id) = self.session(id).restore_state();
        }
        st
    }
}

/// Slot `i`'s deck in a snapshot.
fn slot_snap(snap: &EngineSnapshot, i: usize) -> &earshot_engine::DeckSnap {
    &snap.decks[i]
}

pub type Player = Arc<Mutex<Inner>>;

pub fn new_player() -> Player {
    Arc::new(Mutex::new(Inner {
        engine: None,
        decks: std::array::from_fn(|i| DeckSession::new(session::slot(i))),
        preview: PreviewSession::default(),
        mixer: MixerState::default(),
        sync_leader: None,
        hotcue_mode: HotcueMode::default(),
        fx_decks: DECKS,
        cue_first_beat: true,
        generation: 0,
        load_tickets: [0; SLOTS],
        routing: None,
        midi: Lane::default(),
        dropped_seen: 0,
    }))
}

/// Once a frame: bring the sessions up to date with the engine, and save any hot cues
/// and main cues that changed. Returns the engine's latest state.
pub fn poll(player: &Player, store: &Store) -> Option<EngineSnapshot> {
    let mut inner = player.lock().unwrap();
    let (snap, changed) = inner.catch_up();
    for (deck, change) in changed {
        let s = inner.session(deck);
        let Some(track) = s.track_id else { continue };
        match change {
            Change::Hotcues => store.send(StoreCmd::SaveHotcues {
                track,
                cues: s.state.cues.hot,
            }),
            Change::MainCue => {
                if let Some(at) = s.state.cues.main {
                    store.send(StoreCmd::SaveMainCue { track, at });
                }
            }
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
    /// The headphones on a device of their own (laptop-only: master on the speakers,
    /// headphones on a USB dongle). `None`: on the master's device. Only under
    /// PipeWire ([`earshot_engine::output::can_split`]).
    #[serde(default)]
    pub phones_device: Option<String>,
}

/// The buffer size when none is chosen, in frames: about 5 ms, two of them queued.
/// Under PipeWire it needs PipeWire's chunks no bigger
/// ([`earshot_engine::output::ask_pipewire_for_small_chunks`]).
pub const DEFAULT_BUFFER: u32 = 256;

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
                .unwrap_or(DEFAULT_BUFFER),
        ),
        null: std::env::var("EARSHOT_NULL_AUDIO").is_ok_and(|v| v == "1"),
        phones_device: saved.phones_device.clone(),
    }
}

/// What hot cues do on a stopped deck, from now on (and after an audio restart).
pub fn set_hotcue_mode(player: &Player, mode: HotcueMode) -> Result<(), String> {
    let mut inner = player.lock().unwrap();
    inner.hotcue_mode = mode;
    inner.send(Command::SetHotcueMode(mode))
}

/// Effects go only on the first `n` decks (the ones shown), from now on.
pub fn set_fx_decks(player: &Player, n: usize) -> Result<(), String> {
    let mut inner = player.lock().unwrap();
    inner.fx_decks = n;
    inner.send(Command::SetFxDecks(n))
}

/// Cue newly loaded tracks on their first beat, else at their start, from now on.
pub fn set_cue_first_beat(player: &Player, on: bool) {
    player.lock().unwrap().cue_first_beat = on;
}

/// The cue a load starts on: the grid line nearest the first beat if the setting
/// says so (`None`, the grid's first line, if that isn't known), else the start.
fn load_cue(player: &Player, grid: Option<&Beatgrid>, first_beat: Option<f64>) -> Option<f64> {
    if player.lock().unwrap().cue_first_beat {
        initial_cue(grid, first_beat)
    } else {
        Some(0.0)
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
            let at = match store.outside.get(id) {
                Some(t) => Location::File(t.file),
                None => store.reader.track_location(id).map_err(|e| e.to_string())?,
            };
            Arc::new(decode::decode(&at)?.0)
        }
    };
    // At the same auto-gain as on a deck.
    let stored = match outside::is_outside(id) {
        true => None,
        false => store.reader.loudness(id).map_err(|e| e.to_string())?,
    };
    let gain = match stored {
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
    // A new engine counts its own dropped events.
    inner.dropped_seen = 0;
    engine.send(Command::Restore(Box::new(inner.engine_state())))?;
    engine.send(Command::SetFxDecks(inner.fx_decks))?;
    let p = &inner.preview;
    if let Some(audio) = p.audio.clone() {
        let (start, gain) = (p.position, p.gain);
        engine.send(Command::Preview(PreviewCmd::Start { audio, start, gain }))?;
        if !p.playing {
            engine.send(Command::Preview(PreviewCmd::Play(Set::Off)))?;
        }
    }
    let info = engine.info().clone();
    // Not chosen yet: the controller's layout, or with the headphones on their own
    // device, each on its device's first pair.
    let routing = inner.routing.unwrap_or(if info.phones.is_some() {
        Routing {
            master: earshot_engine::Out::Stereo(0),
            phones: earshot_engine::Out::Stereo(0),
        }
    } else {
        Routing::default_for(info.channels)
    });
    engine.send(Command::SetRouting(routing))?;
    inner.midi.set(engine.sender());
    inner.engine = Some(engine);
    Ok(info)
}

/// Why [`load_track`] didn't put its track on: a newer load for the player was asked
/// for meanwhile.
pub const SUPERSEDED: &str = "a newer load replaced it";

/// Note a new load for `deck`, before starting it. Returns its ticket for
/// [`load_track`]: any older load still decoding won't land after it.
pub fn request_load(player: &Player, deck: DeckId) -> u64 {
    let mut inner = player.lock().unwrap();
    let t = &mut inner.load_tickets[deck.index()];
    *t += 1;
    *t
}

/// Decode a library track and put it on a deck, unless a newer load (than `ticket`,
/// from [`request_load`]) was asked for meanwhile: then it fails with [`SUPERSEDED`].
/// Blocking (about a second): call it from a background thread.
pub fn load_track(
    player: &Player,
    lib: &Library,
    deck: DeckId,
    id: i64,
    ticket: u64,
) -> Result<(), String> {
    // Already replaced: don't decode for nothing.
    if player.lock().unwrap().load_tickets[deck.index()] != ticket {
        return Err(SUPERSEDED.into());
    }
    if outside::is_outside(id) {
        return load_outside(player, lib, deck, id, ticket);
    }
    let reader = &lib.store.reader;
    let track = reader.track(id).map_err(|e| e.to_string())?;
    let at = reader.track_location(id).map_err(|e| e.to_string())?;
    let (audio, tags) = decode::decode(&at)?;
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
    let first_beat = reader.first_beat(id).map_err(|e| e.to_string())?;
    let audio = Arc::new(audio);
    let mut estimate = false;
    let (grid, source) = match stored {
        Some((model, src)) => (
            Some(Beatgrid::from_model(model, audio.duration()).map_err(|e| e.to_string())?),
            src.label().to_string(),
        ),
        None => match sidecar_grid(&at) {
            Some(g) => (Some(g), GridSource::Sidecar.label().into()),
            None => {
                // No analysed, hand-fixed or sidecar grid: analyse in the background,
                // starting now. The deck goes on with no grid, gets an estimated one a
                // moment later, and the analysed one when it arrives.
                lib.analyser
                    .queue(id, Some(audio.clone()), false, false, Mode::Static);
                estimate = true;
                (None, NO_GRID_YET_LABEL.into())
            }
        },
    };
    // The library's title and artist, so the deck agrees with the track list.
    let (title, artist) = (track.title, track.artist);

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
    let hotcues = reader.hotcues(id).map_err(|e| e.to_string())?;
    // A main cue set by hand wins over the first-beat setting.
    let saved_cue = reader.main_cue(id).map_err(|e| e.to_string())?;
    let main = saved_cue.or_else(|| load_cue(player, grid.as_ref(), first_beat));
    let generation = put_on_deck(
        player,
        deck,
        ticket,
        Ready {
            id,
            audio: audio.clone(),
            grid,
            cues: CueSet { main, hot: hotcues },
            level,
            title,
            artist,
            source,
            wave: Arc::new(wave),
            detail,
            cue_saved: saved_cue.is_some(),
        },
    )?;
    super::stems::attach(player, deck, generation, audio.clone(), at);
    if estimate {
        estimate_grid_later(player, deck, generation, tags, audio);
    }
    Ok(())
}

/// A decoded track, with all a deck needs.
struct Ready {
    id: i64,
    audio: Arc<decode::TrackAudio>,
    grid: Option<Beatgrid>,
    cues: CueSet,
    level: loudness::Loudness,
    title: String,
    artist: String,
    source: String,
    wave: Arc<Vec<u8>>,
    detail: Vec<u8>,
    /// `cues.main` was saved by hand: analysis won't move it.
    cue_saved: bool,
}

/// Put a decoded track on a deck, unless a newer load was asked for meanwhile.
/// Returns the load's generation.
fn put_on_deck(player: &Player, deck: DeckId, ticket: u64, r: Ready) -> Result<u64, String> {
    let Ready {
        id,
        audio,
        grid,
        cues,
        level,
        title,
        artist,
        source,
        wave,
        detail,
        cue_saved,
    } = r;
    let mut inner = player.lock().unwrap();
    // Checked under the same lock as the load itself, so an older request finishing
    // late can't replace a newer one.
    if inner.load_tickets[deck.index()] != ticket {
        return Err(SUPERSEDED.into());
    }
    inner.generation += 1;
    let track = Arc::new(LoadedTrack {
        track_id: id,
        generation: inner.generation,
        audio,
        grid: grid.map(Arc::new),
        cues,
        auto_gain: level.auto_gain(),
    });
    // The session first: with no audio running, it's still there for a restart.
    let session = inner.session_mut(deck);
    session.load(&track);
    session.title = title;
    session.artist = artist;
    session.grid_source = source;
    session.waveform = wave;
    session.detail = Arc::new(detail);
    session.cue_saved = cue_saved;
    let generation = track.generation;
    inner.send(Command::Load { deck, track })?;
    Ok(generation)
}

/// An outside track (`core::outside`): its file, with the grid and cues its library
/// gave. Its waveform and loudness are worked out once and kept (a stick's for the
/// session only); nothing else about it is saved, and it isn't analysed.
fn load_outside(
    player: &Player,
    lib: &Library,
    deck: DeckId,
    id: i64,
    ticket: u64,
) -> Result<(), String> {
    let outside = &lib.store.outside;
    let t = outside
        .get(id)
        .ok_or("that track isn't known any more (its library was closed)")?;
    let at = Location::File(t.file.clone());
    let (audio, tags) = decode::decode(&at)?;
    let key = file_key(&t.file);
    let cached = match &key {
        Some(k) if t.on_stick => outside.wave(k),
        Some(k) => lib
            .store
            .reader
            .file_cache(k)
            .ok()
            .flatten()
            .map(|(w, l)| (Arc::new(w), l)),
        None => None,
    };
    let (wave, level) = match cached {
        Some(c) => c,
        None => {
            let (w, l) = (
                Arc::new(waveform::summarise(&audio)),
                loudness::measure(&audio),
            );
            match key {
                Some(k) if t.on_stick => outside.keep_wave(k, w.clone(), l),
                Some(k) => lib.store.send(StoreCmd::SaveFileCache {
                    key: k,
                    waveform: (*w).clone(),
                    level: l,
                }),
                None => {}
            }
            (w, l)
        }
    };
    // The track list's thumbnail.
    lib.store
        .tell(StoreEvent::Previews(vec![(id, waveform::preview(&wave))]));
    let audio = Arc::new(audio);
    let (grid, source, estimate) = match external::grid_of(&t.grid, audio.duration()) {
        Some(g) => (Some(g), GridSource::Import.label().to_string(), false),
        None => match sidecar_grid(&at) {
            Some(g) => (Some(g), GridSource::Sidecar.label().into(), false),
            None => (None, NO_GRID_YET_LABEL.into(), true),
        },
    };
    let detail = waveform::detail(&audio);
    // The library's main cue isn't read; one set by hand this session is kept.
    // Otherwise the deck starts on the grid's first line, or at the start if the
    // setting says so.
    let cues = CueSet {
        main: t.main.or_else(|| load_cue(player, grid.as_ref(), None)),
        hot: t.hot,
    };
    let generation = put_on_deck(
        player,
        deck,
        ticket,
        Ready {
            id,
            audio: audio.clone(),
            grid,
            cues,
            level,
            title: if t.title.is_empty() {
                t.file
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            } else {
                t.title.clone()
            },
            artist: t.artist.clone(),
            source,
            wave,
            detail,
            cue_saved: t.main.is_some(),
        },
    )?;
    if estimate {
        estimate_grid_later(player, deck, generation, tags, audio);
    }
    Ok(())
}

/// A file as it is now (path, size, modified), for caches.
pub fn file_key(p: &Path) -> Option<outside::FileKey> {
    let m = std::fs::metadata(p).ok()?;
    let mtime = m
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos() as i64;
    Some((p.to_owned(), m.len(), mtime))
}

/// Where a track's cue starts: the grid line nearest where analysis found the beats
/// start (`first_beat`), so a grid moved by hand still gets its cue on one of its lines,
/// and a straight grid that runs back over a silent intro doesn't put it there. `None`
/// (no grid, or analysed before first_beat): the engine's default, the grid's first
/// line. The playhead starts here too.
pub fn initial_cue(grid: Option<&Beatgrid>, first_beat: Option<f64>) -> Option<f64> {
    let (grid, first_beat) = (grid?, first_beat?);
    let at = grid.quantize(first_beat);
    // The nearest line can be one before the start of the file.
    Some(if at >= 0.0 {
        at
    } else {
        grid.time_at(0.0).max(0.0)
    })
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
    // A steady grid's edits change its model (tempo, anchor); markers are scaled
    // about the beat nearest the playhead.
    let nearest = grid
        .beat_at(playhead)
        .round()
        .clamp(0.0, (grid.len() - 1) as f64) as usize;
    let edited = match op {
        GridOp::SetBpm(bpm) if bpm > 20.0 && bpm < 400.0 => grid.with_bpm(bpm, nearest, duration),
        GridOp::SetBpm(_) => return Err("BPM out of range".into()),
        GridOp::Nudge(ms) => grid.nudged(ms / 1000.0, duration),
        GridOp::AnchorHere => grid.anchored_at(playhead, duration),
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

/// Give every player with this track loaded the grid the Store has active for it now
/// (after undo or redo switched it). With no grid, the players keep none.
pub fn reload_grid(player: &Player, reader: &Reader, track: i64) -> Result<(), String> {
    let stored = reader.grid(track).map_err(|e| e.to_string())?;
    let mut inner = player.lock().unwrap();
    for i in 0..inner.decks.len() {
        let slot = &inner.decks[i];
        if slot.track_id != Some(track) {
            continue;
        }
        let Some(duration) = slot.audio.as_ref().map(|a| a.duration()) else {
            continue;
        };
        let (grid, source) = match &stored {
            Some((model, src)) => (
                Some(Arc::new(
                    Beatgrid::from_model(model.clone(), duration).map_err(|e| e.to_string())?,
                )),
                src.label(),
            ),
            None => (None, ""),
        };
        let (deck, generation) = (deck_id(i)?, slot.generation);
        inner.send(Command::SetGrid {
            deck,
            generation,
            grid: grid.clone(),
        })?;
        inner.decks[i].grid = grid;
        inner.decks[i].grid_source = source.into();
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

/// How a deck shows its grid's source while the estimate is being worked out.
pub const NO_GRID_YET_LABEL: &str = "none yet";

/// Work out the estimated grid (`estimated_grid`) on a thread of its own, so the
/// track plays at once, and give it to the deck, cued on its first line if the
/// setting says so (the engine does that only if nothing has played, moved or cued
/// the deck since the load). Not if the deck has had a newer load (`generation`) or
/// has a grid by then (analysis was faster): that one is better.
fn estimate_grid_later(
    player: &Player,
    deck: DeckId,
    generation: u64,
    tags: decode::TrackTags,
    audio: Arc<TrackAudio>,
) -> std::thread::JoinHandle<()> {
    let player = player.clone();
    std::thread::spawn(move || {
        let Estimate {
            grid,
            source,
            first_beat,
        } = estimated_grid(&tags, &audio);
        let mut inner = player.lock().unwrap();
        let slot = &inner.decks[deck.index()];
        if slot.generation != generation || slot.grid.is_some() {
            return;
        }
        let cue_saved = slot.cue_saved;
        let grid = grid.map(Arc::new);
        if grid.is_some()
            && inner
                .send(Command::SetGrid {
                    deck,
                    generation,
                    grid: grid.clone(),
                })
                .is_err()
        {
            return;
        }
        let cue =
            initial_cue(grid.as_deref(), first_beat).filter(|_| inner.cue_first_beat && !cue_saved);
        let slot = &mut inner.decks[deck.index()];
        slot.grid = grid;
        slot.grid_source = source;
        if let Some(at) = cue {
            let _ = inner.send(Command::InitialCue {
                deck,
                generation,
                at,
            });
        }
    })
}

/// How a deck shows a quick grid's source.
pub const QUICK_GRID_LABEL: &str = "quick grid";

/// The quick grid is used only when its beat is at least this clear (see
/// `quick::QuickGrid::confidence`); below it, the BPM tag's placeholder is.
const QUICK_GRID_CONFIDENCE: f32 = 1.0;

/// The tempo-lock sidecar's grid, if a local file has a good one beside it.
fn sidecar_grid(at: &Location) -> Option<Beatgrid> {
    let path = at.as_file()?;
    let text = std::fs::read_to_string(sidecar_path(path)).ok()?;
    match serde_json::from_str::<TempoLockAnalysis>(&text)
        .map_err(|e| e.to_string())
        .and_then(|a| {
            Beatgrid::from_indexed(&a.beats, &a.beat_index, &a.is_downbeat)
                .map_err(|e| e.to_string())
        }) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("{}: bad beats sidecar: {e}", path.display());
            None
        }
    }
}

/// A grid worked out on the spot: the quick grid, else a steady one at the BPM tag.
fn estimated_grid(tags: &decode::TrackTags, audio: &TrackAudio) -> Estimate {
    let duration = audio.duration();
    let tag_bpm = tags.bpm.filter(|b| *b > 40.0);
    if let Some(q) = quick::grid(audio, tag_bpm).filter(|q| q.confidence >= QUICK_GRID_CONFIDENCE)
        && let Ok(g) = Beatgrid::from_model(GridModel::Constant(q.grid), duration)
    {
        return Estimate {
            grid: Some(g),
            source: QUICK_GRID_LABEL.into(),
            first_beat: Some(q.grid.anchor),
        };
    }
    match tag_bpm {
        Some(bpm) => Estimate {
            grid: Beatgrid::constant(bpm, 0.0, duration).ok(),
            source: "BPM tag (placeholder)".into(),
            first_beat: None,
        },
        None => Estimate {
            grid: None,
            source: "none".into(),
            first_beat: None,
        },
    }
}

/// What [`estimated_grid`] found.
struct Estimate {
    grid: Option<Beatgrid>,
    /// How the deck shows where it's from.
    source: String,
    /// The first beat with music in it, if it knows (the quick grid's 1).
    first_beat: Option<f64>,
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
            phones_device: None,
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
    use std::time::{Duration, Instant};

    use earshot_engine::{ChannelParam, DeckCmd, EQ_LOW, LoopState, MasterParam, ParamId, Press};

    use super::testing::*;
    use super::*;

    /// More events than the ring holds, unread: the cue, hot cue and loop changes
    /// among the dropped ones still reach the session, from a snapshot.
    #[test]
    fn dropped_events_are_made_up_from_a_snapshot() {
        let p = running();
        let a = DeckId::new(0).unwrap();
        put_on(&p, a, 11);
        wait(&p, |s| s.decks[0].track_id == Some(11));
        {
            let mut inner = p.lock().unwrap();
            // Each sends an event; the ring holds 256.
            for i in 0..400 {
                inner
                    .send(Command::Deck(
                        a,
                        DeckCmd::SetCue(Some(1.0 + i as f64 * 0.01)),
                    ))
                    .unwrap();
            }
            inner.send(Command::Deck(a, DeckCmd::Seek(12.0))).unwrap();
            for press in [Press::Down, Press::Up] {
                inner
                    .send(Command::Deck(a, DeckCmd::Hotcue { slot: 3, press }))
                    .unwrap();
            }
            inner
                .send(Command::Deck(
                    a,
                    DeckCmd::SetLoop {
                        start: 5.0,
                        end: 7.0,
                    },
                ))
                .unwrap();
        }
        // Not caught up meanwhile, so the ring overflows.
        let t = Instant::now();
        loop {
            let inner = p.lock().unwrap();
            let s = inner.snapshot().unwrap();
            if s.decks[0].loop_active && s.decks[0].hotcues[3].is_some() {
                break;
            }
            drop(inner);
            assert!(t.elapsed() < Duration::from_secs(5), "timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
        let (snap, changed) = p.lock().unwrap().catch_up();
        let d = snap.unwrap().decks[0];
        assert!(
            p.lock()
                .unwrap()
                .monitor()
                .unwrap()
                .stats()
                .dropped_events
                .load(Relaxed)
                > 0
        );
        assert_eq!(
            changed,
            vec![(a, Change::MainCue), (a, Change::Hotcues)],
            "the main cue and the hot cue are saved"
        );
        let inner = p.lock().unwrap();
        let st = &inner.session(a).state;
        assert_eq!(st.cues.hot[3], d.hotcues[3]);
        assert_eq!(st.cues.main, Some(d.cue_point));
        assert_eq!(
            st.loop_,
            LoopState {
                start: Some(5.0),
                end: Some(7.0),
                active: true
            }
        );
    }

    #[test]
    fn a_restart_puts_every_player_back() {
        let p = running();
        let a = DeckId::new(0).unwrap();
        let b = DeckId::new(1).unwrap();
        let prep = DeckId::new(2).unwrap();
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
                && s.decks[2].cue_point == 7.0
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
            s.decks[0].track_id == Some(11) && s.decks[2].track_id == Some(12)
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
        assert_eq!(after.decks[2].cue_point, 7.0);
        assert_eq!(after.decks[2].generation, before.decks[2].generation);
        // The crossfader sides are as they were, not reset to "thru".
        assert_eq!(now.crossfader, was.crossfader);
        assert_eq!(after.decks[1].crossfader, before.decks[1].crossfader);
        assert_eq!(p.lock().unwrap().session(a).jump_size, 16.0);
    }

    #[test]
    fn the_cue_starts_where_the_beats_do() {
        let near = |a: Option<f64>, b: f64| a.is_some_and(|a| (a - b).abs() < 1e-9);
        // A straight grid runs back over a 12 s silent intro, to 0.25 s.
        let constant = Beatgrid::constant(120.0, 12.25, 60.0).unwrap();
        assert!((constant.time_at(0.0) - 0.25).abs() < 1e-9);
        assert!(near(initial_cue(Some(&constant), Some(12.26)), 12.25));
        // A grid of detected beats, extended back to the start at their tempo.
        let beats: Vec<f64> = (0..40).map(|i| 4.02 + i as f64 * 0.48).collect();
        let index: Vec<i64> = (0..40).collect();
        let markers = Beatgrid::from_indexed(&beats, &index, &[false; 40])
            .and_then(|g| g.refit(60.0))
            .unwrap();
        assert!(markers.time_at(0.0) < 0.5);
        assert!(near(initial_cue(Some(&markers), Some(4.02)), 4.02));
        // Moved by hand 100 ms later: the cue is on one of its lines.
        let nudged = constant.nudged(0.1, 60.0).unwrap();
        assert!(near(initial_cue(Some(&nudged), Some(12.26)), 12.35));
        // Beats start before the grid's first line: that line, not one before 0.
        assert!(near(initial_cue(Some(&nudged), Some(0.01)), 0.35));
        // Unknown (no grid, or analysed before first_beat): the engine's default.
        assert_eq!(initial_cue(Some(&constant), None), None);
        assert_eq!(initial_cue(None, Some(12.26)), None);
    }

    #[test]
    fn an_unanalysed_track_gets_the_quick_grid_else_its_tag() {
        use earshot_engine::demo::{DemoSpec, generate};
        let (audio, truth) = generate(
            &DemoSpec {
                start_bpm: 126.0,
                end_bpm: 126.0,
                key: "8A".into(),
                seconds: 30.0,
                seed: 3,
            },
            44_100,
        );
        let tags = decode::TrackTags {
            bpm: Some(125.0),
            ..Default::default()
        };
        let Estimate {
            grid,
            source,
            first_beat,
        } = estimated_grid(&tags, &audio);
        assert_eq!(source, QUICK_GRID_LABEL);
        assert!((first_beat.unwrap() - truth.times()[0]).abs() < 0.015);
        let grid = grid.unwrap();
        assert!((grid.mean_bpm() - 126.0).abs() < 0.02);
        // On the real beats, not the tag's.
        for &t in &truth.times()[4..40] {
            assert!((grid.quantize(t) - t).abs() < 0.01, "{t}");
        }

        // No beat to find: the tag's placeholder.
        let quiet = TrackAudio {
            samples: vec![0.0; 44_100 * 2 * 20],
            sample_rate: 44_100,
        };
        let Estimate { grid, source, .. } = estimated_grid(&tags, &quiet);
        assert_eq!(source, "BPM tag (placeholder)");
        assert!((grid.unwrap().mean_bpm() - 125.0).abs() < 1e-9);
    }

    #[test]
    fn the_estimated_grid_lands_after_the_load_unless_overtaken() {
        use earshot_engine::demo::{DemoSpec, generate};
        let (audio, truth) = generate(
            &DemoSpec {
                start_bpm: 126.0,
                end_bpm: 126.0,
                key: "8A".into(),
                seconds: 30.0,
                seed: 3,
            },
            44_100,
        );
        // A second of silence first, so the first beat isn't where the deck is
        // cued already.
        let mut audio = audio;
        audio
            .samples
            .splice(0..0, std::iter::repeat_n(0.0, 2 * 44_100));
        let audio = Arc::new(audio);
        let deck = DeckId::new(0).unwrap();
        let p = running();
        let state = |p: &Player| {
            let inner = p.lock().unwrap();
            let s = inner.session(deck);
            (s.grid.as_ref().map(|g| g.mean_bpm()), s.grid_source.clone())
        };
        let tags = decode::TrackTags::default();

        // Lands on the load it was worked out for.
        put_on(&p, deck, 1);
        let generation = p.lock().unwrap().generation;
        estimate_grid_later(&p, deck, generation, tags.clone(), audio.clone())
            .join()
            .unwrap();
        let (bpm, source) = state(&p);
        assert!((bpm.unwrap() - 126.0).abs() < 0.02);
        assert_eq!(source, QUICK_GRID_LABEL);
        // Cued on its first beat (the deck hasn't been touched).
        let first = truth.times()[0] + 1.0;
        wait(&p, |s| (s.deck(deck).cue_point - first).abs() < 0.015);

        // Not on a newer load.
        put_on(&p, deck, 2);
        estimate_grid_later(&p, deck, generation, tags.clone(), audio.clone())
            .join()
            .unwrap();
        assert_eq!(state(&p).0, None);

        // Not over a grid that came first (the analysis's).
        let generation = p.lock().unwrap().generation;
        {
            let mut inner = p.lock().unwrap();
            let s = inner.session_mut(deck);
            s.grid = Some(Arc::new(Beatgrid::constant(100.0, 0.0, 30.0).unwrap()));
            s.grid_source = "analysis".into();
        }
        estimate_grid_later(&p, deck, generation, tags.clone(), audio.clone())
            .join()
            .unwrap();
        assert_eq!(state(&p), (Some(100.0), "analysis".into()));

        // The grid lands, but a main cue saved by hand stays where it was.
        put_on(&p, deck, 3);
        let generation = p.lock().unwrap().generation;
        p.lock().unwrap().session_mut(deck).cue_saved = true;
        estimate_grid_later(&p, deck, generation, tags, audio)
            .join()
            .unwrap();
        assert_eq!(state(&p).1, QUICK_GRID_LABEL);
        // The grid went to the engine before any cue would have: once it's there, so
        // would the cue be.
        let s = wait(&p, |s| s.deck(deck).bpm.is_some());
        assert_eq!(s.deck(deck).cue_point, 0.0);
    }

    #[test]
    fn deck_ids_for_sessions() {
        assert_eq!(deck_id(0), Ok(DeckId::new(0).unwrap()));
        assert!(deck_id(MAX_DECKS).is_err());
        let inner = new_player();
        assert_eq!(inner.lock().unwrap().decks.len(), MAX_DECKS);
    }
}
