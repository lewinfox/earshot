//! The library in memory: every [`Track`] under the library folders, by id, kept up
//! to date from the Store's events. Lists, suggestions and decks all read it.

use std::collections::HashMap;

use super::query::{self, Query};
use super::store::LibTrack;
use super::suggest::Candidate;
use super::track::{AnalysisState, Status, Track, TrackId};

#[derive(Default)]
pub struct LibraryIndex {
    /// In path order (as the Store lists them).
    tracks: Vec<Track>,
    by_id: HashMap<TrackId, usize>,
    /// Tracks queued, being analysed, or whose analysis failed.
    analysis: HashMap<TrackId, AnalysisState>,
    /// Bumped whenever track data changes, so views built from it know to refresh.
    rev: u64,
}

impl LibraryIndex {
    /// The whole library (`StoreEvent::Library`, after a scan). Analysis states are
    /// kept for tracks still there.
    pub fn replace(&mut self, tracks: Vec<LibTrack>) {
        self.tracks = tracks.into_iter().map(Track::new).collect();
        self.by_id = self
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.id, i))
            .collect();
        self.analysis.retain(|id, _| self.by_id.contains_key(id));
        self.rank_energy();
        self.rev += 1;
    }

    /// Some tracks changed (`StoreEvent::TracksChanged`: analysis, a grid edit,
    /// metadata). Tracks the index doesn't hold (outside the library folders) are
    /// ignored. Energy is ranked again, since one new measurement moves the others.
    pub fn patch(&mut self, tracks: Vec<LibTrack>) {
        let mut changed = false;
        for l in tracks {
            if let Some(&i) = self.by_id.get(&l.id) {
                self.tracks[i] = Track::new(l);
                changed = true;
            }
        }
        if changed {
            self.rank_energy();
            self.rev += 1;
        }
    }

    /// Rank the measured energies across the library, 0..1, so the scorer and the
    /// chips see an even spread whatever the raw numbers are.
    fn rank_energy(&mut self) {
        let mut known: Vec<(f64, usize)> = self
            .tracks
            .iter()
            .enumerate()
            .filter_map(|(i, t)| t.energy_raw.map(|e| (e, i)))
            .collect();
        known.sort_by(|a, b| a.0.total_cmp(&b.0));
        let n = known.len();
        for (rank, (_, i)) in known.into_iter().enumerate() {
            self.tracks[i].energy = Some(if n > 1 {
                rank as f32 / (n - 1) as f32
            } else {
                0.5
            });
        }
    }

    pub fn rev(&self) -> u64 {
        self.rev
    }

    pub fn get(&self, id: TrackId) -> Option<&Track> {
        self.by_id.get(&id).map(|&i| &self.tracks[i])
    }

    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    /// From an analyser event: `None` once it's done.
    pub fn set_analysis(&mut self, id: TrackId, state: Option<AnalysisState>) {
        match state {
            Some(s) => self.analysis.insert(id, s),
            None => self.analysis.remove(&id),
        };
    }

    pub fn analysis(&self, id: TrackId) -> Option<&AnalysisState> {
        self.analysis.get(&id)
    }

    /// Every track, as the suggestion scorer sees it.
    pub fn candidates(&self) -> Vec<Candidate> {
        self.tracks
            .iter()
            .map(|t| Candidate {
                id: t.id,
                bpm: t.bpm,
                key: t.key,
                energy: t.energy_or_mid(),
            })
            .collect()
    }

    /// A track's analysis in one word (the status column).
    pub fn status(&self, t: &Track) -> Status {
        Status::of(t, self.analysis(t.id))
    }

    /// `cx` (collections), with this index's analysis queue for `status:`.
    pub fn context<'a>(&'a self, cx: &'a dyn query::Context) -> impl query::Context + 'a {
        WithAnalysis { ix: self, cx }
    }

    /// The tracks that match, from `within` (in its order, repeats kept) or the whole
    /// library, sorted by the query's sort (stable, so ties keep that order).
    pub fn query(
        &self,
        q: &Query,
        cx: &dyn query::Context,
        within: Option<&[TrackId]>,
    ) -> Vec<TrackId> {
        let cx = self.context(cx);
        let keep = |t: &&Track| query::eval(&q.expr, t, &cx);
        let mut out: Vec<&Track> = match within {
            Some(ids) => ids
                .iter()
                .filter_map(|id| self.get(*id))
                .filter(keep)
                .collect(),
            None => self.tracks.iter().filter(keep).collect(),
        };
        if !q.sort.0.is_empty() {
            out.sort_by(|a, b| query::compare(&q.sort, a, b, &cx));
        }
        out.into_iter().map(|t| t.id).collect()
    }
}

/// Collections for `in:`, and the index's analysis queue for `status:`.
struct WithAnalysis<'a> {
    ix: &'a LibraryIndex,
    cx: &'a dyn query::Context,
}

impl query::Context for WithAnalysis<'_> {
    fn contains(&self, collection: i64, track: TrackId) -> bool {
        self.cx.contains(collection, track)
    }

    fn status(&self, t: &Track) -> Status {
        self.ix.status(t)
    }
}

/// User actions on library tracks. (Loading onto a given deck is
/// `DeckCmd::Load`, and previews `PreviewCmd`.)
#[derive(Clone, Debug, PartialEq)]
pub enum LibraryCmd {
    /// Onto Prepare's player in Prepare mode, else the first deck that isn't
    /// playing (a double-click).
    Load(TrackId),
    Analyse {
        tracks: Vec<TrackId>,
        dynamic: bool,
    },
    /// Ask for music folders to add (a folder picker), then scan them.
    PickRoots,
    /// Add music folders to the library and scan them.
    AddRoots(Vec<std::path::PathBuf>),
    /// Stop scanning a library folder.
    RemoveRoot(String),
    /// Look through the library folders again.
    Rescan,
    /// Show the track's file in the file manager.
    Reveal(TrackId),
    /// Find the tracks that share their audio (answered with `Event::Duplicates`).
    FindDuplicates,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::track::lib_track;

    fn lib(id: i64, title: &str, energy: Option<f64>) -> LibTrack {
        let mut l = lib_track(id, title, "x");
        l.energy = energy;
        l
    }

    #[test]
    fn patching_replaces_rows_and_reranks_energy() {
        let mut ix = LibraryIndex::default();
        ix.replace(vec![
            lib(1, "a", Some(3.0)),
            lib(2, "b", Some(1.0)),
            lib(3, "c", None),
        ]);
        let rev = ix.rev();
        assert_eq!(ix.get(1).unwrap().energy, Some(1.0));
        assert_eq!(ix.get(2).unwrap().energy, Some(0.0));
        assert_eq!(ix.get(3).unwrap().energy, None);
        assert_eq!(ix.get(3).unwrap().energy_or_mid(), 0.5);

        // Track 3 gets measured above both: the others move down.
        ix.patch(vec![
            lib(3, "c (new)", Some(9.0)),
            lib(99, "not here", None),
        ]);
        assert!(ix.rev() > rev);
        assert_eq!(ix.get(3).unwrap().title, "c (new)");
        assert_eq!(ix.get(3).unwrap().energy, Some(1.0));
        assert_eq!(ix.get(1).unwrap().energy, Some(0.5));
        assert!(ix.get(99).is_none(), "unknown tracks are ignored");
        assert_eq!(ix.tracks().len(), 3);

        // Nothing it holds: nothing changes.
        let rev = ix.rev();
        ix.patch(vec![lib(98, "x", None)]);
        assert_eq!(ix.rev(), rev);
    }

    #[test]
    fn analysis_state_survives_a_rescan_while_the_track_does() {
        let mut ix = LibraryIndex::default();
        ix.replace(vec![lib(1, "a", None), lib(2, "b", None)]);
        ix.set_analysis(1, Some(AnalysisState::Queued));
        ix.set_analysis(2, Some(AnalysisState::Failed("no".into())));
        ix.replace(vec![lib(1, "a", None)]);
        assert_eq!(ix.analysis(1), Some(&AnalysisState::Queued));
        assert_eq!(ix.analysis(2), None);
        ix.set_analysis(1, None);
        assert_eq!(ix.analysis(1), None);
    }

    #[test]
    fn query_filters_sorts_and_keeps_list_order() {
        let mut ix = LibraryIndex::default();
        ix.replace(vec![
            lib(1, "b deep", None),
            lib(2, "a deep", None),
            lib(3, "c", None),
        ]);
        let q = query::parse("deep").unwrap();
        assert_eq!(ix.query(&q, &(), None), vec![1, 2]);
        let q = query::parse("deep sort:title").unwrap();
        assert_eq!(ix.query(&q, &(), None), vec![2, 1]);
        // A playlist's order and repeats; ids not in the library are skipped.
        let q = query::parse("").unwrap();
        assert_eq!(ix.query(&q, &(), Some(&[3, 1, 3, 42])), vec![3, 1, 3]);
    }

    /// Three tracks, every field different, in the order 2, 3, 1 (low to high).
    fn every_field() -> LibraryIndex {
        use crate::core::track::GridSource;
        let mut ix = LibraryIndex::default();
        let row = |id: i64, rank: usize| {
            let w = ["b", "c", "a"][rank];
            let mut l = lib_track(id, &format!("{w} title"), &format!("{w} artist"));
            l.path = format!("/music/{w}.mp3");
            l.album = format!("{w} album");
            l.genre = format!("{w} genre");
            l.comment = format!("{w} note");
            l.tags = vec![format!("{w}tag")];
            l.tag_bpm = Some([124.0, 126.0, 120.0][rank]);
            l.key = Some(["2A", "3A", "1A"][rank].into());
            l.energy = Some([2.0, 3.0, 1.0][rank]);
            l.rating = Some([3, 4, 2][rank]);
            l.colour = Some([0x20, 0x30, 0x10][rank]);
            l.duration = Some([200.0, 300.0, 100.0][rank]);
            l.grid_source = (rank != 2).then_some(GridSource::Analysis);
            l
        };
        // Track 1 lowest, then 2, then 3 by every field.
        ix.replace(vec![row(1, 2), row(2, 0), row(3, 1)]);
        // Status: 1 none, 2 done, 3 busy.
        ix.set_analysis(3, Some(AnalysisState::Queued));
        ix
    }

    #[test]
    fn every_column_sorts_and_filters() {
        let ix = every_field();
        let ids = |s: &str| ix.query(&query::parse(s).unwrap(), &(), None);
        for (field, filter) in [
            ("title", "title:c"),
            ("artist", "artist:c"),
            ("album", "album:c"),
            ("genre", "genre:\"c genre\""),
            ("comment", "comment:c"),
            ("path", "path:c.mp3"),
            ("tag", "tag:ctag"),
            ("bpm", "bpm>125"),
            ("key", "key:3A"),
            ("energy", "energy>9"),
            ("rating", "rating>=4"),
            ("colour", "colour=48"),
            ("time", "time>4:00"),
            ("status", "status:busy"),
        ] {
            assert_eq!(ids(&format!("sort:{field}")), vec![1, 2, 3], "sort:{field}");
            assert_eq!(
                ids(&format!("sort:-{field}")),
                vec![3, 2, 1],
                "sort:-{field}"
            );
            assert_eq!(ids(filter), vec![3], "{filter}");
        }
    }

    #[test]
    fn status_follows_the_analysis_queue() {
        let mut ix = every_field();
        let ids = |ix: &LibraryIndex, s: &str| ix.query(&query::parse(s).unwrap(), &(), None);
        assert_eq!(ids(&ix, "status:none"), vec![1]);
        assert_eq!(ids(&ix, "status:green"), vec![2]);
        assert_eq!(ids(&ix, "status:busy OR status:attention"), vec![3]);
        ix.set_analysis(3, Some(AnalysisState::Failed("no".into())));
        assert_eq!(ids(&ix, "status:attention"), vec![3]);
        ix.set_analysis(3, None);
        assert_eq!(ids(&ix, "status:done"), vec![2, 3]);
        assert!(ids(&ix, "status:attention").is_empty());
        assert!(query::parse("status:purple").is_err());
        assert!(query::parse("status>done").is_err());
    }
}
