//! Play history for this session: which tracks have been played, which deck started most
//! recently, and what the suggestions are measured from.

use std::collections::HashSet;
use std::time::{Duration, Instant};

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

#[derive(Default)]
pub struct History {
    played: HashSet<TrackId>,
    /// In the order they counted as played. (Not saved yet: the `plays` table
    /// will take it.)
    #[allow(dead_code)]
    history: Vec<TrackId>,
    /// The deck that most recently started playing (the live reference).
    last_started: Option<usize>,
    /// When each deck started playing.
    started: Vec<Option<Instant>>,
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
    /// deck has been playing for [`PLAYED_AFTER`]. Returns the newly played tracks.
    pub fn tick(&mut self, now: Instant, decks: &[DeckNow]) -> Vec<TrackId> {
        self.started.resize(decks.len(), None);
        let mut newly = vec![];
        for (d, deck) in decks.iter().enumerate() {
            match (deck.playing, self.started[d]) {
                (true, None) => {
                    self.started[d] = Some(now);
                    self.last_started = Some(d);
                }
                (false, Some(_)) => self.started[d] = None,
                (true, Some(t)) if now.duration_since(t) >= PLAYED_AFTER => {
                    if let Some(id) = deck.track
                        && self.mark_played(id)
                    {
                        newly.push(id);
                    }
                }
                _ => {}
            }
        }
        newly
    }

    /// The reference track `source` asks for: a deck's track at its actual tempo and
    /// key, or `selected` as it is in the library.
    pub fn reference(
        &self,
        lib: &LibraryIndex,
        source: RefSource,
        decks: &[DeckNow],
        selected: Option<TrackId>,
    ) -> Option<RefTrack> {
        let on_deck = |d: usize| {
            let deck = decks.get(d)?;
            let t = lib.get(deck.track?)?;
            Some(RefTrack {
                bpm: deck.bpm.or(t.bpm),
                key: suggest::effective_key(t.key, deck.key_speed),
                deck: Some(d),
                playing: deck.playing,
                ..RefTrack::of(t)
            })
        };
        match source {
            RefSource::Master => self
                .last_started
                .into_iter()
                .chain(0..decks.len())
                .filter_map(on_deck)
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
            vec![1]
        );
        assert_eq!(s.last_started, Some(1));
        assert!(s.was_played(1) && !s.was_played(2));
        // Once only.
        assert!(
            s.tick(t0 + PLAYED_AFTER * 2, &[deck(1, true), deck(2, false)])
                .is_empty()
        );
        assert_eq!(s.history, vec![1]);
        // Stopping resets the clock.
        let t1 = t0 + PLAYED_AFTER * 3;
        s.tick(t1, &[deck(1, false), deck(2, true)]);
        assert!(
            s.tick(
                t1 + Duration::from_secs(10),
                &[deck(1, false), deck(2, true)]
            )
            .is_empty()
        );
    }

    #[test]
    fn the_reference_follows_the_source() {
        let ix = lib();
        let mut s = History::default();
        let decks = [deck(1, false), deck(3, true)];
        s.tick(Instant::now(), &decks);
        let master = s.reference(&ix, RefSource::Master, &decks, None).unwrap();
        assert_eq!(
            (master.id, master.deck, master.bpm),
            (3, Some(1), Some(126.0))
        );
        let a = s.reference(&ix, RefSource::Deck(0), &decks, None).unwrap();
        assert_eq!((a.id, a.playing), (1, false));
        let sel = s
            .reference(&ix, RefSource::Selected, &decks, Some(2))
            .unwrap();
        assert_eq!((sel.id, sel.deck, sel.bpm), (2, None, Some(124.0)));
        assert!(
            s.reference(&ix, RefSource::Selected, &decks, None)
                .is_none()
        );
        // Nothing playing: no master.
        let idle = [deck(1, false), deck(3, false)];
        assert!(
            History::default()
                .reference(&ix, RefSource::Master, &idle, None)
                .is_none()
        );
        // Pitched up a semitone without keylock: the key moves.
        let up = [DeckNow {
            key_speed: 2f64.powf(1.0 / 12.0),
            ..deck(1, true)
        }];
        let r = s.reference(&ix, RefSource::Deck(0), &up, None).unwrap();
        assert_eq!(r.key, parse_key("3A"));
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
