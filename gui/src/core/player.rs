//! The decks' side of the app: the audio engine, what is loaded on each deck, loading
//! tracks and editing their beatgrids. Shared between the UI thread and the analysis
//! threads behind one mutex; the UI reads playback state from the engine's snapshots
//! ([`Monitor`]) without taking it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use earshot_engine::beatgrid::Beatgrid;
use earshot_engine::decode::{self, TrackAudio};
use earshot_engine::{
    Command, CueSet, DeckCmd, DeckId, Engine, EngineSnapshot, LoadedTrack, Monitor, OutputInfo,
    OutputOptions, PreviewCmd, Routing, Set, energy, loudness, waveform,
};
use serde::{Deserialize, Serialize};

use super::analysis::Mode;
use super::library::Library;

/// Decks A and B (the engine can run up to `earshot_engine::MAX_DECKS`).
pub const DECKS: usize = 2;
/// The decks plus Prepare mode's player, which plays straight to the main output (no
/// channel fader, crossfader or sync: see `DeckCmd::Direct`).
pub const PLAYERS: usize = 3;
/// Prepare mode's player.
pub const PREP: usize = 2;

/// Player `d`, if the app has it.
pub fn deck_id(d: usize) -> Result<DeckId, String> {
    DeckId::new(d)
        .filter(|_| d < PLAYERS)
        .ok_or_else(|| format!("no deck {d}"))
}
use super::store::{GridSource, Store, StoreCmd};

#[derive(Default, Clone)]
pub struct DeckSlot {
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
    /// Bumped on every load, so the UI can rebuild its caches.
    pub generation: u64,
    /// Linear.
    pub auto_gain: f64,
}

impl DeckSlot {
    /// What the engine needs to play this track again, with the grid as it is now.
    /// (On a restart its cues come from the engine's last state instead.)
    fn loaded(&self) -> Option<Arc<LoadedTrack>> {
        Some(Arc::new(LoadedTrack {
            track_id: self.track_id?,
            generation: self.generation,
            audio: self.audio.clone()?,
            grid: self.grid.clone(),
            cues: CueSet::default(),
            auto_gain: self.auto_gain,
        }))
    }
}

pub struct Inner {
    pub engine: Option<Engine>,
    pub decks: [DeckSlot; PLAYERS],
    pub generation: u64,
    pub jump_size: [f64; PLAYERS],
    pub loop_size: [f64; PLAYERS],
    /// Output routing; `None` means the default for the device's channel count.
    pub routing: Option<Routing>,
    /// Bumped on every preview request, so a slow decode can't start a stale one.
    pub preview_seq: u64,
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
}

pub type Player = Arc<Mutex<Inner>>;

pub fn new_player() -> Player {
    Arc::new(Mutex::new(Inner {
        engine: None,
        decks: Default::default(),
        generation: 0,
        jump_size: [4.0; PLAYERS],
        loop_size: [4.0; PLAYERS],
        routing: None,
        preview_seq: 0,
    }))
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
        inner.preview_seq += 1;
        let on_deck = inner
            .decks
            .iter()
            .find(|d| d.track_id == Some(id))
            .and_then(|d| d.audio.clone());
        (inner.preview_seq, on_deck)
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
    if inner.preview_seq != seq {
        return Ok(()); // another preview was asked for meanwhile
    }
    let start = start * audio.duration();
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
    inner.preview_seq += 1;
    inner.send(Command::Preview(PreviewCmd::Stop))
}

/// Start (or restart) audio. On a restart the decks and mixer come back as they were:
/// tracks, position, cues, loops, tempo, sync, EQ and faders.
pub fn start_engine(player: &Player, opts: &OutputOptions) -> Result<OutputInfo, String> {
    let mut inner = player.lock().unwrap();
    // Stop the old stream first so the device is free, keeping its last state.
    let last = inner.engine.take().map(|e| e.snapshot());
    let mut engine = Engine::start(opts, PLAYERS)?;
    engine.send(Command::Deck(deck_id(PREP)?, DeckCmd::Direct(Set::On)))?;
    match last {
        Some(snap) => {
            let mut state = snap.state();
            for (st, slot) in state.decks.iter_mut().zip(&inner.decks) {
                st.track = slot.loaded();
            }
            engine.send(Command::Restore(Box::new(state)))?;
        }
        // Audio wasn't running (it failed to start last time): just the tracks.
        None => {
            for (d, slot) in inner.decks.iter().enumerate() {
                if let Some(track) = slot.loaded() {
                    engine.send(Command::Load {
                        deck: deck_id(d)?,
                        track,
                    })?;
                }
            }
        }
    }
    let info = engine.info().clone();
    engine.send(Command::SetRouting(
        inner.routing.unwrap_or(Routing::default_for(info.channels)),
    ))?;
    inner.engine = Some(engine);
    Ok(info)
}

/// Decode a library track and put it on a deck. Blocking (about a second): call it
/// from a background thread.
pub fn load_track(player: &Player, lib: &Library, deck: usize, id: i64) -> Result<(), String> {
    let deck_id = deck_id(deck)?;
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
    let grid = grid.map(Arc::new);
    let hotcues = reader.hotcues(id).map_err(|e| e.to_string())?;

    let mut inner = player.lock().unwrap();
    inner.generation += 1;
    let generation = inner.generation;
    let auto_gain = level.auto_gain();
    inner.send(Command::Load {
        deck: deck_id,
        track: Arc::new(LoadedTrack {
            track_id: id,
            generation,
            audio: audio.clone(),
            grid: grid.clone(),
            cues: CueSet {
                main: None,
                hot: hotcues,
            },
            auto_gain,
        }),
    })?;
    inner.decks[deck] = DeckSlot {
        track_id: Some(id),
        title,
        artist,
        audio: Some(audio),
        grid,
        grid_source: source,
        waveform: Arc::new(wave),
        detail: Arc::new(detail),
        generation,
        auto_gain,
    };
    Ok(())
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
pub fn edit_grid(player: &Player, store: &Store, deck: usize, op: GridOp) -> Result<(), String> {
    let deck_id = deck_id(deck)?;
    let mut inner = player.lock().unwrap();
    let playhead = inner.snapshot().map_or(0.0, |s| s.deck(deck_id).position);
    let slot = &inner.decks[deck];
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
        deck: deck_id,
        generation,
        grid: edited.clone(),
    })?;
    inner.decks[deck].grid = edited.clone();
    inner.decks[deck].grid_source = GridSource::User.label().into();
    let track = inner.decks[deck].track_id;
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
