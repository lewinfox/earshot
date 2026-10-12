//! Analysis queue: up to [`JOBS`] background threads work through tracks in
//! parallel, plus one lane for tracks just loaded onto a deck; each computes the
//! waveform here and asks the one Python worker process (`earshot/worker`, Beat This!,
//! see [`worker_client`]) for the beatgrid. The worker queues those by priority, so a
//! deck's track goes first. See [`LOADED_NICE`] for how they share the CPU with
//! playback.
//!
//! The worker never decodes files itself. We send it a mono WAV from our own decoder,
//! so its beat times line up exactly with what the decks play.

use std::collections::{HashMap, VecDeque};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use earshot_engine::Command;
use earshot_engine::beatgrid::Beatgrid;
use earshot_engine::decode::{self, TrackAudio};
use earshot_engine::{energy, loudness, waveform};
use serde::Deserialize;

use super::player::Player;
use super::store::{AnalysisSave, GridSave, GridSource, Store};
use super::worker_client::{
    self, GridData, GridMode, JobPriority, Update, WorkerClient, WorkerConfig,
};
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
    /// One steady tempo: a fast tempo pass plus the small model on five excerpts.
    Static,
    /// Beat This! over the whole track; the grid follows any drift.
    Dynamic,
}

#[derive(Clone, Debug)]
pub struct AnalysisEvent {
    pub track_id: i64,
    /// "queued", "running", "done" or "error".
    pub state: &'static str,
    /// 0..1 while running.
    pub progress: Option<f64>,
    /// What it's doing now, while running.
    pub stage: Option<&'static str>,
    pub error: Option<String>,
}

/// The results themselves (BPM, grid, drift) reach the UI from the Store, as
/// `StoreEvent::TracksChanged`.

impl AnalysisEvent {
    fn new(track_id: i64, state: &'static str) -> Self {
        AnalysisEvent {
            track_id,
            state,
            progress: None,
            stage: None,
            error: None,
        }
    }
}

// Rough share of the total time before the worker starts: decoding a file, then
// the waveform. The worker reports its own progress for the rest.
const DECODED: f64 = 0.1;
const WORKER_START: f64 = 0.15;

/// Parallel background analysis jobs (override with `EARSHOT_ANALYSIS_JOBS`). They
/// decode and measure in parallel; their grids wait their turn in the one worker.
const JOBS: usize = 4;

/// Playback comes first. Background jobs (folders, previews) run in Linux's idle CPU
/// and disk classes, and only one at a time while a deck is playing. A track just
/// loaded onto a deck is decoded straight away on its own lane: normal scheduling,
/// but at this nice value, below the audio engine and the UI. The worker process runs
/// in the idle CPU class for every job; a deck's track just goes first in its queue.
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
    pub fn start(sink: Sink, player: Player, store: Store) -> Analyser {
        let ctx = Ctx {
            sink: sink.clone(),
            player,
        };
        let jobs = std::env::var("EARSHOT_ANALYSIS_JOBS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(JOBS)
            .max(1);
        // One worker for every lane, with every core: it runs one job at a time, in
        // the idle CPU class.
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        let worker = WorkerClient::start(WorkerConfig::python(cores));
        // Start it now (importing torch takes a few seconds), so a track loaded onto a
        // deck is analysed straight away.
        worker.warm_up();
        let queue = Arc::new(Queue::default());
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let playing = Arc::new(AtomicBool::new(false));
        let background_running = Arc::new(AtomicUsize::new(0));
        // The loaded-track lane, then the background ones.
        let lanes = std::iter::once(true).chain(std::iter::repeat_n(false, jobs));
        for (n, urgent) in lanes.enumerate() {
            let (q, p, a, store) = (queue.clone(), pending.clone(), ctx.clone(), store.clone());
            let worker = worker.clone();
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
                    loop {
                        let job = q.pop(urgent);
                        if !urgent {
                            take_background_slot(&playing, &running);
                        }
                        let ev = match run(&a, &store, &worker, urgent, &job) {
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
    store: &Store,
    worker: &WorkerClient,
    urgent: bool,
    job: &Job,
) -> Result<AnalysisEvent, String> {
    let progress = |fraction: f64, stage: &'static str| {
        (app.sink)(Event::Analysis(AnalysisEvent {
            progress: Some(fraction),
            stage: Some(stage),
            ..AnalysisEvent::new(job.id, "running")
        }));
    };
    let started_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    // The grid now; the result only replaces it if it's still this one when saved.
    let existing = store.reader.active_grid(job.id).map_err(|e| e.to_string())?;
    if !job.force && existing.is_some() {
        // Analysed (or hand-fixed) since this job was queued.
        return Ok(AnalysisEvent::new(job.id, "done"));
    }

    let audio = match &job.audio {
        Some(a) => a.clone(),
        None => {
            progress(0.0, "decoding");
            let path = store.reader.track_path(job.id).map_err(|e| e.to_string())?;
            Arc::new(decode::decode_file(&path)?.0)
        }
    };

    progress(DECODED, "waveform");
    let level = loudness::measure(&audio);
    let mut features = vec![
        ("energy", energy::energy(&audio)),
        ("lufs", level.lufs),
        ("peak", level.peak),
    ];
    // Waveform: cheap, done here, cached so later loads skip it.
    let cached = store
        .reader
        .waveform(job.id, waveform::BINS_PER_SECOND)
        .map_err(|e| e.to_string())?
        .is_some();
    let wave = (job.force || !cached).then(|| waveform::summarise(&audio));
    // Everything the job found goes to the Store in one transaction, at the end
    // (also when the worker fails, so the waveform and loudness aren't lost).
    let mode = match job.mode {
        Mode::Static => "static",
        Mode::Dynamic => "dynamic",
    };
    let save = |analyser: String,
                error: Option<String>,
                features: Vec<(&'static str, f64)>,
                grid: Option<GridSave>| {
        let s = AnalysisSave {
            track: job.id,
            analyser,
            mode,
            started_at,
            error,
            features,
            waveform: wave.clone(),
            grid,
        };
        store
            .call(move |w| w.save_analysis(s))
            .map_err(|e| e.to_string())
    };
    let fail = |e: String| {
        if let Err(se) = save("worker".into(), Some(e.clone()), features.clone(), None) {
            eprintln!("saving track {}'s analysis: {se}", job.id);
        }
        e
    };

    // Beatgrid: in the worker.
    let wav = std::env::temp_dir().join(format!(
        "earshot-analyse-{}-{}.wav",
        std::process::id(),
        job.id
    ));
    write_mono_wav(&wav, &audio)
        .map_err(|e| fail(format!("writing {}: {e}", wav.display())))?;
    progress(WORKER_START, "waiting for the analyser");
    let request = worker_client::Request {
        wav: wav.clone(),
        priority: if urgent {
            JobPriority::Deck
        } else {
            JobPriority::Background
        },
        grid: Some(match job.mode {
            Mode::Static => GridMode::Static,
            Mode::Dynamic => GridMode::Dynamic,
        }),
        deadline: worker_client::deadline_for(audio.duration()),
    };
    let outcome = worker.analyse(&request, &mut |u| {
        let f = match u {
            Update::Started => 0.0,
            Update::Progress { fraction, .. } => fraction,
        };
        let stage = match job.mode {
            Mode::Static if f < 0.25 => "finding the tempo",
            Mode::Static => "placing the grid",
            Mode::Dynamic if f < 0.8 => "finding beats",
            Mode::Dynamic => "refining beats",
        };
        progress(WORKER_START + (1.0 - WORKER_START) * f, stage)
    });
    let _ = std::fs::remove_file(&wav);
    let outcome = outcome.map_err(fail)?;
    let reply = match outcome.grid {
        Some(Ok(g)) => g,
        Some(Err(e)) => return Err(fail(e)),
        None => return Err(fail("the worker sent no grid".into())),
    };
    let duration = audio.duration();
    let (grid, bpm, source, min, max, steady) = match &reply.grid {
        // One tempo: a straight grid with a 1 at the worker's downbeat.
        GridData::Constant {
            bpm,
            downbeat,
            steady,
            ..
        } => {
            let g = Beatgrid::constant(*bpm, *downbeat, duration)
                .map_err(|e| fail(format!("worker returned an unusable tempo: {e}")))?;
            (g, *bpm, GridSource::Analysis, *bpm, *bpm, *steady)
        }
        // Every detected beat, so the grid follows the drift. Its BPM is the average.
        GridData::Markers {
            beats,
            beat_index,
            is_downbeat,
            min_bpm,
            max_bpm,
            ..
        } => {
            let g = Beatgrid::from_indexed(beats, beat_index, is_downbeat)
                .and_then(|g| g.refit(duration))
                .map_err(|e| fail(format!("worker returned an unusable grid: {e}")))?;
            let bpm = (g.mean_bpm() * 100.0).round() / 100.0;
            (g, bpm, GridSource::AnalysisDynamic, *min_bpm, *max_bpm, true)
        }
    };
    let drifts = job.mode == Mode::Static && !steady;

    features.extend([
        ("bpm_min", min),
        ("bpm_max", max),
        ("drifts", drifts as u8 as f64),
    ]);
    // The Store makes it the active grid only if the grid hasn't changed since the
    // job started (a grid fixed by hand meanwhile wins), and sets the BPM with it.
    let out = save(
        reply.detector.clone(),
        None,
        features,
        Some(GridSave {
            grid: grid.clone(),
            source,
            bpm,
            expected_active: existing.map(|e| e.0),
            force: job.force,
        }),
    )?;
    if out.switched {
        update_decks(app, job.id, grid, source);
    }
    let detail = match &reply.grid {
        GridData::Constant { spread_ms, .. } => format!("phase spread {spread_ms:.0} ms"),
        GridData::Markers { median_bpm, .. } => format!("median {median_bpm:.2} BPM"),
    };
    eprintln!(
        "analysed track {} ({:?}): {bpm} BPM, {detail}, moved {:.0} ms onto the kicks, {:.1} s in the worker",
        job.id, job.mode, reply.kick_shift_ms, outcome.seconds
    );
    Ok(AnalysisEvent::new(job.id, "done"))
}

/// How hard an analysis thread may compete for the machine.
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
fn update_decks(app: &Ctx, track_id: i64, grid: Beatgrid, source: GridSource) {
    let grid = Some(Arc::new(grid));
    let mut inner = app.player.lock().unwrap();
    for deck in 0..inner.decks.len() {
        if inner.decks[deck].track_id != Some(track_id) {
            continue;
        }
        let Ok(id) = super::player::deck_id(deck) else {
            continue;
        };
        let generation = inner.decks[deck].generation;
        if inner
            .send(Command::SetGrid {
                deck: id,
                generation,
                grid: grid.clone(),
            })
            .is_err()
        {
            continue;
        }
        inner.decks[deck].grid = grid.clone();
        inner.decks[deck].grid_source = source.label().into();
    }
}

/// 32-bit float mono WAV, the average of both channels.
pub(super) fn write_mono_wav(path: &Path, audio: &TrackAudio) -> std::io::Result<()> {
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
