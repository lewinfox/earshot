mod controls;
mod midi;
mod state;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

use earshot_engine::beatgrid::Beatgrid;
use earshot_engine::decode::{self, TrackAudio};
use earshot_engine::output::{self, DeviceSummary};
use earshot_engine::{Command, DECKS, Engine, OutputInfo, OutputOptions, demo, waveform};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use state::{AppState, DeckSlot};

// ---- audio output ----------------------------------------------------------------

fn options_from_env() -> OutputOptions {
    OutputOptions {
        device: std::env::var("EARSHOT_AUDIO_DEVICE")
            .ok()
            .filter(|s| !s.is_empty()),
        buffer_frames: Some(
            std::env::var("EARSHOT_BUFFER")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(256),
        ),
        null: std::env::var("EARSHOT_NULL_AUDIO").is_ok_and(|v| v == "1"),
    }
}

/// Start (or restart) audio and reload whatever was on the decks.
fn start_engine(state: &AppState, opts: &OutputOptions) -> Result<OutputInfo, String> {
    let mut inner = state.inner.lock().unwrap();
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
    inner.engine = Some(engine);
    Ok(info)
}

#[tauri::command]
fn list_audio_devices() -> Vec<DeviceSummary> {
    output::list_devices()
}

#[tauri::command]
fn start_audio(
    state: State<'_, AppState>,
    device: Option<String>,
    buffer_frames: Option<u32>,
    null: bool,
) -> Result<OutputInfo, String> {
    start_engine(
        &state,
        &OutputOptions {
            device,
            buffer_frames,
            null,
        },
    )
}

#[tauri::command]
fn audio_info(state: State<'_, AppState>) -> Option<OutputInfo> {
    state
        .inner
        .lock()
        .unwrap()
        .engine
        .as_ref()
        .map(|e| e.info().clone())
}

// ---- tracks ------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum TrackSource {
    Demo {
        bpm: f64,
        end_bpm: Option<f64>,
        key: String,
        seed: u64,
        seconds: Option<f64>,
        title: Option<String>,
    },
    File {
        path: String,
    },
}

#[derive(Serialize)]
struct GridDto {
    times: Vec<f64>,
    positions: Vec<u8>,
    source: String,
}

impl GridDto {
    fn of(grid: &Option<Arc<Beatgrid>>, source: &str) -> Option<GridDto> {
        grid.as_ref().map(|g| GridDto {
            times: g.times().to_vec(),
            positions: g.positions().to_vec(),
            source: source.into(),
        })
    }
}

#[derive(Serialize)]
struct LoadedTrack {
    title: String,
    artist: String,
    duration: f64,
    sample_rate: u32,
    grid: Option<GridDto>,
    waveform_bins_per_second: f64,
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
            Ok(g) => return (Some(g), "tempo-lock sidecar".into()),
            Err(e) => eprintln!("{}: bad beats sidecar: {e}", path.display()),
        }
    }
    match tags.bpm.filter(|b| *b > 40.0) {
        Some(bpm) => (
            Beatgrid::constant(bpm, 0.0, duration).ok(),
            "BPM tag, starting at 0:00 (placeholder)".into(),
        ),
        None => (None, "none".into()),
    }
}

#[tauri::command]
async fn load_track(
    state: State<'_, AppState>,
    deck: usize,
    source: TrackSource,
) -> Result<LoadedTrack, String> {
    if deck >= DECKS {
        return Err("no such deck".into());
    }
    let sample_rate = state
        .inner
        .lock()
        .unwrap()
        .engine
        .as_ref()
        .map_or(44_100, |e| e.info().sample_rate);
    let (audio, grid, grid_source, title, artist) =
        tauri::async_runtime::spawn_blocking(move || {
            Ok::<_, String>(match source {
                TrackSource::Demo {
                    bpm,
                    end_bpm,
                    key,
                    seed,
                    seconds,
                    title,
                } => {
                    let spec = demo::DemoSpec {
                        start_bpm: bpm,
                        end_bpm: end_bpm.unwrap_or(bpm),
                        key: key.clone(),
                        seconds: seconds.unwrap_or(240.0),
                        seed,
                    };
                    let (audio, grid) = demo::generate(&spec, sample_rate);
                    let title = title.unwrap_or_else(|| format!("Demo {bpm:.0} BPM {key}"));
                    (
                        audio,
                        Some(grid),
                        "demo (exact)".to_string(),
                        title,
                        "earshot".to_string(),
                    )
                }
                TrackSource::File { path } => {
                    let path = PathBuf::from(path);
                    let (audio, tags) = decode::decode_file(&path)?;
                    let (grid, src) = grid_for_file(&path, &tags, audio.duration());
                    let title = tags.title.clone().unwrap_or_else(|| {
                        path.file_stem()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    });
                    (
                        audio,
                        grid,
                        src,
                        title,
                        tags.artist.clone().unwrap_or_default(),
                    )
                }
            })
        })
        .await
        .map_err(|e| e.to_string())??;

    let audio: Arc<TrackAudio> = Arc::new(audio);
    let wave = waveform::summarise(&audio);
    let grid = grid.map(Arc::new);
    let duration = audio.duration();
    let sample_rate = audio.sample_rate;

    let mut inner = state.inner.lock().unwrap();
    inner.generation += 1;
    let generation = inner.generation;
    inner.send(Command::Load {
        deck,
        audio: audio.clone(),
        grid: grid.clone(),
        generation,
    })?;
    let dto = GridDto::of(&grid, &grid_source);
    inner.decks[deck] = DeckSlot {
        audio: Some(audio),
        grid,
        grid_source,
        waveform: Arc::new(wave),
    };
    Ok(LoadedTrack {
        title,
        artist,
        duration,
        sample_rate,
        grid: dto,
        waveform_bins_per_second: waveform::BINS_PER_SECOND,
    })
}

#[tauri::command]
fn get_waveform(state: State<'_, AppState>, deck: usize) -> tauri::ipc::Response {
    let inner = state.inner.lock().unwrap();
    let bytes = inner
        .decks
        .get(deck)
        .map(|d| d.waveform.as_ref().clone())
        .unwrap_or_default();
    tauri::ipc::Response::new(bytes)
}

// ---- grid edits ----------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum GridOp {
    SetBpm { bpm: f64 },
    Nudge { ms: f64 },
    SetDownbeat,
    Halve,
    Double,
}

#[tauri::command]
fn edit_grid(state: State<'_, AppState>, deck: usize, op: GridOp) -> Result<GridDto, String> {
    let mut inner = state.inner.lock().unwrap();
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
        GridOp::SetBpm { bpm } if bpm > 20.0 && bpm < 400.0 => {
            grid.with_bpm(bpm, nearest, duration)
        }
        GridOp::SetBpm { .. } => return Err("BPM out of range".into()),
        GridOp::Nudge { ms } => grid.nudged(ms / 1000.0, duration),
        GridOp::SetDownbeat => Ok(grid.with_downbeat(nearest)),
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
    inner.decks[deck].grid_source = "edited".into();
    Ok(GridDto::of(&edited, "edited").unwrap())
}

// ---- library -------------------------------------------------------------------------

const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "wav", "aif", "aiff", "m4a", "mp4", "aac", "ogg",
];

#[derive(Serialize)]
struct ScannedTrack {
    path: String,
    title: String,
    artist: String,
    bpm: Option<f64>,
    key: Option<String>,
    duration: Option<f64>,
    has_beats_sidecar: bool,
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| AUDIO_EXTENSIONS.contains(&x.to_lowercase().as_str()))
        {
            out.push(p);
        }
    }
}

#[tauri::command]
async fn scan_folder(path: String) -> Result<Vec<ScannedTrack>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut files = Vec::new();
        walk(Path::new(&path), &mut files);
        files.sort();
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
        let chunk = files.len().div_ceil(threads).max(1);
        let mut out: Vec<ScannedTrack> = std::thread::scope(|s| {
            let handles: Vec<_> = files
                .chunks(chunk)
                .map(|c| {
                    s.spawn(move || {
                        c.iter()
                            .map(|p| {
                                let tags = decode::read_tags(p).unwrap_or_default();
                                ScannedTrack {
                                    path: p.to_string_lossy().into_owned(),
                                    title: tags.title.clone().unwrap_or_else(|| {
                                        p.file_stem()
                                            .map(|s| s.to_string_lossy().into_owned())
                                            .unwrap_or_default()
                                    }),
                                    artist: tags.artist.clone().unwrap_or_default(),
                                    bpm: tags.bpm,
                                    key: tags.key.clone(),
                                    duration: tags.duration,
                                    has_beats_sidecar: sidecar_path(p).exists(),
                                }
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().unwrap())
                .collect()
        });
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---- controls, MIDI, reports -------------------------------------------------------

#[tauri::command]
fn control(
    app: AppHandle,
    state: State<'_, AppState>,
    group: String,
    key: String,
    value: f64,
) -> Result<(), String> {
    let mut inner = state.inner.lock().unwrap();
    if let controls::Outcome::Ui = controls::apply(&mut inner, &group, &key, value)? {
        drop(inner);
        app.emit("ui-control", midi::UiControl { group, key, value })
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn midi_connect(app: AppHandle) -> Result<Vec<String>, String> {
    midi::connect_all(&app)
}

#[tauri::command]
fn save_report(app: AppHandle, name: String, json: String) -> Result<String, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("reports");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let path = dir.join(format!("{safe}.json"));
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

// ---- state publisher -----------------------------------------------------------------

#[derive(Serialize, Clone)]
struct DeckState {
    position: f64,
    duration: f64,
    playing: bool,
    speed: f64,
    rate: f64,
    bpm: Option<f64>,
    sync: bool,
    loop_in: Option<f64>,
    loop_out: Option<f64>,
    loop_active: bool,
    cue_point: f64,
    hotcues: Vec<Option<f64>>,
    volume: f64,
    pfl: bool,
    generation: u64,
}

#[derive(Serialize, Clone)]
struct AudioStats {
    callbacks_per_second: f64,
    slow_callbacks: u64,
    xruns: u64,
    max_callback_ms: f64,
    budget_ms: f64,
    block_frames: u32,
    sample_rate: u32,
}

#[derive(Serialize, Clone)]
struct EngineState {
    decks: Vec<DeckState>,
    crossfader: f64,
    quantize: bool,
    audio: AudioStats,
}

fn finite(x: f64) -> Option<f64> {
    x.is_finite().then_some(x)
}

/// Push engine state to the UI at ~60 Hz. The UI interpolates the playhead between
/// updates, so this rate only affects responsiveness, not smoothness.
fn spawn_publisher(app: AppHandle) {
    std::thread::spawn(move || {
        let mut last_callbacks = 0u64;
        let mut last_tick = std::time::Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(16));
            let shared = app.state::<AppState>().inner.lock().unwrap().shared();
            let Some(s) = shared else { continue };
            let now = std::time::Instant::now();
            let callbacks = s.callbacks.load(Relaxed);
            let dt = now.duration_since(last_tick).as_secs_f64();
            let cps = callbacks.saturating_sub(last_callbacks) as f64 / dt;
            last_callbacks = callbacks;
            last_tick = now;
            let block = s.last_block_frames.load(Relaxed);
            let rate = s.sample_rate.load(Relaxed);
            let decks = s
                .decks
                .iter()
                .map(|d| DeckState {
                    position: d.position.get(),
                    duration: d.duration.get(),
                    playing: d.playing.load(Relaxed),
                    speed: d.speed.get(),
                    rate: d.rate.get(),
                    bpm: finite(d.bpm.get()),
                    sync: d.sync.load(Relaxed),
                    loop_in: finite(d.loop_in.get()),
                    loop_out: finite(d.loop_out.get()),
                    loop_active: d.loop_active.load(Relaxed),
                    cue_point: d.cue_point.get(),
                    hotcues: d.hotcues.iter().map(|h| finite(h.get())).collect(),
                    volume: d.volume.get(),
                    pfl: d.pfl.load(Relaxed),
                    generation: d.generation.load(Relaxed),
                })
                .collect();
            let payload = EngineState {
                decks,
                crossfader: s.crossfader.get(),
                quantize: s.quantize.load(Relaxed),
                audio: AudioStats {
                    callbacks_per_second: cps,
                    slow_callbacks: s.slow_callbacks.load(Relaxed),
                    xruns: s.xruns.load(Relaxed),
                    max_callback_ms: s.take_max_callback_ns() as f64 / 1e6,
                    budget_ms: if rate > 0 {
                        block as f64 / rate as f64 * 1000.0
                    } else {
                        0.0
                    },
                    block_frames: block,
                    sample_rate: rate,
                },
            };
            let _ = app.emit("engine-state", payload);
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .setup(|app| {
            let handle = app.handle().clone();
            let state = handle.state::<AppState>();
            let opts = options_from_env();
            if let Err(e) = start_engine(&state, &opts) {
                eprintln!("audio: {e}; falling back to the null output");
                start_engine(&state, &OutputOptions { null: true, ..opts })?;
            }
            if let Err(e) = midi::connect_all(&handle) {
                eprintln!("MIDI: {e}");
            }
            spawn_publisher(handle);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_audio_devices,
            start_audio,
            audio_info,
            load_track,
            get_waveform,
            edit_grid,
            scan_folder,
            control,
            midi_connect,
            save_report,
        ])
        .run(tauri::generate_context!())
        .expect("error while running earshot");
}
