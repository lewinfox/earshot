//! The decks' side of the app: the audio engine, what is loaded on each deck, loading
//! tracks and editing their beatgrids. Shared between the UI thread and the analysis
//! threads behind one mutex; the UI reads playback state straight from the engine's
//! atomics ([`Shared`]) without taking it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use earshot_engine::beatgrid::Beatgrid;
use earshot_engine::decode::{self, TrackAudio};
use earshot_engine::{
    Command, DECKS, Engine, OutputInfo, OutputOptions, Routing, Shared, energy, loudness, waveform,
};
use serde::{Deserialize, Serialize};

use super::analysis::{self, Mode};
use super::db;
use super::library::Library;

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
}

pub struct Inner {
    pub engine: Option<Engine>,
    pub decks: [DeckSlot; DECKS],
    pub generation: u64,
    pub jump_size: [f64; DECKS],
    pub loop_size: [f64; DECKS],
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

    pub fn shared(&self) -> Option<Arc<Shared>> {
        self.engine.as_ref().map(|e| e.shared().clone())
    }
}

pub type Player = Arc<Mutex<Inner>>;

pub fn new_player() -> Player {
    Arc::new(Mutex::new(Inner {
        engine: None,
        decks: Default::default(),
        generation: 0,
        jump_size: [4.0; DECKS],
        loop_size: [4.0; DECKS],
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
pub fn preview(player: &Player, db: &db::Db, id: i64, start: f64) -> Result<(), String> {
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
            let path = PathBuf::from(db::track_path(&db.lock().unwrap(), id)?);
            Arc::new(decode::decode_file(&path)?.0)
        }
    };
    // At the same auto-gain as on a deck.
    let gain = match db::loudness(&db.lock().unwrap(), id)? {
        Some(l) => l.auto_gain(),
        None => loudness::measure(&audio).auto_gain(),
    };
    let mut inner = player.lock().unwrap();
    if inner.preview_seq != seq {
        return Ok(()); // another preview was asked for meanwhile
    }
    let start = start * audio.duration();
    inner.send(Command::Preview { audio, start, gain })
}

/// Pause or resume the preview.
pub fn play_preview(player: &Player, on: bool) -> Result<(), String> {
    player.lock().unwrap().send(Command::PreviewPlay(on))
}

/// Move the preview to `at` (0..1 of the way in).
pub fn seek_preview(player: &Player, at: f64) -> Result<(), String> {
    let mut inner = player.lock().unwrap();
    let d = inner.shared().map_or(0.0, |s| s.preview_duration.get());
    inner.send(Command::PreviewSeek(at * d))
}

pub fn stop_preview(player: &Player) -> Result<(), String> {
    let mut inner = player.lock().unwrap();
    inner.preview_seq += 1;
    inner.send(Command::PreviewStop)
}

/// Start (or restart) audio and reload whatever was on the decks.
pub fn start_engine(player: &Player, opts: &OutputOptions) -> Result<OutputInfo, String> {
    let mut inner = player.lock().unwrap();
    inner.engine = None; // stop the old stream first so the device is free
    let mut engine = Engine::start(opts)?;
    for (deck, slot) in inner.decks.iter().enumerate() {
        if let Some(audio) = &slot.audio {
            engine.send(Command::Load {
                deck,
                audio: audio.clone(),
                grid: slot.grid.clone(),
                generation: 0,
            })?;
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
    if deck >= DECKS {
        return Err("no such deck".into());
    }
    let db = &lib.db;
    let path = PathBuf::from(db::track_path(&db.lock().unwrap(), id)?);
    let (audio, tags) = decode::decode_file(&path)?;
    // Measured every load (it's quick), so tracks analysed before auto-gain get it.
    let level = loudness::measure(&audio);
    let (stored, cached_wave) = {
        let conn = db.lock().unwrap();
        db::set_energy(&conn, id, energy::energy(&audio))?;
        db::set_loudness(&conn, id, &level)?;
        (
            db::grid(&conn, id)?,
            db::waveform(&conn, id, waveform::BINS_PER_SECOND)?,
        )
    };
    let (grid, source) = match stored {
        // Analysed before grids were straightened to one tempo: straighten and save.
        Some((g, src)) if src == "analysis" && !is_straight(&g) => {
            match g.straightened(audio.duration()) {
                Ok((s, bpm)) => {
                    let conn = db.lock().unwrap();
                    db::save_grid(&conn, id, &s, "analysis")?;
                    db::set_bpm(&conn, id, bpm)?;
                    (Some(s), src)
                }
                Err(_) => (Some(g), src),
            }
        }
        Some((g, src)) => (Some(g), src),
        None => grid_for_file(&path, &tags, audio.duration()),
    };
    let title = tags
        .title
        .clone()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| {
            path.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        });

    let audio = Arc::new(audio);
    let wave = match cached_wave {
        Some(w) => w,
        None => {
            // Saved, so the track list can show its thumbnail from now on.
            let w = waveform::summarise(&audio);
            db::save_waveform(&db.lock().unwrap(), id, &w, waveform::BINS_PER_SECOND)?;
            w
        }
    };
    let detail = waveform::detail(&audio);
    // No analysed or hand-fixed grid yet: analyse in the background. The deck plays
    // with the placeholder grid meanwhile and is updated when the result arrives.
    if !matches!(
        source.as_str(),
        "analysis" | analysis::DYNAMIC_SOURCE | "user" | "tempo-lock sidecar"
    ) {
        lib.analyser
            .queue(id, Some(audio.clone()), false, Mode::Static);
    }
    let grid = grid.map(Arc::new);

    let mut inner = player.lock().unwrap();
    inner.generation += 1;
    let generation = inner.generation;
    inner.send(Command::Load {
        deck,
        audio: audio.clone(),
        grid: grid.clone(),
        generation,
    })?;
    inner.send(Command::AutoGain {
        deck,
        gain: level.auto_gain(),
    })?;
    inner.decks[deck] = DeckSlot {
        track_id: Some(id),
        title,
        artist: tags.artist.clone().unwrap_or_default(),
        audio: Some(audio),
        grid,
        grid_source: source,
        waveform: Arc::new(wave),
        detail: Arc::new(detail),
        generation,
    };
    Ok(())
}

/// Whole-grid edits from the grid editor.
#[derive(Debug, Clone, Copy)]
pub enum GridOp {
    SetBpm(f64),
    /// Milliseconds; negative is earlier.
    Nudge(f64),
    /// Make the beat nearest the playhead the 1.
    SetDownbeat,
    /// Shift the grid so a beat lands exactly on the playhead, and make it the 1.
    AnchorHere,
    Halve,
    Double,
}

pub fn edit_grid(player: &Player, db: &db::Db, deck: usize, op: GridOp) -> Result<(), String> {
    let mut inner = player.lock().unwrap();
    let playhead = inner.shared().map_or(0.0, |s| s.decks[deck].position.get());
    let slot = inner.decks.get(deck).ok_or("no such deck")?;
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
        GridOp::SetDownbeat => Ok(grid.with_downbeat(nearest)),
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
    inner.send(Command::SetGrid {
        deck,
        grid: edited.clone(),
    })?;
    inner.decks[deck].grid = edited.clone();
    inner.decks[deck].grid_source = "user".into();
    // Saved, so the fix survives a restart and re-analysis on load doesn't undo it.
    if let (Some(id), Some(g)) = (inner.decks[deck].track_id, &edited) {
        db::save_grid(&db.lock().unwrap(), id, g, "user")?;
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

/// Every beat the same distance apart (to within a microsecond).
fn is_straight(g: &Beatgrid) -> bool {
    let t = g.times();
    let p = (t[t.len() - 1] - t[0]) / (t.len() - 1) as f64;
    t.windows(2).all(|w| ((w[1] - w[0]) - p).abs() < 1e-6)
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
            Ok(g) => return (Some(g), "tempo-lock sidecar".into()),
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
