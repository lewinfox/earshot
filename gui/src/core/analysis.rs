//! Analysis queue: up to [`JOBS`] background threads work through tracks in
//! parallel, plus one lane for tracks just loaded onto a deck; each computes the
//! waveform here and the beatgrid in its own Python worker process
//! (`earshot/worker`, Beat This!). See [`LOADED_NICE`] for how they share the CPU
//! with playback.
//!
//! The worker never decodes files itself. We send it a mono WAV from our own decoder,
//! so its beat times line up exactly with what the decks play.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use earshot_engine::Command;
use earshot_engine::beatgrid::Beatgrid;
use earshot_engine::decode::{self, TrackAudio};
use earshot_engine::{energy, loudness, waveform};
use serde::Deserialize;

use super::db::{self, Db};
use super::player::Player;
use super::{Event, Sink};

struct Job {
    id: i64,
    /// Already-decoded audio when the track was just loaded onto a deck.
    audio: Option<Arc<TrackAudio>>,
    /// Re-analyse even if the track already has a grid.
    force: bool,
    mode: Mode,
}

/// How to analyse a track, like Rekordbox's Normal / Dynamic.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// One steady tempo: a fast tempo pass plus the small model on three excerpts.
    Static,
    /// Beat This! over the whole track; the grid follows any drift.
    Dynamic,
}

/// Grid source for a dynamic grid. Static grids are plain "analysis".
pub const DYNAMIC_SOURCE: &str = "analysis (dynamic)";

#[derive(Clone, Debug)]
pub struct AnalysisEvent {
    pub track_id: i64,
    /// "queued", "running", "done" or "error".
    pub state: &'static str,
    /// 0..1 while running.
    pub progress: Option<f64>,
    /// What it's doing now, while running.
    pub stage: Option<&'static str>,
    pub bpm: Option<f64>,
    pub grid_source: Option<String>,
    pub error: Option<String>,
    /// After a static analysis: the tempo seems to drift, so dynamic might suit it.
    pub drifts: Option<bool>,
}

impl AnalysisEvent {
    fn new(track_id: i64, state: &'static str) -> Self {
        AnalysisEvent {
            track_id,
            state,
            progress: None,
            stage: None,
            bpm: None,
            grid_source: None,
            error: None,
            drifts: None,
        }
    }
}

// Rough share of the total time before the worker starts: decoding a file, then
// the waveform. The worker reports its own progress for the rest.
const DECODED: f64 = 0.1;
const WORKER_START: f64 = 0.15;

/// Parallel background analysis jobs (override with `EARSHOT_ANALYSIS_JOBS`). Each
/// runs its own worker process with its own copy of the model (a few hundred MB each).
const JOBS: usize = 4;

/// Playback comes first. Background jobs (folders, previews) run in Linux's idle CPU
/// and disk classes, and only one at a time while a deck is playing. A track just
/// loaded onto a deck is analysed straight away on its own lane: normal scheduling,
/// but at this nice value, below the audio engine and the UI.
const LOADED_NICE: i32 = 5;

#[derive(Default)]
struct Queue {
    /// (tracks just loaded onto a deck, everything else)
    jobs: Mutex<(VecDeque<Job>, VecDeque<Job>)>,
    ready: Condvar,
}

impl Queue {
    fn pop(&self, urgent: bool) -> Job {
        let mut jobs = self.jobs.lock().unwrap();
        loop {
            let lane = if urgent { &mut jobs.0 } else { &mut jobs.1 };
            if let Some(j) = lane.pop_front() {
                return j;
            }
            jobs = self.ready.wait(jobs).unwrap();
        }
    }
}

pub struct Analyser {
    queue: Arc<Queue>,
    /// Tracks waiting or running, and whether on the loaded-track lane.
    pending: Arc<Mutex<HashMap<i64, bool>>>,
    sink: Sink,
    /// A deck is playing (set by the UI every frame).
    playing: Arc<AtomicBool>,
}

/// What the analysis threads need: where to report, and the decks to update.
#[derive(Clone)]
struct Ctx {
    sink: Sink,
    player: Player,
}

impl Analyser {
    pub fn start(sink: Sink, player: Player, db: Db) -> Analyser {
        let ctx = Ctx {
            sink: sink.clone(),
            player,
        };
        let jobs = std::env::var("EARSHOT_ANALYSIS_JOBS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(JOBS)
            .max(1);
        // Split the cores between the workers: torch would otherwise give each one
        // every core, and the processes would fight over them.
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        let threads = (cores / jobs).max(1);
        let queue = Arc::new(Queue::default());
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let playing = Arc::new(AtomicBool::new(false));
        let background_running = Arc::new(AtomicUsize::new(0));
        // The loaded-track lane, then the background ones.
        let lanes = std::iter::once((true, (cores / 2).max(1)))
            .chain(std::iter::repeat_n((false, threads), jobs));
        for (n, (urgent, threads)) in lanes.enumerate() {
            let (q, p, a, db) = (queue.clone(), pending.clone(), ctx.clone(), db.clone());
            let (playing, running) = (playing.clone(), background_running.clone());
            std::thread::Builder::new()
                .name(format!("earshot-analysis-{n}"))
                .spawn(move || {
                    let priority = if urgent {
                        Priority::Nice(LOADED_NICE)
                    } else {
                        Priority::Idle
                    };
                    priority.apply();
                    let mut worker: Option<Worker> = None;
                    if urgent {
                        // Start this lane's worker now (importing torch takes a few
                        // seconds), so a loaded track is analysed straight away.
                        match Worker::spawn(threads, priority) {
                            Ok(w) => worker = Some(w),
                            Err(e) => eprintln!("analysis worker: {e}"),
                        }
                    }
                    loop {
                        let job = q.pop(urgent);
                        if !urgent {
                            take_background_slot(&playing, &running);
                        }
                        let ev = match run(&a, &db, &mut worker, threads, priority, &job) {
                            Ok(ev) => ev,
                            Err(e) => {
                                eprintln!("analysis of track {}: {e}", job.id);
                                AnalysisEvent {
                                    error: Some(e),
                                    ..AnalysisEvent::new(job.id, "error")
                                }
                            }
                        };
                        if !urgent {
                            running.fetch_sub(1, Ordering::SeqCst);
                        }
                        // Leave it pending if the other lane has taken it over.
                        let mut pending = p.lock().unwrap();
                        if pending.get(&job.id) == Some(&urgent) {
                            pending.remove(&job.id);
                        }
                        drop(pending);
                        (a.sink)(Event::Analysis(ev));
                    }
                })
                .expect("spawn analysis thread");
        }
        Analyser {
            queue,
            pending,
            sink,
            playing,
        }
    }

    /// Tell the queue whether a deck is playing, which holds background jobs back.
    pub fn set_playing(&self, on: bool) {
        self.playing.store(on, Ordering::Relaxed);
    }

    /// Queue a track unless it is already waiting. A track just loaded onto a deck
    /// (`audio` given) goes to the front of the loaded-track lane, ahead of everything,
    /// and is taken over from the background if it was waiting or running there.
    pub fn queue(&self, id: i64, audio: Option<Arc<TrackAudio>>, force: bool, mode: Mode) {
        let urgent = audio.is_some();
        let job = Job {
            id,
            audio,
            force,
            mode,
        };
        let mut jobs = self.queue.jobs.lock().unwrap();
        let mut pending = self.pending.lock().unwrap();
        match pending.get(&id) {
            // Already on the loaded-track lane, or waiting in the background.
            Some(true) => return,
            Some(false) if !urgent => return,
            // In the background (waiting or already running) but now loaded: take it
            // out of the background queue and analyse it on the loaded-track lane. A
            // background run already under way finishes too; the grid is the same.
            Some(false) => {
                jobs.1.retain(|j| j.id != id);
            }
            None => (self.sink)(Event::Analysis(AnalysisEvent::new(id, "queued"))),
        }
        pending.insert(id, urgent);
        if urgent {
            // The newest load first.
            jobs.0.push_front(job);
        } else {
            jobs.1.push_back(job);
        }
        self.queue.ready.notify_all();
    }
}

/// Wait for a turn to run a background job: any time while nothing is playing, but
/// only one at a time while a deck plays.
fn take_background_slot(playing: &AtomicBool, running: &AtomicUsize) {
    loop {
        let n = running.load(Ordering::SeqCst);
        if playing.load(Ordering::Relaxed) && n >= 1 {
            std::thread::sleep(std::time::Duration::from_millis(250));
            continue;
        }
        if running
            .compare_exchange(n, n + 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return;
        }
    }
}

fn run(
    app: &Ctx,
    db: &Db,
    worker: &mut Option<Worker>,
    threads: usize,
    priority: Priority,
    job: &Job,
) -> Result<AnalysisEvent, String> {
    let progress = |fraction: f64, stage: &'static str| {
        (app.sink)(Event::Analysis(AnalysisEvent {
            progress: Some(fraction),
            stage: Some(stage),
            ..AnalysisEvent::new(job.id, "running")
        }));
    };
    let existing = db::grid(&db.lock().unwrap(), job.id)?;
    if !job.force
        && let Some((_, source)) = &existing
    {
        // Analysed (or hand-fixed) since this job was queued.
        return Ok(AnalysisEvent {
            grid_source: Some(source.clone()),
            ..AnalysisEvent::new(job.id, "done")
        });
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
    {
        let conn = db.lock().unwrap();
        db::set_energy(&conn, job.id, energy::energy(&audio))?;
        db::set_loudness(&conn, job.id, &loudness::measure(&audio))?;
    }
    // Waveform: cheap, done here, cached so later loads skip it.
    let cached = db::waveform(&db.lock().unwrap(), job.id, waveform::BINS_PER_SECOND)?.is_some();
    if job.force || !cached {
        let wave = waveform::summarise(&audio);
        db::save_waveform(
            &db.lock().unwrap(),
            job.id,
            &wave,
            waveform::BINS_PER_SECOND,
        )?;
    }

    // Beatgrid: in the worker.
    let wav = std::env::temp_dir().join(format!(
        "earshot-analyse-{}-{}.wav",
        std::process::id(),
        job.id
    ));
    write_mono_wav(&wav, &audio).map_err(|e| format!("writing {}: {e}", wav.display()))?;
    if worker.is_none() {
        progress(WORKER_START, "starting the analyser");
        *worker = Some(Worker::spawn(threads, priority)?);
    }
    progress(WORKER_START, "finding the tempo");
    let reply = worker.as_mut().unwrap().analyse(&wav, job.mode, &mut |f| {
        let stage = match job.mode {
            Mode::Static if f < 0.25 => "finding the tempo",
            Mode::Static => "placing the grid",
            Mode::Dynamic if f < 0.8 => "finding beats",
            Mode::Dynamic => "refining beats",
        };
        progress(WORKER_START + (1.0 - WORKER_START) * f, stage)
    });
    let _ = std::fs::remove_file(&wav);
    let reply = match reply {
        Ok(r) => r,
        Err(e) => {
            *worker = None; // start a fresh one for the next job
            return Err(e);
        }
    };
    let duration = audio.duration();
    let (grid, bpm, source, min, max) = match job.mode {
        // One tempo: a straight grid with a 1 at the worker's downbeat.
        Mode::Static => {
            let g = Beatgrid::constant(reply.bpm, reply.downbeat, duration)
                .map_err(|e| format!("worker returned an unusable tempo: {e}"))?;
            (g, reply.bpm, "analysis", reply.bpm, reply.bpm)
        }
        // Every detected beat, so the grid follows the drift. Its BPM is the average.
        Mode::Dynamic => {
            let g = Beatgrid::from_indexed(&reply.beats, &reply.beat_index, &reply.is_downbeat)
                .and_then(|g| g.refit(duration))
                .map_err(|e| format!("worker returned an unusable grid: {e}"))?;
            let bpm = (g.mean_bpm() * 100.0).round() / 100.0;
            (g, bpm, DYNAMIC_SOURCE, reply.min_bpm, reply.max_bpm)
        }
    };
    let drifts = job.mode == Mode::Static && !reply.steady;

    // A grid fixed by hand while this job waited wins over an automatic analysis.
    let keep_user = !job.force && existing.as_ref().is_some_and(|(_, s)| s == "user");
    {
        let conn = db.lock().unwrap();
        db::save_analysis(&conn, job.id, bpm, min, max, &reply.detector)?;
        db::set_mode(&conn, job.id, job.mode == Mode::Dynamic, drifts)?;
        if !keep_user {
            db::save_grid(&conn, job.id, &grid, source)?;
        }
    }
    if !keep_user {
        update_decks(app, job.id, grid, source);
    }
    eprintln!(
        "analysed track {} ({:?}): {bpm} BPM (dynamic median {:.2}), phase spread {:.0} ms, {:.1} s in the worker",
        job.id, job.mode, reply.median_bpm, reply.spread_ms, reply.seconds
    );
    Ok(AnalysisEvent {
        bpm: Some(bpm),
        grid_source: Some(if keep_user { "user" } else { source }.into()),
        drifts: Some(drifts),
        ..AnalysisEvent::new(job.id, "done")
    })
}

/// How hard an analysis thread and its worker process may compete for the machine.
#[derive(Clone, Copy, Debug)]
enum Priority {
    /// Linux's idle CPU and disk classes: only what nothing else wants.
    Idle,
    /// Normal scheduling at this nice value (higher is nicer).
    Nice(i32),
}

impl Priority {
    /// Apply to the calling thread (on Linux, priorities are per thread). A new
    /// process inherits it from the thread that starts it.
    fn apply(self) {
        #[cfg(target_os = "linux")]
        // SAFETY: plain syscalls on the calling thread (pid/who 0) with valid args.
        unsafe {
            match self {
                Priority::Idle => {
                    let param = libc::sched_param { sched_priority: 0 };
                    if libc::sched_setscheduler(0, libc::SCHED_IDLE, &param) != 0 {
                        libc::setpriority(libc::PRIO_PROCESS, 0, 19);
                    }
                    // ioprio_set(IOPRIO_WHO_PROCESS, 0, IOPRIO_CLASS_IDLE << 13)
                    libc::syscall(libc::SYS_ioprio_set, 1, 0, 3 << 13);
                }
                Priority::Nice(n) => {
                    libc::setpriority(libc::PRIO_PROCESS, 0, n);
                }
            }
        }
    }
}

/// Give the new grid to any deck that has this track loaded.
fn update_decks(app: &Ctx, track_id: i64, grid: Beatgrid, source: &str) {
    let grid = Some(Arc::new(grid));
    let mut inner = app.player.lock().unwrap();
    for deck in 0..inner.decks.len() {
        if inner.decks[deck].track_id != Some(track_id) {
            continue;
        }
        if inner
            .send(Command::SetGrid {
                deck,
                grid: grid.clone(),
            })
            .is_err()
        {
            continue;
        }
        inner.decks[deck].grid = grid.clone();
        inner.decks[deck].grid_source = source.into();
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
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../worker"))
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
    // Static analysis.
    #[serde(default)]
    bpm: f64,
    #[serde(default)]
    downbeat: f64,
    #[serde(default)]
    spread_ms: f64,
    #[serde(default = "yes")]
    steady: bool,
}

fn yes() -> bool {
    true
}

/// `python -m earshot_worker` speaking JSON lines on stdin/stdout.
struct Worker {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Worker {
    /// `threads`: CPU threads the worker's torch may use.
    /// `priority`: the worker inherits it from this (analysis) thread; the env var
    /// stops it putting itself in the idle class when it shouldn't.
    fn spawn(threads: usize, priority: Priority) -> Result<Worker, String> {
        let dir = worker_dir();
        let python = dir.join(".venv/bin/python");
        if !python.exists() {
            return Err(format!(
                "the analysis worker isn't set up: run {}/setup.sh",
                dir.canonicalize().unwrap_or(dir.clone()).display()
            ));
        }
        let mut command = std::process::Command::new(&python);
        command
            .args(["-m", "earshot_worker"])
            .current_dir(&dir)
            .env("OMP_NUM_THREADS", threads.to_string())
            .env("MKL_NUM_THREADS", threads.to_string())
            .env("OPENBLAS_NUM_THREADS", threads.to_string())
            .env(
                "EARSHOT_WORKER_PRIORITY",
                match priority {
                    Priority::Idle => "idle",
                    Priority::Nice(_) => "inherit",
                },
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        // End the worker when the app does, even mid-analysis (the analysis threads
        // aren't joined on exit, so `Drop` never runs for them).
        #[cfg(target_os = "linux")]
        // SAFETY: prctl is async-signal-safe; it runs in the child before exec.
        unsafe {
            use std::os::unix::process::CommandExt;
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("starting the analysis worker: {e}"))?;
        let stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        // Wait for {"type": "ready"}: importing torch takes a few seconds.
        let mut line = String::new();
        loop {
            line.clear();
            if stdout.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
                return Err(
                    "the analysis worker exited while starting (see the terminal for its error)"
                        .into(),
                );
            }
            if line.contains("\"ready\"") {
                break;
            }
        }
        Ok(Worker {
            child,
            stdin,
            stdout,
            next_id: 1,
        })
    }

    /// `on_progress` gets the worker's 0..1 progress as it arrives.
    fn analyse(
        &mut self,
        wav: &Path,
        mode: Mode,
        on_progress: &mut dyn FnMut(f64),
    ) -> Result<Reply, String> {
        let id = self.next_id;
        self.next_id += 1;
        let kind = match mode {
            Mode::Static => "analyse_static",
            Mode::Dynamic => "analyse",
        };
        let req = serde_json::json!({ "id": id, "type": kind, "wav": wav });
        writeln!(self.stdin, "{req}")
            .and_then(|_| self.stdin.flush())
            .map_err(|e| format!("the analysis worker has stopped: {e}"))?;
        let mut line = String::new();
        loop {
            line.clear();
            if self
                .stdout
                .read_line(&mut line)
                .map_err(|e| e.to_string())?
                == 0
            {
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

    /// End to end against the real worker, both modes:
    /// `EARSHOT_TEST_TRACK=/path/song.mp3 cargo test -p earshot-gui -- --include-ignored analyses_a_real_track`.
    #[test]
    #[ignore]
    fn analyses_a_real_track() {
        let path = std::env::var("EARSHOT_TEST_TRACK").expect("set EARSHOT_TEST_TRACK");
        let (audio, _) = decode::decode_file(Path::new(&path)).unwrap();
        let wav = std::env::temp_dir().join(format!("earshot-test-{}.wav", std::process::id()));
        write_mono_wav(&wav, &audio).unwrap();
        let mut w = Worker::spawn(4, Priority::Nice(LOADED_NICE)).unwrap();
        for mode in [Mode::Static, Mode::Dynamic] {
            let mut seen = Vec::new();
            let r = w.analyse(&wav, mode, &mut |f| seen.push(f)).unwrap();
            assert!(
                !seen.is_empty() && seen.windows(2).all(|p| p[0] <= p[1]),
                "progress {seen:?}"
            );
            eprintln!(
                "{mode:?}: bpm {} (dynamic median {:.2}), downbeat {:.3} s, spread {:.1} ms, steady {}, {} beats, {:.1} s",
                r.bpm,
                r.median_bpm,
                r.downbeat,
                r.spread_ms,
                r.steady,
                r.beats.len(),
                r.seconds
            );
        }
        let _ = std::fs::remove_file(&wav);
    }
}
