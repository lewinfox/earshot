//! Stems (issue #2): split a track into drums, bass, vocals and "other", cache them,
//! and give them to whichever deck has the track.
//!
//! The flow:
//!
//! 1. [`StemsCmd::Calculate`]: decode the track (our own decoder, so the stems line
//!    up with what the decks play, sample for sample), and have the worker separate
//!    it ([`crate::core::worker_client`], job type `stems`). It writes drums, bass
//!    and vocals as FLAC; "other" is never stored (the engine works it out as the
//!    track minus those three).
//! 2. The stems land in the cache, `<data dir>/stems/<audio hash>/`, keyed by the
//!    track's audio hash, so a retagged or moved file (or a copy) keeps its stems.
//! 3. Any deck with that track gets them ([`attach`]), as does every later load.
//!    Decks with the same audio share one copy in memory.
//!
//! The track is the unit: a track has at most one job, and Calculate does nothing
//! while it has one (cancelling included). Each job works in its track's own folder,
//! `<data dir>/stems/<track id>.part`, renamed into place once it's all there; any
//! left from a crash are cleared at start-up. Jobs for different tracks wait their
//! turn in the worker, one at a time (separation takes about 2.4 GB and most of the
//! CPU). Each is listed in the status line's jobs, where it can be cancelled.
//!
//! Not done yet: a size cap on the cache, Opus instead of FLAC, the "always calculate"
//! setting, stems for tracks from other DJ programs' libraries, and stems surviving
//! an audio restart.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use earshot_engine::decode::{self, TrackAudio};
use earshot_engine::media::Location;
use earshot_engine::stems::{self as engine_stems, StemAudio};
use earshot_engine::{Command, DeckId, waveform};
use serde_json::json;

use super::analysis::write_stereo_wav;
use super::hash;
use super::jobs::{self, Jobs};
use super::outside;
use super::player::Player;
use super::session;
use super::store::Store;
use super::track::TrackId;
use super::worker_client::{
    self, CANCELLED, JobEvent, JobPriority, Update, WorkerClient, WorkerConfig,
};

/// The stored stems' names, in the order of [`engine_stems::NAMES`]: `drums.flac` or
/// `drums.ogg` (FLAC for a lossless track, Vorbis for a lossy one).
const FILES: [&str; engine_stems::STORED] = ["drums", "bass", "vocals"];
const EXTENSIONS: [&str; 2] = ["flac", "ogg"];
/// Touched whenever a deck gets a folder's stems: what's used least recently goes
/// first when the cache is over its limit.
const USED: &str = "used";
/// Share of a calculation's progress before the worker starts (decoding the track).
const DECODED: f64 = 0.05;

static CACHE: OnceLock<PathBuf> = OnceLock::new();

/// The cache's size on disk, bytes, as of start-up (after eviction) plus what's been
/// added since.
static USAGE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Where stems are kept: `<data dir>/stems`. Set once at startup, which then (in the
/// background) clears out half-written folders left by a crash, and removes the
/// least recently used stems until the cache is within `limit` bytes.
pub fn init(data_dir: &Path, limit: u64) {
    let dir = data_dir.join("stems");
    let _ = CACHE.set(dir.clone());
    spawn("earshot-stems-evict", move || {
        let used = tidy(&dir, limit);
        USAGE.store(used, std::sync::atomic::Ordering::Relaxed);
    });
}

/// How much the stems cache takes on disk, bytes.
pub fn usage() -> u64 {
    USAGE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Remove `.part` folders, then the least recently used stems until the rest fit in
/// `limit` bytes. Returns what's left, bytes.
fn tidy(dir: &Path, limit: u64) -> u64 {
    let mut kept = vec![];
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = e.path();
        if !path.is_dir() {
            continue;
        }
        if e.file_name().to_string_lossy().ends_with(".part") {
            let _ = std::fs::remove_dir_all(&path);
            continue;
        }
        let size: u64 = std::fs::read_dir(&path)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|f| f.metadata().ok())
            .map(|m| m.len())
            .sum();
        let when = std::fs::metadata(path.join(USED))
            .or_else(|_| std::fs::metadata(&path))
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        kept.push((when, size, path));
    }
    // Oldest first.
    kept.sort();
    let mut total: u64 = kept.iter().map(|k| k.1).sum();
    for (_, size, path) in &kept {
        if total <= limit {
            break;
        }
        if std::fs::remove_dir_all(path).is_ok() {
            eprintln!("stems: over the limit, removed {}", path.display());
            total -= size;
        }
    }
    total
}

/// A stored stem's file in `dir`, whichever format it's in.
fn stem_file(dir: &Path, name: &str) -> Option<PathBuf> {
    EXTENSIONS
        .iter()
        .map(|e| dir.join(format!("{name}.{e}")))
        .find(|p| p.is_file())
}

fn cache_dir() -> Option<&'static Path> {
    CACHE.get().map(PathBuf::as_path)
}

/// The cached stems' folder for a track's audio, if it has them.
fn cached(hash: &hash::Hash) -> Option<PathBuf> {
    let dir = cache_dir()?.join(hash::to_hex(hash));
    FILES
        .iter()
        .all(|f| stem_file(&dir, f).is_some())
        .then_some(dir)
}

#[derive(Clone, Debug, PartialEq)]
pub enum StemsCmd {
    /// Work out this track's stems (nothing, if it already has a job).
    Calculate(TrackId),
    /// Stop this track's job.
    Cancel(TrackId),
}

/// What a calculation is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Fetching the model, the first time it's used (about 80 MB).
    Download,
    Separate,
}

/// A track's job, as the UI shows it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Status {
    pub stage: Stage,
    /// The stage's progress, 0..1.
    pub fraction: f64,
    /// Waiting for the worker to finish another track's.
    pub queued: bool,
    /// Asked to stop, and finishing the slice it was in the middle of.
    pub cancelling: bool,
}

/// How each job ends. (How far it's got is read with [`Stems::status`].)
#[derive(Clone, Debug)]
pub enum StemsEvent {
    /// The stems are in the cache (and on any deck with the track).
    Done {
        track: TrackId,
    },
    Failed {
        track: TrackId,
        error: String,
    },
    Cancelled {
        track: TrackId,
    },
}

pub type StemsSink = Arc<dyn Fn(StemsEvent) + Send + Sync>;

/// Each stem's waveform (all four, "other" included), as the decks have theirs.
pub struct StemWaves {
    /// `waveform::summarise` and `waveform::detail` per stem.
    pub waves: [(Arc<Vec<u8>>, Arc<Vec<u8>>); engine_stems::COUNT],
}

// ---- loading onto a deck -------------------------------------------------------------

/// Stems in memory, by audio hash, while any deck holds them: a second deck with the
/// same audio gets the same copy. Locked for the whole of a load, so two decks
/// loading the same stems at once decode them once.
type Loaded = HashMap<hash::Hash, (Weak<StemAudio>, Weak<StemWaves>)>;
static LOADED: Mutex<Option<Loaded>> = Mutex::new(None);

/// Give the track just put on `deck` (as `generation`) its stems, if they're cached:
/// loaded on a thread of its own, then sent to the engine. Quietly does nothing
/// without them.
pub fn attach(
    player: &Player,
    deck: DeckId,
    generation: u64,
    audio: Arc<TrackAudio>,
    at: Location,
) {
    let player = player.clone();
    spawn("earshot-stems-load", move || {
        let Ok(h) = hash::content_hash(&at) else {
            return;
        };
        let mut loaded = LOADED.lock().unwrap();
        let loaded = loaded.get_or_insert_with(HashMap::new);
        loaded.retain(|_, (s, w)| s.strong_count() > 0 && w.strong_count() > 0);
        let shared = loaded
            .get(&h)
            .and_then(|(s, w)| Some((s.upgrade()?, w.upgrade()?)));
        let (stems, waves) = match shared {
            Some(both) => both,
            None => {
                let Some(dir) = cached(&h) else { return };
                let _ = std::fs::write(dir.join(USED), b"");
                match load(&dir, &audio) {
                    Ok(both) => {
                        loaded.insert(h, (Arc::downgrade(&both.0), Arc::downgrade(&both.1)));
                        both
                    }
                    Err(e) => {
                        eprintln!("stems: {}: {e}", dir.display());
                        return;
                    }
                }
            }
        };
        give(&player, deck, generation, stems, waves);
    });
}

/// Decode a cache folder's stems, matched to `track`'s length, with every stem's
/// waveform.
fn load(dir: &Path, track: &TrackAudio) -> Result<(Arc<StemAudio>, Arc<StemWaves>), String> {
    let decoded = FILES
        .iter()
        .map(|f| {
            let path = stem_file(dir, f).ok_or_else(|| format!("no {f} stem"))?;
            decode::decode(&Location::File(path)).map(|(a, _)| a)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if decoded.iter().any(|a| a.sample_rate != track.sample_rate) {
        return Err("the stems' sample rate isn't the track's".into());
    }
    let stems = StemAudio::new([&decoded[0], &decoded[1], &decoded[2]], track.frames());
    drop(decoded);
    let waves = std::array::from_fn(|s| {
        let a = stems.stem(s, track);
        (
            Arc::new(waveform::summarise(&a)),
            Arc::new(waveform::detail(&a)),
        )
    });
    Ok((Arc::new(stems), Arc::new(StemWaves { waves })))
}

/// Hand stems to `deck` if it still has the load they were made for.
fn give(
    player: &Player,
    deck: DeckId,
    generation: u64,
    stems: Arc<StemAudio>,
    waves: Arc<StemWaves>,
) {
    let mut inner = player.lock().unwrap();
    if inner.session(deck).generation != generation {
        return;
    }
    let lock = super::memlock::MemLock::stems(&stems);
    let sent = inner.send(Command::SetStems {
        deck,
        generation,
        stems: Some(stems),
    });
    if sent.is_ok() {
        let session = inner.session_mut(deck);
        session.stem_waves = Some(waves);
        session.locks.push(lock);
    }
}

// ---- calculating -----------------------------------------------------------------------

/// The stems tool: its own background worker. Cheap to clone; commands never block.
#[derive(Clone)]
pub struct Stems {
    shared: Arc<Shared>,
}

struct Shared {
    worker: WorkerClient,
    store: Store,
    player: Player,
    /// The status line's list of background jobs.
    jobs: Jobs,
    sink: StemsSink,
    /// Each track's job, until it has ended.
    running: Mutex<HashMap<TrackId, Running>>,
}

struct Running {
    /// The worker's job, once submitted.
    job: Option<u64>,
    status: Status,
    /// Its line in the status line's jobs (gone when this is).
    listed: jobs::Job,
}

impl Stems {
    pub fn with_worker(
        config: WorkerConfig,
        store: Store,
        player: Player,
        jobs: Jobs,
        sink: StemsSink,
    ) -> Stems {
        Stems {
            shared: Arc::new(Shared {
                worker: WorkerClient::start(config),
                store,
                player,
                jobs,
                sink,
                running: Mutex::default(),
            }),
        }
    }

    pub fn run(&self, cmd: StemsCmd) {
        match cmd {
            StemsCmd::Calculate(track) => self.calculate(track),
            StemsCmd::Cancel(track) => self.shared.cancel(track),
        }
    }

    /// `track`'s job, if it has one.
    pub fn status(&self, track: TrackId) -> Option<Status> {
        let running = self.shared.running.lock().unwrap();
        running.get(&track).map(|r| r.status)
    }

    fn calculate(&self, track: TrackId) {
        let shared = self.shared.clone();
        if outside::is_outside(track) {
            (shared.sink)(StemsEvent::Failed {
                track,
                error: "library tracks only".into(),
            });
            return;
        }
        {
            let mut running = shared.running.lock().unwrap();
            if running.contains_key(&track) {
                return;
            }
            let title = shared
                .store
                .reader
                .track(track)
                .map(|t| t.title)
                .unwrap_or_default();
            let listed = shared.jobs.queue(format!("Stems for {title}"));
            let weak = Arc::downgrade(&shared);
            listed.on_cancel(move || {
                if let Some(s) = weak.upgrade() {
                    s.cancel(track);
                }
            });
            running.insert(
                track,
                Running {
                    job: None,
                    status: Status {
                        stage: Stage::Separate,
                        fraction: 0.0,
                        queued: true,
                        cancelling: false,
                    },
                    listed,
                },
            );
        }
        spawn("earshot-stems", move || {
            let result = shared.separate(track);
            shared.finish(track, result);
        });
    }
}

impl Shared {
    /// Stop `track`'s job: it's shown as cancelling until the worker has stopped.
    fn cancel(&self, track: TrackId) {
        let mut running = self.running.lock().unwrap();
        if let Some(r) = running.get_mut(&track)
            && !r.status.cancelling
        {
            r.status.cancelling = true;
            r.listed.cancelling();
            if let Some(job) = r.job {
                self.worker.cancel(job);
            }
        }
    }

    fn live(&self, track: TrackId) -> bool {
        let running = self.running.lock().unwrap();
        running.get(&track).is_some_and(|r| !r.status.cancelling)
    }

    fn progress(&self, track: TrackId, stage: Stage, fraction: f64) {
        let mut running = self.running.lock().unwrap();
        let Some(r) = running.get_mut(&track).filter(|r| !r.status.cancelling) else {
            return;
        };
        let fraction = fraction.clamp(0.0, 1.0);
        r.status.stage = stage;
        r.status.fraction = fraction;
        r.listed.say(match stage {
            Stage::Download => "downloading model",
            Stage::Separate => "separating",
        });
        r.listed.progress((fraction * 1000.0) as u64, 1000);
    }

    /// The worker has started on it.
    fn started(&self, track: TrackId) {
        let mut running = self.running.lock().unwrap();
        if let Some(r) = running.get_mut(&track) {
            r.status.queued = false;
            r.listed.running();
        }
    }

    fn separate(&self, track: TrackId) -> Result<(), String> {
        let cache = cache_dir().ok_or("no stems folder")?;
        let at = self
            .store
            .reader
            .track_location(track)
            .map_err(|e| format!("track {track}: {e}"))?;
        let h = hash::content_hash(&at).map_err(|e| e.to_string())?;
        if cached(&h).is_none() {
            // The track's own folder (it has one job at most), renamed into place
            // once it's all there.
            let part = cache.join(format!("{track}.part"));
            let _ = std::fs::remove_dir_all(&part);
            let result = self.separate_into(track, &at, &part, &cache.join(hash::to_hex(&h)));
            let _ = std::fs::remove_dir_all(&part);
            result?;
        }
        self.attach_everywhere(track, &at);
        Ok(())
    }

    /// Separate the track at `at` into `part`, then move that to `dir`.
    fn separate_into(
        &self,
        track: TrackId,
        at: &Location,
        part: &Path,
        dir: &Path,
    ) -> Result<(), String> {
        let (audio, _) = decode::decode(at)?;
        // Stems as good as the track: lossless for a lossless one.
        let codec = match decode::is_lossless(at) {
            Ok(true) => "flac",
            _ => "vorbis",
        };
        if !self.live(track) {
            return Err(CANCELLED.into());
        }
        let wav = part.join("source.wav");
        std::fs::create_dir_all(part)
            .and_then(|_| write_stereo_wav(&wav, &audio))
            .map_err(|e| format!("{}: {e}", wav.display()))?;
        let duration = audio.duration();
        drop(audio);
        self.progress(track, Stage::Separate, DECODED);

        let job = {
            let mut running = self.running.lock().unwrap();
            let Some(r) = running.get_mut(&track).filter(|r| !r.status.cancelling) else {
                return Err(CANCELLED.into());
            };
            let req = json!({
                "type": "stems",
                "wav": wav,
                // Asked for by the user: ahead of background analysis.
                "priority": JobPriority::Deck.wire(),
                "out_dir": part,
                "codec": codec,
            });
            let job = self.worker.submit_job(
                req.as_object().unwrap().clone(),
                worker_client::deadline_for(duration),
            );
            r.job = Some(job.id());
            job
        };
        let mut error = None;
        let result = job.wait_events(&mut |e| match e {
            JobEvent::Update(Update::Started) => self.started(track),
            JobEvent::Update(Update::Progress { analyser, fraction }) => {
                if analyser == "download" {
                    self.progress(track, Stage::Download, fraction)
                } else {
                    self.progress(track, Stage::Separate, DECODED + (1.0 - DECODED) * fraction)
                }
            }
            JobEvent::Failed { error: e, .. } => error = Some(e),
            _ => {}
        });
        let _ = std::fs::remove_file(&wav);
        result.map_err(|e| error.clone().unwrap_or(e))?;
        if let Some(e) = error {
            return Err(e);
        }
        let _ = std::fs::remove_dir_all(dir);
        std::fs::rename(part, dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let size: u64 = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|f| f.metadata().ok())
            .map(|m| m.len())
            .sum();
        USAGE.fetch_add(size, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// Every player with `track` on it gets its stems.
    fn attach_everywhere(&self, track: TrackId, at: &Location) {
        let inner = self.player.lock().unwrap();
        let on: Vec<_> = session::players(super::player::DECKS)
            .filter_map(|d| {
                let s = inner.session(d);
                (s.track_id == Some(track)).then(|| Some((d, s.generation, s.audio.clone()?)))?
            })
            .collect();
        drop(inner);
        for (deck, generation, audio) in on {
            attach(&self.player, deck, generation, audio, at.clone());
        }
    }

    /// `track`'s job has ended: it leaves the list, and says how it ended.
    fn finish(&self, track: TrackId, result: Result<(), String>) {
        let ended = self.running.lock().unwrap().remove(&track);
        let cancelled = ended.is_some_and(|r| r.status.cancelling);
        (self.sink)(match result {
            Ok(()) => StemsEvent::Done { track },
            Err(_) if cancelled => StemsEvent::Cancelled { track },
            Err(e) if e == CANCELLED => StemsEvent::Cancelled { track },
            Err(error) => StemsEvent::Failed { track, error },
        });
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    super::background::spawn(name, f);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stems folder of `bytes`, last used `age` seconds ago.
    fn folder(root: &Path, name: &str, bytes: usize, age: u64) {
        let d = root.join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("drums.ogg"), vec![0u8; bytes]).unwrap();
        let used = d.join(USED);
        std::fs::write(&used, b"").unwrap();
        let t = std::time::SystemTime::now() - std::time::Duration::from_secs(age);
        std::fs::File::options()
            .write(true)
            .open(&used)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    #[test]
    fn tidying_removes_half_written_then_least_recently_used() {
        let root = std::env::temp_dir().join(format!("earshot-stems-tidy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        folder(&root, "old", 1000, 300);
        folder(&root, "middle", 1000, 200);
        folder(&root, "new", 1000, 100);
        folder(&root, "7.part", 5000, 0);
        // Room for two (each is its stem plus an empty marker).
        let left = tidy(&root, 2500);
        assert_eq!(left, 2000);
        assert!(!root.join("7.part").exists());
        assert!(!root.join("old").exists());
        assert!(root.join("middle").exists() && root.join("new").exists());
        // Within the limit: nothing more goes.
        assert_eq!(tidy(&root, 2500), 2000);
        let _ = std::fs::remove_dir_all(&root);
    }
}
