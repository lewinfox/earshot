//! The tempo lock tool: take a track whose tempo wanders (live drums), chart its
//! tempo, re-time it so every beat sits on one BPM without changing its pitch, and
//! save the result into the library. From tempo-lock (`../tempo-lock/`), whose code
//! runs in the worker (`worker/earshot_worker/tempo_lock.py`).
//!
//! The flow, one track at a time ("the session"):
//!
//! 1. [`TempoLockCmd::Analyse`]: decode the track (our own decoder, so the charts line
//!    up with what the decks play), and have the worker find its beats: a
//!    [`TempoLockEvent::Analysed`] with the [`Chart`] (tempo over time, histogram data,
//!    sections, stats), and the waveform the decks would draw, from our decode.
//! 2. [`TempoLockCmd::Straighten`] at a BPM: Rubber Band stretches it onto the grid and
//!    ffmpeg writes an MP3 named and tagged as tempo-lock names its downloads:
//!    [`TempoLockEvent::Straightened`]. Then the worker finds the beats in the output
//!    again, to show how straight it came out: [`TempoLockEvent::Measured`].
//! 3. [`TempoLockCmd::SaveToLibrary`] into one of the library folders: the MP3 is copied
//!    there (never over a file: a clashing name gets " (2)", " (3)", …) and scanned in,
//!    then [`TempoLockEvent::Saved`].
//!
//! Every step reports [`TempoLockEvent::Progress`] (a 0 when it starts) and ends with
//! its result, `Failed` or `Cancelled`. [`TempoLockCmd::Cancel`] stops whatever is
//! running. A new command replaces one that is still running (its events stop). The
//! worker jobs run on the tool's own background worker process (started when needed,
//! stopped after a minute idle), never the deck worker.
//!
//! Files: `<data dir>/tempo-lock/<track id>/` holds the decoded audio and the
//! straightened MP3 until another track is analysed. The folder is the tool's own and
//! is emptied at start-up.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use earshot_engine::decode::{self, TrackAudio};
use earshot_engine::waveform;
use serde::Deserialize;
use serde_json::json;

use super::analysis::write_stereo_wav;
use super::library;
use super::store::Store;
use super::track::TrackId;
use super::worker_client::{
    self, CANCELLED, JobEvent, JobPriority, Role, Update, WorkerClient, WorkerConfig,
};

/// What the tool can be asked to do (on the [`super::command::Command`] bus as
/// `Command::TempoLock`; the UI passes them to [`TempoLock::run`]).
#[derive(Clone, Debug, PartialEq)]
pub enum TempoLockCmd {
    /// Start a session with this library track: decode it and chart its tempo.
    Analyse(TrackId),
    /// Straighten the session's track (once analysed) to a BPM.
    Straighten(StraightenOptions),
    /// Stop what's running (analysis, straightening or measuring).
    Cancel,
    /// Copy the straightened MP3 into this library folder (one of the Store's
    /// `library_roots`) and scan it in.
    SaveToLibrary(PathBuf),
    /// Listen to the original or the straightened version in the headphones. The UI's
    /// shell plays these (it has the player); [`TempoLock::run`] ignores them.
    Listen(ListenCmd),
}

/// The tab's A/B player, on the headphones' preview player.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ListenCmd {
    /// Play `version` from `at` seconds into it (in its own time), with a click on
    /// every beat of its grid if `clicks`.
    Play {
        version: Version,
        at: f64,
        clicks: bool,
    },
    Pause,
    Stop,
}

/// Which audio: the track as it was, or straightened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Version {
    #[default]
    Original,
    Straightened,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StraightenOptions {
    /// The tempo of the result.
    pub bpm: f64,
    pub level: BeatLevel,
    pub engine: Engine,
}

impl StraightenOptions {
    /// At this BPM, with the defaults: the detected beats are the beat, best engine.
    #[cfg_attr(not(test), allow(dead_code))] // the tab sets every option
    pub fn at(bpm: f64) -> StraightenOptions {
        StraightenOptions {
            bpm,
            level: BeatLevel::Beat,
            engine: Engine::R3,
        }
    }
}

/// How the detected beats relate to the BPM asked for (tempo-lock's "Detected beats
/// are"). Use this, not 2× the BPM, when the detector locked onto half-time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BeatLevel {
    #[default]
    Beat,
    /// The detector found half-time: its beats are two of the target's apart.
    HalfTime,
    /// The detector found double-time.
    DoubleTime,
}

impl BeatLevel {
    /// Target beats per detected beat.
    pub fn factor(self) -> f64 {
        match self {
            BeatLevel::Beat => 1.0,
            BeatLevel::HalfTime => 2.0,
            BeatLevel::DoubleTime => 0.5,
        }
    }
}

/// Rubber Band's engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Engine {
    /// R3: the best quality.
    #[default]
    R3,
    /// R2: faster; worth an A/B on very dry drums.
    R2,
}

impl Engine {
    fn wire(self) -> &'static str {
        match self {
            Engine::R3 => "r3",
            Engine::R2 => "r2",
        }
    }
}

/// Which part of the flow an event is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Analyse,
    Straighten,
    /// Finding the beats in the straightened audio.
    Measure,
    SaveToLibrary,
}

/// News from the tool. Every event names the session's track, so a stale one (from
/// before another track was opened) can be told apart.
#[derive(Clone, Debug)]
pub enum TempoLockEvent {
    /// A stage is running (0 when it starts), 0..1.
    Progress {
        track: TrackId,
        stage: Stage,
        fraction: f64,
    },
    Analysed {
        track: TrackId,
        chart: Arc<Chart>,
    },
    /// The MP3 is written (and can be saved); measuring it follows.
    Straightened {
        track: TrackId,
        result: Arc<Straightened>,
    },
    /// The beats found again in the straightened audio.
    Measured {
        track: TrackId,
        analysis: Arc<TempoAnalysis>,
    },
    /// The straightened MP3 is in the library, at `path`.
    Saved {
        track: TrackId,
        path: PathBuf,
    },
    Failed {
        track: TrackId,
        stage: Stage,
        error: String,
    },
    Cancelled {
        track: TrackId,
        stage: Stage,
    },
}

/// Where the tool sends its events. (The UI wraps them in its own event type.)
pub type TempoLockSink = Arc<dyn Fn(TempoLockEvent) + Send + Sync>;

// ---- what the charts draw --------------------------------------------------------------

/// Everything tempo-lock's page draws for a track: its tempo analysis and waveform.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Chart {
    pub analysis: TempoAnalysis,
    /// Ours, not the worker's.
    #[serde(skip)]
    pub wave: Waveform,
}

/// The beats and the tempo they make (tempo-lock's `Analysis`). Times in seconds from
/// the start of the track as earshot decodes it.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct TempoAnalysis {
    pub sr: u32,
    pub duration: f64,
    /// "beat_this", or "librosa" if torch is missing.
    pub detector: String,
    /// One per beat found (strictly increasing).
    pub beats: Vec<f64>,
    /// Counts beats including missed ones, so it can jump by 2+.
    pub beat_index: Vec<i64>,
    pub is_downbeat: Vec<bool>,
    /// Detections thrown away (doubles, or in silence): tempo-lock's red ticks.
    pub dropped_beats: Vec<f64>,
    /// Tempo over time: `bpm_curve[i]` is the tempo between beats i and i+1, drawn at
    /// `bpm_times[i]`, the middle of that interval. The histogram is of `bpm_curve`.
    pub bpm_times: Vec<f64>,
    pub bpm_curve: Vec<f64>,
    pub median_bpm: f64,
    /// 2nd / 98th percentile of `bpm_curve` ("tempo range").
    pub min_bpm: f64,
    pub max_bpm: f64,
    /// The rounded median: the BPM to offer.
    pub suggested_bpm: f64,
    /// Stretches at one tempo, split where the tempo jumps (one section: drift only).
    pub sections: Vec<Section>,
    /// The jumps between sections: deliberate changes, worth a warning.
    pub tempo_changes: Vec<TempoChange>,
    /// How far the beats were moved onto the kicks (median, s).
    pub kick_shift: f64,
}

/// A stretch of the track at one tempo: beat intervals `start_beat..end_beat` of
/// `bpm_curve`, from `start` to `end` (s).
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Section {
    pub start_beat: usize,
    pub end_beat: usize,
    pub start: f64,
    pub end: f64,
    /// Its median tempo.
    pub bpm: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct TempoChange {
    pub beat: usize,
    pub time: f64,
    pub from_bpm: f64,
    pub to_bpm: f64,
    pub ratio: f64,
    pub kind: ChangeKind,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub enum ChangeKind {
    #[serde(rename = "tempo change")]
    TempoChange,
    #[serde(rename = "half-time")]
    HalfTime,
    #[serde(rename = "double-time")]
    DoubleTime,
}

/// A version's waveform as the decks have theirs, from our own decode of it (empty if
/// it wouldn't decode).
#[derive(Clone, Default, PartialEq)]
pub struct Waveform {
    /// 4 bytes per 1/150 s: peak, low, mid, high (`waveform::summarise`).
    pub bands: Arc<Vec<u8>>,
    /// Min/max per 1/2000 s (`waveform::detail`).
    pub detail: Arc<Vec<u8>>,
}

impl Waveform {
    pub fn of(audio: &TrackAudio) -> Waveform {
        Waveform {
            bands: Arc::new(waveform::summarise(audio)),
            detail: Arc::new(waveform::detail(audio)),
        }
    }

    /// The waveform of the audio file at `path`.
    fn of_file(path: &Path) -> Result<Waveform, String> {
        let (audio, _) = decode::decode(&path.into())?;
        Ok(Waveform::of(&audio))
    }
}

impl std::fmt::Debug for Waveform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Waveform({} band bytes, {} detail bytes)",
            self.bands.len(),
            self.detail.len()
        )
    }
}

/// A straightened track.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Straightened {
    /// The MP3, in the tool's folder until saved to the library.
    pub output: PathBuf,
    pub bpm: f64,
    /// [`BeatLevel::factor`].
    pub level: f64,
    pub engine: String,
    pub grid: StraightGrid,
    /// Ours, from decoding `output`, not the worker's.
    #[serde(skip)]
    pub wave: Waveform,
    pub duration: f64,
    pub bitrate_kbps: u32,
}

/// Where each detected beat was in the original, and where it is now.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct StraightGrid {
    pub target_bpm: f64,
    pub source_beats: Vec<f64>,
    pub target_beats: Vec<f64>,
}

impl TempoAnalysis {
    /// Half the tempo range as a share of the median, in % (tempo-lock's "Drift").
    pub fn drift_pct(&self) -> f64 {
        if self.median_bpm > 0.0 {
            (self.max_bpm - self.min_bpm) / 2.0 / self.median_bpm * 100.0
        } else {
            0.0
        }
    }

    /// Whether straightening to `bpm` makes sense (the worker checks the same): from
    /// 20 to 400, and within 30% of the detected tempo, or it would change the
    /// track's speed rather than straighten it.
    pub fn check_target(&self, bpm: f64, level: BeatLevel) -> Result<(), String> {
        if !(20.0..=400.0).contains(&bpm) {
            return Err("the BPM must be between 20 and 400".into());
        }
        let detected = self.median_bpm * level.factor();
        if detected > 0.0 && (bpm / detected - 1.0).abs() > 0.3 {
            return Err(format!(
                "{bpm} BPM is more than 30% away from the detected {detected:.1} BPM; that \
                 would change the speed of the track rather than straighten it. If the \
                 detector locked onto half- or double-time, change the beat level instead."
            ));
        }
        Ok(())
    }

    /// The beat-interval histogram, binned as tempo-lock's is: `bin_width` wide bins
    /// over `lo..=hi` BPM (use [`histogram_range`] so two charts share a scale), with
    /// a count per section in each, to stack (one section: one count).
    pub fn histogram(&self, lo: f64, hi: f64, bin_width: f64) -> Histogram {
        let first = (lo / bin_width).floor() as i64 - 1;
        let bins = ((hi / bin_width).ceil() as i64 + 1 - first).max(1) as usize;
        let whole = [Section {
            start_beat: 0,
            end_beat: self.bpm_curve.len(),
            start: 0.0,
            end: self.duration,
            bpm: self.median_bpm,
        }];
        let sections = if self.sections.len() > 1 {
            &self.sections[..]
        } else {
            &whole[..]
        };
        let counts = sections
            .iter()
            .map(|s| {
                let mut c = vec![0u32; bins];
                for &v in &self.bpm_curve
                    [s.start_beat.min(self.bpm_curve.len())..s.end_beat.min(self.bpm_curve.len())]
                {
                    let b = (v / bin_width).floor() as i64 - first;
                    if (0..bins as i64).contains(&b) {
                        c[b as usize] += 1;
                    }
                }
                c
            })
            .collect();
        Histogram {
            first_bpm: first as f64 * bin_width,
            bin_width,
            counts,
        }
    }
}

/// Bins of BPM: bin i is `first_bpm + i·bin_width` up to the next. `counts[s][i]`:
/// beats of section s in bin i.
#[derive(Clone, Debug, PartialEq)]
pub struct Histogram {
    pub first_bpm: f64,
    pub bin_width: f64,
    pub counts: Vec<Vec<u32>>,
}

/// tempo-lock's histogram scale for these analyses (the original and the measured
/// output share one, so a narrower hump really means a steadier tempo) and target
/// tempos: (lo, hi, bin width), about 30 bins across.
pub fn histogram_range(analyses: &[&TempoAnalysis], targets: &[f64]) -> (f64, f64, f64) {
    let values = analyses
        .iter()
        .flat_map(|a| a.bpm_curve.iter())
        .chain(targets)
        .copied();
    let (lo, hi) = values.fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
        (lo.min(v), hi.max(v))
    });
    if !lo.is_finite() {
        return (0.0, 0.0, 1.0);
    }
    (lo, hi, nice_step(((hi - lo) / 30.0).max(0.1)))
}

/// The first "nice" step at least `raw` (tempo-lock's axis and bin steps).
pub fn nice_step(raw: f64) -> f64 {
    const STEPS: [f64; 18] = [
        0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0,
        60.0, 120.0, 300.0,
    ];
    STEPS.into_iter().find(|&s| s >= raw).unwrap_or(600.0)
}

impl Straightened {
    /// The time in the straightened audio of `t` (s) in the original, or back with
    /// `to_original`: piecewise linear through the beats, for A/B playback that keeps
    /// its place.
    pub fn map_time(&self, t: f64, to_original: bool) -> f64 {
        let (src, dst) = match to_original {
            false => (&self.grid.source_beats, &self.grid.target_beats),
            true => (&self.grid.target_beats, &self.grid.source_beats),
        };
        let n = src.len();
        if n == 0 {
            return t;
        }
        if t <= src[0] {
            return t - src[0] + dst[0];
        }
        if t >= src[n - 1] {
            return t - src[n - 1] + dst[n - 1];
        }
        let i = src.partition_point(|&s| s < t);
        let (s0, s1, d0, d1) = (src[i - 1], src[i], dst[i - 1], dst[i]);
        d0 + (t - s0) / (s1 - s0) * (d1 - d0)
    }

    /// How far the beats found in the output sit from the grid they were put on, ms:
    /// (median, 95th percentile). tempo-lock's "Off the grid".
    pub fn off_grid_ms(&self, measured: &TempoAnalysis) -> (f64, f64) {
        let target = &self.grid.target_beats;
        let mut err: Vec<f64> = measured
            .beats
            .iter()
            .map(|&b| {
                let i = target.partition_point(|&t| t < b);
                let after = target.get(i).map_or(f64::INFINITY, |t| (b - t).abs());
                let before = i
                    .checked_sub(1)
                    .map_or(f64::INFINITY, |j| (b - target[j]).abs());
                after.min(before) * 1000.0
            })
            .collect();
        err.sort_by(f64::total_cmp);
        let pct = |q: f64| match err.len() {
            0 => 0.0,
            n => err[((q * n as f64) as usize).min(n - 1)],
        };
        (pct(0.5), pct(0.95))
    }
}

// ---- listening -------------------------------------------------------------------------

/// The beats a version's click track and grid are drawn on, and which are downbeats:
/// the detected beats in the original, and where they were put in the straightened.
pub fn version_beats<'a>(
    version: Version,
    chart: &'a Chart,
    straightened: Option<&'a Straightened>,
) -> (&'a [f64], &'a [bool]) {
    let a = &chart.analysis;
    match (version, straightened) {
        (Version::Straightened, Some(s)) => (&s.grid.target_beats, &a.is_downbeat),
        _ => (&a.beats, &a.is_downbeat),
    }
}

/// A copy of `audio` with a short blip on every beat, higher on downbeats: tempo-lock's
/// click track (30 ms of sine, 1760 Hz on a downbeat and 1175 Hz on the others).
pub fn with_clicks(audio: &TrackAudio, beats: &[f64], downbeats: &[bool]) -> TrackAudio {
    let mut out = TrackAudio {
        samples: audio.samples.clone(),
        sample_rate: audio.sample_rate,
    };
    let sr = audio.sample_rate as f64;
    let len = (0.03 * sr) as usize;
    let frames = audio.frames();
    for (i, &b) in beats.iter().enumerate() {
        let freq = if downbeats.get(i).copied().unwrap_or(false) {
            1760.0
        } else {
            1175.0
        };
        let start = (b * sr).round();
        if start < 0.0 {
            continue;
        }
        for k in 0..len {
            let f = start as usize + k;
            if f >= frames {
                break;
            }
            let t = k as f64 / sr;
            // A 1 ms rise, then down by ~60 dB over the rest.
            let env = (t / 0.001).min(1.0) * (-(t / 0.03) * 7.0).exp();
            let v = (0.5 * env * (std::f64::consts::TAU * freq * t).sin()) as f32;
            out.samples[2 * f] += v;
            out.samples[2 * f + 1] += v;
        }
    }
    out
}

// ---- the tool --------------------------------------------------------------------------

/// The tool's state and worker. Cheap to clone; commands never block.
#[derive(Clone)]
pub struct TempoLock {
    shared: Arc<Shared>,
}

struct Shared {
    worker: WorkerClient,
    store: Store,
    sink: TempoLockSink,
    /// `<data dir>/tempo-lock`: the tool's own.
    dir: PathBuf,
    /// Nudged after a file is saved to the library, so its audio hash is worked out.
    hasher: Option<Sender<()>>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Bumped by every command that starts something; a step whose number is no
    /// longer `running`'s has been replaced, and says nothing more.
    next_op: u64,
    running: Option<Running>,
    session: Option<Session>,
}

struct Running {
    op: u64,
    stage: Stage,
    /// The worker job, once submitted.
    job: Option<u64>,
    cancelled: bool,
}

struct Session {
    track: TrackId,
    /// The track's file (for its tags and name).
    source: PathBuf,
    /// Its folder in the tool's.
    dir: PathBuf,
    /// Our decode of it, once written.
    wav: Option<PathBuf>,
    duration: f64,
    chart: Option<Arc<Chart>>,
    straightened: Option<Arc<Straightened>>,
    measured: Option<Arc<TempoAnalysis>>,
}

/// What the tool has for its track, for a view opened part way through.
#[derive(Clone, Debug, Default)]
pub struct Current {
    pub track: Option<TrackId>,
    pub chart: Option<Arc<Chart>>,
    pub straightened: Option<Arc<Straightened>>,
    #[cfg_attr(not(test), allow(dead_code))] // the tab hears it as an event
    pub measured: Option<Arc<TempoAnalysis>>,
    /// What's running, if anything.
    #[cfg_attr(not(test), allow(dead_code))]
    pub busy: Option<Stage>,
}

/// Share of an analysis's progress before the worker starts: decoding the track and
/// writing it out for the worker.
const DECODED: f64 = 0.15;

impl TempoLock {
    /// The tool with its own background worker process. `data_dir` is the app's data
    /// folder; the tool keeps its files in `tempo-lock` there.
    #[allow(dead_code)] // the app starts it with the worker its `Services` give
    pub fn start(
        store: Store,
        sink: TempoLockSink,
        data_dir: &Path,
        hasher: Option<Sender<()>>,
    ) -> TempoLock {
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        let config = WorkerConfig::python(Role::Background, cores);
        TempoLock::with_worker(config, store, sink, data_dir, hasher)
    }

    pub fn with_worker(
        config: WorkerConfig,
        store: Store,
        sink: TempoLockSink,
        data_dir: &Path,
        hasher: Option<Sender<()>>,
    ) -> TempoLock {
        let dir = data_dir.join("tempo-lock");
        // Leftovers from the last run: decodes, and MP3s that were never saved.
        let _ = std::fs::remove_dir_all(&dir);
        TempoLock {
            shared: Arc::new(Shared {
                worker: WorkerClient::start(config),
                store,
                sink,
                dir,
                hasher,
                state: Mutex::default(),
            }),
        }
    }

    /// Carry out a command, in the background: the outcome comes back as events.
    pub fn run(&self, cmd: TempoLockCmd) {
        match cmd {
            TempoLockCmd::Analyse(track) => self.analyse(track),
            TempoLockCmd::Straighten(opts) => self.straighten(opts),
            TempoLockCmd::Cancel => self.shared.cancel(),
            TempoLockCmd::SaveToLibrary(root) => self.save(root),
            TempoLockCmd::Listen(_) => {}
        }
    }

    pub fn current(&self) -> Current {
        let st = self.shared.state.lock().unwrap();
        let s = st.session.as_ref();
        Current {
            track: s.map(|s| s.track),
            chart: s.and_then(|s| s.chart.clone()),
            straightened: s.and_then(|s| s.straightened.clone()),
            measured: s.and_then(|s| s.measured.clone()),
            busy: st.running.as_ref().map(|r| r.stage),
        }
    }

    fn analyse(&self, track: TrackId) {
        let shared = self.shared.clone();
        let op = {
            let mut st = shared.state.lock().unwrap();
            shared.stop(&mut st);
            if let Some(old) = st.session.take() {
                let _ = std::fs::remove_dir_all(&old.dir);
            }
            st.session = Some(Session {
                track,
                source: PathBuf::new(),
                dir: shared.dir.join(track.to_string()),
                wav: None,
                duration: 0.0,
                chart: None,
                straightened: None,
                measured: None,
            });
            shared.begin(&mut st, Stage::Analyse)
        };
        spawn("earshot-tempo-lock-analyse", move || {
            let result = shared.run_analysis(op, track);
            shared.finish(op, track, Stage::Analyse, result);
        });
    }

    fn straighten(&self, opts: StraightenOptions) {
        let shared = self.shared.clone();
        let (op, track, job) = {
            let mut st = shared.state.lock().unwrap();
            let Some(s) = st.session.as_ref() else {
                return; // no track: nothing to say it to
            };
            let track = s.track;
            let ready = match (&s.chart, &s.wav) {
                (Some(chart), Some(wav)) => {
                    chart.analysis.check_target(opts.bpm, opts.level).map(|_| {
                        json!({
                            "type": "straighten",
                            "wav": wav,
                            "priority": JobPriority::Background.wire(),
                            "target_bpm": opts.bpm,
                            "level": opts.level.factor(),
                            "engine": opts.engine.wire(),
                            "beats": chart.analysis.beats,
                            "beat_index": chart.analysis.beat_index,
                            "out_dir": s.dir,
                            "tags_from": s.source,
                            "name": s.source.file_name().map(|n| n.to_string_lossy()),
                        })
                    })
                }
                _ => Err("analyse the track first".to_string()),
            };
            let req = match ready {
                Ok(req) => req,
                Err(error) => {
                    drop(st);
                    (shared.sink)(TempoLockEvent::Failed {
                        track,
                        stage: Stage::Straighten,
                        error,
                    });
                    return;
                }
            };
            shared.stop(&mut st);
            let s = st.session.as_mut().unwrap();
            // The last straightened file goes (a copy saved to the library stays).
            if let Some(old) = s.straightened.take() {
                let _ = std::fs::remove_file(&old.output);
            }
            s.measured = None;
            let duration = s.duration;
            let op = shared.begin(&mut st, Stage::Straighten);
            let job = shared.worker.submit_job(
                req.as_object().unwrap().clone(),
                // The stretch, writing the MP3 and finding the beats again.
                worker_client::deadline_for(3.0 * duration),
            );
            st.running.as_mut().unwrap().job = Some(job.id());
            (op, track, job)
        };
        spawn("earshot-tempo-lock-straighten", move || {
            let mut stage = Stage::Straighten;
            let result =
                job.wait_events(&mut |e| shared.on_straighten_event(op, track, &mut stage, e));
            shared.finish(op, track, stage, result.map(|_| ()));
        });
    }

    fn save(&self, root: PathBuf) {
        let shared = self.shared.clone();
        let (track, output) = {
            let st = shared.state.lock().unwrap();
            let Some(s) = st.session.as_ref() else {
                return;
            };
            (s.track, s.straightened.as_ref().map(|r| r.output.clone()))
        };
        (shared.sink)(TempoLockEvent::Progress {
            track,
            stage: Stage::SaveToLibrary,
            fraction: 0.0,
        });
        spawn("earshot-tempo-lock-save", move || {
            let event = match output
                .ok_or_else(|| "straighten the track first".to_string())
                .and_then(|output| shared.save_to_library(&output, &root))
            {
                Ok(path) => TempoLockEvent::Saved { track, path },
                Err(error) => TempoLockEvent::Failed {
                    track,
                    stage: Stage::SaveToLibrary,
                    error,
                },
            };
            (shared.sink)(event);
        });
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .expect("spawn tempo lock thread");
}

impl Shared {
    /// A new step is running (replacing any other): its op number.
    fn begin(&self, st: &mut State, stage: Stage) -> u64 {
        st.next_op += 1;
        let op = st.next_op;
        st.running = Some(Running {
            op,
            stage,
            job: None,
            cancelled: false,
        });
        if let Some(s) = &st.session {
            (self.sink)(TempoLockEvent::Progress {
                track: s.track,
                stage,
                fraction: 0.0,
            });
        }
        op
    }

    /// Stop what's running without a word (it's being replaced).
    fn stop(&self, st: &mut State) {
        if let Some(r) = st.running.take()
            && let Some(job) = r.job
        {
            self.worker.cancel(job);
        }
    }

    /// The `Cancel` command: stop what's running; it ends with `Cancelled`.
    fn cancel(&self) {
        let mut st = self.state.lock().unwrap();
        if let Some(r) = st.running.as_mut() {
            r.cancelled = true;
            if let Some(job) = r.job {
                self.worker.cancel(job);
            }
        }
    }

    /// Whether step `op` is still the one running (not replaced or cancelled).
    fn live(&self, op: u64) -> bool {
        let st = self.state.lock().unwrap();
        st.running
            .as_ref()
            .is_some_and(|r| r.op == op && !r.cancelled)
    }

    /// Send `event` if step `op` is still the one running; `update` the session first.
    fn emit(&self, op: u64, update: impl FnOnce(&mut Session), event: TempoLockEvent) {
        self.emit_inner(op, update, event, false);
    }

    /// [`Shared::emit`] for the step's last word: it's no longer running by the time
    /// the event arrives.
    fn emit_last(&self, op: u64, update: impl FnOnce(&mut Session), event: TempoLockEvent) {
        self.emit_inner(op, update, event, true);
    }

    fn emit_inner(
        &self,
        op: u64,
        update: impl FnOnce(&mut Session),
        event: TempoLockEvent,
        last: bool,
    ) {
        let mut st = self.state.lock().unwrap();
        if st.running.as_ref().is_none_or(|r| r.op != op) {
            return;
        }
        if let Some(s) = st.session.as_mut() {
            update(s);
        }
        if last {
            st.running = None;
        }
        drop(st);
        (self.sink)(event);
    }

    /// Step `op` has ended: say how, unless it was replaced.
    fn finish(&self, op: u64, track: TrackId, stage: Stage, result: Result<(), String>) {
        let mut st = self.state.lock().unwrap();
        let Some(r) = st.running.as_ref().filter(|r| r.op == op) else {
            return;
        };
        let cancelled = r.cancelled;
        st.running = None;
        drop(st);
        let event = match result {
            Ok(()) => return,
            Err(_) if cancelled => TempoLockEvent::Cancelled { track, stage },
            Err(e) if e == CANCELLED => TempoLockEvent::Cancelled { track, stage },
            Err(error) => TempoLockEvent::Failed {
                track,
                stage,
                error,
            },
        };
        (self.sink)(event);
    }

    fn progress(&self, op: u64, track: TrackId, stage: Stage, fraction: f64) {
        let event = TempoLockEvent::Progress {
            track,
            stage,
            fraction: fraction.clamp(0.0, 1.0),
        };
        self.emit(op, |_| {}, event);
    }

    /// Decode the track, then chart it in the worker.
    fn run_analysis(&self, op: u64, track: TrackId) -> Result<(), String> {
        let source = self
            .store
            .reader
            .track_path(track)
            .map_err(|e| format!("track {track}: {e}"))?;
        let (audio, _) = decode::decode(&source.as_path().into())?;
        if !self.live(op) {
            return Err(CANCELLED.into());
        }
        let dir = self.dir.join(track.to_string());
        let wav = dir.join("source.wav");
        std::fs::create_dir_all(&dir)
            .and_then(|_| write_stereo_wav(&wav, &audio))
            .map_err(|e| format!("{}: {e}", wav.display()))?;
        let wave = Waveform::of(&audio);
        self.progress(op, track, Stage::Analyse, DECODED);
        let duration = audio.duration();
        drop(audio);

        let job = {
            let mut st = self.state.lock().unwrap();
            match st.running.as_mut() {
                Some(r) if r.op == op && !r.cancelled => {}
                _ => return Err(CANCELLED.into()),
            }
            let s = st.session.as_mut().unwrap();
            (s.source, s.wav, s.duration) = (source, Some(wav.clone()), duration);
            let req = json!({
                "type": "tempo_lock",
                "wav": wav,
                "priority": JobPriority::Background.wire(),
            });
            let job = self.worker.submit_job(
                req.as_object().unwrap().clone(),
                worker_client::deadline_for(duration),
            );
            st.running.as_mut().unwrap().job = Some(job.id());
            job
        };
        let mut chart = None;
        job.wait_events(&mut |e| match e {
            JobEvent::Update(Update::Progress { fraction, .. }) => {
                self.progress(
                    op,
                    track,
                    Stage::Analyse,
                    DECODED + (1.0 - DECODED) * fraction,
                );
            }
            JobEvent::Result { step, data, .. } if step == "tempo_lock" => {
                chart = Some(serde_json::from_value::<Chart>(data));
            }
            _ => {}
        })?;
        let chart = match chart {
            Some(Ok(c)) => Arc::new(Chart { wave, ..c }),
            Some(Err(e)) => return Err(format!("the worker sent an unusable chart: {e}")),
            None => return Err("the worker sent no chart".into()),
        };
        let event = TempoLockEvent::Analysed {
            track,
            chart: chart.clone(),
        };
        self.emit_last(op, |s| s.chart = Some(chart), event);
        Ok(())
    }

    fn on_straighten_event(&self, op: u64, track: TrackId, stage: &mut Stage, e: JobEvent) {
        match e {
            JobEvent::Update(Update::Progress { analyser, fraction }) => {
                if analyser == "measure" {
                    if *stage != Stage::Measure {
                        // The measure step's progress starts again from 0.
                        *stage = Stage::Measure;
                        let mut st = self.state.lock().unwrap();
                        if let Some(r) = st.running.as_mut().filter(|r| r.op == op) {
                            r.stage = Stage::Measure;
                        }
                    }
                    self.progress(op, track, Stage::Measure, fraction);
                } else {
                    self.progress(op, track, Stage::Straighten, fraction);
                }
            }
            JobEvent::Update(Update::Started) => {}
            JobEvent::Result { step, data, .. } if step == "straighten" => {
                match serde_json::from_value::<Straightened>(data) {
                    Ok(mut r) => {
                        // Not worth failing for: the tab just has no waveform for it.
                        r.wave = Waveform::of_file(&r.output).unwrap_or_else(|e| {
                            eprintln!("tempo lock: {}: {e}", r.output.display());
                            Waveform::default()
                        });
                        let r = Arc::new(r);
                        let event = TempoLockEvent::Straightened {
                            track,
                            result: r.clone(),
                        };
                        self.emit(op, |s| s.straightened = Some(r), event);
                    }
                    Err(e) => {
                        let error = format!("the worker sent an unusable result: {e}");
                        self.emit(
                            op,
                            |_| {},
                            TempoLockEvent::Failed {
                                track,
                                stage: Stage::Straighten,
                                error,
                            },
                        );
                    }
                }
            }
            JobEvent::Result { step, data, .. } if step == "measure" => {
                #[derive(Deserialize)]
                struct Measured {
                    analysis: TempoAnalysis,
                }
                let event = match serde_json::from_value::<Measured>(data) {
                    Ok(m) => {
                        let a = Arc::new(m.analysis);
                        let event = TempoLockEvent::Measured {
                            track,
                            analysis: a.clone(),
                        };
                        self.emit_last(op, |s| s.measured = Some(a), event);
                        return;
                    }
                    Err(e) => TempoLockEvent::Failed {
                        track,
                        stage: Stage::Measure,
                        error: format!("the worker sent an unusable measurement: {e}"),
                    },
                };
                self.emit(op, |_| {}, event);
            }
            JobEvent::Failed { step, error } => {
                let stage = if step == "measure" {
                    Stage::Measure
                } else {
                    Stage::Straighten
                };
                self.emit(
                    op,
                    |_| {},
                    TempoLockEvent::Failed {
                        track,
                        stage,
                        error,
                    },
                );
            }
            JobEvent::Result { .. } => {}
        }
    }

    /// Copy `output` into the library folder `root` under a name no file there has,
    /// and scan it in. The new file's path.
    fn save_to_library(&self, output: &Path, root: &Path) -> Result<PathBuf, String> {
        let roots = self.store.call(|w| w.roots()).map_err(|e| e.to_string())?;
        if !roots.iter().any(|r| same_dir(r, root)) {
            return Err(format!(
                "{} isn't one of the library folders",
                root.display()
            ));
        }
        let path = copy_without_clobbering(output, root)?;
        library::scan_file(&self.store, &path).map_err(|e| e.to_string())?;
        if let Some(h) = &self.hasher {
            let _ = h.send(());
        }
        Ok(path)
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    a.components().eq(b.components())
}

/// Copy `from` into `dir` with its own file name, or "name (2).mp3", "name (3).mp3", …
/// when that's taken. Never replaces a file (the name is claimed with create-new).
pub fn copy_without_clobbering(from: &Path, dir: &Path) -> Result<PathBuf, String> {
    let name = from
        .file_name()
        .ok_or_else(|| format!("{}: not a file", from.display()))?;
    let name = Path::new(name);
    let stem = name.file_stem().unwrap_or_default().to_string_lossy();
    let ext = name.extension().map(|e| e.to_string_lossy());
    let mut src = std::fs::File::open(from).map_err(|e| format!("{}: {e}", from.display()))?;
    for n in 1..=999 {
        let file = match (n, &ext) {
            (1, _) => name.to_path_buf(),
            (_, Some(ext)) => format!("{stem} ({n}).{ext}").into(),
            (_, None) => format!("{stem} ({n})").into(),
        };
        let path = dir.join(file);
        let mut dst = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        if let Err(e) = std::io::copy(&mut src, &mut dst).and_then(|_| dst.sync_all()) {
            drop(dst);
            let _ = std::fs::remove_file(&path);
            return Err(format!("{}: {e}", path.display()));
        }
        return Ok(path);
    }
    Err(format!(
        "{} already has 999 files called {}",
        dir.display(),
        name.display()
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{Receiver, channel};
    use std::time::{Duration, Instant};

    use earshot_engine::decode::TrackAudio;

    use super::*;
    use crate::core::store::FILE_NAME;

    /// A stand-in worker: answers hello, then runs one job at a time on a thread while
    /// it reads cancels. `tempo_lock` sends a small chart; `straighten` writes a fake
    /// MP3 named as the real worker names it, then measures it. A straighten to 121
    /// BPM is slow (progress for ~5 s, checking for a cancel).
    const FAKE: &str = r#"
import json, os, sys, threading, time
lock = threading.Lock()
cancelled = set()
def send(m):
    with lock:
        sys.stdout.write(json.dumps(m) + "\n"); sys.stdout.flush()
beats = [0.5, 1.0, 1.5, 2.0, 2.5]
analysis = {"sr": 44100, "duration": 3.0, "detector": "fake", "beats": beats,
    "beat_index": [0, 1, 2, 3, 4], "is_downbeat": [True, False, False, False, True],
    "dropped_beats": [0.1], "bpm_times": [0.75, 1.25, 1.75, 2.25], "bpm_curve": [118, 121, 120, 122],
    "median_bpm": 120.5, "min_bpm": 118.1, "max_bpm": 121.9, "suggested_bpm": 120.0,
    "sections": [{"start_beat": 0, "end_beat": 4, "start": 0.5, "end": 2.5, "bpm": 120.5}],
    "tempo_changes": [], "kick_shift": -0.01}
def run(r):
    i = r["id"]
    send({"id": i, "type": "started"})
    if r["type"] == "tempo_lock":
        assert os.path.getsize(r["wav"]) > 44
        send({"id": i, "type": "progress", "analyser": "tempo_lock", "fraction": 0.5})
        send({"id": i, "type": "result", "analyser": "tempo_lock", "version": 1,
              "data": {"analysis": analysis}})
    else:
        steps = 100 if r["target_bpm"] == 121 else 2
        for k in range(steps):
            if i in cancelled:
                send({"id": i, "type": "cancelled"}); return
            send({"id": i, "type": "progress", "analyser": "straighten", "fraction": (k + 1) / steps})
            time.sleep(0.05 if steps > 2 else 0)
        stem = os.path.splitext(r["name"])[0]
        out = os.path.join(r["out_dir"], f"{stem} (straightened {r['target_bpm']:g} bpm).mp3")
        with open(out, "w") as f:
            f.write("straightened at %g" % r["target_bpm"])
        p = 60 / r["target_bpm"] * r["level"]
        grid = {"target_bpm": r["target_bpm"], "source_beats": r["beats"],
                "target_beats": [r["beats"][0] + k * p for k in r["beat_index"]]}
        send({"id": i, "type": "result", "analyser": "straighten", "version": 1, "data": {
            "output": out, "bpm": r["target_bpm"], "level": r["level"], "engine": r["engine"],
            "grid": grid, "duration": 3.0, "bitrate_kbps": 128}})
        send({"id": i, "type": "progress", "analyser": "measure", "fraction": 0.5})
        send({"id": i, "type": "result", "analyser": "measure", "version": 1,
              "data": {"analysis": dict(analysis, beats=grid["target_beats"])}})
    send({"id": i, "type": "done", "seconds": 0.1})
jobs = []
cond = threading.Condition()
def runner():
    while True:
        with cond:
            while not jobs:
                cond.wait()
            r = jobs.pop(0)
        run(r)
threading.Thread(target=runner, daemon=True).start()
for line in sys.stdin:
    r = json.loads(line)
    if r["type"] == "hello":
        send({"type": "hello", "protocol": 2, "analysers": {"grid": 3},
              "steps": {"tempo_lock": 1, "straighten": 1, "measure": 1},
              "detector": "fake", "models_loaded": []})
    elif r["type"] == "cancel":
        cancelled.add(r["id"])
    else:
        with cond:
            jobs.append(r); cond.notify()
"#;

    fn fake() -> WorkerConfig {
        WorkerConfig {
            program: PathBuf::from("/usr/bin/python3"),
            args: vec!["-c".into(), FAKE.into()],
            dir: std::env::temp_dir(),
            envs: vec![],
            start_timeout: Duration::from_secs(20),
            heartbeat_timeout: Duration::from_secs(30),
            grace: Duration::from_secs(5),
            idle_exit: None,
            keep_alive: false,
            expect: vec![],
            max_memory: None,
        }
    }

    struct Setup {
        tool: TempoLock,
        events: Receiver<TempoLockEvent>,
        store: Store,
        root: PathBuf,
        track: TrackId,
        dir: PathBuf,
    }

    impl Drop for Setup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// A library folder with one track ("Slow Burn.wav", 3 s), scanned in.
    fn setup(name: &str) -> Setup {
        setup_with(name, None, fake())
    }

    /// `track`: a file to copy into the library folder instead.
    fn setup_with(name: &str, track: Option<&Path>, config: WorkerConfig) -> Setup {
        let dir = std::env::temp_dir().join(format!("earshot-tempo-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = dir.join("music");
        std::fs::create_dir_all(&root).unwrap();
        if let Some(track) = track {
            std::fs::copy(track, root.join(track.file_name().unwrap())).unwrap();
        } else {
            let sr = 44100;
            let samples = (0..3 * sr)
                .flat_map(|i| {
                    let v = (i as f32 * 0.05).sin() * 0.5;
                    [v, -v]
                })
                .collect();
            let audio = TrackAudio {
                samples,
                sample_rate: sr as u32,
            };
            write_stereo_wav(&root.join("Slow Burn.wav"), &audio).unwrap();
        }
        let store = Store::open(&dir.join(FILE_NAME), Arc::new(|_| {})).unwrap();
        let r = root.clone();
        store.call(move |w| w.add_roots(&[r])).unwrap();
        library::scan_library(&store).unwrap();
        let track = store
            .reader
            .with(|c| Ok(c.query_row("SELECT track_id FROM files", [], |r| r.get(0))?))
            .unwrap();
        let (tx, events) = channel();
        let sink: TempoLockSink = Arc::new(move |e| {
            let _ = tx.send(e);
        });
        let tool = TempoLock::with_worker(config, store.clone(), sink, &dir.join("data"), None);
        Setup {
            tool,
            events,
            store,
            root,
            track,
            dir,
        }
    }

    impl Setup {
        /// Events up to and including the first one `stop` picks (10 s at most).
        fn until(&self, stop: impl Fn(&TempoLockEvent) -> bool) -> Vec<TempoLockEvent> {
            self.until_within(10, stop)
        }

        fn until_within(
            &self,
            seconds: u64,
            stop: impl Fn(&TempoLockEvent) -> bool,
        ) -> Vec<TempoLockEvent> {
            let t0 = Instant::now();
            let mut seen = vec![];
            while t0.elapsed() < Duration::from_secs(seconds) {
                if let Ok(e) = self.events.recv_timeout(Duration::from_millis(100)) {
                    let done = stop(&e);
                    seen.push(e);
                    if done {
                        return seen;
                    }
                }
            }
            panic!("timed out; events so far: {seen:#?}");
        }

        fn analysed(&self) -> Arc<Chart> {
            self.tool.run(TempoLockCmd::Analyse(self.track));
            let seen = self.until(|e| {
                matches!(
                    e,
                    TempoLockEvent::Analysed { .. } | TempoLockEvent::Failed { .. }
                )
            });
            match seen.last() {
                Some(TempoLockEvent::Analysed { chart, .. }) => chart.clone(),
                other => panic!("{other:?}"),
            }
        }

        fn files_in_library(&self) -> Vec<PathBuf> {
            self.store
                .reader
                .with(|c| {
                    let mut st = c.prepare("SELECT path FROM files ORDER BY path")?;
                    let rows = st.query_map([], |r| r.get::<_, Vec<u8>>(0))?;
                    Ok(rows
                        .map(|r| crate::core::store::bytes_path(r.unwrap()))
                        .collect())
                })
                .unwrap()
        }
    }

    fn progress_of(seen: &[TempoLockEvent], of: Stage) -> Vec<f64> {
        seen.iter()
            .filter_map(|e| match e {
                TempoLockEvent::Progress {
                    stage, fraction, ..
                } if *stage == of => Some(*fraction),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn analyse_straighten_and_save_to_the_library() {
        let s = setup("flow");
        s.tool.run(TempoLockCmd::Analyse(s.track));
        let seen = s.until(|e| matches!(e, TempoLockEvent::Analysed { .. }));
        let p = progress_of(&seen, Stage::Analyse);
        assert_eq!(p[0], 0.0, "{p:?}");
        assert!(p.windows(2).all(|w| w[0] <= w[1]) && p.len() >= 3, "{p:?}");
        let TempoLockEvent::Analysed { track, chart } = seen.last().unwrap() else {
            unreachable!()
        };
        assert_eq!(*track, s.track);
        assert_eq!(chart.analysis.beats.len(), 5);
        assert_eq!(
            chart.analysis.bpm_curve.len(),
            chart.analysis.bpm_times.len()
        );
        // The waveform is ours, from the decode: 3 s at 2000 min/max pairs a second.
        assert!((chart.wave.detail.len() / 2).abs_diff(3 * 2000) <= 1);
        assert_eq!(s.tool.current().chart.as_deref(), Some(&**chart));

        s.tool
            .run(TempoLockCmd::Straighten(StraightenOptions::at(120.0)));
        let seen = s.until(|e| matches!(e, TempoLockEvent::Measured { .. }));
        let straight = seen
            .iter()
            .find_map(|e| match e {
                TempoLockEvent::Straightened { result, .. } => Some(result.clone()),
                _ => None,
            })
            .expect("straightened before measured");
        assert_eq!(progress_of(&seen, Stage::Straighten), vec![0.0, 0.5, 1.0]);
        assert_eq!(progress_of(&seen, Stage::Measure), vec![0.5]);
        assert!(straight.output.exists());
        assert_eq!(
            straight.output.file_name().unwrap(),
            "Slow Burn (straightened 120 bpm).mp3"
        );
        assert_eq!(straight.grid.target_beats, vec![0.5, 1.0, 1.5, 2.0, 2.5]);
        let now = s.tool.current();
        assert!(now.measured.is_some() && now.busy.is_none());

        // Saved into the library folder and scanned in.
        s.tool.run(TempoLockCmd::SaveToLibrary(s.root.clone()));
        let saved = |s: &Setup| match s
            .until(|e| !matches!(e, TempoLockEvent::Progress { .. }))
            .pop()
        {
            Some(TempoLockEvent::Saved { path, .. }) => path,
            other => panic!("{other:?}"),
        };
        let first = saved(&s);
        assert_eq!(first, s.root.join("Slow Burn (straightened 120 bpm).mp3"));
        assert_eq!(
            std::fs::read_to_string(&first).unwrap(),
            "straightened at 120"
        );
        assert!(s.files_in_library().contains(&first));

        // Again: the first copy stays as it is, and this one gets another name.
        std::fs::write(&first, "the user's own file").unwrap();
        s.tool.run(TempoLockCmd::SaveToLibrary(s.root.clone()));
        let second = saved(&s);
        assert_eq!(
            second,
            s.root.join("Slow Burn (straightened 120 bpm) (2).mp3")
        );
        assert_eq!(
            std::fs::read_to_string(&first).unwrap(),
            "the user's own file"
        );
        assert_eq!(
            std::fs::read_to_string(&second).unwrap(),
            "straightened at 120"
        );
        assert_eq!(s.files_in_library().len(), 3);

        // Only into a library folder.
        let elsewhere = s.dir.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        s.tool.run(TempoLockCmd::SaveToLibrary(elsewhere.clone()));
        match s
            .until(|e| !matches!(e, TempoLockEvent::Progress { .. }))
            .pop()
        {
            Some(TempoLockEvent::Failed {
                stage: Stage::SaveToLibrary,
                error,
                ..
            }) => assert!(error.contains("library folders"), "{error}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    }

    #[test]
    fn cancel_stops_a_straighten() {
        let s = setup("cancel");
        s.analysed();
        s.tool
            .run(TempoLockCmd::Straighten(StraightenOptions::at(121.0)));
        s.until(|e| {
            matches!(e, TempoLockEvent::Progress { stage: Stage::Straighten, fraction, .. } if *fraction > 0.0)
        });
        assert_eq!(s.tool.current().busy, Some(Stage::Straighten));
        s.tool.run(TempoLockCmd::Cancel);
        let seen = s.until(|e| !matches!(e, TempoLockEvent::Progress { .. }));
        assert!(
            matches!(
                seen.last(),
                Some(TempoLockEvent::Cancelled {
                    stage: Stage::Straighten,
                    ..
                })
            ),
            "{seen:?}"
        );
        let now = s.tool.current();
        assert!(now.busy.is_none() && now.straightened.is_none() && now.chart.is_some());
        // Nothing to save, and the tool still works.
        s.tool.run(TempoLockCmd::SaveToLibrary(s.root.clone()));
        let last = s.until(|e| !matches!(e, TempoLockEvent::Progress { .. }));
        assert!(matches!(last.last(), Some(TempoLockEvent::Failed { .. })));
        s.tool
            .run(TempoLockCmd::Straighten(StraightenOptions::at(120.0)));
        s.until(|e| matches!(e, TempoLockEvent::Measured { .. }));
    }

    #[test]
    fn a_new_command_replaces_a_running_one_quietly() {
        let s = setup("replace");
        s.analysed();
        s.tool
            .run(TempoLockCmd::Straighten(StraightenOptions::at(121.0)));
        s.until(|e| {
            matches!(e, TempoLockEvent::Progress { stage: Stage::Straighten, fraction, .. } if *fraction > 0.0)
        });
        // Another BPM while the first is running: the first says nothing more.
        s.tool
            .run(TempoLockCmd::Straighten(StraightenOptions::at(119.0)));
        let seen = s.until(|e| matches!(e, TempoLockEvent::Measured { .. }));
        assert!(
            !seen.iter().any(|e| matches!(
                e,
                TempoLockEvent::Cancelled { .. } | TempoLockEvent::Failed { .. }
            )),
            "{seen:#?}"
        );
        let out = s.tool.current().straightened.unwrap();
        assert_eq!(out.bpm, 119.0);
        // Opening a track again starts a new session.
        let chart = s.analysed();
        assert_eq!(chart.analysis.suggested_bpm, 120.0);
        assert!(s.tool.current().straightened.is_none());
        assert!(!out.output.exists(), "the old session's files are gone");
    }

    #[test]
    fn straighten_needs_an_analysis_and_a_sensible_bpm() {
        let s = setup("refuse");
        // No track yet: nothing happens.
        s.tool
            .run(TempoLockCmd::Straighten(StraightenOptions::at(120.0)));
        assert!(s.events.recv_timeout(Duration::from_millis(300)).is_err());
        s.analysed();
        s.tool
            .run(TempoLockCmd::Straighten(StraightenOptions::at(200.0)));
        match s.until(|_| true).pop() {
            Some(TempoLockEvent::Failed {
                stage: Stage::Straighten,
                error,
                ..
            }) => assert!(error.contains("30%"), "{error}"),
            other => panic!("{other:?}"),
        }
        // Half-time detected: 241 is the real tempo, so fine.
        let opts = StraightenOptions {
            level: BeatLevel::HalfTime,
            ..StraightenOptions::at(241.0)
        };
        s.tool.run(TempoLockCmd::Straighten(opts));
        let seen = s.until(|e| matches!(e, TempoLockEvent::Straightened { .. }));
        let Some(TempoLockEvent::Straightened { result, .. }) = seen.last() else {
            unreachable!()
        };
        assert_eq!(result.level, 2.0);
    }

    #[test]
    fn copies_never_clobber() {
        let dir = std::env::temp_dir().join(format!("earshot-tempo-copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (from, to) = (dir.join("from"), dir.join("to"));
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        let src = from.join("a.mp3");
        std::fs::write(&src, "new").unwrap();
        std::fs::write(to.join("a.mp3"), "old").unwrap();
        std::fs::write(to.join("a (2).mp3"), "old 2").unwrap();
        let got = copy_without_clobbering(&src, &to).unwrap();
        assert_eq!(got, to.join("a (3).mp3"));
        assert_eq!(std::fs::read_to_string(to.join("a.mp3")).unwrap(), "old");
        assert_eq!(std::fs::read_to_string(&got).unwrap(), "new");
        let e = copy_without_clobbering(&src, &dir.join("missing")).unwrap_err();
        assert!(e.contains("missing"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn analysis(curve: Vec<f64>, sections: Vec<Section>) -> TempoAnalysis {
        TempoAnalysis {
            sr: 44100,
            duration: 10.0,
            detector: "fake".into(),
            beats: vec![],
            beat_index: vec![],
            is_downbeat: vec![],
            dropped_beats: vec![],
            bpm_times: vec![],
            median_bpm: 120.0,
            min_bpm: 118.0,
            max_bpm: 122.0,
            suggested_bpm: 120.0,
            bpm_curve: curve,
            sections,
            tempo_changes: vec![],
            kick_shift: 0.0,
        }
    }

    #[test]
    fn chart_helpers() {
        let a = analysis(vec![119.0, 120.0, 120.4, 121.0], vec![]);
        assert!((a.drift_pct() - 100.0 / 60.0).abs() < 1e-9);
        let (lo, hi, bin) = histogram_range(&[&a], &[124.0]);
        assert_eq!((lo, hi, bin), (119.0, 124.0, 0.2));
        let h = a.histogram(lo, hi, bin);
        assert_eq!(h.counts.len(), 1);
        assert_eq!(h.counts[0].iter().sum::<u32>(), 4);
        let at = |bpm: f64| ((bpm - h.first_bpm) / h.bin_width).floor() as usize;
        assert_eq!(h.counts[0][at(120.1)], 1);
        // Two sections: a count each, to stack.
        let sec = |a, b, bpm| Section {
            start_beat: a,
            end_beat: b,
            start: 0.0,
            end: 0.0,
            bpm,
        };
        let two = analysis(
            vec![100.0, 100.0, 140.0],
            vec![sec(0, 2, 100.0), sec(2, 3, 140.0)],
        );
        let h = two.histogram(100.0, 140.0, 2.0);
        assert_eq!(
            h.counts
                .iter()
                .map(|c| c.iter().sum::<u32>())
                .collect::<Vec<_>>(),
            [2, 1]
        );

        assert!(a.check_target(124.0, BeatLevel::Beat).is_ok());
        assert!(a.check_target(240.0, BeatLevel::Beat).is_err());
        assert!(a.check_target(240.0, BeatLevel::HalfTime).is_ok());
        assert!(a.check_target(10.0, BeatLevel::Beat).is_err());

        let s = Straightened {
            output: PathBuf::new(),
            bpm: 120.0,
            level: 1.0,
            engine: "r3".into(),
            grid: StraightGrid {
                target_bpm: 120.0,
                source_beats: vec![1.0, 1.6, 2.0],
                target_beats: vec![1.0, 1.5, 2.0],
            },
            wave: Waveform::default(),
            duration: 3.0,
            bitrate_kbps: 128,
        };
        assert_eq!(s.map_time(0.5, false), 0.5);
        assert!((s.map_time(1.3, false) - 1.25).abs() < 1e-9);
        assert!((s.map_time(1.25, true) - 1.3).abs() < 1e-9);
        assert_eq!(s.map_time(2.5, false), 2.5);
        let mut m = analysis(vec![], vec![]);
        m.beats = vec![1.001, 1.5, 2.004];
        let (median, p95) = s.off_grid_ms(&m);
        assert!(
            (median - 1.0).abs() < 1e-6 && (p95 - 4.0).abs() < 1e-6,
            "{median} {p95}"
        );
    }

    #[test]
    fn clicks_land_on_the_beats() {
        let audio = TrackAudio {
            samples: vec![0.0; 2 * 44100],
            sample_rate: 44100,
        };
        let out = with_clicks(&audio, &[0.25, 0.5, 2.0], &[true, false, false]);
        let loud = |t: f64| {
            let f = (t * 44100.0) as usize;
            (f..f + 441)
                .map(|i| out.samples[2 * i].abs())
                .fold(0.0, f32::max)
        };
        assert!(loud(0.25) > 0.1 && loud(0.5) > 0.1);
        assert_eq!(loud(0.1), 0.0);
        assert_eq!(loud(0.75), 0.0);
        // Past the end: nothing, and no panic.
        assert_eq!(out.samples.len(), audio.samples.len());
    }

    #[test]
    fn chart_json_has_no_defaults() {
        let change = r#"{"beat": 3, "time": 1.0, "from_bpm": 120, "to_bpm": 60, "ratio": 0.5,
                         "kind": "half-time"}"#;
        let c: TempoChange = serde_json::from_str(change).unwrap();
        assert_eq!(c.kind, ChangeKind::HalfTime);
    }

    /// End to end against the real worker (needs rubberband-cli and ffmpeg):
    /// `EARSHOT_TEST_TRACK=/path/song.mp3 cargo test -p earshot-gui -- --include-ignored straightens_a_real_track`.
    #[test]
    #[ignore]
    fn straightens_a_real_track() {
        let track =
            PathBuf::from(std::env::var("EARSHOT_TEST_TRACK").expect("set EARSHOT_TEST_TRACK"));
        let s = setup_with(
            "real",
            Some(&track),
            WorkerConfig::python(Role::Background, 4),
        );
        s.tool.run(TempoLockCmd::Analyse(s.track));
        let seen = s.until_within(600, |e| {
            matches!(
                e,
                TempoLockEvent::Analysed { .. } | TempoLockEvent::Failed { .. }
            )
        });
        let Some(TempoLockEvent::Analysed { chart, .. }) = seen.last() else {
            panic!("{:?}", seen.last());
        };
        let a = &chart.analysis;
        eprintln!(
            "{} beats, median {:.2} BPM, range {:.1}-{:.1}, {} sections, {} changes",
            a.beats.len(),
            a.median_bpm,
            a.min_bpm,
            a.max_bpm,
            a.sections.len(),
            a.tempo_changes.len()
        );
        s.tool.run(TempoLockCmd::Straighten(StraightenOptions::at(
            a.suggested_bpm,
        )));
        let seen = s.until_within(900, |e| {
            matches!(
                e,
                TempoLockEvent::Measured { .. } | TempoLockEvent::Failed { .. }
            )
        });
        let now = s.tool.current();
        let (Some(out), Some(m)) = (now.straightened, now.measured) else {
            panic!("{:?}", seen.last());
        };
        let (median, p95) = out.off_grid_ms(&m);
        eprintln!(
            "{}: measured median {:.2} BPM, drift ±{:.1}%, off the grid {median:.0} ms (95% within {p95:.0} ms)",
            out.output.display(),
            m.median_bpm,
            m.drift_pct()
        );
        assert!((m.median_bpm - a.suggested_bpm).abs() < 1.0);
        s.tool.run(TempoLockCmd::SaveToLibrary(s.root.clone()));
        let seen = s.until(|e| !matches!(e, TempoLockEvent::Progress { .. }));
        let Some(TempoLockEvent::Saved { path, .. }) = seen.last() else {
            panic!("{:?}", seen.last());
        };
        assert!(s.files_in_library().contains(path));
    }
}
