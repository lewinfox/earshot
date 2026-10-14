//! Other DJ programs' libraries (Rekordbox, Serato, Mixxx, Rekordbox XML, USB
//! sticks), read through cratelib by the worker's `python -m earshot_worker.libraries`
//! (one short-lived process per read; see worker/README.md, "Other libraries"), and
//! importing from them.
//!
//! Importing (a playlist, crate, folder, track or the whole library) brings in
//! tracks whose files are on this machine (as tracks, linked in `external_links`),
//! and the playlists and crates, in a folder named after the library. Importing
//! again updates what an earlier import made (`collections.external`) instead of
//! making copies. What else comes is [`ImportOptions`]:
//! - Metadata, as source `import:<library>`. It fills fields earshot has no value
//!   for; with `overwrite`, it replaces earshot's (the user's edits included).
//! - The beat grid, as a new grid (source `import`) made the active one.
//! - Hot cues, into empty pads (`overwrite`: over set ones too).
//! - Memory cues and loops (Rekordbox's), unless the same one is already there.
//!
//! The worker hands cues and grids over on our own decoder's timeline (it applies
//! cratelib's decoder offsets: an MP3 starts earlier for us than for Rekordbox).

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use earshot_engine::beatgrid::{Beatgrid, Constant, GridModel};
use serde::{Deserialize, Serialize};

use super::collections::Section;
use super::store::ScannedFile;
use super::worker_client::worker_dir;

/// A library another program keeps.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Source {
    /// cratelib's `Format`: rekordbox_db, rekordbox_xml, rekordbox_usb, serato, mixxx.
    pub format: String,
    /// Its file or folder.
    pub path: String,
    /// Serato only: the drive its paths are relative to.
    pub volume_root: Option<String>,
    /// For showing: "Rekordbox", "Rekordbox USB (STICK)" …
    pub name: String,
}

/// The programs the "Other libraries" pane always lists: (cratelib format, name).
pub const PROGRAMS: [(&str, &str); 3] = [
    ("rekordbox_db", "Rekordbox"),
    ("serato", "Serato"),
    ("mixxx", "Mixxx"),
];

/// Whether a program's library is looked for in its usual place, or only where the
/// user says (Set location…). On Linux, Rekordbox and Serato only run under Wine or
/// in a container, where looking for them would be guessing.
pub fn found_by_itself(format: &str) -> bool {
    !cfg!(target_os = "linux") || format == "mixxx"
}

/// Path rules for one library: our folder → the same folder as its program stores
/// it (cratelib's `PathMap`).
pub type PathRules = BTreeMap<String, String>;

/// What's kept about other libraries, in the Store's settings
/// (`settings::EXTERNAL`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExternalSettings {
    /// Libraries the user pointed earshot at (Set location…, Add…).
    pub locations: Vec<Source>,
    /// Each library's path rules, by [`Source::key`], as last found.
    pub paths: HashMap<String, PathRules>,
}

impl ExternalSettings {
    /// Drop path rules whose folder of ours has gone (a drive unplugged for good, a
    /// folder renamed): the next read looks again. True if anything changed.
    pub fn revalidate(&mut self) -> bool {
        let mut changed = false;
        for rules in self.paths.values_mut() {
            let before = rules.len();
            rules.retain(|ours, _| Path::new(ours).is_dir());
            changed |= rules.len() != before;
        }
        let before = self.paths.len();
        self.paths.retain(|_, r| !r.is_empty());
        changed || self.paths.len() != before
    }
}

impl Source {
    /// The library's name in the database: `external_links.library`, and
    /// `track_meta.source` is `import:` and this.
    pub fn key(&self) -> String {
        format!("{}:{}", self.format, self.path)
    }
}

/// A track as the other program has it.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ExtTrack {
    /// Its id in that library.
    pub id: String,
    /// The path as the program stores it (maybe a Windows one).
    pub location: String,
    /// The file on this machine, if it was found.
    pub file: Option<PathBuf>,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub genre: String,
    pub comment: String,
    pub bpm: Option<f64>,
    /// Musical notation ("Am", "F#").
    pub key: Option<String>,
    /// 0-5 stars; 0 is none.
    pub rating: u8,
    /// 0xRRGGBB.
    pub colour: Option<u32>,
    pub duration_s: Option<f64>,
    /// Only when read for importing them ([`read`] with `cues`).
    #[serde(default)]
    pub cues: Vec<ExtCue>,
    /// Tempo markers, in order. Only when read with `cues`.
    #[serde(default)]
    pub grid: Vec<Marker>,
}

/// A cue point or loop, in seconds of our own decode.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ExtCue {
    /// cratelib's `CueRole`: cue, loop, main, intro, outro, fade_in, fade_out.
    pub role: String,
    pub start_s: f64,
    pub end_s: Option<f64>,
    /// The hot cue pad (0 = A); `None`: a memory cue or loop.
    pub slot: Option<u8>,
    pub name: String,
    /// 0xRRGGBB.
    pub colour: Option<u32>,
}

/// A beat from which `bpm` holds until the next marker. `beat` is its place in
/// the bar (1 = the downbeat); bars are 4/4.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct Marker {
    pub start_s: f64,
    pub bpm: f64,
    pub beat: u8,
}

/// A grid from tempo markers, over `0..=duration` seconds: one marker is a steady
/// grid; several give every beat (each marker's tempo up to the next). `None` if
/// there aren't two beats.
pub fn grid_of(markers: &[Marker], duration: f64) -> Option<Beatgrid> {
    let pos = |beat: u8, k: usize| ((beat.clamp(1, 4) as usize - 1 + k) % 4) as u8 + 1;
    match markers {
        [] => None,
        [m] => Beatgrid::from_model(
            GridModel::Constant(Constant {
                bpm: m.bpm,
                anchor: m.start_s,
                anchor_pos: m.beat.clamp(1, 4),
                meter: 4,
            }),
            duration,
        )
        .ok(),
        _ => {
            let (mut times, mut positions) = (vec![], vec![]);
            for (i, m) in markers.iter().enumerate() {
                let end = markers.get(i + 1).map_or(duration, |n| n.start_s);
                let period = 60.0 / m.bpm;
                let mut k = 0;
                // Stop short of the next marker: it's a beat of its own.
                while m.start_s + k as f64 * period < end - period / 4.0 {
                    times.push(m.start_s + k as f64 * period);
                    positions.push(pos(m.beat, k));
                    k += 1;
                }
            }
            Beatgrid::new(times, positions).ok()
        }
    }
}

/// Is this a library whose memory cues can be imported (Rekordbox's)?
pub fn has_memory_cues(format: &str) -> bool {
    format.starts_with("rekordbox")
}

impl ExtTrack {
    /// What it would put in `track_meta`, by field (empty values left out).
    pub fn meta(&self) -> Vec<(&'static str, String)> {
        let bpm = self.bpm.map(|b| format!("{b}"));
        [
            ("title", Some(&self.title)),
            ("artist", Some(&self.artist)),
            ("album", Some(&self.album)),
            ("genre", Some(&self.genre)),
            ("comment", Some(&self.comment)),
            ("bpm", bpm.as_ref()),
            ("key", self.key.as_ref()),
        ]
        .into_iter()
        .filter_map(|(f, v)| v.filter(|v| !v.trim().is_empty()).map(|v| (f, v.clone())))
        .collect()
    }

    /// For showing: "Artist - Title", or the file name.
    pub fn display(&self) -> String {
        match (self.artist.is_empty(), self.title.is_empty()) {
            (false, false) => format!("{} - {}", self.artist, self.title),
            (_, false) => self.title.clone(),
            _ => self
                .location
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&self.location)
                .to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeKind {
    Folder,
    /// Ordered, and a track may repeat.
    Playlist,
    /// Mixxx's and Serato's crates: a set.
    Crate,
}

impl NodeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            NodeKind::Folder => "folder",
            NodeKind::Playlist => "playlist",
            NodeKind::Crate => "crate",
        }
    }
}

/// A folder, playlist or crate.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Node {
    pub name: String,
    pub kind: NodeKind,
    /// A folder's.
    #[serde(default)]
    pub children: Vec<Node>,
    /// A playlist's or crate's, by [`ExtTrack::id`], in order.
    #[serde(default)]
    pub tracks: Vec<String>,
}

/// Where a node is in [`ExtLibrary::playlists`]: the index at each level.
pub type NodePath = Vec<usize>;

/// A whole library, as read.
#[derive(Debug, Deserialize)]
pub struct ExtLibrary {
    /// cratelib's description of where it came from.
    #[allow(dead_code)] // for the log
    pub source: String,
    pub tracks: Vec<ExtTrack>,
    pub playlists: Vec<Node>,
    pub warnings: Vec<String>,
    /// The path rules that found its files (those given, and any new ones).
    #[serde(default)]
    pub paths: PathRules,
    /// [`ExtTrack::id`] → index in `tracks`.
    #[serde(skip)]
    by_id: HashMap<String, usize>,
}

impl ExtLibrary {
    fn index(mut self) -> ExtLibrary {
        self.by_id = self
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.id.clone(), i))
            .collect();
        self
    }

    pub fn track(&self, id: &str) -> Option<&ExtTrack> {
        self.by_id.get(id).map(|&i| &self.tracks[i])
    }

    pub fn node(&self, path: &[usize]) -> Option<&Node> {
        let (first, rest) = path.split_first()?;
        let mut n = self.playlists.get(*first)?;
        for &i in rest {
            n = n.children.get(i)?;
        }
        Some(n)
    }

    /// The tracks a node holds (everything inside a folder), each once, in order;
    /// `None`: the whole library.
    pub fn tracks_in(&self, path: Option<&[usize]>) -> Vec<&ExtTrack> {
        let Some(path) = path else {
            return self.tracks.iter().collect();
        };
        let mut seen = std::collections::HashSet::new();
        let mut out = vec![];
        let mut stack: Vec<&Node> = self.node(path).into_iter().collect();
        while let Some(n) = stack.pop() {
            stack.extend(n.children.iter().rev());
            for id in &n.tracks {
                if seen.insert(id.as_str())
                    && let Some(t) = self.track(id)
                {
                    out.push(t);
                }
            }
        }
        out
    }
}

/// One line from `earshot_worker.libraries`.
#[derive(Deserialize)]
struct Line {
    progress: Option<String>,
    libraries: Option<Vec<Source>>,
    library: Option<ExtLibrary>,
    error: Option<String>,
}

/// Run `python -m earshot_worker.libraries <args>` and return its last line.
/// `progress` gets each progress message on the way.
fn run(args: Vec<OsString>, mut progress: impl FnMut(String)) -> Result<Line, String> {
    let dir = worker_dir();
    let python = dir.join(".venv/bin/python");
    if !python.exists() {
        return Err(format!(
            "the worker isn't set up: run {}/setup.sh",
            dir.canonicalize().unwrap_or(dir.clone()).display()
        ));
    }
    let mut child = std::process::Command::new(&python)
        .args(["-m", "earshot_worker.libraries"])
        .args(&args)
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("starting {}: {e}", python.display()))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let mut last = None;
    for line in BufReader::new(stdout).lines() {
        let line = line.map_err(|e| format!("reading from the worker: {e}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: Line = serde_json::from_str(&line)
            .map_err(|e| format!("the worker sent something unreadable: {e}"))?;
        match msg.progress {
            Some(p) => progress(p),
            None => last = Some(msg),
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    match last {
        Some(Line { error: Some(e), .. }) => Err(e),
        Some(l) => Ok(l),
        None => Err(format!("the library reader stopped ({status})")),
    }
}

/// The libraries in each program's usual place on this computer (`at: None`), or
/// in a file, folder or drive. Blocking: call from a background thread.
pub fn find(at: Option<&Path>) -> Result<Vec<Source>, String> {
    let mut args: Vec<OsString> = vec!["find".into()];
    args.extend(at.map(|p| p.as_os_str().to_owned()));
    run(args, |_| {})?
        .libraries
        .ok_or_else(|| "the worker sent no libraries".into())
}

/// Read a whole library. `roots` (earshot's music folders) are where to look for
/// files the program stores under other paths (it runs under Wine, or the library
/// came from another computer), starting from `paths`, the rules found last time.
/// `cues`: with
/// every track's cues and grid, which is slower (Serato keeps them in the audio
/// files, Rekordbox its grids in analysis files). Blocking: call from a background
/// thread.
pub fn read(
    source: &Source,
    roots: &[PathBuf],
    paths: &PathRules,
    cues: bool,
    progress: impl FnMut(String),
) -> Result<ExtLibrary, String> {
    let mut args: Vec<OsString> = vec![
        "read".into(),
        source.format.clone().into(),
        source.path.clone().into(),
    ];
    if let Some(v) = &source.volume_root {
        args.extend(["--volume-root".into(), v.into()]);
    }
    for r in roots {
        args.extend(["--root".into(), r.as_os_str().to_owned()]);
    }
    if cues {
        args.push("--cues".into());
    }
    if !paths.is_empty() {
        let rules = serde_json::to_string(paths).expect("rules serialise");
        args.extend(["--paths".into(), rules.into()]);
    }
    run(args, progress)?
        .library
        .map(ExtLibrary::index)
        .ok_or_else(|| "the worker sent no library".into())
}

// ---- importing ------------------------------------------------------------------------

/// What an import brings besides the tracks and playlists (chosen in the import
/// dialog).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImportOptions {
    pub grid: bool,
    pub hot_cues: bool,
    /// Rekordbox only ([`has_memory_cues`]).
    pub memory_cues: bool,
    /// Imported metadata and hot cues replace what earshot has, instead of only
    /// filling gaps.
    pub overwrite: bool,
}

impl Default for ImportOptions {
    fn default() -> Self {
        ImportOptions {
            grid: true,
            hot_cues: true,
            memory_cues: true,
            overwrite: false,
        }
    }
}

impl ImportOptions {
    /// The library has to be read again, with its cues and grids.
    pub fn needs_cues(&self) -> bool {
        self.grid || self.hot_cues || self.memory_cues
    }
}

/// What to import from a library.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Library,
    /// A folder (everything in it), playlist or crate.
    Node(NodePath),
    /// One track, by [`ExtTrack::id`] (no playlist comes with it).
    Track(String),
}

/// A track to bring in: its file is on this machine.
#[derive(Debug, Clone)]
pub struct ImportTrack {
    pub external_id: String,
    pub file: PathBuf,
    pub meta: Vec<(&'static str, String)>,
    /// 1-5 stars.
    pub rating: Option<u8>,
    pub colour: Option<u32>,
    /// Empty unless the options ask for the grid.
    pub grid: Vec<Marker>,
    /// Hot cues (points, by pad), if asked for.
    pub hot_cues: Vec<ExtCue>,
    /// Memory cues and loops, if asked for.
    pub memory_cues: Vec<ExtCue>,
}

/// A folder, playlist or crate to make (or update), in one section's tree.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportNode {
    pub name: String,
    pub kind: NodeKind,
    /// Who it is across imports: its section and the names down to it
    /// ([`external_key`]).
    pub external: String,
    pub children: Vec<ImportNode>,
    /// External track ids, in order.
    pub tracks: Vec<String>,
}

/// Everything one import writes, in one transaction (`Writer::import_external`).
#[derive(Debug)]
pub struct ImportSave {
    /// [`Source::key`].
    pub library: String,
    pub options: ImportOptions,
    /// Files new to earshot, or changed since it last looked, with their tags.
    pub files: Vec<ScannedFile>,
    pub tracks: Vec<ImportTrack>,
    /// Top-level folders, one per section used: [`Section::Playlists`] and
    /// [`Section::Crates`].
    pub roots: Vec<(Section, ImportNode)>,
}

impl ImportSave {
    /// How many collections it could make, so ids can be handed out first.
    pub fn nodes(&self) -> usize {
        fn count(n: &ImportNode) -> usize {
            1 + n.children.iter().map(count).sum::<usize>()
        }
        self.roots.iter().map(|(_, n)| count(n)).sum()
    }
}

/// What an import did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Imported {
    /// Tracks brought in (new to earshot, or linked again).
    pub tracks: usize,
    /// Tracks left out: their files aren't on this machine.
    pub missing: usize,
    /// Playlists and crates made or updated.
    pub collections: usize,
    /// Grids made active.
    pub grids: usize,
    /// Hot cues and memory cues saved.
    pub cues: usize,
}

/// A collection's `external`: the library, its section, the names down to it and
/// its kind (a folder and a playlist may share a name), joined by a character no
/// name has.
pub fn external_key(library: &str, section: Section, names: &[&str], kind: NodeKind) -> String {
    let mut k = format!("{library}\u{1f}{}", section.as_str());
    for n in names {
        k.push('\u{1f}');
        k.push_str(n);
    }
    k.push('\u{1f}');
    k.push_str(kind.as_str());
    k
}

/// What importing `target` brings in, except the files' tags (read later, off
/// this thread); and how many of its tracks are left out (no file here). A crate
/// inside a playlist folder becomes a playlist, and the other way round: a folder
/// only holds its own section's things. Folders above a node come too, so
/// importing more of the library later puts things beside it.
pub fn plan(
    source: &Source,
    lib: &ExtLibrary,
    target: &Target,
    options: ImportOptions,
) -> (ImportSave, usize) {
    let library = source.key();
    let tracks_in = match target {
        Target::Library => lib.tracks_in(None),
        Target::Node(path) => lib.tracks_in(Some(path)),
        Target::Track(id) => lib.track(id).into_iter().collect(),
    };
    let missing = tracks_in.iter().filter(|t| t.file.is_none()).count();
    let memory = options.memory_cues && has_memory_cues(&source.format);
    let tracks = tracks_in
        .iter()
        .filter_map(|t| {
            Some(ImportTrack {
                external_id: t.id.clone(),
                file: t.file.clone()?,
                meta: t.meta(),
                rating: (1..=5).contains(&t.rating).then_some(t.rating),
                colour: t.colour,
                grid: if options.grid { t.grid.clone() } else { vec![] },
                hot_cues: t
                    .cues
                    .iter()
                    .filter(|c| options.hot_cues && c.role == "cue" && c.slot.is_some())
                    .cloned()
                    .collect(),
                memory_cues: t
                    .cues
                    .iter()
                    .filter(|c| {
                        memory && matches!(c.role.as_str(), "cue" | "loop") && c.slot.is_none()
                    })
                    .cloned()
                    .collect(),
            })
        })
        .collect();

    // The nodes to bring: the whole tree, or the chosen one with its folders.
    let chosen: Vec<Node> = match target {
        Target::Library => lib.playlists.clone(),
        Target::Track(_) => vec![],
        Target::Node(path) => {
            let mut wrapped = lib.node(path).cloned();
            for depth in (1..path.len()).rev() {
                let folder = lib.node(&path[..depth]).expect("a folder above");
                wrapped = wrapped.map(|n| Node {
                    name: folder.name.clone(),
                    kind: NodeKind::Folder,
                    children: vec![n],
                    tracks: vec![],
                });
            }
            wrapped.into_iter().collect()
        }
    };
    let mut roots = vec![];
    for section in [Section::Playlists, Section::Crates] {
        let children: Vec<ImportNode> = chosen
            .iter()
            .filter(|n| node_section(n) == section)
            .map(|n| import_node(&library, section, n, &mut vec![]))
            .collect();
        if !children.is_empty() {
            let root = ImportNode {
                name: source.name.clone(),
                kind: NodeKind::Folder,
                external: external_key(&library, section, &[], NodeKind::Folder),
                children,
                tracks: vec![],
            };
            roots.push((section, root));
        }
    }
    let save = ImportSave {
        library,
        options,
        files: vec![],
        tracks,
        roots,
    };
    (save, missing)
}

/// Crates go in Crates; playlists, and folders with any playlist in them, in
/// Playlists.
fn node_section(n: &Node) -> Section {
    fn has_playlist(n: &Node) -> bool {
        n.kind == NodeKind::Playlist || n.children.iter().any(has_playlist)
    }
    match n.kind {
        NodeKind::Crate => Section::Crates,
        NodeKind::Playlist => Section::Playlists,
        NodeKind::Folder if has_playlist(n) || n.children.is_empty() => Section::Playlists,
        NodeKind::Folder => Section::Crates,
    }
}

fn import_node<'a>(
    library: &str,
    section: Section,
    n: &'a Node,
    names: &mut Vec<&'a str>,
) -> ImportNode {
    names.push(&n.name);
    let kind = match (n.kind, section) {
        (NodeKind::Folder, _) => NodeKind::Folder,
        (_, Section::Crates) => NodeKind::Crate,
        _ => NodeKind::Playlist,
    };
    let out = ImportNode {
        name: n.name.clone(),
        kind,
        external: external_key(library, section, names, kind),
        children: n
            .children
            .iter()
            .map(|c| import_node(library, section, c, names))
            .collect(),
        tracks: n.tracks.clone(),
    };
    names.pop();
    out
}

/// What the UI asks for. Run by the UI (`EarshotApp::external_cmd`), which keeps
/// the results in the library pane.
#[derive(Debug, Clone)]
pub enum ExternalCmd {
    /// Look for libraries in each program's usual place.
    Find,
    /// Ask for a library file, folder or drive to look in (a picker).
    Pick,
    /// Look for libraries in this file, folder or drive (Add…).
    Detect(PathBuf),
    /// Ask where this program's library is (a picker), and keep it.
    SetLocation {
        format: String,
    },
    /// Look for this program's library here.
    Locate {
        format: String,
        at: PathBuf,
    },
    /// Stop listing a library the user pointed earshot at.
    Forget(Source),
    Read(Source),
    /// Bring this in. `read`: the library as already read, if it was (it's read
    /// again when the options need cues and grids).
    Import {
        source: Source,
        target: Target,
        options: ImportOptions,
        read: Option<Arc<ExtLibrary>>,
    },
}

/// What came of an [`ExternalCmd`].
#[derive(Debug)]
pub enum ExternalEvent {
    /// The libraries found in the usual places, or (`at`) in a file or folder.
    Found {
        at: Option<PathBuf>,
        result: Result<Vec<Source>, String>,
    },
    /// Where the user says a program's library is: found there, or not.
    Located {
        format: String,
        result: Result<Source, String>,
    },
    /// What a read is doing ("Reading Mixxx tracks 120/900").
    Progress { source: Source, message: String },
    Read {
        source: Source,
        result: Result<Arc<ExtLibrary>, String>,
    },
    Imported {
        source: Source,
        result: Result<Imported, String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str, file: Option<&str>) -> ExtTrack {
        ExtTrack {
            id: id.into(),
            location: format!("C:/Music/{id}.mp3"),
            file: file.map(PathBuf::from),
            title: format!("T{id}"),
            artist: String::new(),
            album: String::new(),
            genre: String::new(),
            comment: " ".into(),
            bpm: Some(124.0),
            key: Some("Am".into()),
            rating: 0,
            colour: None,
            duration_s: None,
            cues: vec![
                cue("cue", 1.0, Some(0)),
                cue("cue", 2.0, None),
                cue("loop", 3.0, Some(1)),
                cue("main", 0.5, None),
            ],
            grid: vec![Marker {
                start_s: 0.1,
                bpm: 124.0,
                beat: 1,
            }],
        }
    }

    fn cue(role: &str, start_s: f64, slot: Option<u8>) -> ExtCue {
        ExtCue {
            role: role.into(),
            start_s,
            end_s: None,
            slot,
            name: String::new(),
            colour: None,
        }
    }

    fn leaf(name: &str, kind: NodeKind, tracks: &[&str]) -> Node {
        Node {
            name: name.into(),
            kind,
            children: vec![],
            tracks: tracks.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn library() -> ExtLibrary {
        ExtLibrary {
            source: "test".into(),
            tracks: vec![
                track("1", Some("/m/1.mp3")),
                track("2", None),
                track("3", Some("/m/3.mp3")),
            ],
            playlists: vec![
                Node {
                    name: "Sets".into(),
                    kind: NodeKind::Folder,
                    children: vec![
                        leaf("Friday", NodeKind::Playlist, &["3", "1", "3"]),
                        leaf("Saturday", NodeKind::Playlist, &["2"]),
                    ],
                    tracks: vec![],
                },
                leaf("Digging", NodeKind::Crate, &["1"]),
            ],
            warnings: vec![],
            paths: PathRules::new(),
            by_id: HashMap::new(),
        }
        .index()
    }

    fn source() -> Source {
        Source {
            format: "rekordbox_xml".into(),
            path: "/x/rb.xml".into(),
            volume_root: None,
            name: "Rekordbox XML (rb.xml)".into(),
        }
    }

    #[test]
    fn parses_what_the_worker_sends() {
        let line = r#"{"library": {"source": "s", "warnings": [],
            "tracks": [{"id": "rb:1", "location": "C:/a.mp3", "file": "/m/a.mp3", "title": "A",
              "artist": "X", "album": "", "genre": "", "comment": "", "bpm": 124.0, "key": "Am",
              "rating": 3, "colour": 16711680, "duration_s": null}],
            "playlists": [{"name": "Sets", "kind": "folder", "children": [
              {"name": "Friday", "kind": "playlist", "tracks": ["rb:1"]}]},
              {"name": "C", "kind": "crate", "tracks": []}]}}"#;
        let l: Line = serde_json::from_str(line).unwrap();
        let lib = l.library.unwrap().index();
        assert_eq!(lib.track("rb:1").unwrap().colour, Some(0xFF0000));
        assert_eq!(lib.node(&[0, 0]).unwrap().name, "Friday");
        assert_eq!(lib.node(&[1]).unwrap().kind, NodeKind::Crate);
        assert!(lib.node(&[0, 1]).is_none());
        let found: Line = serde_json::from_str(
            r#"{"libraries": [{"format": "serato", "path": "/m/_Serato_", "volume_root": "C:/", "name": "Serato"}]}"#,
        )
        .unwrap();
        assert_eq!(found.libraries.unwrap()[0].key(), "serato:/m/_Serato_");
    }

    #[test]
    fn a_folders_tracks_are_everything_in_it_once() {
        let lib = library();
        let ids = |p: Option<&[usize]>| -> Vec<String> {
            lib.tracks_in(p).iter().map(|t| t.id.clone()).collect()
        };
        assert_eq!(ids(Some(&[0])), ["3", "1", "2"]);
        assert_eq!(ids(Some(&[0, 0])), ["3", "1"]);
        assert_eq!(ids(None), ["1", "2", "3"]);
    }

    #[test]
    fn meta_leaves_out_empty_fields() {
        let m = track("1", None).meta();
        assert_eq!(
            m,
            [
                ("title", "T1".to_string()),
                ("bpm", "124".into()),
                ("key", "Am".into())
            ]
        );
    }

    #[test]
    fn importing_everything_splits_playlists_and_crates() {
        let (save, missing) = plan(
            &source(),
            &library(),
            &Target::Library,
            ImportOptions::default(),
        );
        assert_eq!(missing, 1, "track 2 has no file here");
        let ids: Vec<_> = save.tracks.iter().map(|t| t.external_id.as_str()).collect();
        assert_eq!(ids, ["1", "3"]);
        assert_eq!(save.roots.len(), 2);
        let (s, playlists) = &save.roots[0];
        assert_eq!(*s, Section::Playlists);
        assert_eq!(playlists.name, "Rekordbox XML (rb.xml)");
        assert_eq!(playlists.children[0].children[0].tracks, ["3", "1", "3"]);
        assert_eq!(
            playlists.children[0].children[0].external,
            external_key(
                "rekordbox_xml:/x/rb.xml",
                Section::Playlists,
                &["Sets", "Friday"],
                NodeKind::Playlist
            )
        );
        let (s, crates) = &save.roots[1];
        assert_eq!(*s, Section::Crates);
        assert_eq!(crates.children[0].kind, NodeKind::Crate);
        assert_eq!(save.nodes(), 6);
        // Rekordbox XML: everything the options ask for.
        let t = &save.tracks[0];
        assert_eq!(t.grid.len(), 1);
        assert_eq!(t.hot_cues.len(), 1, "a hot loop isn't a hot cue");
        assert_eq!(t.memory_cues.len(), 1, "the main cue isn't a memory cue");
    }

    #[test]
    fn options_and_formats_choose_what_comes() {
        let none = ImportOptions {
            grid: false,
            hot_cues: false,
            memory_cues: false,
            overwrite: false,
        };
        assert!(!none.needs_cues());
        let (save, _) = plan(&source(), &library(), &Target::Library, none);
        let t = &save.tracks[0];
        assert!(t.grid.is_empty() && t.hot_cues.is_empty() && t.memory_cues.is_empty());
        // Mixxx has no memory cues to bring.
        let mixxx = Source {
            format: "mixxx".into(),
            ..source()
        };
        let (save, _) = plan(
            &mixxx,
            &library(),
            &Target::Library,
            ImportOptions::default(),
        );
        assert!(save.tracks[0].memory_cues.is_empty());
        assert_eq!(save.tracks[0].hot_cues.len(), 1);
    }

    #[test]
    fn one_marker_is_a_steady_grid() {
        let m = |start_s, bpm, beat| Marker { start_s, bpm, beat };
        let g = grid_of(&[m(0.25, 120.0, 3)], 10.0).unwrap();
        let c = g.as_constant().unwrap();
        assert_eq!(
            (c.bpm, c.anchor, c.anchor_pos, c.meter),
            (120.0, 0.25, 3, 4)
        );
        assert!(grid_of(&[], 10.0).is_none());
    }

    #[test]
    fn several_markers_give_every_beat() {
        let m = |start_s, bpm, beat| Marker { start_s, bpm, beat };
        // 2 beats at 120 (0.5 s apart), then 60 bpm from 1.0 s to the end at 3.0 s.
        let g = grid_of(&[m(0.0, 120.0, 1), m(1.0, 60.0, 3)], 3.0).unwrap();
        assert_eq!(g.times(), [0.0, 0.5, 1.0, 2.0]);
        assert_eq!(g.positions(), [1, 2, 3, 4]);
        assert!(g.as_constant().is_none());
    }

    #[test]
    fn revalidating_drops_rules_whose_folder_has_gone() {
        let here = std::env::temp_dir().to_string_lossy().into_owned();
        let mut s = ExternalSettings::default();
        s.paths.insert(
            "a".into(),
            PathRules::from([
                (here.clone(), "C:/M".into()),
                ("/no/such/dir".into(), "D:/".into()),
            ]),
        );
        s.paths.insert(
            "b".into(),
            PathRules::from([("/no/such/dir".into(), "E:/".into())]),
        );
        assert!(s.revalidate());
        assert_eq!(s.paths.len(), 1);
        assert_eq!(s.paths["a"], PathRules::from([(here, "C:/M".into())]));
        assert!(!s.revalidate(), "nothing more to drop");
    }

    #[test]
    fn importing_a_track_makes_no_playlist() {
        let (save, missing) = plan(
            &source(),
            &library(),
            &Target::Track("3".into()),
            ImportOptions::default(),
        );
        assert_eq!(missing, 0);
        assert_eq!(save.tracks.len(), 1);
        assert!(save.roots.is_empty());
    }

    #[test]
    fn importing_one_playlist_brings_its_folders() {
        let (save, missing) = plan(
            &source(),
            &library(),
            &Target::Node(vec![0, 1]),
            ImportOptions::default(),
        );
        assert_eq!(missing, 1);
        assert!(save.tracks.is_empty());
        let [(Section::Playlists, root)] = &save.roots[..] else {
            panic!("{:?}", save.roots)
        };
        assert_eq!(root.children.len(), 1);
        let sets = &root.children[0];
        assert_eq!((sets.name.as_str(), sets.kind), ("Sets", NodeKind::Folder));
        assert_eq!(sets.children.len(), 1, "only the chosen playlist");
        assert_eq!(sets.children[0].name, "Saturday");
        assert_eq!(
            sets.children[0].external,
            external_key(
                "rekordbox_xml:/x/rb.xml",
                Section::Playlists,
                &["Sets", "Saturday"],
                NodeKind::Playlist
            )
        );
    }
}
