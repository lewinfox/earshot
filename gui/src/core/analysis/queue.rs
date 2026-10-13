//! The analysis job queue, keyed by (track, analyser). Only the bookkeeping lives
//! here (no threads, no I/O), so it can be tested on its own; `super::Analyser` wraps
//! it in a mutex and runs the lanes.
//!
//! - Asking again for something already queued merges the two requests: the more
//!   urgent priority wins, the stronger [`Want`] wins, and the mode comes with the
//!   stronger want (the newer one on a tie).
//! - A lane takes a whole track at once (every analyser waiting for it), so the file
//!   is decoded once. The deck lane takes only deck work; background lanes take the
//!   rest, and only when [`JobQueue::may_take`] lets them, so a lane never holds a job
//!   while it waits for a turn.
//! - A request that the running work can't satisfy *supersedes* it: a deck load of a
//!   track a background lane is analysing, or a forced re-analysis of a running track.
//!   Its entries go straight back to waiting (so the deck lane can take them at once),
//!   the lane is told to drop what it finds, and the caller cancels the lane's worker
//!   job.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use earshot_engine::decode::TrackAudio;

use super::Mode;
use super::registry::AnalyserId;
use crate::core::worker_client::JobPriority;

/// How much a request insists, weakest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Want {
    /// Only if there's no result at all (a grid: no grid of any kind).
    Missing,
    /// If the result is from an older analyser version or other audio.
    Stale,
    /// Whatever there is (the user asked).
    Force,
}

impl Want {
    pub const fn as_str(self) -> &'static str {
        match self {
            Want::Missing => "missing",
            Want::Stale => "stale",
            Want::Force => "force",
        }
    }

    pub fn parse(s: &str) -> Option<Want> {
        [Want::Missing, Want::Stale, Want::Force]
            .into_iter()
            .find(|w| w.as_str() == s)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Req {
    pub priority: JobPriority,
    pub want: Want,
    /// The grid mode (other analysers ignore it).
    pub mode: Mode,
}

impl Req {
    /// This request, asked for again as `new`.
    pub fn merge(self, new: Req) -> Req {
        Req {
            priority: self.priority.min(new.priority),
            want: self.want.max(new.want),
            mode: if new.want >= self.want {
                new.mode
            } else {
                self.mode
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lane {
    /// Tracks just loaded onto a deck, on the deck worker.
    Deck,
    /// Library work, on background worker `n`.
    Background(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Waiting,
    Running(Lane),
}

#[derive(Debug)]
struct Entry {
    req: Req,
    /// When it last became more urgent; orders the queue.
    seq: u64,
    state: State,
}

/// What a lane is running.
struct LaneJob {
    track: i64,
    /// The worker job, once submitted.
    worker_job: Option<u64>,
    superseded: Arc<AtomicBool>,
}

/// A track's work, as a lane takes it.
pub struct Batch {
    pub track: i64,
    /// In registry order.
    pub analysers: Vec<(AnalyserId, Req)>,
    /// Already decoded (a track just loaded onto a deck).
    pub audio: Option<Arc<TrackAudio>>,
    /// Set when a newer request took this work over: drop the results.
    pub superseded: Arc<AtomicBool>,
}

/// What a request did.
#[derive(Debug, Default, PartialEq)]
pub struct Requested {
    /// The track had nothing queued or running before.
    pub new_track: bool,
    /// Running work was superseded: the lane, and its worker job to cancel.
    pub superseded: Option<(Lane, Option<u64>)>,
}

/// Background tracks (waiting or running) per background worker before another one
/// starts: a big import ramps up to the cap, a handful of tracks uses one process.
pub const TRACKS_PER_WORKER: usize = 4;

pub struct JobQueue {
    entries: BTreeMap<(i64, AnalyserId), Entry>,
    audio: HashMap<i64, Arc<TrackAudio>>,
    lanes: HashMap<Lane, LaneJob>,
    seq: u64,
    /// A deck is playing: background work runs one track at a time.
    pub playing: bool,
    /// Background lanes allowed at once.
    pub background_cap: usize,
}

impl JobQueue {
    pub fn new(background_cap: usize) -> JobQueue {
        JobQueue {
            entries: BTreeMap::new(),
            audio: HashMap::new(),
            lanes: HashMap::new(),
            seq: 0,
            playing: false,
            background_cap: background_cap.max(1),
        }
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn track_entries(&self, track: i64) -> impl Iterator<Item = (&(i64, AnalyserId), &Entry)> {
        self.entries.range(
            (track, AnalyserId::ALL[0])..=(track, AnalyserId::ALL[AnalyserId::ALL.len() - 1]),
        )
    }

    /// Whether anything is queued or running for this track.
    pub fn has(&self, track: i64) -> bool {
        self.track_entries(track).next().is_some()
    }

    /// What's queued or running for a track, merged (for saving the queue).
    pub fn requests(&self, track: i64) -> Vec<(AnalyserId, Req)> {
        self.track_entries(track)
            .map(|(k, e)| (k.1, e.req))
            .collect()
    }

    /// Ask for analysers of a track. `audio`, if given, is used instead of decoding.
    pub fn request(
        &mut self,
        track: i64,
        wants: &[(AnalyserId, Req)],
        audio: Option<Arc<TrackAudio>>,
    ) -> Requested {
        let mut out = Requested {
            new_track: !self.has(track),
            superseded: None,
        };
        if let Some(a) = audio {
            self.audio.insert(track, a);
        }
        // A deck load can't wait for a background lane running this track.
        let mut supersede = self
            .lanes
            .iter()
            .find(|(l, j)| j.track == track && **l != Lane::Deck)
            .map(|(l, _)| *l)
            .filter(|_| wants.iter().any(|w| w.1.priority == JobPriority::Deck));
        for &(id, req) in wants {
            let seq = self.next_seq();
            let Some(e) = self.entries.get_mut(&(track, id)) else {
                self.entries.insert(
                    (track, id),
                    Entry {
                        req,
                        seq,
                        state: State::Waiting,
                    },
                );
                continue;
            };
            let merged = e.req.merge(req);
            // A forced run isn't satisfied by the run under way, unless that's the same.
            if let State::Running(lane) = e.state
                && req.want == Want::Force
                && (e.req.want != Want::Force || e.req.mode != req.mode)
            {
                supersede = Some(lane);
            }
            if merged.priority < e.req.priority {
                e.seq = seq;
            }
            e.req = merged;
        }
        if let Some(lane) = supersede {
            out.superseded = Some((lane, self.supersede(lane)));
        }
        out
    }

    /// Take a lane's work back: its entries wait again, and it's told to drop its
    /// results. Returns its worker job, to cancel.
    fn supersede(&mut self, lane: Lane) -> Option<u64> {
        let job = self.lanes.get(&lane)?;
        job.superseded.store(true, Ordering::Relaxed);
        let (track, worker_job) = (job.track, job.worker_job);
        let seq = self.next_seq();
        for (_, e) in self.entries.range_mut(
            (track, AnalyserId::ALL[0])..=(track, AnalyserId::ALL[AnalyserId::ALL.len() - 1]),
        ) {
            if e.state == State::Running(lane) {
                e.state = State::Waiting;
                e.seq = seq;
            }
        }
        worker_job
    }

    /// Background lanes wanted for the work there is: one per [`TRACKS_PER_WORKER`]
    /// tracks, up to the cap; one while a deck plays in Perform mode.
    pub fn background_wanted(&self) -> usize {
        if self.playing {
            return 1;
        }
        let mut tracks: Vec<i64> = self
            .entries
            .iter()
            .filter(|(_, e)| match e.state {
                State::Waiting => e.req.priority != JobPriority::Deck,
                State::Running(lane) => lane != Lane::Deck,
            })
            .map(|(k, _)| k.0)
            .collect();
        tracks.dedup(); // entries are sorted by track
        tracks
            .len()
            .div_ceil(TRACKS_PER_WORKER)
            .clamp(1, self.background_cap)
    }

    /// Whether this lane may start something now (whether or not there's work).
    pub fn may_take(&self, lane: Lane) -> bool {
        if self.lanes.contains_key(&lane) {
            return false;
        }
        match lane {
            Lane::Deck => true,
            Lane::Background(n) => {
                let wanted = self.background_wanted();
                let running = self.lanes.keys().filter(|l| **l != Lane::Deck).count();
                n < wanted && running < wanted
            }
        }
    }

    /// The next track for this lane, if it may run one now: every analyser waiting
    /// for that track, now marked as running on the lane.
    pub fn take(&mut self, lane: Lane) -> Option<Batch> {
        if !self.may_take(lane) {
            return None;
        }
        let busy: Vec<i64> = self
            .entries
            .iter()
            .filter(|(_, e)| matches!(e.state, State::Running(_)))
            .map(|(k, _)| k.0)
            .collect();
        // Per track: is any of it deck work, and its place in the queue. Deck work
        // goes newest first (the track just loaded), the rest first come first served.
        let mut best: Option<((JobPriority, u64), i64)> = None;
        let mut tracks: HashMap<i64, (bool, (JobPriority, u64))> = HashMap::new();
        for (&(track, _), e) in &self.entries {
            if e.state != State::Waiting || busy.contains(&track) {
                continue;
            }
            let order = match e.req.priority {
                JobPriority::Deck => u64::MAX - e.seq,
                _ => e.seq,
            };
            let key = (e.req.priority, order);
            let t = tracks.entry(track).or_insert((false, key));
            t.0 |= e.req.priority == JobPriority::Deck;
            t.1 = t.1.min(key);
        }
        for (track, (deck, key)) in tracks {
            if deck != (lane == Lane::Deck) {
                continue;
            }
            if best.is_none_or(|b| key < b.0) {
                best = Some((key, track));
            }
        }
        let (_, track) = best?;
        let mut analysers = vec![];
        for (k, e) in self.entries.range_mut(
            (track, AnalyserId::ALL[0])..=(track, AnalyserId::ALL[AnalyserId::ALL.len() - 1]),
        ) {
            if e.state == State::Waiting {
                e.state = State::Running(lane);
                analysers.push((k.1, e.req));
            }
        }
        let superseded = Arc::new(AtomicBool::new(false));
        self.lanes.insert(
            lane,
            LaneJob {
                track,
                worker_job: None,
                superseded: superseded.clone(),
            },
        );
        Some(Batch {
            track,
            analysers,
            audio: self.audio.get(&track).cloned(),
            superseded,
        })
    }

    /// The lane has sent its track to a worker as job `id`. False if the work was
    /// superseded meanwhile: the caller cancels the job itself.
    pub fn worker_job(&mut self, lane: Lane, id: u64) -> bool {
        match self.lanes.get_mut(&lane) {
            Some(j) => {
                j.worker_job = Some(id);
                !j.superseded.load(Ordering::Relaxed)
            }
            None => false,
        }
    }

    /// Drop all background work: what's waiting, and what's running (told to drop its
    /// results). A deck's track is kept. Returns the tracks dropped, and the running
    /// worker jobs to cancel.
    pub fn stop_background(&mut self) -> (Vec<i64>, Vec<(Lane, u64)>) {
        let mut cancel = vec![];
        for (&lane, job) in &self.lanes {
            if lane != Lane::Deck {
                job.superseded.store(true, Ordering::Relaxed);
                cancel.extend(job.worker_job.map(|j| (lane, j)));
            }
        }
        let mut dropped: Vec<i64> = vec![];
        self.entries.retain(|k, e| {
            let keep = match e.state {
                State::Waiting => e.req.priority == JobPriority::Deck,
                State::Running(lane) => lane == Lane::Deck,
            };
            if !keep {
                dropped.push(k.0);
            }
            keep
        });
        dropped.dedup(); // entries are sorted by track
        let entries = &self.entries;
        self.audio.retain(|t, _| entries.keys().any(|k| k.0 == *t));
        (dropped, cancel)
    }

    /// Drop everything for these tracks (removed from the library): what's waiting,
    /// and what's running, on any lane (told to drop its results). Returns the tracks
    /// that had something, and the running worker jobs to cancel.
    pub fn remove(&mut self, tracks: &[i64]) -> (Vec<i64>, Vec<(Lane, u64)>) {
        let mut cancel = vec![];
        for (&lane, job) in &self.lanes {
            if tracks.contains(&job.track) {
                job.superseded.store(true, Ordering::Relaxed);
                cancel.extend(job.worker_job.map(|j| (lane, j)));
            }
        }
        let mut dropped: Vec<i64> = vec![];
        self.entries.retain(|k, _| {
            let gone = tracks.contains(&k.0);
            if gone {
                dropped.push(k.0);
            }
            !gone
        });
        dropped.dedup(); // entries are sorted by track
        for t in tracks {
            self.audio.remove(t);
        }
        (dropped, cancel)
    }

    /// The lane is done with its track (finished, failed, or superseded). Returns the
    /// track, and whether nothing more is queued for it.
    pub fn finish(&mut self, lane: Lane) -> Option<(i64, bool)> {
        let job = self.lanes.remove(&lane)?;
        let track = job.track;
        // Superseded entries were already put back to wait; the rest are done.
        self.entries
            .retain(|k, e| k.0 != track || e.state != State::Running(lane));
        let idle = !self.has(track);
        if idle {
            self.audio.remove(&track);
        }
        Some((track, idle))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use AnalyserId::*;
    use JobPriority::*;

    fn req(priority: JobPriority, want: Want, mode: Mode) -> Req {
        Req {
            priority,
            want,
            mode,
        }
    }

    fn grid(priority: JobPriority, want: Want, mode: Mode) -> Vec<(AnalyserId, Req)> {
        vec![(Grid, req(priority, want, mode))]
    }

    #[test]
    fn repeat_requests_merge() {
        let mut q = JobQueue::new(4);
        assert!(
            q.request(1, &grid(Background, Want::Missing, Mode::Static), None)
                .new_track
        );
        // Asked again, more urgently and forced: one entry, upgraded.
        let r = q.request(1, &grid(Preview, Want::Force, Mode::Dynamic), None);
        assert_eq!(r, Requested::default());
        assert_eq!(q.requests(1), grid(Preview, Want::Force, Mode::Dynamic));
        // A weaker request later doesn't undo either, nor change the mode.
        q.request(1, &grid(Background, Want::Missing, Mode::Static), None);
        assert_eq!(q.requests(1), grid(Preview, Want::Force, Mode::Dynamic));
        // An equally strong one does change the mode: the newest wins.
        q.request(1, &grid(Background, Want::Force, Mode::Static), None);
        assert_eq!(q.requests(1), grid(Preview, Want::Force, Mode::Static));
    }

    #[test]
    fn a_lane_takes_every_analyser_of_a_track_at_once() {
        let mut q = JobQueue::new(4);
        let r = req(Background, Want::Missing, Mode::Static);
        q.request(1, &[(Waveform, r), (Grid, r)], None);
        q.request(2, &[(Grid, r)], None);
        q.request(1, &[(Loudness, r)], None);
        let b = q.take(Lane::Background(0)).unwrap();
        assert_eq!(b.track, 1);
        let ids: Vec<AnalyserId> = b.analysers.iter().map(|a| a.0).collect();
        assert_eq!(ids, [Waveform, Loudness, Grid]);
        assert_eq!(q.finish(Lane::Background(0)), Some((1, true)));
        assert_eq!(q.take(Lane::Background(0)).unwrap().track, 2);
    }

    #[test]
    fn stopping_drops_background_work_but_keeps_the_decks() {
        let mut q = JobQueue::new(4);
        for t in [1, 2, 3] {
            q.request(t, &grid(Background, Want::Missing, Mode::Static), None);
        }
        q.request(9, &grid(Deck, Want::Missing, Mode::Static), None);
        let running = q.take(Lane::Background(0)).unwrap();
        assert!(q.worker_job(Lane::Background(0), 5));
        let (dropped, cancel) = q.stop_background();
        assert_eq!(dropped, [1, 2, 3]);
        assert_eq!(cancel, [(Lane::Background(0), 5)]);
        assert!(running.superseded.load(Ordering::Relaxed));
        // The stopped lane finishes with nothing left for its track.
        assert_eq!(q.finish(Lane::Background(0)), Some((1, true)));
        assert!(q.take(Lane::Background(0)).is_none());
        assert_eq!(q.take(Lane::Deck).unwrap().track, 9);
    }

    /// A track removed from the library: its waiting work goes, its running work is
    /// cancelled, and other tracks are untouched.
    #[test]
    fn removing_tracks_drops_and_cancels_their_work() {
        let mut q = JobQueue::new(4);
        for t in [1, 2, 3] {
            q.request(t, &grid(Background, Want::Missing, Mode::Static), None);
        }
        q.request(9, &grid(Deck, Want::Missing, Mode::Static), None);
        let running = q.take(Lane::Background(0)).unwrap();
        assert_eq!(running.track, 1);
        assert!(q.worker_job(Lane::Background(0), 5));
        let deck = q.take(Lane::Deck).unwrap();
        assert_eq!(deck.track, 9);

        let (dropped, cancel) = q.remove(&[1, 3, 9, 42]);
        assert_eq!(dropped, [1, 3, 9]);
        // The deck lane hadn't sent its job yet: it learns when it does.
        assert_eq!(cancel, [(Lane::Background(0), 5)]);
        assert!(running.superseded.load(Ordering::Relaxed));
        assert!(deck.superseded.load(Ordering::Relaxed));
        assert!(!q.worker_job(Lane::Deck, 6));
        assert!(!q.has(1) && !q.has(3) && !q.has(9));
        assert_eq!(q.requests(2), grid(Background, Want::Missing, Mode::Static));
        // The lanes end with nothing left for their tracks, and 2 runs next.
        assert_eq!(q.finish(Lane::Background(0)), Some((1, true)));
        assert_eq!(q.finish(Lane::Deck), Some((9, true)));
        assert_eq!(q.take(Lane::Background(0)).unwrap().track, 2);
    }

    #[test]
    fn deck_work_goes_to_the_deck_lane_newest_first() {
        let mut q = JobQueue::new(4);
        q.request(1, &grid(Background, Want::Missing, Mode::Static), None);
        q.request(2, &grid(Deck, Want::Missing, Mode::Static), None);
        q.request(3, &grid(Deck, Want::Missing, Mode::Static), None);
        assert_eq!(q.take(Lane::Deck).unwrap().track, 3);
        assert_eq!(q.take(Lane::Background(0)).unwrap().track, 1);
        q.finish(Lane::Deck);
        assert_eq!(q.take(Lane::Deck).unwrap().track, 2);
        assert!(q.take(Lane::Background(0)).is_none()); // busy
    }

    /// Bug #2: a forced re-analysis of a track already queued or running was dropped.
    #[test]
    fn forced_reanalysis_of_a_queued_or_running_track_is_kept() {
        let mut q = JobQueue::new(4);
        // Queued: the force is merged in.
        q.request(1, &grid(Background, Want::Missing, Mode::Static), None);
        q.request(1, &grid(Background, Want::Force, Mode::Dynamic), None);
        let b = q.take(Lane::Background(0)).unwrap();
        assert_eq!(b.analysers, grid(Background, Want::Force, Mode::Dynamic));
        q.finish(Lane::Background(0));
        // Running a static analysis: "Analyse (dynamic)" supersedes it.
        q.request(2, &grid(Background, Want::Missing, Mode::Static), None);
        let b = q.take(Lane::Background(0)).unwrap();
        assert!(q.worker_job(Lane::Background(0), 77));
        let r = q.request(2, &grid(Background, Want::Force, Mode::Dynamic), None);
        assert_eq!(r.superseded, Some((Lane::Background(0), Some(77))));
        assert!(b.superseded.load(Ordering::Relaxed));
        // The old run ends (cancelled); the forced one is still queued, and runs.
        assert_eq!(q.finish(Lane::Background(0)), Some((2, false)));
        let b = q.take(Lane::Background(0)).unwrap();
        assert_eq!(b.analysers, grid(Background, Want::Force, Mode::Dynamic));
        assert!(!b.superseded.load(Ordering::Relaxed));
        // Asking for the same forced run again while it runs changes nothing.
        let r = q.request(2, &grid(Background, Want::Force, Mode::Dynamic), None);
        assert_eq!(r.superseded, None);
        assert_eq!(q.finish(Lane::Background(0)), Some((2, true)));
    }

    /// Bug #3: a background lane took a job before waiting for a slot, so a deck
    /// load of that track couldn't re-route it and waited behind throttled work.
    #[test]
    fn a_deck_load_takes_a_track_over_from_the_background() {
        let mut q = JobQueue::new(4);
        q.playing = true; // one background track at a time
        q.request(1, &grid(Background, Want::Missing, Mode::Static), None);
        q.request(2, &grid(Background, Want::Missing, Mode::Static), None);
        assert_eq!(q.take(Lane::Background(0)).unwrap().track, 1);
        // Track 2 isn't held by a lane waiting for a turn: it stays in the queue.
        assert!(!q.may_take(Lane::Background(1)));
        assert!(q.take(Lane::Background(1)).is_none());
        let r = q.request(2, &grid(Deck, Want::Missing, Mode::Static), None);
        assert_eq!(r.superseded, None);
        assert_eq!(q.take(Lane::Deck).unwrap().track, 2);
        q.finish(Lane::Deck);
        // And a track already running in the background moves to the deck lane at
        // once; the background run is told to stop.
        assert!(q.worker_job(Lane::Background(0), 5));
        let r = q.request(1, &grid(Deck, Want::Missing, Mode::Static), None);
        assert_eq!(r.superseded, Some((Lane::Background(0), Some(5))));
        let b = q.take(Lane::Deck).unwrap();
        assert_eq!((b.track, b.analysers[0].1.priority), (1, Deck));
        // The cancelled background run finishing doesn't drop the deck's work.
        assert_eq!(q.finish(Lane::Background(0)), Some((1, false)));
        assert_eq!(q.finish(Lane::Deck), Some((1, true)));
    }

    #[test]
    fn superseded_before_the_worker_job_is_sent() {
        let mut q = JobQueue::new(4);
        q.request(1, &grid(Background, Want::Missing, Mode::Static), None);
        q.take(Lane::Background(0)).unwrap();
        let r = q.request(1, &grid(Deck, Want::Missing, Mode::Static), None);
        assert_eq!(r.superseded, Some((Lane::Background(0), None)));
        // The lane learns when it registers its job, and cancels it itself.
        assert!(!q.worker_job(Lane::Background(0), 9));
    }

    #[test]
    fn background_lanes_ramp_up_with_the_backlog() {
        let mut q = JobQueue::new(3);
        let r = req(Background, Want::Missing, Mode::Static);
        for t in 0..2 {
            q.request(t, &[(Grid, r)], None);
        }
        assert_eq!(q.background_wanted(), 1);
        assert!(q.may_take(Lane::Background(0)) && !q.may_take(Lane::Background(1)));
        for t in 2..(TRACKS_PER_WORKER as i64 * 10) {
            q.request(t, &[(Grid, r)], None);
        }
        assert_eq!(q.background_wanted(), 3); // the cap
        assert!(q.take(Lane::Background(0)).is_some());
        assert!(q.take(Lane::Background(2)).is_some());
        assert!(q.take(Lane::Background(1)).is_some());
        assert!(!q.may_take(Lane::Background(3)));
        // While a deck plays, no more than one at a time.
        q.playing = true;
        q.finish(Lane::Background(1));
        assert!(!q.may_take(Lane::Background(0)) && !q.may_take(Lane::Background(1)));
    }
}
