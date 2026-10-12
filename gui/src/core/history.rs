//! Play history for this session: which tracks have been played, which deck started most
//! recently, and what the suggestions are measured from. A session is one run of the
//! app; the plays themselves are saved (the `plays` table) as [`History::tick`]
//! reports them, but "played" means played in this run.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use earshot_engine::DeckId;

use super::index::LibraryIndex;
use super::keys::Camelot;
use super::suggest::{self, Direction, Reference, Scored};
use super::track::{Track, TrackId};

/// A track counts as played once it has been playing this long.
pub const PLAYED_AFTER: Duration = Duration::from_secs(30);

/// A deck as the history sees it this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DeckNow {
    pub track: Option<TrackId>,
    pub playing: bool,
    /// The BPM it's playing at, if it has a grid.
    pub bpm: Option<f64>,
    /// How far the pitch has moved the key: 1.0 with keylock on.
    pub key_speed: f64,
}

/// Where the suggestions' reference track comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RefSource {
    /// Whichever deck is playing (the one started most recently, if both are).
    #[default]
    Master,
    /// What's loaded on a deck, playing or not.
    Deck(usize),
    /// The track selected in the focused list (or the selected Flow card).
    Selected,
}

/// What suggestions are measured from: a track as it's playing now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RefTrack {
    pub id: TrackId,
    /// Effective BPM: what the deck is actually playing at.
    pub bpm: Option<f64>,
    /// Effective key: shifted by the tempo fader unless the deck has keylock on.
    pub key: Option<Camelot>,
    pub energy: f32,
    /// The deck it's on, if it came from one.
    pub deck: Option<usize>,
    pub playing: bool,
}

impl RefTrack {
    /// A track as it is in the library (not on a deck).
    pub fn of(t: &Track) -> RefTrack {
        RefTrack {
            id: t.id,
            bpm: t.bpm,
            key: t.key,
            energy: t.energy_or_mid(),
            deck: None,
            playing: false,
        }
    }

    pub fn reference<'a>(
        &self,
        direction: Direction,
        played: &'a HashSet<TrackId>,
        exclude: &'a HashSet<TrackId>,
    ) -> Reference<'a> {
        Reference {
            id: self.id,
            bpm: self.bpm,
            key: self.key,
            energy: self.energy,
            direction,
            played,
            exclude,
        }
    }
}

/// What [`History::tick`] saw happen, for saving.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PlayEvent {
    /// A track counted as played on a deck (by index), playing since `since`.
    Counted {
        track: TrackId,
        deck: usize,
        since: Instant,
    },
    /// That deck's counted play ended: it stopped, or another track went on it.
    Ended { deck: usize },
}

/// A deck playing: since when, what, and whether it has counted as played yet.
#[derive(Clone, Copy, Debug)]
struct DeckPlay {
    since: Instant,
    track: Option<TrackId>,
    counted: bool,
}

#[derive(Default)]
pub struct History {
    played: HashSet<TrackId>,
    /// In the order they counted as played.
    history: Vec<TrackId>,
    /// The deck that most recently started playing (the live reference).
    last_started: Option<usize>,
    /// Each deck's play under way.
    playing: Vec<Option<DeckPlay>>,
    rev: u64,
}

impl History {
    /// Bumped when the played set changes.
    pub fn rev(&self) -> u64 {
        self.rev
    }

    pub fn was_played(&self, id: TrackId) -> bool {
        self.played.contains(&id)
    }

    pub fn mark_played(&mut self, id: TrackId) -> bool {
        if !self.played.insert(id) {
            return false;
        }
        self.history.push(id);
        self.rev += 1;
        true
    }

    /// Note which deck started playing last, and count a track as played once its
    /// deck has been playing it for [`PLAYED_AFTER`]. Returns the plays that counted
    /// and the counted plays that ended (each play counts, even of a track played
    /// before).
    pub fn tick(&mut self, now: Instant, decks: &[DeckNow]) -> Vec<PlayEvent> {
        self.playing.resize(decks.len(), None);
        let mut events = vec![];
        for (d, deck) in decks.iter().enumerate() {
            // Stopped, or another track on it: that play is over.
            if let Some(p) = self.playing[d]
                && (!deck.playing || p.track != deck.track)
            {
                if p.counted {
                    events.push(PlayEvent::Ended { deck: d });
                }
                self.playing[d] = None;
            }
            if !deck.playing {
                continue;
            }
            let p = self.playing[d].get_or_insert_with(|| {
                self.last_started = Some(d);
                DeckPlay {
                    since: now,
                    track: deck.track,
                    counted: false,
                }
            });
            if let Some(track) = p.track
                && !p.counted
                && now.duration_since(p.since) >= PLAYED_AFTER
            {
                p.counted = true;
                let since = p.since;
                self.mark_played(track);
                events.push(PlayEvent::Counted {
                    track,
                    deck: d,
                    since,
                });
            }
        }
        events
    }

    /// The reference track `source` asks for: a deck's track at its actual tempo and
    /// key, or `selected` as it is in the library. `prepare` is Prepare's player, in
    /// Prepare mode: while it plays, it's the master (its `deck` is
    /// `DeckId::PREPARE.index()`).
    pub fn reference(
        &self,
        lib: &LibraryIndex,
        source: RefSource,
        decks: &[DeckNow],
        prepare: Option<&DeckNow>,
        selected: Option<TrackId>,
    ) -> Option<RefTrack> {
        let as_ref = |deck: &DeckNow, d: usize| {
            let t = lib.get(deck.track?)?;
            Some(RefTrack {
                bpm: deck.bpm.or(t.bpm),
                key: suggest::effective_key(t.key, deck.key_speed),
                deck: Some(d),
                playing: deck.playing,
                ..RefTrack::of(t)
            })
        };
        let on_deck = |d: usize| as_ref(decks.get(d)?, d);
        match source {
            RefSource::Master => prepare
                .and_then(|p| as_ref(p, DeckId::PREPARE.index()))
                .into_iter()
                .chain(
                    self.last_started
                        .into_iter()
                        .chain(0..decks.len())
                        .filter_map(on_deck),
                )
                .find(|r| r.playing),
            RefSource::Deck(d) => on_deck(d),
            RefSource::Selected => lib.get(selected?).map(RefTrack::of),
        }
    }

    /// The best `n` tracks to play after `r`, leaving out `exclude` (e.g. the tracks
    /// already in a Flow). Tracks played this session score lower.
    pub fn suggest(
        &self,
        lib: &LibraryIndex,
        r: &RefTrack,
        direction: Direction,
        exclude: &HashSet<TrackId>,
        n: usize,
    ) -> Vec<Scored> {
        let r = r.reference(direction, &self.played, exclude);
        suggest::suggest(&r, &lib.candidates(), n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::keys::parse_key;
    use crate::core::track::lib_track;

    fn lib() -> LibraryIndex {
        let mut ix = LibraryIndex::default();
        let mut a = lib_track(1, "A", "x");
        a.key = Some("8A".into());
        a.tag_bpm = Some(124.0);
        let mut b = lib_track(2, "B", "x");
        b.key = Some("8A".into());
        b.tag_bpm = Some(124.0);
        let mut c = lib_track(3, "C", "x");
        c.key = Some("9A".into());
        c.tag_bpm = Some(125.0);
        ix.replace(vec![a, b, c]);
        ix
    }

    fn deck(track: i64, playing: bool) -> DeckNow {
        DeckNow {
            track: Some(track),
            playing,
            bpm: Some(126.0),
            key_speed: 1.0,
        }
    }

    /// The tracks that counted as played.
    fn counted(events: Vec<PlayEvent>) -> Vec<TrackId> {
        events
            .into_iter()
            .filter_map(|e| match e {
                PlayEvent::Counted { track, .. } => Some(track),
                PlayEvent::Ended { .. } => None,
            })
            .collect()
    }

    #[test]
    fn a_track_counts_as_played_after_thirty_seconds() {
        let mut s = History::default();
        let t0 = Instant::now();
        assert!(s.tick(t0, &[deck(1, true), deck(2, false)]).is_empty());
        assert_eq!(s.last_started, Some(0));
        assert!(
            s.tick(
                t0 + Duration::from_secs(29),
                &[deck(1, true), deck(2, false)]
            )
            .is_empty()
        );
        assert_eq!(
            s.tick(t0 + PLAYED_AFTER, &[deck(1, true), deck(2, true)]),
            vec![PlayEvent::Counted {
                track: 1,
                deck: 0,
                since: t0
            }]
        );
        assert_eq!(s.last_started, Some(1));
        assert!(s.was_played(1) && !s.was_played(2));
        // Once only.
        assert!(
            s.tick(t0 + PLAYED_AFTER * 2, &[deck(1, true), deck(2, false)])
                .is_empty()
        );
        assert_eq!(s.history, vec![1]);
        // Stopping ends the play and resets the clock.
        let t1 = t0 + PLAYED_AFTER * 3;
        assert_eq!(
            s.tick(t1, &[deck(1, false), deck(2, true)]),
            vec![PlayEvent::Ended { deck: 0 }]
        );
        assert!(
            s.tick(
                t1 + Duration::from_secs(10),
                &[deck(1, false), deck(2, true)]
            )
            .is_empty()
        );
    }

    /// Another track on a playing deck ends the play and starts a new one; playing a
    /// track again counts again (for the saved history), though it was played already.
    #[test]
    fn each_play_counts_and_ends() {
        let mut s = History::default();
        let t0 = Instant::now();
        s.tick(t0, &[deck(1, true)]);
        assert_eq!(
            counted(s.tick(t0 + PLAYED_AFTER, &[deck(1, true)])),
            vec![1]
        );
        let t1 = t0 + PLAYED_AFTER * 2;
        assert_eq!(
            s.tick(t1, &[deck(2, true)]),
            vec![PlayEvent::Ended { deck: 0 }]
        );
        assert!(s.tick(t1 + PLAYED_AFTER / 2, &[deck(2, true)]).is_empty());
        let t2 = t1 + PLAYED_AFTER;
        assert_eq!(counted(s.tick(t2, &[deck(2, true)])), vec![2]);
        s.tick(t2, &[deck(1, true)]);
        assert_eq!(
            counted(s.tick(t2 + PLAYED_AFTER, &[deck(1, true)])),
            vec![1]
        );
        assert_eq!(s.history, vec![1, 2]);
    }

    #[test]
    fn the_reference_follows_the_source() {
        let ix = lib();
        let mut s = History::default();
        let decks = [deck(1, false), deck(3, true)];
        s.tick(Instant::now(), &decks);
        let master = s
            .reference(&ix, RefSource::Master, &decks, None, None)
            .unwrap();
        assert_eq!(
            (master.id, master.deck, master.bpm),
            (3, Some(1), Some(126.0))
        );
        let a = s
            .reference(&ix, RefSource::Deck(0), &decks, None, None)
            .unwrap();
        assert_eq!((a.id, a.playing), (1, false));
        let sel = s
            .reference(&ix, RefSource::Selected, &decks, None, Some(2))
            .unwrap();
        assert_eq!((sel.id, sel.deck, sel.bpm), (2, None, Some(124.0)));
        assert!(
            s.reference(&ix, RefSource::Selected, &decks, None, None)
                .is_none()
        );
        // Nothing playing: no master.
        let idle = [deck(1, false), deck(3, false)];
        assert!(
            History::default()
                .reference(&ix, RefSource::Master, &idle, None, None)
                .is_none()
        );
        // Pitched up a semitone without keylock: the key moves.
        let up = [DeckNow {
            key_speed: 2f64.powf(1.0 / 12.0),
            ..deck(1, true)
        }];
        let r = s
            .reference(&ix, RefSource::Deck(0), &up, None, None)
            .unwrap();
        assert_eq!(r.key, parse_key("3A"));
    }

    /// The FEATURES.md bug: in Prepare, Master didn't see Prepare's player playing.
    #[test]
    fn in_prepare_the_master_is_the_player() {
        let ix = lib();
        let s = History::default();
        let idle = [deck(1, false), deck(3, false)];
        let player = deck(2, true);
        let m = s
            .reference(&ix, RefSource::Master, &idle, Some(&player), None)
            .unwrap();
        assert_eq!(
            (m.id, m.deck, m.playing),
            (2, Some(DeckId::PREPARE.index()), true)
        );
        // It goes first while it plays; paused, a live deck that plays is the master.
        let live = [deck(1, true), deck(3, false)];
        let m = s
            .reference(&ix, RefSource::Master, &live, Some(&player), None)
            .unwrap();
        assert_eq!(m.id, 2);
        let paused = deck(2, false);
        let m = s
            .reference(&ix, RefSource::Master, &live, Some(&paused), None)
            .unwrap();
        assert_eq!((m.id, m.deck), (1, Some(0)));
        assert!(
            s.reference(&ix, RefSource::Master, &idle, Some(&paused), None)
                .is_none()
        );
    }

    #[test]
    fn excluded_tracks_are_left_out_and_played_ones_score_lower() {
        let ix = lib();
        let mut s = History::default();
        let r = RefTrack::of(ix.get(1).unwrap());
        let ids = |v: Vec<Scored>| v.into_iter().map(|s| s.id).collect::<Vec<_>>();
        assert_eq!(
            ids(s.suggest(&ix, &r, Direction::Hold, &HashSet::new(), 9)),
            vec![2, 3]
        );
        s.mark_played(2);
        assert_eq!(
            ids(s.suggest(&ix, &r, Direction::Hold, &HashSet::new(), 9)),
            vec![3, 2]
        );
        assert_eq!(
            ids(s.suggest(&ix, &r, Direction::Hold, &HashSet::from([3]), 9)),
            vec![2]
        );
    }
}
