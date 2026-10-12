//! The analysis queue and the lanes that work through it.
//!
//! - **What.** The analysers are listed in [`registry`]; each result is saved with the
//!   analyser's version and the hash of the audio, so stale results can be found.
//! - **Queue.** [`queue::JobQueue`] is keyed by (track, analyser): asking again
//!   merges into what's queued. It is saved in the `jobs` table, so it survives a
//!   restart.
//! - **Lanes.** One deck lane, for tracks just loaded onto a deck, sends its grids to
//!   the deck worker: a worker process started with the app and kept running, at
//!   normal CPU priority, so a loaded track is analysed straight away. Background
//!   lanes (library work) each have their own worker process, started when there's
//!   work and stopped after a minute without (`worker_client::IDLE_EXIT`), in the idle
//!   CPU class. They ramp up with the backlog to [`SETTING_BACKGROUND_WORKERS`].
//! - **One decode.** A lane takes every analyser waiting for a track at once: it
//!   decodes the file once (or uses the deck's copy), runs the Rust analysers, then
//!   sends the worker one job for the worker analysers, and saves everything in one
//!   transaction.
//!
//! The worker never decodes files itself. We send it a mono WAV from our own decoder,
//! so its beat times line up exactly with what the decks play.

pub mod queue;
pub mod registry;

use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex};

use earshot_engine::Command;
use earshot_engine::beatgrid::{Beatgrid, Constant, GridError, GridModel};
use earshot_engine::decode::{self, TrackAudio};
use earshot_engine::{energy, loudness, waveform};
use serde::Deserialize;

use self::queue::{Batch, JobQueue, Lane, Req, Want};
use self::registry::{AnalyserId, REGISTRY, RunsIn};
use super::player::Player;
use super::store::{
    AnalysisSave, GridSave, GridSource, JobRow, ResultState, Store, StoreCmd, grid_bpm,
};
use super::track::AnalysisState;
use super::worker_client::{
    self, GridData, GridMode, JobPriority, Role, Update, WorkerClient, WorkerConfig,
};
use super::{Event, Sink};

/// How to analyse a track, like Rekordbox's Normal / Dynamic.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// One steady tempo: a fast tempo pass plus the small model on five excerpts.
    Static,
    /// Beat This! over the whole track; the grid follows any drift.
    Dynamic,
}

impl Mode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Mode::Static => "static",
            Mode::Dynamic => "dynamic",
        }
    }

    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "static" => Some(Mode::Static),
            "dynamic" => Some(Mode::Dynamic),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AnalysisEvent {
    pub track_id: i64,
    /// Queued, running or failed; `None` once it's done.
    pub state: Option<AnalysisState>,
}

/// The results themselves (BPM, grid, drift) reach the UI from the Store, as
/// `StoreEvent::TracksChanged`.

impl AnalysisEvent {
    fn new(track_id: i64, state: Option<AnalysisState>) -> Self {
        AnalysisEvent { track_id, state }
    }
}

// Rough share of the total time before the worker starts: decoding a file, then
// the waveform. The worker reports its own progress for the rest.
const DECODED: f64 = 0.1;
const WORKER_START: f64 = 0.15;

/// The setting (in the Store's `settings`) for how many background worker processes
/// may run at once, [`DEFAULT_BACKGROUND_WORKERS`] if unset; read at startup, changed
/// live by [`Analyser::set_background_workers`], and kept to 1..=CPU cores. Each
/// process holds its own copy of torch and the models, and they share the cores, so
/// more than the core count only adds memory.
pub const SETTING_BACKGROUND_WORKERS: &str = "analysis.background_workers";
pub const DEFAULT_BACKGROUND_WORKERS: usize = 4;

/// Playback comes first. Background lanes (folders, previews) run in Linux's idle CPU
/// and disk classes, and only one at a time while a deck plays in Perform mode; so do their
/// worker processes. The deck lane decodes at this nice value, below the audio engine
/// and the UI; its worker process runs at normal priority.
const LOADED_NICE: i32 = 5;

pub struct Analyser {
    shared: Arc<Shared>,
}

/// The CPU cores: the most background workers there can be.
pub fn cores() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get())
}

struct Shared {
    queue: Mutex<JobQueue>,
    /// Signalled whenever a lane might now be able to take work.
    ready: Condvar,
    store: Store,
    sink: Sink,
    deck: WorkerClient,
    /// One per background lane.
    background: Vec<WorkerClient>,
}

impl Analyser {
    /// `worker` says how to start a worker process (the real one, or a test's stand-in).
    pub fn start(
        sink: Sink,
        player: Player,
        store: Store,
        worker: fn(Role, usize) -> WorkerConfig,
    ) -> Analyser {
        let cores = cores();
        let cap = store
            .reader
            .setting(SETTING_BACKGROUND_WORKERS)
            .ok()
            .flatten()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(DEFAULT_BACKGROUND_WORKERS)
            .clamp(1, cores);
        // Started now (importing torch takes a few seconds), so a track loaded onto a
        // deck is analysed straight away. It runs at normal priority, so it gets half
        // the cores: the rest are left for the audio engine and the UI.
        let deck = WorkerClient::start(worker(Role::Deck, (cores / 2).max(1)));
        deck.warm_up();
        // A lane (and a worker, which starts only when it has work) per core, so the
        // cap can change while the app runs. The cores are shared between the workers
        // the cap allowed at startup.
        let threads = (cores / cap).max(1);
        let background = (0..cores)
            .map(|_| WorkerClient::start(worker(Role::Background, threads)))
            .collect();
        let shared = Arc::new(Shared {
            queue: Mutex::new(JobQueue::new(cap)),
            ready: Condvar::new(),
            store,
            sink,
            deck,
            background,
        });
        let lanes = std::iter::once(Lane::Deck).chain((0..cores).map(Lane::Background));
        for (n, lane) in lanes.enumerate() {
            let (shared, player) = (shared.clone(), player.clone());
            std::thread::Builder::new()
                .name(format!("earshot-analysis-{n}"))
                .spawn(move || shared.lane(lane, &player))
                .expect("spawn analysis thread");
        }
        let analyser = Analyser { shared };
        analyser.restore();
        analyser
    }

    /// How many background workers may run at once.
    pub fn background_workers(&self) -> usize {
        self.shared.queue.lock().unwrap().background_cap
    }

    /// Let up to `n` background workers run at once (kept to 1..=cores), from now on.
    /// Fewer: the ones over finish their track and stop taking more. Returns the cap.
    /// (Saving it is the caller's: [`SETTING_BACKGROUND_WORKERS`].)
    pub fn set_background_workers(&self, n: usize) -> usize {
        let n = n.clamp(1, self.shared.background.len());
        self.shared.queue.lock().unwrap().background_cap = n;
        self.shared.ready.notify_all();
        n
    }

    /// Tell the queue whether a deck is playing, which holds background jobs back.
    pub fn set_playing(&self, on: bool) {
        let mut q = self.shared.queue.lock().unwrap();
        if q.playing != on {
            q.playing = on;
            drop(q);
            self.shared.ready.notify_all();
        }
    }

    /// Make sure a track is analysed. A track just loaded onto a deck (`audio` given)
    /// goes to the deck lane, ahead of everything, taking it over from the background
    /// if it was waiting or running there. `force` re-analyses the grid whatever there
    /// is (in `mode`), and anything else whose result is stale.
    pub fn queue(&self, id: i64, audio: Option<Arc<TrackAudio>>, force: bool, mode: Mode) {
        let priority = match audio {
            Some(_) => JobPriority::Deck,
            None => JobPriority::Background,
        };
        let wants: Vec<(AnalyserId, Req)> = AnalyserId::ALL
            .iter()
            .map(|&a| {
                let want = match (a, force) {
                    (_, false) => Want::Missing,
                    (AnalyserId::Grid, true) => Want::Force,
                    (_, true) => Want::Stale,
                };
                (
                    a,
                    Req {
                        priority,
                        want,
                        mode,
                    },
                )
            })
            .collect();
        self.shared.request(id, &wants, audio);
    }

    /// The tracks with a stale result: an older analyser version, other audio,
    /// failed, or missing. Sorted, each once.
    pub fn stale_tracks(&self) -> Result<Vec<i64>, String> {
        let mut all: Vec<i64> = vec![];
        for spec in REGISTRY {
            all.extend(self.stale(spec)?);
        }
        all.sort_unstable();
        all.dedup();
        Ok(all)
    }

    fn stale(&self, spec: &registry::AnalyserSpec) -> Result<Vec<i64>, String> {
        self.shared
            .store
            .reader
            .stale_tracks(spec.id.as_str(), spec.version)
            .map_err(|e| e.to_string())
    }

    /// Queue everything whose results are stale (see [`Analyser::stale_tracks`]), as
    /// background work. A grid is redone in the mode it has. Returns how many tracks.
    pub fn queue_stale(&self) -> Result<usize, String> {
        let reader = &self.shared.store.reader;
        let mut tracks = vec![];
        for spec in REGISTRY {
            for id in self.stale(spec)? {
                let mode = match reader.active_grid(id) {
                    Ok(Some((_, GridSource::AnalysisDynamic))) => Mode::Dynamic,
                    _ => Mode::Static,
                };
                let req = Req {
                    priority: JobPriority::Background,
                    want: Want::Stale,
                    mode,
                };
                self.shared.request(id, &[(spec.id, req)], None);
                tracks.push(id);
            }
        }
        tracks.sort_unstable();
        tracks.dedup();
        Ok(tracks.len())
    }

    /// Put back what was queued when the app last closed (as background work).
    fn restore(&self) {
        let rows = match self.shared.store.reader.queued_jobs() {
            Ok(r) => r,
            Err(e) => return eprintln!("reading the analysis queue: {e}"),
        };
        for row in rows {
            let (Some(a), Some(want)) = (AnalyserId::parse(&row.analyser), Want::parse(&row.want))
            else {
                continue;
            };
            let mode = row
                .params
                .as_deref()
                .and_then(Mode::parse)
                .unwrap_or(Mode::Static);
            let req = Req {
                priority: JobPriority::Background,
                want,
                mode,
            };
            self.shared.request(row.track, &[(a, req)], None);
        }
    }
}

impl Shared {
    fn emit(&self, ev: AnalysisEvent) {
        (self.sink)(Event::Analysis(ev));
    }

    fn worker(&self, lane: Lane) -> &WorkerClient {
        match lane {
            Lane::Deck => &self.deck,
            Lane::Background(n) => &self.background[n],
        }
    }

    fn request(&self, track: i64, wants: &[(AnalyserId, Req)], audio: Option<Arc<TrackAudio>>) {
        let mut q = self.queue.lock().unwrap();
        let r = q.request(track, wants, audio);
        self.save_queue(&q, track);
        drop(q);
        if let Some((lane, Some(job))) = r.superseded {
            self.worker(lane).cancel(job);
        }
        if r.new_track || r.superseded.is_some() {
            self.emit(AnalysisEvent::new(track, Some(AnalysisState::Queued)));
        }
        self.ready.notify_all();
    }

    /// Save what's queued for a track. Called with the queue locked, so the Store gets
    /// the changes in order.
    fn save_queue(&self, q: &JobQueue, track: i64) {
        let jobs = q
            .requests(track)
            .into_iter()
            .map(|(a, r)| JobRow {
                track,
                analyser: a.as_str().into(),
                want: r.want.as_str().into(),
                params: Some(r.mode.as_str().into()),
            })
            .collect();
        self.store.send(StoreCmd::SaveJobs { track, jobs });
    }

    /// A lane's thread: take a track, analyse it, repeat.
    fn lane(&self, lane: Lane, player: &Player) {
        match lane {
            Lane::Deck => Priority::Nice(LOADED_NICE).apply(),
            Lane::Background(_) => Priority::Idle.apply(),
        }
        loop {
            let batch = {
                let mut q = self.queue.lock().unwrap();
                loop {
                    if let Some(b) = q.take(lane) {
                        break b;
                    }
                    q = self.ready.wait(q).unwrap();
                }
            };
            let track = batch.track;
            let result = self.run(lane, player, &batch);
            let superseded = batch.superseded.load(Ordering::Relaxed);
            if let Err(e) = &result
                && !superseded
            {
                eprintln!("analysis of track {track}: {e}");
            }
            let mut q = self.queue.lock().unwrap();
            let idle = q.finish(lane).is_some_and(|(_, idle)| idle);
            self.save_queue(&q, track);
            drop(q);
            self.ready.notify_all();
            self.emit(match result {
                // More was asked for meanwhile (or this was taken over): still waiting.
                _ if !idle => AnalysisEvent::new(track, Some(AnalysisState::Queued)),
                Ok(()) => AnalysisEvent::new(track, None),
                Err(e) => AnalysisEvent::new(track, Some(AnalysisState::Failed(e))),
            });
        }
    }

    fn run(&self, lane: Lane, player: &Player, batch: &Batch) -> Result<(), String> {
        let id = batch.track;
        let store = &self.store;
        let progress = |fraction: f64, stage: &'static str| {
            self.emit(AnalysisEvent::new(
                id,
                Some(AnalysisState::Running {
                    progress: fraction,
                    stage,
                }),
            ));
        };
        let started_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        // The grid now; the result only replaces it if it's still this one when saved.
        // Read on the writer thread, so a run this one took over either saved before
        // this or finds itself superseded when it tries (and saves nothing).
        let existing = store
            .call(move |w| w.active_grid(id))
            .map_err(|e| e.to_string())?;
        let results = store
            .reader
            .analysis_results(id)
            .map_err(|e| e.to_string())?;
        // Done or hand-fixed since it was queued, perhaps.
        let todo: Vec<(AnalyserId, Req)> = batch
            .analysers
            .iter()
            .copied()
            .filter(|(a, r)| needed(*a, r.want, &results, existing))
            .collect();
        if todo.is_empty() {
            return Ok(());
        }
        let has = |a: AnalyserId| todo.iter().any(|t| t.0 == a);

        let audio = match &batch.audio {
            Some(a) => a.clone(),
            None => {
                progress(0.0, "decoding");
                let path = store.reader.track_path(id).map_err(|e| e.to_string())?;
                Arc::new(decode::decode_file(&path)?.0)
            }
        };

        // The Rust analysers, in registry order.
        progress(DECODED, "waveform");
        let mut done: Vec<(&'static str, u32, Option<String>)> = vec![];
        let mut ran = |a: AnalyserId| done.push((a.as_str(), a.spec().version, None));
        let wave = has(AnalyserId::Waveform).then(|| waveform::summarise(&audio));
        if wave.is_some() {
            ran(AnalyserId::Waveform);
        }
        let mut features = vec![];
        if has(AnalyserId::Loudness) {
            let level = loudness::measure(&audio);
            features.extend([("lufs", level.lufs), ("peak", level.peak)]);
            ran(AnalyserId::Loudness);
        }
        if has(AnalyserId::Energy) {
            features.push(("energy", energy::energy(&audio)));
            ran(AnalyserId::Energy);
        }
        // The worker's part: so far just the grid.
        let grid_req = todo
            .iter()
            .find(|t| t.0.spec().runs_in == RunsIn::Worker)
            .inspect(|t| debug_assert_eq!(t.0, AnalyserId::Grid))
            .map(|t| t.1);
        let mode = grid_req.map_or(Mode::Static, |r| r.mode);
        // Everything the job found goes to the Store in one transaction, at the end
        // (also when the worker fails, so the waveform and loudness aren't lost). Not
        // if a newer request took this work over: its own run saves.
        let save = |analyser: String,
                    features: Vec<(&'static str, f64)>,
                    grid: Option<GridSave>,
                    results: Vec<(&'static str, u32, Option<String>)>| {
            let s = AnalysisSave {
                track: id,
                analyser,
                mode: mode.as_str(),
                started_at,
                error: results.iter().find_map(|r| r.2.clone()),
                features,
                waveform: wave.clone(),
                grid,
                results,
            };
            let superseded = batch.superseded.clone();
            store
                .call(move |w| {
                    if superseded.load(Ordering::Relaxed) {
                        return Ok(None);
                    }
                    w.save_analysis(s).map(Some)
                })
                .map_err(|e| e.to_string())
        };
        let Some(grid_req) = grid_req else {
            let names: Vec<&str> = done.iter().map(|d| d.0).collect();
            save(names.join(","), features, None, done)?;
            return Ok(());
        };
        let grid_version = AnalyserId::Grid.spec().version;
        let fail = |e: String| {
            let mut results = done.clone();
            results.push((AnalyserId::Grid.as_str(), grid_version, Some(e.clone())));
            if !batch.superseded.load(Ordering::Relaxed)
                && let Err(se) = save("worker".into(), features.clone(), None, results)
            {
                eprintln!("saving track {id}'s analysis: {se}");
            }
            e
        };

        // Beatgrid: in the worker.
        let wav =
            std::env::temp_dir().join(format!("earshot-analyse-{}-{}.wav", std::process::id(), id));
        write_mono_wav(&wav, &audio)
            .map_err(|e| fail(format!("writing {}: {e}", wav.display())))?;
        progress(WORKER_START, "waiting for the analyser");
        let request = worker_client::Request {
            wav: wav.clone(),
            priority: match lane {
                Lane::Deck => JobPriority::Deck,
                Lane::Background(_) => JobPriority::Background,
            },
            grid: Some(match grid_req.mode {
                Mode::Static => GridMode::Static,
                Mode::Dynamic => GridMode::Dynamic,
            }),
            deadline: worker_client::deadline_for(audio.duration()),
        };
        let worker = self.worker(lane);
        let job = worker.submit(&request);
        if !self.queue.lock().unwrap().worker_job(lane, job.id()) {
            worker.cancel(job.id()); // taken over while we decoded
        }
        let outcome = job.wait(&mut |u| {
            let f = match u {
                Update::Started => 0.0,
                Update::Progress { fraction, .. } => fraction,
            };
            let stage = match grid_req.mode {
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
        let (grid, source) = grid_from_result(&reply.grid, audio.duration())
            .map_err(|e| fail(format!("worker returned an unusable grid: {e}")))?;
        let bpm = grid_bpm(&grid);
        let (min, max, steady) = match &reply.grid {
            GridData::Constant { bpm, steady, .. } => (*bpm, *bpm, *steady),
            GridData::Markers {
                min_bpm, max_bpm, ..
            } => (*min_bpm, *max_bpm, true),
        };
        let drifts = grid_req.mode == Mode::Static && !steady;

        features.extend([
            ("bpm_min", min),
            ("bpm_max", max),
            ("drifts", drifts as u8 as f64),
            ("first_beat", reply.first_beat),
        ]);
        done.push((AnalyserId::Grid.as_str(), reply.version, None));
        // The Store makes it the active grid only if the grid hasn't changed since the
        // job started (a grid fixed by hand meanwhile wins), and sets the BPM with it.
        let out = save(
            reply.detector.clone(),
            features,
            Some(GridSave {
                grid: grid.clone(),
                source,
                expected_active: existing.map(|e| e.0),
                force: grid_req.want == Want::Force,
            }),
            done,
        )?;
        if let Some(o) = out {
            update_decks(
                player,
                id,
                o.switched.then_some((grid, source)),
                reply.first_beat,
            );
        }
        let detail = match &reply.grid {
            GridData::Constant { spread_ms, .. } => format!("phase spread {spread_ms:.0} ms"),
            GridData::Markers { median_bpm, .. } => format!("median {median_bpm:.2} BPM"),
        };
        eprintln!(
            "analysed track {id} ({:?}): {bpm} BPM, {detail}, moved {:.0} ms onto the kicks, {:.1} s in the worker",
            grid_req.mode, reply.kick_shift_ms, outcome.seconds
        );
        Ok(())
    }
}

/// The grid a worker's result describes, and where it came from. One tempo: a steady
/// grid with a 1 at the worker's downbeat, saved as just those numbers. Otherwise
/// every detected beat, so the grid follows the drift.
fn grid_from_result(data: &GridData, duration: f64) -> Result<(Beatgrid, GridSource), GridError> {
    Ok(match data {
        GridData::Constant { bpm, downbeat, .. } => (
            Beatgrid::from_model(
                GridModel::Constant(Constant::new(*bpm, *downbeat)),
                duration,
            )?,
            GridSource::Analysis,
        ),
        GridData::Markers {
            beats,
            beat_index,
            is_downbeat,
            ..
        } => (
            Beatgrid::from_indexed(beats, beat_index, is_downbeat)?.refit(duration)?,
            GridSource::AnalysisDynamic,
        ),
    })
}

/// Whether analyser `a` still has to run for a track, given what it asked for, the
/// saved results, and the active grid.
fn needed(
    a: AnalyserId,
    want: Want,
    results: &[ResultState],
    grid: Option<(i64, GridSource)>,
) -> bool {
    let spec = a.spec();
    let result = results.iter().find(|r| r.analyser == spec.id.as_str());
    let fresh = result.is_some_and(|r| !r.failed && r.version >= spec.version && r.same_audio);
    match (a, want) {
        (_, Want::Force) => true,
        // Any grid will do: a hand-fixed or imported one, or an analysed one from
        // before results were versioned.
        (AnalyserId::Grid, Want::Missing) => grid.is_none(),
        (_, Want::Missing) => result.is_none(),
        // Only ever replaces an analysed grid.
        (AnalyserId::Grid, Want::Stale) => {
            !fresh
                && grid.is_none_or(|g| {
                    matches!(g.1, GridSource::Analysis | GridSource::AnalysisDynamic)
                })
        }
        (_, Want::Stale) => !fresh,
    }
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

/// Give a finished grid analysis to every player that has this track loaded (Prepare's
/// too): the new grid, if it's now the active one, then the initial cue on the grid
/// line nearest `first_beat`. The engine moves the cue and the playhead there only if
/// nothing has played, moved or cued that player since the load (`InitialCue`), so a
/// cue set by hand is never overridden.
fn update_decks(
    player: &Player,
    track_id: i64,
    grid: Option<(Beatgrid, GridSource)>,
    first_beat: f64,
) {
    let grid = grid.map(|(g, source)| (Some(Arc::new(g)), source));
    let mut inner = player.lock().unwrap();
    for deck in 0..inner.decks.len() {
        if inner.decks[deck].track_id != Some(track_id) {
            continue;
        }
        let Ok(id) = super::player::deck_id(deck) else {
            continue;
        };
        let generation = inner.decks[deck].generation;
        if let Some((grid, source)) = &grid {
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
        let at = super::player::initial_cue(inner.decks[deck].grid.as_deref(), Some(first_beat));
        if let Some(at) = at {
            let _ = inner.send(Command::InitialCue {
                deck: id,
                generation,
                at,
            });
        }
    }
}

/// 32-bit float mono WAV, the average of both channels.
pub(super) fn write_mono_wav(path: &Path, audio: &TrackAudio) -> std::io::Result<()> {
    write_wav(path, audio, false)
}

/// A float WAV of both channels (the tempo lock tool stretches the stereo).
pub(super) fn write_stereo_wav(path: &Path, audio: &TrackAudio) -> std::io::Result<()> {
    write_wav(path, audio, true)
}

fn write_wav(path: &Path, audio: &TrackAudio, stereo: bool) -> std::io::Result<()> {
    let mut w = BufWriter::new(std::fs::File::create(path)?);
    let channels: u16 = if stereo { 2 } else { 1 };
    let frame = 4 * channels as u32;
    let n = audio.frames() as u32;
    let sr = audio.sample_rate;
    w.write_all(b"RIFF")?;
    w.write_all(&(36 + n * frame).to_le_bytes())?;
    w.write_all(b"WAVEfmt ")?;
    w.write_all(&16u32.to_le_bytes())?;
    w.write_all(&3u16.to_le_bytes())?; // IEEE float
    w.write_all(&channels.to_le_bytes())?;
    w.write_all(&sr.to_le_bytes())?;
    w.write_all(&(sr * frame).to_le_bytes())?;
    w.write_all(&(frame as u16).to_le_bytes())?;
    w.write_all(&32u16.to_le_bytes())?;
    w.write_all(b"data")?;
    w.write_all(&(n * frame).to_le_bytes())?;
    for f in audio.samples.chunks_exact(2) {
        if stereo {
            w.write_all(&f[0].to_le_bytes())?;
            w.write_all(&f[1].to_le_bytes())?;
        } else {
            w.write_all(&(0.5 * (f[0] + f[1])).to_le_bytes())?;
        }
    }
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(analyser: &str, version: u32, failed: bool, same_audio: bool) -> ResultState {
        ResultState {
            analyser: analyser.into(),
            version,
            failed,
            same_audio,
        }
    }

    #[test]
    fn a_finished_analysis_cues_only_untouched_players() {
        use crate::core::player::testing::{put_on, running, wait};
        use earshot_engine::{DeckCmd, DeckId, Set};

        let p = running();
        let (played, cued, untouched) = (
            DeckId::new(0).unwrap(),
            DeckId::new(1).unwrap(),
            DeckId::PREPARE,
        );
        for deck in [played, cued, untouched] {
            put_on(&p, deck, 11);
        }
        p.lock()
            .unwrap()
            .send(Command::Deck(played, DeckCmd::Play(Set::On)))
            .unwrap();
        wait(&p, |s| s.decks[0].playing);
        {
            let mut inner = p.lock().unwrap();
            inner
                .send(Command::Deck(played, DeckCmd::Play(Set::Off)))
                .unwrap();
            inner
                .send(Command::Deck(cued, DeckCmd::SetCue(Some(3.0))))
                .unwrap();
        }
        let before = wait(&p, |s| !s.decks[0].playing && s.decks[1].cue_point == 3.0);

        // Silence until 12 s, then beats: the straight grid runs back to 0.25 s.
        let grid = Beatgrid::constant(120.0, 12.25, 30.0).unwrap();
        update_decks(&p, 11, Some((grid, GridSource::Analysis)), 12.26);
        // Prepare's player goes last, so the decks have had theirs by then.
        let after = wait(&p, |s| s.prepare.cue_point == 12.25);
        assert_eq!(after.prepare.position, 12.25, "the playhead goes with it");
        assert_eq!(after.decks[0].cue_point, before.decks[0].cue_point);
        assert_eq!(after.decks[0].position, before.decks[0].position);
        assert_eq!(after.decks[1].cue_point, 3.0);
        let inner = p.lock().unwrap();
        assert_eq!(inner.session(untouched).state.cues.main, Some(12.25));
        // Every player got the grid, touched or not.
        assert!(inner.decks[..2].iter().all(|d| d.grid.is_some()));
    }

    #[test]
    fn worker_results_become_grid_models() {
        let constant = GridData::Constant {
            bpm: 123.45,
            downbeat: 30.07,
            steady: true,
            spread_ms: 3.0,
            drift_pct: 0.1,
        };
        let (g, source) = grid_from_result(&constant, 200.0).unwrap();
        assert_eq!(source, GridSource::Analysis);
        assert_eq!(
            g.model(),
            GridModel::Constant(Constant::new(123.45, 30.07)),
            "exactly the worker's tempo and downbeat"
        );
        assert_eq!(
            grid_bpm(&g),
            123.45,
            "the track's BPM is the grid's, unrounded"
        );

        let beats: Vec<f64> = (0..40).map(|i| 4.02 + i as f64 * 0.48).collect();
        let markers = GridData::Markers {
            beat_index: (0..40).collect(),
            is_downbeat: (0..40).map(|i| i % 4 == 1).collect(),
            beats: beats.clone(),
            median_bpm: 125.0,
            min_bpm: 124.0,
            max_bpm: 126.0,
        };
        let (g, source) = grid_from_result(&markers, 60.0).unwrap();
        assert_eq!(source, GridSource::AnalysisDynamic);
        assert!(g.as_constant().is_none());
        // The detected beats are kept, and the grid carried on to cover the track.
        let near = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert!(beats.iter().all(|&b| g.times().iter().any(|&t| near(t, b))));
        assert!(g.times()[0] < 0.48 && *g.times().last().unwrap() > 59.5);
        let one = g.times().iter().position(|&t| near(t, 4.5)).unwrap();
        assert_eq!(g.positions()[one], 1);
        assert_eq!(grid_bpm(&g), 125.0);
    }

    #[test]
    fn what_still_needs_running() {
        use AnalyserId::*;
        let analysed = Some((1, GridSource::Analysis));
        let user = Some((1, GridSource::User));
        let old = [
            result("grid", 0, false, true),
            result("waveform", 1, false, true),
        ];
        // A deck load wants anything missing: an old grid is still a grid.
        assert!(!needed(Grid, Want::Missing, &old, analysed));
        assert!(needed(Grid, Want::Missing, &[], None));
        assert!(!needed(Waveform, Want::Missing, &old, None));
        assert!(needed(Loudness, Want::Missing, &old, None));
        // "Re-analyse stale" redoes the old analysed grid, but never a hand-fixed one.
        assert!(needed(Grid, Want::Stale, &old, analysed));
        assert!(!needed(Grid, Want::Stale, &old, user));
        assert!(!needed(Waveform, Want::Stale, &old, None));
        assert!(needed(
            Waveform,
            Want::Stale,
            &[result("waveform", 1, false, false)],
            None
        ));
        assert!(needed(
            Waveform,
            Want::Stale,
            &[result("waveform", 1, true, true)],
            None
        ));
        // Forced: always.
        assert!(needed(
            Grid,
            Want::Force,
            &[result("grid", 2, false, true)],
            user
        ));
    }
}
