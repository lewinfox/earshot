//! Writing to other DJ programs' libraries and USB sticks: earshot's playlists,
//! crates and Flows, or another library (whole, or some of its playlists)
//! converted. cratelib writes them, through the worker's
//! `python -m earshot_worker.libraries write` (one process per export; see
//! worker/README.md, "Other libraries").
//!
//! A drive holds one DJ library. Writing Serato to a stick with a Rekordbox library
//! on it (or the other way round) needs [`Export::replace`]: the old library's
//! folder is renamed on the stick (`PIONEER.earshot-<time>`), and the audio files
//! stay where they are. Every write also keeps a backup of the files it replaces
//! (cratelib's `*.cratelib-<time>`).
//!
//! earshot's cues and grids go out on Rekordbox's timeline: the worker undoes the
//! decoder offset it applies on import (an MP3 starts earlier for us).

use std::collections::HashSet;
use std::path::PathBuf;

use earshot_engine::beatgrid::GridModel;
use earshot_engine::media::Location;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::collections::{Collection, CollectionId, CollectionKind, Collections, Section};
use super::drives::Drive;
use super::external::{Marker, NodeKind, NodePath, run_with};
use super::sources::{Source, SourceKind};
use super::store::{CueKind, Reader};
use super::track::TrackId;

/// What an export writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TargetKind {
    RekordboxUsb,
    SeratoUsb,
    /// Serato's library on this computer (the Serato source's).
    Serato,
    /// Mixxx's library on this computer (the Mixxx source's).
    Mixxx,
    RekordboxXml,
}

impl TargetKind {
    pub const ALL: [TargetKind; 5] = [
        TargetKind::RekordboxUsb,
        TargetKind::SeratoUsb,
        TargetKind::Serato,
        TargetKind::Mixxx,
        TargetKind::RekordboxXml,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            TargetKind::RekordboxUsb => "Rekordbox USB",
            TargetKind::SeratoUsb => "Serato USB",
            TargetKind::Serato => "Serato (this computer)",
            TargetKind::Mixxx => "Mixxx (this computer)",
            TargetKind::RekordboxXml => "Rekordbox XML",
        }
    }

    /// It goes on a drive (a USB stick).
    pub const fn on_drive(self) -> bool {
        matches!(self, TargetKind::RekordboxUsb | TargetKind::SeratoUsb)
    }

    /// cratelib's format for what it writes.
    pub const fn format(self) -> &'static str {
        match self {
            TargetKind::RekordboxUsb => "rekordbox_usb",
            TargetKind::SeratoUsb | TargetKind::Serato => "serato",
            TargetKind::Mixxx => "mixxx",
            TargetKind::RekordboxXml => "rekordbox_xml",
        }
    }

    /// The kind of library it makes on a stick.
    pub const fn stick_kind(self) -> Option<SourceKind> {
        match self {
            TargetKind::RekordboxUsb => Some(SourceKind::RekordboxUsb),
            TargetKind::SeratoUsb => Some(SourceKind::SeratoUsb),
            _ => None,
        }
    }

    /// What writes a stick library of `kind`.
    pub fn for_stick(kind: SourceKind) -> Option<TargetKind> {
        match kind {
            SourceKind::RekordboxUsb => Some(TargetKind::RekordboxUsb),
            SourceKind::SeratoUsb => Some(TargetKind::SeratoUsb),
            _ => None,
        }
    }

    /// The program on this computer whose library it updates.
    pub const fn program(self) -> Option<SourceKind> {
        match self {
            TargetKind::Serato => Some(SourceKind::Serato),
            TargetKind::Mixxx => Some(SourceKind::Mixxx),
            _ => None,
        }
    }

    /// What it writes on a stick that already holds `source`'s kind of library.
    pub fn converting(source: SourceKind) -> Option<TargetKind> {
        match source {
            SourceKind::RekordboxUsb => Some(TargetKind::SeratoUsb),
            SourceKind::SeratoUsb => Some(TargetKind::RekordboxUsb),
            _ => None,
        }
    }
}

/// Where an export goes.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub kind: TargetKind,
    /// The drive (USB kinds), the library's file or folder (Serato, Mixxx), or the
    /// XML file.
    pub path: PathBuf,
    /// Serato on this computer: what its stored paths are relative to.
    pub volume_root: Option<String>,
}

impl Target {
    fn json(&self) -> Value {
        let path = self.path.to_string_lossy();
        match self.kind {
            TargetKind::SeratoUsb => json!({
                "format": "serato",
                "path": self.path.join("_Serato_").to_string_lossy(),
                "volume_root": path,
            }),
            k => json!({ "format": k.format(), "path": path, "volume_root": self.volume_root }),
        }
    }
}

/// The other programs' libraries on `drive` that writing `kind` there would replace
/// (moved aside with [`Export::replace`]).
pub fn in_the_way(kind: TargetKind, drive: &Drive) -> Vec<SourceKind> {
    drive
        .libraries
        .iter()
        .copied()
        .filter(|&k| Some(k) != kind.stick_kind())
        .collect()
}

/// How a format is written (each matters to some targets only: [`ExportOptions::applies`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExportOptions {
    /// Sticks: copy tracks that aren't on the stick onto it.
    pub copy_missing: bool,
    /// Rekordbox USB: measure waveforms (slower; else placeholders).
    pub waveforms: bool,
    /// Rekordbox USB: convert formats players can't read (Ogg, Opus …) to MP3.
    pub transcode: bool,
    /// Rekordbox USB: a `rekordbox.xml` at the stick's root, for rekordbox on a computer.
    pub xml: bool,
    /// Serato: write cues and grids into the audio files, where Serato keeps them.
    pub file_tags: bool,
    /// Mixxx: replace the cues and grids of tracks it already has.
    pub overwrite: bool,
}

impl ExportOptions {
    /// The defaults for a target. Serato's cues are only in the audio files, so a
    /// stick gets them there; the files on this computer are left alone unless asked.
    pub fn for_kind(kind: TargetKind) -> ExportOptions {
        ExportOptions {
            copy_missing: true,
            waveforms: true,
            transcode: true,
            xml: true,
            file_tags: kind == TargetKind::SeratoUsb,
            overwrite: true,
        }
    }

    fn json(&self) -> Value {
        json!({
            "copy_missing": self.copy_missing,
            "waveforms": self.waveforms,
            "transcode": self.transcode,
            "xml": self.xml,
            "file_tags": self.file_tags,
            "overwrite": self.overwrite,
        })
    }
}

/// What an export takes.
#[derive(Clone, Debug)]
pub enum From {
    /// earshot's collections, as [`picked`] lays them out.
    Earshot(Vec<Picked>),
    /// Another library: everything (`nodes: None`) or these playlists and folders.
    Library {
        source: Source,
        nodes: Option<Vec<NodePath>>,
    },
}

/// One export, as the Export tab asks for it.
#[derive(Clone, Debug)]
pub struct Export {
    pub from: From,
    pub target: Target,
    pub options: ExportOptions,
    /// Move another program's library on the target drive aside.
    pub replace: bool,
    /// Add to the library there instead of replacing it (cratelib's `merge`).
    pub merge: bool,
}

/// How an export went.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Written {
    pub files: Vec<PathBuf>,
    pub warnings: Vec<String>,
    pub tracks: usize,
    pub playlists: usize,
    /// Where the libraries moved aside went.
    pub moved: Vec<PathBuf>,
    /// Tracks in the library written (with what was there, when adding to it).
    #[serde(default)]
    pub on_target: usize,
    /// earshot's tracks left out: no file on this computer.
    #[serde(default)]
    pub skipped: usize,
}

// ---- earshot's side -----------------------------------------------------------------

/// A collection to export, with the folders it's in.
#[derive(Clone, Debug, PartialEq)]
pub struct Picked {
    pub name: String,
    pub kind: NodeKind,
    pub children: Vec<Picked>,
    pub tracks: Members,
}

/// A playlist's or crate's tracks.
#[derive(Clone, Debug, PartialEq)]
pub enum Members {
    Listed(Vec<TrackId>),
    /// A smart playlist's, found when the export runs.
    Smart(CollectionId),
}

/// `picked` (any of earshot's collections) as trees, each in the folders it's in:
/// a folder brings everything in it, and empty folders are left out. Smart
/// playlists and Flows become playlists.
pub fn picked(cs: &Collections, picked: &[CollectionId]) -> Vec<Picked> {
    let wanted = |c: &Collection| {
        picked
            .iter()
            .any(|&p| cs.is_within(c.id, p) || cs.is_within(p, c.id))
    };
    fn walk(
        cs: &Collections,
        section: Section,
        parent: Option<CollectionId>,
        wanted: &dyn Fn(&Collection) -> bool,
    ) -> Vec<Picked> {
        let mut out = vec![];
        for c in cs.children_in(section, parent) {
            if !wanted(c) {
                continue;
            }
            let (kind, tracks) = match c.kind {
                CollectionKind::Folder => {
                    let children = walk(cs, section, Some(c.id), wanted);
                    if !children.is_empty() {
                        out.push(Picked {
                            name: c.name.clone(),
                            kind: NodeKind::Folder,
                            children,
                            tracks: Members::Listed(vec![]),
                        });
                    }
                    continue;
                }
                CollectionKind::Smart => (NodeKind::Playlist, Members::Smart(c.id)),
                CollectionKind::Crate => (NodeKind::Crate, Members::Listed(c.tracks.clone())),
                CollectionKind::Playlist | CollectionKind::Flow => {
                    (NodeKind::Playlist, Members::Listed(c.tracks.clone()))
                }
            };
            out.push(Picked {
                name: c.name.clone(),
                kind,
                children: vec![],
                tracks,
            });
        }
        out
    }
    Section::ALL
        .into_iter()
        .flat_map(|s| walk(cs, s, None, &wanted))
        .collect()
}

/// A grid as tempo markers: a steady grid is one, from its first beat in the
/// track; markers are one per change of tempo (a beat more than 1 ms from where the
/// marker before puts it starts a new one). The inverse of `external::grid_of`.
pub fn markers_of(model: &GridModel) -> Vec<Marker> {
    match model {
        GridModel::Constant(c) => {
            let period = c.period();
            let k = (c.anchor / period).floor();
            let meter = i64::from(c.meter.max(1));
            let beat = (i64::from(c.anchor_pos.max(1)) - 1 - k as i64).rem_euclid(meter) + 1;
            vec![Marker {
                start_s: c.anchor - k * period,
                bpm: c.bpm,
                beat: beat as u8,
            }]
        }
        GridModel::Markers { times, positions } => {
            let mut out: Vec<(usize, Marker)> = vec![];
            for i in 0..times.len().saturating_sub(1) {
                let bpm = 60.0 / (times[i + 1] - times[i]);
                if let Some((at, m)) = out.last() {
                    let period = 60.0 / m.bpm;
                    let fits =
                        |j: usize| (m.start_s + (j - at) as f64 * period - times[j]).abs() < 0.001;
                    if fits(i) && fits(i + 1) {
                        continue;
                    }
                }
                let beat = match positions.get(i) {
                    Some(&p) if (1..=4).contains(&p) => p,
                    _ => (i % 4) as u8 + 1,
                };
                out.push((
                    i,
                    Marker {
                        start_s: times[i],
                        bpm,
                        beat,
                    },
                ));
            }
            out.into_iter().map(|(_, m)| m).collect()
        }
    }
}

#[derive(Serialize)]
struct OutNode {
    name: String,
    kind: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    children: Vec<OutNode>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tracks: Vec<TrackId>,
}

#[derive(Serialize)]
struct OutCue {
    role: &'static str,
    start_s: f64,
    end_s: Option<f64>,
    slot: Option<u8>,
    name: String,
    colour: Option<u32>,
}

#[derive(Serialize)]
struct OutTrack {
    id: TrackId,
    file: PathBuf,
    title: String,
    artist: String,
    album: String,
    genre: String,
    comment: String,
    bpm: Option<f64>,
    key: Option<String>,
    rating: u8,
    colour: Option<u32>,
    duration_s: Option<f64>,
    cues: Vec<OutCue>,
    grid: Vec<Marker>,
}

/// The playlists' tree with every member listed, and each track once, in order.
fn resolve(store: &Reader, picked: &[Picked]) -> Result<(Vec<OutNode>, Vec<TrackId>), String> {
    fn walk(
        store: &Reader,
        p: &Picked,
        seen: &mut HashSet<TrackId>,
        order: &mut Vec<TrackId>,
    ) -> Result<OutNode, String> {
        let tracks = match &p.tracks {
            Members::Listed(t) => t.clone(),
            Members::Smart(id) => store.collection_tracks(*id).map_err(|e| e.to_string())?,
        };
        order.extend(tracks.iter().filter(|t| seen.insert(**t)));
        Ok(OutNode {
            name: p.name.clone(),
            kind: p.kind.as_str(),
            children: p
                .children
                .iter()
                .map(|c| walk(store, c, seen, order))
                .collect::<Result<_, _>>()?,
            tracks,
        })
    }
    let (mut seen, mut order) = (HashSet::new(), vec![]);
    let nodes = picked
        .iter()
        .map(|p| walk(store, p, &mut seen, &mut order))
        .collect::<Result<_, _>>()?;
    Ok((nodes, order))
}

/// A track as the worker takes it; `None` if its audio isn't a file here.
fn out_track(store: &Reader, id: TrackId) -> Result<Option<OutTrack>, String> {
    let e = |e: super::store::StoreError| e.to_string();
    let Location::File(file) = store.track_location(id).map_err(e)? else {
        return Ok(None);
    };
    let t = store.track(id).map_err(e)?;
    let grid = store
        .grid(id)
        .map_err(e)?
        .map(|(m, _)| markers_of(&m))
        .unwrap_or_default();
    let cues = store
        .all_cues(id)
        .map_err(e)?
        .into_iter()
        .map(|c| OutCue {
            role: if c.end.is_some() { "loop" } else { "cue" },
            start_s: c.start,
            end_s: c.end,
            slot: if c.kind == CueKind::Hot { c.slot } else { None },
            name: c.label.unwrap_or_default(),
            colour: c.color,
        })
        .collect();
    Ok(Some(OutTrack {
        id,
        file,
        title: t.title,
        artist: t.artist,
        album: t.album,
        genre: t.genre,
        comment: t.comment,
        bpm: t.bpm.or(t.tag_bpm),
        key: t.key,
        rating: t.rating.unwrap_or(0),
        colour: t.colour,
        duration_s: t.duration,
        cues,
        grid,
    }))
}

/// The worker's spec for `x`. `roots`: where to look for another library's files.
fn spec(store: &Reader, x: &Export, roots: &[PathBuf]) -> Result<(Value, usize), String> {
    let (from, skipped) = match &x.from {
        From::Earshot(picked) => {
            let (nodes, order) = resolve(store, picked)?;
            let mut tracks = vec![];
            for &id in &order {
                tracks.extend(out_track(store, id)?);
            }
            let skipped = order.len() - tracks.len();
            (
                json!({ "earshot": { "tracks": tracks, "playlists": nodes } }),
                skipped,
            )
        }
        From::Library { source, nodes } => {
            let Some(path) = &source.location else {
                return Err(format!("{}: where it is isn't set", source.name));
            };
            (
                json!({
                    "source": {
                        "format": source.kind.format(),
                        "path": path.to_string_lossy(),
                        "volume_root": source.config.volume_root,
                        "paths": source.config.paths,
                    },
                    "playlists": nodes,
                }),
                0,
            )
        }
    };
    let spec = json!({
        "from": from,
        "target": x.target.json(),
        "options": x.options.json(),
        "replace": x.replace,
        "merge": x.merge,
        "roots": roots,
    });
    Ok((spec, skipped))
}

/// Run an export. Blocking: call from a background thread.
pub fn run(
    store: &Reader,
    x: &Export,
    roots: &[PathBuf],
    progress: impl FnMut(String),
) -> Result<Written, String> {
    #[derive(Deserialize)]
    struct Reply {
        written: Written,
    }
    let (spec, skipped) = spec(store, x, roots)?;
    let mut w = run_with::<Reply>(vec!["write".into()], Some(spec.to_string()), progress)?.written;
    w.skipped = skipped;
    Ok(w)
}

/// What the Export tab asks for. Run by the UI (`EarshotApp::export_cmd`).
#[derive(Debug, Clone)]
pub enum ExportCmd {
    /// Ask for a folder to write a stick's library to (a picker).
    PickFolder,
    /// Ask where to save a Rekordbox XML (a picker).
    PickXml,
    Run(Box<Export>),
    /// Open the Export tab set to convert this stick's library to the other format.
    Convert(Source),
}

/// What came of an [`ExportCmd`].
#[derive(Debug)]
pub enum ExportEvent {
    /// A folder or file picked for the target.
    Picked(PathBuf),
    Progress(String),
    Done(Result<Written, String>),
}

#[cfg(test)]
mod tests {
    use earshot_engine::beatgrid::Constant;

    use super::*;
    use crate::core::external::grid_of;

    fn coll(id: i64, parent: Option<i64>, kind: CollectionKind, name: &str) -> Collection {
        Collection {
            id,
            parent,
            kind,
            section: kind.section().unwrap_or(Section::Playlists),
            name: name.into(),
            tracks: vec![id * 10],
            query: None,
            source_flow: None,
            linear: false,
        }
    }

    fn collections() -> Collections {
        let mut cs = Collections::default();
        cs.set(vec![
            coll(1, None, CollectionKind::Folder, "Sets"),
            coll(2, Some(1), CollectionKind::Playlist, "Friday"),
            coll(3, Some(1), CollectionKind::Playlist, "Saturday"),
            coll(4, Some(1), CollectionKind::Folder, "Empty"),
            coll(5, None, CollectionKind::Crate, "Digging"),
            coll(6, None, CollectionKind::Smart, "Recent"),
        ]);
        cs
    }

    #[test]
    fn a_playlist_comes_in_its_folders() {
        let p = picked(&collections(), &[3]);
        let [sets] = &p[..] else { panic!("{p:?}") };
        assert_eq!((sets.name.as_str(), sets.kind), ("Sets", NodeKind::Folder));
        assert_eq!(sets.children.len(), 1);
        assert_eq!(sets.children[0].name, "Saturday");
        assert_eq!(sets.children[0].tracks, Members::Listed(vec![30]));
    }

    #[test]
    fn a_folder_brings_everything_in_it_but_empty_folders() {
        let p = picked(&collections(), &[1, 2, 5, 6]);
        let names: Vec<_> = p.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["Sets", "Recent", "Digging"]);
        let sub: Vec<_> = p[0].children.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(sub, ["Friday", "Saturday"]);
        assert_eq!(p[1].tracks, Members::Smart(6));
        assert_eq!(p[2].kind, NodeKind::Crate);
    }

    #[test]
    fn a_steady_grid_is_one_marker_from_its_first_beat() {
        let m = markers_of(&GridModel::Constant(Constant {
            bpm: 120.0,
            anchor: 10.25,
            anchor_pos: 1,
            meter: 4,
        }));
        // 20 beats (0.5 s each) back from 10.25 is 0.25, and 20 is 5 bars: a 1.
        assert_eq!(
            m,
            [Marker {
                start_s: 0.25,
                bpm: 120.0,
                beat: 1
            }]
        );
        let m = markers_of(&GridModel::Constant(Constant {
            bpm: 120.0,
            anchor: 0.75,
            anchor_pos: 1,
            meter: 4,
        }));
        assert_eq!((m[0].start_s, m[0].beat), (0.25, 4));
    }

    #[test]
    fn markers_go_out_and_come_back() {
        let mk = |start_s, bpm, beat| Marker { start_s, bpm, beat };
        let given = [mk(0.0, 120.0, 1), mk(2.0, 60.0, 1), mk(6.0, 90.0, 1)];
        let grid = grid_of(&given, 8.0).unwrap();
        let back = markers_of(&grid.model());
        assert_eq!(back.len(), 3, "{back:?}");
        for (a, b) in back.iter().zip(&given) {
            assert!((a.start_s - b.start_s).abs() < 1e-9 && (a.bpm - b.bpm).abs() < 1e-6);
            assert_eq!(a.beat, b.beat);
        }
    }
}
