//! Outside tracks: ones earshot plays without them being in its library, from
//! another DJ program's library on this computer or a USB stick. Each gets a
//! negative [`TrackId`] for the session, so decks, the track table, dragging and
//! menus treat it like any other track; what's known about it (file, tags, grid,
//! cues) is kept here, in memory, and nothing about it is saved: the Store drops
//! anything it's asked to save for a negative id ([`Outside::apply`] keeps the cue
//! edits a deck makes, until earshot quits).
//!
//! Waveforms and loudness are worked out on load. For a file on this computer they
//! go in the Store's file cache (kept); for a file on a stick, in [`Outside`]'s own
//! cache, for the session only, up to [`WAVES`] tracks.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use earshot_engine::HOTCUES;
use earshot_engine::loudness::Loudness;

use super::external::{ExtCue, ExtTrack, Marker};
use super::sources::SourceId;
use super::store::{GridSource, LibTrack, Memory, StoreCmd, StoreEvent, TrackCues};
use super::track::TrackId;

/// Stick tracks' waveforms kept for the session, at most.
pub const WAVES: usize = 10_000;

/// A track earshot plays from outside its library.
#[derive(Clone, Debug, PartialEq)]
pub struct OutsideTrack {
    /// Negative.
    pub id: TrackId,
    /// The library it's from, and its id there.
    pub source: SourceId,
    pub external_id: String,
    pub file: PathBuf,
    /// On a USB stick: its waveform is kept for the session only.
    pub on_stick: bool,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub genre: String,
    pub comment: String,
    pub bpm: Option<f64>,
    pub key: Option<String>,
    pub rating: u8,
    pub colour: Option<u32>,
    pub duration: Option<f64>,
    /// On earshot's timeline. Empty: unknown (the deck works one out as for any file).
    pub grid: Vec<Marker>,
    /// Hot cues by pad, and memory cues and loops: as read, then as edited on a deck.
    pub hot: [Option<f64>; HOTCUES],
    pub hot_labels: [Option<String>; HOTCUES],
    pub memories: Vec<Memory>,
    /// The main cue, once set by hand on a deck (kept for the session only).
    pub main: Option<f64>,
    /// The library was read with its cues and grids (else they're still to come).
    pub has_cues: bool,
}

impl OutsideTrack {
    /// As the library index lists tracks.
    pub fn lib_track(&self) -> LibTrack {
        LibTrack {
            id: self.id,
            path: self.file.to_string_lossy().into_owned(),
            title: if self.title.is_empty() {
                self.file
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            } else {
                self.title.clone()
            },
            artist: self.artist.clone(),
            duration: self.duration,
            bpm: self.bpm,
            tag_bpm: self.bpm,
            key: self.key.clone(),
            album: self.album.clone(),
            genre: self.genre.clone(),
            comment: self.comment.clone(),
            grid_source: (!self.grid.is_empty()).then_some(GridSource::Import),
            energy: None,
            drifts: false,
            needs_attention: None,
            rating: (self.rating > 0).then_some(self.rating),
            colour: self.colour,
            tags: vec![],
            plays: 0,
            last_played: None,
        }
    }
}

/// A file as it is now: a cached waveform is for this version of it.
pub type FileKey = (PathBuf, u64, i64);

#[derive(Default)]
struct Inner {
    tracks: HashMap<TrackId, OutsideTrack>,
    by_origin: HashMap<(SourceId, String), TrackId>,
    /// The last id handed out (counting down from -1).
    last: TrackId,
    waves: HashMap<FileKey, (Arc<Vec<u8>>, Loudness)>,
    /// Oldest first, to drop past [`WAVES`].
    wave_order: VecDeque<FileKey>,
    /// Ids for memory cues made on a deck (negative too).
    last_memory: i64,
}

/// The session's outside tracks. Cheap to clone; shared by the UI, the loader and
/// the Store.
#[derive(Clone, Default)]
pub struct Outside(Arc<Mutex<Inner>>);

/// It's an outside track's id.
pub const fn is_outside(id: TrackId) -> bool {
    id < 0
}

impl Outside {
    /// A library's track, as read: its id for the session (the same one each time
    /// it's read). `None` if its file isn't on this computer. A read with cues
    /// replaces what was known of them; one without keeps them.
    pub fn register(&self, source: SourceId, t: &ExtTrack, on_stick: bool) -> Option<TrackId> {
        let file = t.file.clone()?;
        let mut inner = self.0.lock().unwrap();
        let key = (source, t.id.clone());
        let id = match inner.by_origin.get(&key) {
            Some(&id) => id,
            None => {
                inner.last -= 1;
                let id = inner.last;
                inner.by_origin.insert(key, id);
                id
            }
        };
        let had = inner.tracks.remove(&id);
        let read_cues = !t.cues.is_empty() || !t.grid.is_empty();
        let mut track = OutsideTrack {
            id,
            source,
            external_id: t.id.clone(),
            file,
            on_stick,
            title: t.title.clone(),
            artist: t.artist.clone(),
            album: t.album.clone(),
            genre: t.genre.clone(),
            comment: t.comment.clone(),
            bpm: t.bpm,
            key: t.key.clone(),
            rating: t.rating,
            colour: t.colour,
            duration: t.duration_s,
            grid: t.grid.clone(),
            hot: [None; HOTCUES],
            hot_labels: Default::default(),
            memories: vec![],
            main: None,
            has_cues: read_cues,
        };
        match had {
            Some(old) if !read_cues => {
                track.grid = old.grid;
                track.hot = old.hot;
                track.hot_labels = old.hot_labels;
                track.memories = old.memories;
                track.main = old.main;
                track.has_cues = old.has_cues;
            }
            _ => inner.set_cues(&mut track, &t.cues),
        }
        inner.tracks.insert(id, track);
        Some(id)
    }

    pub fn get(&self, id: TrackId) -> Option<OutsideTrack> {
        self.0.lock().unwrap().tracks.get(&id).cloned()
    }

    /// A stick track's waveform and loudness, if worked out this session.
    pub fn wave(&self, key: &FileKey) -> Option<(Arc<Vec<u8>>, Loudness)> {
        self.0.lock().unwrap().waves.get(key).cloned()
    }

    /// Keep a stick track's waveform for the session (the oldest go past [`WAVES`]).
    pub fn keep_wave(&self, key: FileKey, wave: Arc<Vec<u8>>, level: Loudness) {
        let mut inner = self.0.lock().unwrap();
        if inner.waves.insert(key.clone(), (wave, level)).is_none() {
            inner.wave_order.push_back(key);
        }
        while inner.wave_order.len() > WAVES {
            if let Some(old) = inner.wave_order.pop_front() {
                inner.waves.remove(&old);
            }
        }
    }

    /// The cues of an outside track as the Store would send them.
    pub fn cues(&self, id: TrackId) -> Option<TrackCues> {
        let inner = self.0.lock().unwrap();
        let t = inner.tracks.get(&id)?;
        Some(TrackCues {
            hot_labels: t.hot_labels.clone(),
            memories: t.memories.clone(),
        })
    }

    /// A Store command for an outside track: cue edits are kept here (answered with
    /// [`StoreEvent::Cues`], as the Store would); anything else is dropped, as nothing
    /// about an outside track is saved.
    pub fn apply(&self, cmd: StoreCmd) -> Option<StoreEvent> {
        let mut inner = self.0.lock().unwrap();
        let track = match &cmd {
            StoreCmd::SaveHotcues { track, .. }
            | StoreCmd::SetHotcueLabel { track, .. }
            | StoreCmd::AddMemory { track, .. }
            | StoreCmd::DeleteMemory { track, .. }
            | StoreCmd::SetMemoryLabel { track, .. }
            | StoreCmd::SetMemoryColor { track, .. }
            | StoreCmd::SetLoopActive { track, .. } => *track,
            StoreCmd::SaveMainCue { track, at } => {
                // Nothing on screen lists it: no event to answer with.
                if let Some(t) = inner.tracks.get_mut(track) {
                    t.main = Some(*at);
                }
                return None;
            }
            _ => return None,
        };
        let next_memory = {
            inner.last_memory -= 1;
            inner.last_memory
        };
        let t = inner.tracks.get_mut(&track)?;
        match cmd {
            StoreCmd::SaveHotcues { cues, .. } => {
                for (pad, at) in cues.iter().enumerate() {
                    if at.is_none() {
                        t.hot_labels[pad] = None;
                    }
                }
                t.hot = cues;
            }
            StoreCmd::SetHotcueLabel { slot, label, .. } => {
                if t.hot[slot as usize].is_none() {
                    // Like the Store: a pad that isn't set keeps no name.
                } else if let Some(l) = t.hot_labels.get_mut(slot as usize) {
                    *l = label;
                }
            }
            StoreCmd::AddMemory { start, end, .. } => {
                t.memories.push(Memory {
                    id: next_memory,
                    start,
                    end,
                    label: None,
                    color: None,
                    active: false,
                });
                t.memories.sort_by(|a, b| a.start.total_cmp(&b.start));
            }
            StoreCmd::DeleteMemory { id, .. } => t.memories.retain(|m| m.id != id),
            StoreCmd::SetMemoryLabel { id, label, .. } => {
                if let Some(m) = t.memories.iter_mut().find(|m| m.id == id) {
                    m.label = label;
                }
            }
            StoreCmd::SetMemoryColor { id, color, .. } => {
                if let Some(m) = t.memories.iter_mut().find(|m| m.id == id) {
                    m.color = color;
                }
            }
            StoreCmd::SetLoopActive { id, active, .. } => {
                for m in &mut t.memories {
                    if m.id == id && m.end.is_some() {
                        m.active = active;
                    } else if active {
                        m.active = false;
                    }
                }
            }
            _ => {}
        }
        Some(StoreEvent::Cues {
            track,
            cues: TrackCues {
                hot_labels: t.hot_labels.clone(),
                memories: t.memories.clone(),
            },
        })
    }
}

impl Inner {
    /// Hot cues (points on pads) and memory cues and loops, from a library's.
    fn set_cues(&mut self, t: &mut OutsideTrack, cues: &[ExtCue]) {
        for c in cues {
            match (c.role.as_str(), c.slot) {
                ("cue", Some(pad)) if (pad as usize) < HOTCUES => {
                    t.hot[pad as usize] = Some(c.start_s);
                    t.hot_labels[pad as usize] = (!c.name.is_empty()).then(|| c.name.clone());
                }
                ("cue" | "loop", None) => {
                    self.last_memory -= 1;
                    t.memories.push(Memory {
                        id: self.last_memory,
                        start: c.start_s,
                        end: c.end_s,
                        label: (!c.name.is_empty()).then(|| c.name.clone()),
                        color: c.colour,
                        active: false,
                    });
                }
                _ => {}
            }
        }
        t.memories.sort_by(|a, b| a.start.total_cmp(&b.start));
    }
}

/// Every track id a Store command would save something for: one that's outside
/// means the command isn't the Store's.
pub fn touches_outside(cmd: &StoreCmd) -> bool {
    match cmd {
        StoreCmd::SaveUserGrid { track, .. }
        | StoreCmd::SaveWaveform { track, .. }
        | StoreCmd::SaveFeatures { track, .. }
        | StoreCmd::SaveJobs { track, .. }
        | StoreCmd::SetMeta { track, .. }
        | StoreCmd::SaveEmbedding { track, .. }
        | StoreCmd::SaveHotcues { track, .. }
        | StoreCmd::SaveMainCue { track, .. }
        | StoreCmd::SetHotcueLabel { track, .. }
        | StoreCmd::AddMemory { track, .. }
        | StoreCmd::DeleteMemory { track, .. }
        | StoreCmd::SetMemoryLabel { track, .. }
        | StoreCmd::SetMemoryColor { track, .. }
        | StoreCmd::SetLoopActive { track, .. }
        | StoreCmd::SavePlay { track, .. } => is_outside(*track),
        StoreCmd::AddTracks { tracks, .. } | StoreCmd::RemoveTracks { tracks, .. } => {
            tracks.iter().any(|&t| is_outside(t))
        }
        // A Flow holding one couldn't be saved (its cards must be earshot's tracks).
        StoreCmd::SaveFlow { graph, .. } => graph.nodes.iter().any(|n| is_outside(n.track)),
        _ => false,
    }
}
