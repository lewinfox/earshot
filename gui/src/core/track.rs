//! The one canonical track: a library row with its metadata resolved, its effective
//! BPM and key, and a lowercase `search_text` worked out once. Everything outside the
//! Store (lists, suggestions, decks, queries) reads tracks as this.

use super::keys::{Camelot, parse_key};
use super::store::LibTrack;

pub use super::store::GridSource;

/// A track's id in the database. Stable across moves and renames.
pub type TrackId = i64;

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub id: TrackId,
    /// Where the track is now, for showing (lossy if the name isn't UTF-8).
    pub path: String,
    /// The file name when it has no title.
    pub title: String,
    pub artist: String,
    pub album: String,
    pub genre: String,
    pub comment: String,
    pub duration: Option<f64>,
    /// The BPM to use: the active grid's, or the tag's when there's no grid.
    pub bpm: Option<f64>,
    /// The BPM in the metadata (tag, or what the user typed).
    pub tag_bpm: Option<f64>,
    pub key: Option<Camelot>,
    /// Where the active grid came from; `None`: no grid (not analysed).
    pub grid_source: Option<GridSource>,
    /// Stand-in energy as measured; only its order means anything.
    pub energy_raw: Option<f64>,
    /// Energy ranked across the library, 0..1 (see `LibraryIndex`). `None` until
    /// the track has been measured.
    pub energy: Option<f32>,
    /// A static analysis found the tempo seems to drift.
    pub drifts: bool,
    /// 0-5 stars.
    pub rating: Option<u8>,
    pub colour: Option<u32>,
    pub tags: Vec<String>,
    /// Title, artist, album, genre, comment, file name, tags: lowercase, one per
    /// line (see [`TextField`]).
    pub search_text: Box<str>,
}

/// The lines of `search_text`, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextField {
    Title,
    Artist,
    Album,
    Genre,
    Comment,
    FileName,
    Tags,
}

impl Track {
    pub fn new(l: LibTrack) -> Track {
        let file = l.path.rsplit('/').next().unwrap_or(&l.path);
        let search_text = [
            l.title.as_str(),
            &l.artist,
            &l.album,
            &l.genre,
            &l.comment,
            file,
            &l.tags.join(" "),
        ]
        .map(|s| s.replace('\n', " ").to_lowercase())
        .join("\n")
        .into_boxed_str();
        Track {
            id: l.id,
            key: l.key.as_deref().and_then(parse_key),
            bpm: l.bpm.or(l.tag_bpm),
            tag_bpm: l.tag_bpm,
            grid_source: l.grid_source,
            energy_raw: l.energy,
            energy: None,
            drifts: l.drifts,
            rating: l.rating,
            colour: l.colour,
            duration: l.duration,
            search_text,
            title: l.title,
            artist: l.artist,
            album: l.album,
            genre: l.genre,
            comment: l.comment,
            tags: l.tags,
            path: l.path,
        }
    }

    /// Has a beatgrid: analysed, fixed by hand or imported.
    pub fn analysed(&self) -> bool {
        self.grid_source.is_some()
    }

    /// The grid follows the tempo (dynamic analysis).
    pub fn dynamic(&self) -> bool {
        self.grid_source == Some(GridSource::AnalysisDynamic)
    }

    /// Ranked energy, or the middle when it hasn't been measured (what the
    /// suggestion scorer uses).
    pub fn energy_or_mid(&self) -> f32 {
        self.energy.unwrap_or(0.5)
    }

    /// Energy on the 1-10 scale the chips show and queries use.
    pub fn energy_level(&self) -> Option<f64> {
        self.energy.map(|e| e as f64 * 9.0 + 1.0)
    }

    /// One line of `search_text`.
    pub fn text(&self, f: TextField) -> &str {
        self.search_text.split('\n').nth(f as usize).unwrap_or("")
    }
}

/// Where a track's analysis is, while it's queued or running, or after it failed;
/// what the analyser reports (`AnalysisEvent`). Kept beside the index (not in
/// [`Track`]): it's transient and comes from the analyser, not the Store.
#[derive(Clone, Debug, PartialEq)]
pub enum AnalysisState {
    Queued,
    Running { progress: f64, stage: &'static str },
    Failed(String),
}

#[cfg(test)]
pub(crate) fn lib_track(id: TrackId, title: &str, artist: &str) -> LibTrack {
    LibTrack {
        id,
        path: format!("/music/{title}.mp3"),
        title: title.into(),
        artist: artist.into(),
        album: String::new(),
        genre: String::new(),
        comment: String::new(),
        duration: Some(300.0),
        bpm: None,
        tag_bpm: None,
        key: None,
        grid_source: None,
        energy: None,
        drifts: false,
        rating: None,
        colour: None,
        tags: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_bpm_key_and_search_text() {
        let mut l = lib_track(1, "Deep Space", "Some\nOne");
        l.tag_bpm = Some(124.0);
        l.key = Some("Am".into());
        l.tags = vec!["warm".into(), "vocal".into()];
        let t = Track::new(l.clone());
        assert_eq!(t.bpm, Some(124.0), "the tag's BPM without a grid");
        assert_eq!(t.key, parse_key("8A"));
        assert!(!t.analysed());
        assert_eq!(t.text(TextField::Title), "deep space");
        assert_eq!(t.text(TextField::Artist), "some one");
        assert_eq!(t.text(TextField::FileName), "deep space.mp3");
        assert_eq!(t.text(TextField::Tags), "warm vocal");

        l.bpm = Some(125.0);
        l.grid_source = Some(GridSource::AnalysisDynamic);
        let t = Track::new(l);
        assert_eq!(t.bpm, Some(125.0), "the grid's BPM wins");
        assert!(t.analysed() && t.dynamic());
    }
}
