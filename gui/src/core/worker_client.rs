//! The analysis worker (`earshot/worker`, Python) and the client that drives it over
//! protocol v2 (written down in `worker/README.md`).
//!
//! A [`WorkerClient`] owns one long-lived worker process, so each model is loaded once
//! per process. Any number of threads may call [`WorkerClient::analyse`] at once; one
//! supervisor thread owns the process and routes its replies by job id. The analysis
//! queue (`core::analysis`) runs several: one for deck loads, always up, and
//! background ones that start when there's work and stop after
//! [`WorkerConfig::idle_exit`] without any (see [`Role`]).
//!
//! The supervisor kills and restarts the process when it stops answering (no
//! heartbeat for [`WorkerConfig::heartbeat_timeout`]) or a job runs past its deadline.
//! The job that was running fails; jobs that were only waiting in the dead process are
//! sent again to the new one. Job ids and priorities are per process.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::json;

/// The protocol version this client speaks; the worker must answer hello with it.
pub const PROTOCOL: u32 = 2;

/// What [`Job::wait`] returns for a cancelled job.
pub const CANCELLED: &str = "cancelled";

/// How often the supervisor checks deadlines and heartbeats.
const TICK: Duration = Duration::from_millis(250);
/// A job that was only waiting in a worker that died is sent again, at most this often.
const MAX_RESENDS: u32 = 2;

/// How soon a job must be done once the worker starts it (not counting time spent
/// waiting behind other jobs): generous, since the worker runs in the idle CPU class
/// and a busy machine can starve it. It is for catching hangs, not slow jobs.
pub fn deadline_for(audio_seconds: f64) -> Duration {
    Duration::from_secs_f64(300.0 + 4.0 * audio_seconds.max(0.0))
}

/// Which jobs run first, most urgent first (so `Deck < Background`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum JobPriority {
    /// A track just loaded onto a deck.
    Deck,
    #[allow(dead_code)] // previews are queued as background jobs for now
    Preview,
    Background,
}

impl JobPriority {
    pub fn wire(self) -> &'static str {
        match self {
            JobPriority::Deck => "deck",
            JobPriority::Preview => "preview",
            JobPriority::Background => "background",
        }
    }
}

/// The grid analyser's two modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridMode {
    /// One tempo for the whole track.
    Static,
    /// One grid line per detected beat.
    Dynamic,
}

/// One job: a mono f32 WAV and what to work out from it.
#[derive(Clone, Debug)]
pub struct Request {
    pub wav: PathBuf,
    pub priority: JobPriority,
    /// Run the grid analyser, in this mode. (The only analyser so far.)
    pub grid: Option<GridMode>,
    /// From when the worker starts the job.
    pub deadline: Duration,
}

impl Request {
    fn to_json(&self, id: u64) -> serde_json::Value {
        let mut analysers = serde_json::Map::new();
        if let Some(mode) = self.grid {
            let mode = match mode {
                GridMode::Static => "static",
                GridMode::Dynamic => "dynamic",
            };
            analysers.insert("grid".into(), json!({ "mode": mode }));
        }
        json!({
            "id": id,
            "type": "analyse",
            "wav": self.wav,
            "priority": self.priority.wire(),
            "analysers": analysers,
            "deadline_s": self.deadline.as_secs_f64(),
        })
    }
}

/// Everything the worker sends, one JSON object per stdout line. No field has a
/// default: a reply missing one is a protocol error, not a zero.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Reply {
    Hello {
        protocol: u32,
        /// Analyser name -> version.
        analysers: BTreeMap<String, u32>,
        detector: String,
        models_loaded: Vec<String>,
    },
    Heartbeat,
    /// The job has left the worker's queue and is running; its deadline starts now.
    Started {
        id: u64,
    },
    Progress {
        id: u64,
        analyser: String,
        fraction: f64,
    },
    /// One analyser's result; `data` is parsed per analyser.
    Result {
        id: u64,
        analyser: String,
        version: u32,
        data: serde_json::Value,
    },
    /// One analyser failed; the job goes on with the others.
    Failed {
        id: u64,
        analyser: String,
        error: String,
    },
    /// The job's last message when it ran.
    Done {
        id: u64,
        seconds: f64,
    },
    Cancelled {
        id: u64,
    },
    /// The job's last message when it couldn't run (or `id` null: a request the worker
    /// couldn't read at all).
    Error {
        id: Option<u64>,
        error: String,
    },
}

/// The grid analyser's result.
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct GridResult {
    /// "static", or the beat tracker used: "beat_this" ("librosa" if torch is missing).
    pub detector: String,
    /// How far the grid was moved onto the kicks (median, ms; negative is earlier).
    pub kick_shift_ms: f64,
    /// Where the music's beats start (s, after the kick snap): where beats were found,
    /// not where a straight grid runs back to.
    pub first_beat: f64,
    pub grid: GridData,
    /// The analyser's version, from the result message.
    #[serde(skip)]
    pub version: u32,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GridData {
    /// A straight grid: one tempo, a 1 at `downbeat` (s).
    Constant {
        bpm: f64,
        downbeat: f64,
        /// False when the tempo seems to drift, so dynamic analysis might suit it.
        steady: bool,
        /// How far the sampled excerpts disagree on the phase.
        spread_ms: f64,
        /// The furthest excerpt's own tempo from `bpm`.
        drift_pct: f64,
    },
    /// One line per detected beat. `beat_index` counts beats including missed ones.
    Markers {
        beats: Vec<f64>,
        beat_index: Vec<i64>,
        is_downbeat: Vec<bool>,
        median_bpm: f64,
        /// 2nd / 98th percentile of the per-beat tempo.
        min_bpm: f64,
        max_bpm: f64,
    },
}

/// What a finished job produced, per analyser asked for.
#[derive(Debug)]
pub struct Outcome {
    pub grid: Option<Result<GridResult, String>>,
    /// Wall-clock time in the worker, from start to done.
    pub seconds: f64,
}

/// News about a job while it runs.
#[derive(Clone, Debug, PartialEq)]
pub enum Update {
    /// It left the worker's queue (until then it waits behind other jobs).
    Started,
    Progress {
        analyser: String,
        fraction: f64,
    },
}

/// How to start the worker, and how long to give it.
#[derive(Clone, Debug)]
pub struct WorkerConfig {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub dir: PathBuf,
    pub envs: Vec<(String, String)>,
    /// From spawning to its hello (importing torch takes a while on a busy machine).
    pub start_timeout: Duration,
    /// Silence (no message of any kind) while it has jobs, before it's restarted. The
    /// worker sends a heartbeat every 2 s.
    pub heartbeat_timeout: Duration,
    /// Past a job's own deadline, before the worker is killed. (The worker gives up on
    /// its own at the deadline, at its next progress report.)
    pub grace: Duration,
    /// Stop the process after this long with no jobs (it starts again on the next).
    /// `None`: keep it.
    pub idle_exit: Option<Duration>,
    /// Start a new process straight away when one dies, not on the next job.
    pub keep_alive: bool,
}

/// What a worker process is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Tracks just loaded onto a deck: started with the app and kept running, at
    /// normal CPU priority, so a loaded track is analysed straight away.
    Deck,
    /// Library work: started when there is some, stopped after [`IDLE_EXIT`] without,
    /// in Linux's idle CPU class (it only gets what nothing else wants).
    Background,
}

/// How long a background worker waits for more work before it exits. Analysis is
/// mostly needed on import, so there's no point keeping a process (and its models in
/// memory) around for long after.
pub const IDLE_EXIT: Duration = Duration::from_secs(60);

impl WorkerConfig {
    /// The real worker: `worker/.venv/bin/python -m earshot_worker`, letting torch use
    /// `threads` CPU threads.
    pub fn python(role: Role, threads: usize) -> WorkerConfig {
        let dir = std::env::var_os("EARSHOT_WORKER_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../worker"));
        let threads = threads.to_string();
        let mut envs: Vec<(String, String)> =
            ["OMP_NUM_THREADS", "MKL_NUM_THREADS", "OPENBLAS_NUM_THREADS"]
                .iter()
                .map(|k| (k.to_string(), threads.clone()))
                .collect();
        if role == Role::Deck {
            // The worker would otherwise put itself in the idle CPU class.
            envs.push(("EARSHOT_WORKER_PRIORITY".into(), "inherit".into()));
        }
        WorkerConfig {
            program: dir.join(".venv/bin/python"),
            args: vec!["-m".into(), "earshot_worker".into()],
            dir,
            envs,
            start_timeout: Duration::from_secs(180),
            heartbeat_timeout: Duration::from_secs(60),
            grace: Duration::from_secs(15),
            idle_exit: (role == Role::Background).then_some(IDLE_EXIT),
            keep_alive: role == Role::Deck,
        }
    }
}

/// Talks to the worker process. Cheap to clone; the process starts on the first job
/// and is stopped when the last clone is dropped.
#[derive(Clone)]
pub struct WorkerClient {
    inner: Arc<Inner>,
}

struct Inner {
    tx: Mutex<Sender<Ctl>>,
    next_id: AtomicU64,
    /// A process is up (starting or ready), as the supervisor last left it.
    running: Arc<AtomicBool>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        let _ = self.tx.lock().unwrap().send(Ctl::Shutdown);
    }
}

/// A submitted job: wait for it, or cancel it from another thread by its id.
pub struct Job {
    id: u64,
    rx: Receiver<JobMsg>,
}

impl WorkerClient {
    pub fn start(config: WorkerConfig) -> WorkerClient {
        let (tx, rx) = mpsc::channel();
        let supervisor_tx = tx.clone();
        let running = Arc::new(AtomicBool::new(false));
        let flag = running.clone();
        std::thread::Builder::new()
            .name("earshot-worker-supervisor".into())
            .spawn(move || Supervisor::new(config, supervisor_tx, flag).run(rx))
            .expect("spawn worker supervisor");
        WorkerClient {
            inner: Arc::new(Inner {
                tx: Mutex::new(tx),
                next_id: AtomicU64::new(1),
                running,
            }),
        }
    }

    /// Whether the worker process is up (starting or ready).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_running(&self) -> bool {
        self.inner.running.load(Ordering::Relaxed)
    }

    fn send(&self, ctl: Ctl) {
        let _ = self.inner.tx.lock().unwrap().send(ctl);
    }

    /// Start the worker process now rather than on the first job.
    pub fn warm_up(&self) {
        self.send(Ctl::WarmUp);
    }

    /// Queue a job in the worker.
    pub fn submit(&self, req: &Request) -> Job {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.send(Ctl::Submit {
            id,
            req: req.to_json(id),
            deadline: req.deadline,
            tx,
        });
        Job { id, rx }
    }

    /// Stop a job: dropped if it's still waiting, stopped at its next progress report
    /// if it's running. Its `wait` then returns [`CANCELLED`]. Unknown ids (already
    /// finished) are ignored.
    pub fn cancel(&self, id: u64) {
        self.send(Ctl::Cancel(id));
    }

    /// Queue a job of another type (the tempo lock tool's `tempo_lock` and
    /// `straighten`): `req` is its request without the `id`. Wait for it with
    /// [`Job::wait_events`].
    pub fn submit_job(
        &self,
        mut req: serde_json::Map<String, serde_json::Value>,
        deadline: Duration,
    ) -> Job {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        req.insert("id".into(), id.into());
        req.insert("deadline_s".into(), deadline.as_secs_f64().into());
        self.send(Ctl::Submit {
            id,
            req: req.into(),
            deadline,
            tx,
        });
        Job { id, rx }
    }

    /// Submit a job and wait for it.
    #[cfg_attr(not(test), allow(dead_code))] // the queue submits, to be able to cancel
    pub fn analyse(
        &self,
        req: &Request,
        on_update: &mut dyn FnMut(Update),
    ) -> Result<Outcome, String> {
        self.submit(req).wait(on_update)
    }
}

impl Job {
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Block until the job ends, passing on its updates. Err when the job as a whole
    /// failed (cancelled, past its deadline, the worker died, unreadable audio); one
    /// analyser failing shows in the [`Outcome`] instead.
    pub fn wait(self, on_update: &mut dyn FnMut(Update)) -> Result<Outcome, String> {
        let mut grid = None;
        loop {
            let msg = self
                .rx
                .recv()
                .map_err(|_| "the analysis worker has stopped".to_string())?;
            match msg {
                JobMsg::Update(u) => on_update(u),
                JobMsg::Result {
                    analyser,
                    version,
                    data,
                } => {
                    if analyser == "grid" {
                        grid = Some(
                            serde_json::from_value::<GridResult>(data)
                                .map(|g| GridResult { version, ..g })
                                .map_err(|e| format!("the worker sent an unusable grid: {e}")),
                        );
                    }
                }
                JobMsg::Failed { analyser, error } => {
                    if analyser == "grid" {
                        grid = Some(Err(error));
                    }
                }
                JobMsg::End(Ok(seconds)) => return Ok(Outcome { grid, seconds }),
                JobMsg::End(Err(e)) => return Err(e),
            }
        }
    }

    /// Block until the job ends, passing on everything it sends as it comes (for jobs
    /// whose results arrive one step at a time). Ok(seconds in the worker), or why
    /// the job as a whole failed, as for [`Job::wait`].
    pub fn wait_events(self, on_event: &mut dyn FnMut(JobEvent)) -> Result<f64, String> {
        loop {
            let msg = self
                .rx
                .recv()
                .map_err(|_| "the worker has stopped".to_string())?;
            match msg {
                JobMsg::Update(u) => on_event(JobEvent::Update(u)),
                JobMsg::Result { analyser, data, .. } => on_event(JobEvent::Result {
                    step: analyser,
                    data,
                }),
                JobMsg::Failed { analyser, error } => on_event(JobEvent::Failed {
                    step: analyser,
                    error,
                }),
                JobMsg::End(result) => return result,
            }
        }
    }
}

/// What a job sends while it runs (see [`Job::wait_events`]). `step` is the
/// `analyser` of the worker's message: an analyser, or a tempo lock job's step.
#[derive(Debug)]
pub enum JobEvent {
    Update(Update),
    Result {
        step: String,
        data: serde_json::Value,
    },
    Failed {
        step: String,
        error: String,
    },
}

// ---- the supervisor ---------------------------------------------------------------------

enum Ctl {
    Submit {
        id: u64,
        req: serde_json::Value,
        deadline: Duration,
        tx: Sender<JobMsg>,
    },
    #[allow(dead_code)]
    Cancel(u64),
    WarmUp,
    /// A line from process `generation`.
    Reply {
        generation: u64,
        reply: Reply,
    },
    /// Process `generation` closed its stdout (it exited).
    Eof {
        generation: u64,
    },
    Shutdown,
}

enum JobMsg {
    Update(Update),
    Result {
        analyser: String,
        version: u32,
        data: serde_json::Value,
    },
    Failed {
        analyser: String,
        error: String,
    },
    /// Ok(seconds in the worker) or why it failed.
    End(Result<f64, String>),
}

struct Proc {
    child: Child,
    stdin: ChildStdin,
    generation: u64,
    /// Answered hello with our protocol.
    ready: bool,
    spawned: Instant,
    last_heard: Instant,
}

struct Slot {
    req: serde_json::Value,
    deadline: Duration,
    tx: Sender<JobMsg>,
    /// The process generation it was sent to, if any.
    sent_to: Option<u64>,
    started: Option<Instant>,
    resends: u32,
    /// Cancel was asked for: never send it again.
    cancelled: bool,
}

struct Supervisor {
    config: WorkerConfig,
    /// For the reader threads.
    tx: Sender<Ctl>,
    proc: Option<Proc>,
    jobs: HashMap<u64, Slot>,
    generations: u64,
    /// Since when there have been no jobs.
    idle_since: Option<Instant>,
    /// Shared with the client: `proc` is Some.
    running: Arc<AtomicBool>,
}

impl Supervisor {
    fn new(config: WorkerConfig, tx: Sender<Ctl>, running: Arc<AtomicBool>) -> Self {
        Supervisor {
            config,
            tx,
            proc: None,
            jobs: HashMap::new(),
            generations: 0,
            idle_since: None,
            running,
        }
    }

    fn run(mut self, rx: Receiver<Ctl>) {
        loop {
            match rx.recv_timeout(TICK) {
                Ok(Ctl::Submit {
                    id,
                    req,
                    deadline,
                    tx,
                }) => {
                    self.jobs.insert(
                        id,
                        Slot {
                            req,
                            deadline,
                            tx,
                            sent_to: None,
                            started: None,
                            resends: 0,
                            cancelled: false,
                        },
                    );
                    self.send_waiting();
                }
                Ok(Ctl::Cancel(id)) => self.cancel(id),
                Ok(Ctl::WarmUp) => {
                    if self.proc.is_none()
                        && let Err(e) = self.spawn()
                    {
                        eprintln!("analysis worker: {e}");
                    }
                }
                Ok(Ctl::Reply { generation, reply }) => {
                    if self.current(generation) {
                        self.on_reply(reply);
                    }
                }
                Ok(Ctl::Eof { generation }) => {
                    if self.current(generation) {
                        self.lost("the analysis worker exited (see the terminal for its error)");
                    }
                }
                Ok(Ctl::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.check_timers();
        }
        self.kill();
    }

    fn current(&self, generation: u64) -> bool {
        self.proc
            .as_ref()
            .is_some_and(|p| p.generation == generation)
    }

    /// Send every job not yet with the current process, starting one if needed.
    fn send_waiting(&mut self) {
        if !self.jobs.values().any(|s| s.sent_to.is_none()) {
            return;
        }
        if self.proc.is_none()
            && let Err(e) = self.spawn()
        {
            self.fail_all(&e);
            return;
        }
        let proc = self.proc.as_mut().unwrap();
        if !proc.ready {
            return; // sent on hello
        }
        let generation = proc.generation;
        if !self.jobs.values().any(|s| s.sent_to == Some(generation)) {
            proc.last_heard = Instant::now(); // silence while idle doesn't count
        }
        let mut ids: Vec<u64> = self
            .jobs
            .iter()
            .filter(|(_, s)| s.sent_to.is_none())
            .map(|(id, _)| *id)
            .collect();
        ids.sort_unstable(); // in the order they were submitted
        for id in ids {
            let line = self.jobs[&id].req.to_string();
            if writeln!(proc.stdin, "{line}")
                .and_then(|_| proc.stdin.flush())
                .is_err()
            {
                self.lost("the analysis worker has stopped");
                return;
            }
            self.jobs.get_mut(&id).unwrap().sent_to = Some(generation);
        }
    }

    fn spawn(&mut self) -> Result<(), String> {
        let c = &self.config;
        if !c.program.exists() {
            return Err(format!(
                "the analysis worker isn't set up: run {}/setup.sh",
                c.dir.canonicalize().unwrap_or(c.dir.clone()).display()
            ));
        }
        let mut command = std::process::Command::new(&c.program);
        command
            .args(&c.args)
            .current_dir(&c.dir)
            .envs(c.envs.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        // End the worker when the app does, even mid-analysis (on exit nothing joins
        // this thread, so `Drop` never runs). It fires when the thread that started the
        // child ends: this supervisor thread, which lives as long as the app.
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
        let mut stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        self.generations += 1;
        let generation = self.generations;
        let tx = self.tx.clone();
        std::thread::Builder::new()
            .name(format!("earshot-worker-reader-{generation}"))
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    match serde_json::from_str::<Reply>(&line) {
                        Ok(reply) => {
                            if tx.send(Ctl::Reply { generation, reply }).is_err() {
                                return;
                            }
                        }
                        Err(e) => eprintln!("analysis worker: unreadable reply ({e}): {line}"),
                    }
                }
                let _ = tx.send(Ctl::Eof { generation });
            })
            .map_err(|e| format!("starting the analysis worker: {e}"))?;
        // Answered once it has imported torch.
        let hello = json!({ "type": "hello", "protocol": PROTOCOL });
        let _ = writeln!(stdin, "{hello}").and_then(|_| stdin.flush());
        let now = Instant::now();
        self.proc = Some(Proc {
            child,
            stdin,
            generation,
            ready: false,
            spawned: now,
            last_heard: now,
        });
        self.running.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn on_reply(&mut self, reply: Reply) {
        let proc = self.proc.as_mut().unwrap();
        proc.last_heard = Instant::now();
        match reply {
            Reply::Hello {
                protocol,
                analysers,
                detector,
                models_loaded,
            } => {
                if protocol != PROTOCOL || !analysers.contains_key("grid") {
                    self.kill();
                    self.fail_all(&format!(
                        "the analysis worker speaks protocol {protocol} with {analysers:?}; \
                         this app needs {PROTOCOL} with a grid analyser"
                    ));
                    return;
                }
                eprintln!(
                    "analysis worker ready: protocol {protocol}, analysers {analysers:?}, \
                     detector {detector}, models loaded {models_loaded:?}"
                );
                proc.ready = true;
                self.send_waiting();
            }
            Reply::Heartbeat => {}
            Reply::Started { id } => {
                if let Some(s) = self.jobs.get_mut(&id) {
                    s.started = Some(Instant::now());
                    let _ = s.tx.send(JobMsg::Update(Update::Started));
                }
            }
            Reply::Progress {
                id,
                analyser,
                fraction,
            } => self.to_job(id, JobMsg::Update(Update::Progress { analyser, fraction })),
            Reply::Result {
                id,
                analyser,
                version,
                data,
            } => self.to_job(
                id,
                JobMsg::Result {
                    analyser,
                    version,
                    data,
                },
            ),
            Reply::Failed {
                id,
                analyser,
                error,
            } => self.to_job(id, JobMsg::Failed { analyser, error }),
            Reply::Done { id, seconds } => self.end(id, Ok(seconds)),
            Reply::Cancelled { id } => self.end(id, Err(CANCELLED.into())),
            Reply::Error {
                id: Some(id),
                error,
            } => self.end(id, Err(error)),
            Reply::Error { id: None, error } => {
                if proc.ready {
                    eprintln!("analysis worker: {error}");
                } else {
                    // It refused our hello.
                    self.kill();
                    self.fail_all(&format!("the analysis worker refused to start: {error}"));
                }
            }
        }
    }

    fn to_job(&self, id: u64, msg: JobMsg) {
        if let Some(s) = self.jobs.get(&id) {
            let _ = s.tx.send(msg);
        }
    }

    fn end(&mut self, id: u64, result: Result<f64, String>) {
        if let Some(s) = self.jobs.remove(&id) {
            let _ = s.tx.send(JobMsg::End(result));
        }
    }

    fn cancel(&mut self, id: u64) {
        let Some(slot) = self.jobs.get_mut(&id) else {
            return;
        };
        slot.cancelled = true;
        match (&mut self.proc, slot.sent_to) {
            (Some(p), Some(generation)) if p.generation == generation => {
                // The worker answers with `cancelled`, which ends the job.
                let msg = json!({ "type": "cancel", "id": id });
                if writeln!(p.stdin, "{msg}")
                    .and_then(|_| p.stdin.flush())
                    .is_err()
                {
                    self.lost("the analysis worker has stopped");
                }
            }
            _ => self.end(id, Err(CANCELLED.into())),
        }
    }

    fn check_timers(&mut self) {
        let idle_for = match self.jobs.is_empty() {
            true => self.idle_since.get_or_insert_with(Instant::now).elapsed(),
            false => {
                self.idle_since = None;
                Duration::ZERO
            }
        };
        let Some(proc) = &self.proc else { return };
        let generation = proc.generation;
        if proc.ready && self.config.idle_exit.is_some_and(|d| idle_for > d) {
            eprintln!("analysis worker: idle, stopping it");
            self.kill();
            return;
        }
        if !proc.ready {
            if proc.spawned.elapsed() > self.config.start_timeout {
                self.kill();
                self.fail_all("the analysis worker didn't start in time");
            }
            return;
        }
        let busy = self.jobs.values().any(|s| s.sent_to == Some(generation));
        if busy && proc.last_heard.elapsed() > self.config.heartbeat_timeout {
            self.lost("the analysis worker stopped responding, so it was restarted");
            return;
        }
        let late = self.jobs.iter().find(|(_, s)| {
            s.sent_to == Some(generation)
                && s.started
                    .is_some_and(|t| t.elapsed() > s.deadline + self.config.grace)
        });
        if let Some((id, s)) = late {
            eprintln!("analysis worker: job {id} is past its deadline; restarting the worker");
            let why = format!(
                "analysis took longer than {:.0} s, so the worker was restarted",
                s.deadline.as_secs_f64()
            );
            self.lost(&why);
        }
    }

    /// The process died or was given up on: kill it. Jobs it had started fail with
    /// `why`; jobs only waiting in it go to a new process.
    fn lost(&mut self, why: &str) {
        let Some(generation) = self.proc.as_ref().map(|p| p.generation) else {
            return;
        };
        let was_ready = self.proc.as_ref().is_some_and(|p| p.ready);
        self.kill();
        if !was_ready {
            // Died while starting: another start would most likely die too.
            self.fail_all(why);
            return;
        }
        let ids: Vec<u64> = self
            .jobs
            .iter()
            .filter(|(_, s)| s.sent_to == Some(generation))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            let s = self.jobs.get_mut(&id).unwrap();
            if s.cancelled {
                self.end(id, Err(CANCELLED.into()));
            } else if s.started.is_some() || s.resends >= MAX_RESENDS {
                self.end(id, Err(why.to_string()));
            } else {
                s.sent_to = None;
                s.resends += 1;
            }
        }
        self.send_waiting();
        if self.config.keep_alive
            && self.proc.is_none()
            && let Err(e) = self.spawn()
        {
            eprintln!("analysis worker: {e}");
        }
    }

    fn kill(&mut self) {
        if let Some(mut p) = self.proc.take() {
            let _ = p.child.kill();
            let _ = p.child.wait();
        }
        self.running.store(false, Ordering::Relaxed);
    }

    fn fail_all(&mut self, why: &str) {
        for (_, s) in self.jobs.drain() {
            let _ = s.tx.send(JobMsg::End(Err(why.to_string())));
        }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// A stand-in worker: answers hello (with protocol argv[1]), then runs jobs one at
    /// a time. A job whose wav is "hang" starts and never finishes; "fail" fails its
    /// grid analyser; "die" makes the process exit. With argv[2] = "beat", it sends
    /// heartbeats every 0.1 s.
    const FAKE: &str = r#"
import json, sys, threading, time
lock = threading.Lock()
def send(m):
    with lock:
        sys.stdout.write(json.dumps(m) + "\n"); sys.stdout.flush()
if len(sys.argv) > 2 and sys.argv[2] == "beat":
    def beat():
        while True:
            time.sleep(0.1); send({"type": "heartbeat"})
    threading.Thread(target=beat, daemon=True).start()
for line in sys.stdin:
    r = json.loads(line)
    if r["type"] == "hello":
        send({"type": "hello", "protocol": int(sys.argv[1]), "analysers": {"grid": 3},
              "detector": "fake", "models_loaded": []})
    elif r["type"] == "analyse":
        i = r["id"]
        send({"id": i, "type": "started"})
        if r["wav"] == "die":
            sys.exit(1)
        if r["wav"] == "hang":
            time.sleep(1000)
        send({"id": i, "type": "progress", "analyser": "grid", "fraction": 0.5})
        if r["wav"] == "fail":
            send({"id": i, "type": "failed", "analyser": "grid", "error": "no beats"})
        else:
            send({"id": i, "type": "result", "analyser": "grid", "version": 3, "data": {
                "detector": "fake", "kick_shift_ms": -20.0, "first_beat": 0.5, "grid": {"kind": "constant",
                "bpm": 124.0, "downbeat": 0.5, "steady": True, "spread_ms": 1.0, "drift_pct": 0.1}}})
        send({"id": i, "type": "done", "seconds": 0.1})
"#;

    pub fn fake(protocol: u32, heartbeat: bool) -> WorkerConfig {
        let mut args: Vec<OsString> = vec!["-c".into(), FAKE.into(), protocol.to_string().into()];
        if heartbeat {
            args.push("beat".into());
        }
        WorkerConfig {
            program: PathBuf::from("/usr/bin/python3"),
            args,
            dir: std::env::temp_dir(),
            envs: vec![],
            start_timeout: Duration::from_secs(20),
            heartbeat_timeout: Duration::from_secs(30),
            grace: Duration::from_millis(200),
            idle_exit: None,
            keep_alive: false,
        }
    }

    /// A stand-in for the real worker, for tests that start the whole app.
    pub fn stand_in(_role: Role, _threads: usize) -> WorkerConfig {
        fake(PROTOCOL, false)
    }
}

#[cfg(test)]
mod tests {
    use super::testing::fake;
    use super::*;

    fn req(wav: &str, deadline_ms: u64) -> Request {
        Request {
            wav: wav.into(),
            priority: JobPriority::Background,
            grid: Some(GridMode::Static),
            deadline: Duration::from_millis(deadline_ms),
        }
    }

    #[test]
    fn replies_are_typed() {
        let r: Reply = serde_json::from_str(r#"{"id": 3, "type": "started"}"#).unwrap();
        assert_eq!(r, Reply::Started { id: 3 });
        // A substring isn't enough: "ready" somewhere in a line is not a hello.
        assert!(serde_json::from_str::<Reply>(r#"{"type": "ready"}"#).is_err());
        assert!(serde_json::from_str::<Reply>(r#"{"type": "hello", "protocol": 2}"#).is_err());
        // A grid missing a field is an error, not a zero.
        let g = r#"{"detector": "static", "kick_shift_ms": -20.0, "first_beat": 0.5,
                    "grid": {"kind": "constant", "bpm": 124.0, "steady": true,
                             "spread_ms": 1.0, "drift_pct": 0.1}}"#;
        assert!(serde_json::from_str::<GridResult>(g).is_err());
        let g = g.replace("\"bpm\"", "\"downbeat\": 0.5, \"bpm\"");
        let g: GridResult = serde_json::from_str(&g).unwrap();
        assert!(matches!(g.grid, GridData::Constant { bpm: 124.0, .. }));
    }

    #[test]
    fn runs_a_job() {
        let client = WorkerClient::start(fake(PROTOCOL, false));
        let mut updates = vec![];
        let out = client
            .analyse(&req("x", 5000), &mut |u| updates.push(u))
            .unwrap();
        assert_eq!(updates[0], Update::Started);
        assert!(matches!(&updates[1], Update::Progress { fraction, .. } if *fraction == 0.5));
        let grid = out.grid.unwrap().unwrap();
        assert_eq!(grid.version, 3);
        assert_eq!(grid.first_beat, 0.5);
        assert!(matches!(
            grid.grid,
            GridData::Constant { downbeat: 0.5, .. }
        ));
        // One analyser failing is in the outcome, not a failed job.
        let out = client.analyse(&req("fail", 5000), &mut |_| {}).unwrap();
        assert_eq!(out.grid.unwrap().unwrap_err(), "no beats");
    }

    /// Poll until `f` holds, for up to 10 s.
    fn eventually(f: impl Fn() -> bool) -> bool {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(10) {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn deck_worker_stays_up_while_a_background_one_comes_and_goes() {
        let mut deck = fake(PROTOCOL, false);
        deck.keep_alive = true;
        let deck = WorkerClient::start(deck);
        let mut background = fake(PROTOCOL, false);
        background.idle_exit = Some(Duration::from_millis(300));
        let background = WorkerClient::start(background);
        deck.warm_up();
        assert!(eventually(|| deck.is_running()));
        // No background work yet: no background process.
        std::thread::sleep(Duration::from_millis(400));
        assert!(!background.is_running());
        // Work arrives: it starts, does it, and exits once idle for a while.
        assert!(background.analyse(&req("x", 5000), &mut |_| {}).is_ok());
        assert!(background.is_running());
        assert!(eventually(|| !background.is_running()));
        // And starts again for the next job.
        assert!(background.analyse(&req("x", 5000), &mut |_| {}).is_ok());
        // The deck worker was there throughout, and still answers straight away.
        assert!(deck.is_running());
        assert!(deck.analyse(&req("x", 5000), &mut |_| {}).is_ok());
        // If it dies it's started again at once, not on the next deck load.
        assert!(deck.analyse(&req("die", 5000), &mut |_| {}).is_err());
        assert!(eventually(|| deck.is_running()));
    }

    #[test]
    fn wrong_protocol_is_refused() {
        let client = WorkerClient::start(fake(1, false));
        let e = client.analyse(&req("x", 5000), &mut |_| {}).unwrap_err();
        assert!(e.contains("protocol 1"), "{e}");
    }

    #[test]
    fn missing_worker_says_how_to_set_it_up() {
        let mut config = fake(PROTOCOL, false);
        config.program = PathBuf::from("/nonexistent/python");
        let e = WorkerClient::start(config)
            .analyse(&req("x", 5000), &mut |_| {})
            .unwrap_err();
        assert!(e.contains("setup.sh"), "{e}");
    }

    #[test]
    fn a_hung_job_is_killed_and_the_queue_goes_on() {
        let client = WorkerClient::start(fake(PROTOCOL, true));
        // The fake runs one job at a time, so the second waits behind the hung one.
        let hung = client.submit(&req("hang", 300));
        let next = client.submit(&req("x", 5000));
        let t0 = Instant::now();
        let e = hung.wait(&mut |_| {}).unwrap_err();
        assert!(e.contains("longer than"), "{e}");
        assert!(t0.elapsed() < Duration::from_secs(10));
        // Sent again to a fresh worker.
        assert!(next.wait(&mut |_| {}).unwrap().grid.unwrap().is_ok());
    }

    #[test]
    fn a_silent_worker_is_restarted() {
        let mut config = fake(PROTOCOL, false); // no heartbeats
        config.heartbeat_timeout = Duration::from_millis(500);
        let client = WorkerClient::start(config);
        let e = client
            .analyse(&req("hang", 60_000), &mut |_| {})
            .unwrap_err();
        assert!(e.contains("stopped responding"), "{e}");
        // A quick job still works: it's busy for less than the timeout.
        assert!(client.analyse(&req("x", 5000), &mut |_| {}).is_ok());
    }

    #[test]
    fn cancelling_a_job_that_waits() {
        let client = WorkerClient::start(fake(PROTOCOL, true));
        let hung = client.submit(&req("hang", 300));
        let waiting = client.submit(&req("x", 5000));
        // The fake doesn't read the cancel while it hangs. When the hung job is killed,
        // the waiting one would be sent to a new worker, but it was cancelled.
        client.cancel(waiting.id());
        assert!(hung.wait(&mut |_| {}).is_err());
        let e = waiting.wait(&mut |_| {}).unwrap_err();
        assert_eq!(e, "cancelled");
    }

    /// End to end against the real worker, both modes:
    /// `EARSHOT_TEST_TRACK=/path/song.mp3 cargo test -p earshot-gui -- --include-ignored analyses_a_real_track`.
    #[test]
    #[ignore]
    fn analyses_a_real_track() {
        use earshot_engine::decode;
        let path = std::env::var("EARSHOT_TEST_TRACK").expect("set EARSHOT_TEST_TRACK");
        let (audio, _) = decode::decode_file(Path::new(&path)).unwrap();
        let wav = std::env::temp_dir().join(format!("earshot-test-{}.wav", std::process::id()));
        super::super::analysis::write_mono_wav(&wav, &audio).unwrap();
        let client = WorkerClient::start(WorkerConfig::python(Role::Deck, 4));
        for mode in [GridMode::Static, GridMode::Dynamic] {
            let mut seen = Vec::new();
            let r = Request {
                wav: wav.clone(),
                priority: JobPriority::Deck,
                grid: Some(mode),
                deadline: deadline_for(audio.duration()),
            };
            let out = client
                .analyse(&r, &mut |u| {
                    if let Update::Progress { fraction, .. } = u {
                        seen.push(fraction)
                    }
                })
                .unwrap();
            assert!(
                !seen.is_empty() && seen.windows(2).all(|p| p[0] <= p[1]),
                "progress {seen:?}"
            );
            let g = out.grid.unwrap().unwrap();
            eprintln!(
                "{mode:?}: {} (v{}), kick shift {:.1} ms, {:.1} s: {:?}",
                g.detector,
                g.version,
                g.kick_shift_ms,
                out.seconds,
                match &g.grid {
                    GridData::Constant {
                        bpm,
                        downbeat,
                        steady,
                        spread_ms,
                        ..
                    } => format!(
                        "{bpm} BPM, downbeat {downbeat:.3} s, spread {spread_ms:.1} ms, steady {steady}"
                    ),
                    GridData::Markers {
                        beats, median_bpm, ..
                    } => format!("{} beats, median {median_bpm:.2} BPM", beats.len()),
                }
            );
        }
        let _ = std::fs::remove_file(&wav);
    }
}
