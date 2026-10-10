//! Analysis queue: one thread works through tracks, computing the waveform here and
//! the beatgrid in the Python worker (`earshot/worker`, Beat This!).
//!
//! The worker never decodes files itself. We send it a mono WAV from our own decoder,
//! so its beat times line up exactly with what the decks play.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::sync::{Arc, Mutex, mpsc};

use earshot_engine::Command;
use earshot_engine::beatgrid::Beatgrid;
use earshot_engine::decode::{self, TrackAudio};
use earshot_engine::waveform;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::GridDto;
use crate::db::{self, Db};
use crate::state::AppState;

struct Job {
    id: i64,
    /// Already-decoded audio when the track was just loaded onto a deck.
    audio: Option<Arc<TrackAudio>>,
    /// Re-analyse even if the track already has a grid.
    force: bool,
}

#[derive(Serialize, Clone)]
struct AnalysisEvent {
    track_id: i64,
    /// "queued", "running", "done" or "error".
    state: &'static str,
    /// 0..1 while running.
    progress: Option<f64>,
    /// What it's doing now, while running.
    stage: Option<&'static str>,
    bpm: Option<f64>,
    grid_source: Option<String>,
    error: Option<String>,
}

impl AnalysisEvent {
    fn new(track_id: i64, state: &'static str) -> Self {
        AnalysisEvent { track_id, state, progress: None, stage: None, bpm: None, grid_source: None, error: None }
    }
}

// Rough share of the total time before the worker starts: decoding a file, then
// the waveform. The worker reports its own progress for the rest.
const DECODED: f64 = 0.1;
const WORKER_START: f64 = 0.15;

#[derive(Serialize, Clone)]
struct DeckGridEvent {
    deck: usize,
    grid: Option<GridDto>,
}

pub struct Analyser {
    tx: mpsc::Sender<Job>,
    pending: Arc<Mutex<HashSet<i64>>>,
    app: AppHandle,
}

impl Analyser {
    pub fn start(app: AppHandle, db: Db) -> Analyser {
        let (tx, rx) = mpsc::channel::<Job>();
        let pending = Arc::new(Mutex::new(HashSet::new()));
        let p = pending.clone();
        let a = app.clone();
        std::thread::Builder::new()
            .name("earshot-analysis".into())
            .spawn(move || {
                let mut worker: Option<Worker> = None;
                for job in rx {
                    let ev = match run(&a, &db, &mut worker, &job) {
                        Ok(ev) => ev,
                        Err(e) => {
                            eprintln!("analysis of track {}: {e}", job.id);
                            AnalysisEvent { error: Some(e), ..AnalysisEvent::new(job.id, "error") }
                        }
                    };
                    p.lock().unwrap().remove(&job.id);
                    let _ = a.emit("analysis", ev);
                }
            })
            .expect("spawn analysis thread");
        Analyser { tx, pending, app }
    }

    /// Queue a track unless it is already waiting.
    pub fn queue(&self, id: i64, audio: Option<Arc<TrackAudio>>, force: bool) {
        if !self.pending.lock().unwrap().insert(id) {
            return;
        }
        let _ = self.app.emit("analysis", AnalysisEvent::new(id, "queued"));
        let _ = self.tx.send(Job { id, audio, force });
    }
}

fn run(app: &AppHandle, db: &Db, worker: &mut Option<Worker>, job: &Job) -> Result<AnalysisEvent, String> {
    let progress = |fraction: f64, stage: &'static str| {
        let _ = app.emit(
            "analysis",
            AnalysisEvent { progress: Some(fraction), stage: Some(stage), ..AnalysisEvent::new(job.id, "running") },
        );
    };
    let existing = db::grid(&db.lock().unwrap(), job.id)?;
    if !job.force && let Some((_, source)) = &existing {
        // Analysed (or hand-fixed) since this job was queued.
        return Ok(AnalysisEvent { grid_source: Some(source.clone()), ..AnalysisEvent::new(job.id, "done") });
    }

    let audio = match &job.audio {
        Some(a) => a.clone(),
        None => {
            progress(0.0, "decoding");
            let path = db::track_path(&db.lock().unwrap(), job.id)?;
            Arc::new(decode::decode_file(Path::new(&path))?.0)
        }
    };

    progress(DECODED, "waveform");
    // Waveform: cheap, done here, cached so later loads skip it.
    let cached = db::waveform(&db.lock().unwrap(), job.id, waveform::BINS_PER_SECOND)?.is_some();
    if job.force || !cached {
        let wave = waveform::summarise(&audio);
        db::save_waveform(&db.lock().unwrap(), job.id, &wave, waveform::BINS_PER_SECOND)?;
    }

    // Beatgrid: in the worker.
    let wav = std::env::temp_dir().join(format!("earshot-analyse-{}-{}.wav", std::process::id(), job.id));
    write_mono_wav(&wav, &audio).map_err(|e| format!("writing {}: {e}", wav.display()))?;
    if worker.is_none() {
        progress(WORKER_START, "starting the analyser");
        *worker = Some(Worker::spawn()?);
    }
    progress(WORKER_START, "finding beats");
    let reply = worker.as_mut().unwrap().analyse(&wav, &mut |f| {
        progress(WORKER_START + (1.0 - WORKER_START) * f, if f < 0.8 { "finding beats" } else { "refining beats" })
    });
    let _ = std::fs::remove_file(&wav);
    let reply = match reply {
        Ok(r) => r,
        Err(e) => {
            *worker = None; // start a fresh one for the next job
            return Err(e);
        }
    };
    let grid = Beatgrid::from_indexed(&reply.beats, &reply.beat_index, &reply.is_downbeat)
        .and_then(|g| g.refit(audio.duration()))
        .map_err(|e| format!("worker returned an unusable grid: {e}"))?;

    let bpm = fitted_bpm(grid.times()).unwrap_or(reply.median_bpm);

    // A grid fixed by hand while this job waited wins over an automatic analysis.
    let keep_user = !job.force && existing.as_ref().is_some_and(|(_, s)| s == "user");
    {
        let conn = db.lock().unwrap();
        db::save_analysis(&conn, job.id, bpm, reply.min_bpm, reply.max_bpm, &reply.detector)?;
        if !keep_user {
            db::save_grid(&conn, job.id, &grid, "analysis")?;
        }
    }
    if !keep_user {
        update_decks(app, job.id, grid);
    }
    eprintln!(
        "analysed track {}: {:.2} BPM (worker's median {:.2}), {} beats, {:.1} s in the worker",
        job.id,
        bpm,
        reply.median_bpm,
        reply.beats.len(),
        reply.seconds
    );
    Ok(AnalysisEvent {
        bpm: Some(bpm),
        grid_source: Some(if keep_user { "user" } else { "analysis" }.into()),
        ..AnalysisEvent::new(job.id, "done")
    })
}

/// Average tempo from a least-squares line through beat number against time. The
/// worker's median beat gap is quantised by its onset frames (~6 ms), which can put it
/// half a BPM out; a fit over every beat averages that away.
fn fitted_bpm(times: &[f64]) -> Option<f64> {
    let n = times.len() as f64;
    if n < 8.0 {
        return None;
    }
    let mean_i = (n - 1.0) / 2.0;
    let mean_t = times.iter().sum::<f64>() / n;
    let (mut num, mut den) = (0.0, 0.0);
    for (i, t) in times.iter().enumerate() {
        let di = i as f64 - mean_i;
        num += di * (t - mean_t);
        den += di * di;
    }
    let period = num / den;
    (period > 0.0).then(|| 60.0 / period)
}

/// Give the new grid to any deck that has this track loaded.
fn update_decks(app: &AppHandle, track_id: i64, grid: Beatgrid) {
    let grid = Some(Arc::new(grid));
    let state = app.state::<AppState>();
    let mut inner = state.inner.lock().unwrap();
    for deck in 0..inner.decks.len() {
        if inner.decks[deck].track_id != Some(track_id) {
            continue;
        }
        if inner.send(Command::SetGrid { deck, grid: grid.clone() }).is_err() {
            continue;
        }
        inner.decks[deck].grid = grid.clone();
        inner.decks[deck].grid_source = "analysis".into();
        let _ = app.emit("deck-grid", DeckGridEvent { deck, grid: GridDto::of(&grid, "analysis") });
    }
}

/// 32-bit float mono WAV, the average of both channels.
fn write_mono_wav(path: &Path, audio: &TrackAudio) -> std::io::Result<()> {
    let mut w = BufWriter::new(std::fs::File::create(path)?);
    let n = audio.frames() as u32;
    let sr = audio.sample_rate;
    w.write_all(b"RIFF")?;
    w.write_all(&(36 + n * 4).to_le_bytes())?;
    w.write_all(b"WAVEfmt ")?;
    w.write_all(&16u32.to_le_bytes())?;
    w.write_all(&3u16.to_le_bytes())?; // IEEE float
    w.write_all(&1u16.to_le_bytes())?; // mono
    w.write_all(&sr.to_le_bytes())?;
    w.write_all(&(sr * 4).to_le_bytes())?;
    w.write_all(&4u16.to_le_bytes())?;
    w.write_all(&32u16.to_le_bytes())?;
    w.write_all(b"data")?;
    w.write_all(&(n * 4).to_le_bytes())?;
    for f in audio.samples.chunks_exact(2) {
        w.write_all(&(0.5 * (f[0] + f[1])).to_le_bytes())?;
    }
    w.flush()
}

// ---- the worker process ----------------------------------------------------------------

fn worker_dir() -> PathBuf {
    std::env::var_os("EARSHOT_WORKER_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../worker"))
}

#[derive(Deserialize)]
struct Reply {
    id: Option<u64>,
    ok: bool,
    error: Option<String>,
    #[serde(default)]
    detector: String,
    #[serde(default)]
    beats: Vec<f64>,
    #[serde(default)]
    beat_index: Vec<i64>,
    #[serde(default)]
    is_downbeat: Vec<bool>,
    #[serde(default)]
    median_bpm: f64,
    #[serde(default)]
    min_bpm: f64,
    #[serde(default)]
    max_bpm: f64,
    #[serde(default)]
    seconds: f64,
}

/// `python -m earshot_worker` speaking JSON lines on stdin/stdout.
struct Worker {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Worker {
    fn spawn() -> Result<Worker, String> {
        let dir = worker_dir();
        let python = dir.join(".venv/bin/python");
        if !python.exists() {
            return Err(format!(
                "the analysis worker isn't set up: run {}/setup.sh",
                dir.canonicalize().unwrap_or(dir.clone()).display()
            ));
        }
        let mut child = std::process::Command::new(&python)
            .args(["-m", "earshot_worker"])
            .current_dir(&dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("starting the analysis worker: {e}"))?;
        let stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        // Wait for {"type": "ready"}: importing torch takes a few seconds.
        let mut line = String::new();
        loop {
            line.clear();
            if stdout.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
                return Err("the analysis worker exited while starting (see the terminal for its error)".into());
            }
            if line.contains("\"ready\"") {
                break;
            }
        }
        Ok(Worker { child, stdin, stdout, next_id: 1 })
    }

    /// `on_progress` gets the worker's 0..1 progress as it arrives.
    fn analyse(&mut self, wav: &Path, on_progress: &mut dyn FnMut(f64)) -> Result<Reply, String> {
        let id = self.next_id;
        self.next_id += 1;
        let req = serde_json::json!({ "id": id, "type": "analyse", "wav": wav });
        writeln!(self.stdin, "{req}")
            .and_then(|_| self.stdin.flush())
            .map_err(|e| format!("the analysis worker has stopped: {e}"))?;
        let mut line = String::new();
        loop {
            line.clear();
            if self.stdout.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
                return Err("the analysis worker exited (see the terminal for its error)".into());
            }
            let Ok(msg) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if msg["type"] == "progress" {
                if msg["id"] == id
                    && let Some(f) = msg["fraction"].as_f64()
                {
                    on_progress(f);
                }
                continue;
            }
            let Ok(reply) = serde_json::from_value::<Reply>(msg) else {
                continue;
            };
            if reply.id.is_some_and(|r| r != id) {
                continue;
            }
            if !reply.ok {
                return Err(reply.error.unwrap_or_else(|| "analysis failed".into()));
            }
            return Ok(reply);
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End to end against the real worker: `EARSHOT_TEST_TRACK=/path/song.mp3 cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn analyses_a_real_track() {
        let path = std::env::var("EARSHOT_TEST_TRACK").expect("set EARSHOT_TEST_TRACK");
        let (audio, tags) = decode::decode_file(Path::new(&path)).unwrap();
        let wav = std::env::temp_dir().join("earshot-test.wav");
        write_mono_wav(&wav, &audio).unwrap();
        let mut w = Worker::spawn().unwrap();
        let mut seen = Vec::new();
        let r = w.analyse(&wav, &mut |f| seen.push(f)).unwrap();
        assert!(!seen.is_empty() && seen.windows(2).all(|p| p[0] <= p[1]), "progress {seen:?}");
        let grid = Beatgrid::from_indexed(&r.beats, &r.beat_index, &r.is_downbeat)
            .and_then(|g| g.refit(audio.duration()))
            .unwrap();
        let bpm = fitted_bpm(grid.times()).unwrap();
        eprintln!(
            "tag {:?} · fitted {bpm:.2} · median {:.2} · {} beats · first downbeat at {:?} s · {:.1} s",
            tags.bpm,
            r.median_bpm,
            grid.len(),
            grid.first_downbeat().map(|i| grid.times()[i]),
            r.seconds
        );
        assert!(bpm > 60.0 && bpm < 200.0);
    }

    #[test]
    fn fit_recovers_a_steady_tempo() {
        let times: Vec<f64> = (0..100).map(|i| 0.3 + i as f64 * 60.0 / 124.0).collect();
        assert!((fitted_bpm(&times).unwrap() - 124.0).abs() < 1e-9);
    }
}
