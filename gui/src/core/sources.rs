//! Where tracks come from: earshot's music folders, other DJ programs' libraries
//! and, later, streaming services. Each is a [`Source`], a row of the Store's
//! `sources` table; the library pane's Sources lists them, and a track is in the
//! library while an enabled source has it (`track_sources`). What reads each kind is
//! elsewhere: folders are scanned by `core::library`, other programs' libraries are
//! read and imported through cratelib (`core::external`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub type SourceId = i64;

/// What kind of place a source is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// A folder of audio files on this computer, scanned for music.
    Folder,
    /// Rekordbox 6/7's library (`master.db`).
    RekordboxDb,
    /// A Rekordbox XML export.
    RekordboxXml,
    /// A USB stick exported for CDJs.
    RekordboxUsb,
    /// A `_Serato_` folder.
    Serato,
    /// Mixxx's `mixxxdb.sqlite`.
    Mixxx,
}

impl SourceKind {
    /// Every kind, in the order the pane lists them.
    pub const ALL: [SourceKind; 6] = [
        SourceKind::Folder,
        SourceKind::RekordboxDb,
        SourceKind::Serato,
        SourceKind::Mixxx,
        SourceKind::RekordboxXml,
        SourceKind::RekordboxUsb,
    ];

    /// The DJ programs the pane always lists, set up or not.
    pub const PROGRAMS: [SourceKind; 3] = [
        SourceKind::RekordboxDb,
        SourceKind::Serato,
        SourceKind::Mixxx,
    ];

    /// As stored, and as cratelib names the format.
    pub const fn as_str(self) -> &'static str {
        match self {
            SourceKind::Folder => "folder",
            SourceKind::RekordboxDb => "rekordbox_db",
            SourceKind::RekordboxXml => "rekordbox_xml",
            SourceKind::RekordboxUsb => "rekordbox_usb",
            SourceKind::Serato => "serato",
            SourceKind::Mixxx => "mixxx",
        }
    }

    pub fn parse(s: &str) -> Option<SourceKind> {
        SourceKind::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// For showing.
    pub const fn label(self) -> &'static str {
        match self {
            SourceKind::Folder => "Folder",
            SourceKind::RekordboxDb => "Rekordbox",
            SourceKind::RekordboxXml => "Rekordbox XML",
            SourceKind::RekordboxUsb => "Rekordbox USB",
            SourceKind::Serato => "Serato",
            SourceKind::Mixxx => "Mixxx",
        }
    }

    /// Another DJ program's library, read through cratelib (not a folder).
    pub const fn is_library(self) -> bool {
        !matches!(self, SourceKind::Folder)
    }

    /// One of [`SourceKind::PROGRAMS`]: always listed, its location set by the user
    /// or found in the program's usual place.
    pub fn is_program(self) -> bool {
        SourceKind::PROGRAMS.contains(&self)
    }

    /// Looked for in the program's usual place, rather than only where the user
    /// says. On Linux, Rekordbox and Serato only run under Wine or in a container,
    /// where looking for them would be guessing.
    pub fn found_by_itself(self) -> bool {
        self.is_program() && (!cfg!(target_os = "linux") || self == SourceKind::Mixxx)
    }

    /// Has memory cues to import (Rekordbox's).
    pub const fn has_memory_cues(self) -> bool {
        matches!(
            self,
            SourceKind::RekordboxDb | SourceKind::RekordboxXml | SourceKind::RekordboxUsb
        )
    }
}

/// Path rules for one library: our folder → the same folder as its program stores
/// it (cratelib's `PathMap`).
pub type PathRules = BTreeMap<String, String>;

/// A source's settings beyond where it is (`sources.config`, JSON).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SourceConfig {
    /// Serato: the drive its paths are relative to ("/", "C:/", a stick's mount).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume_root: Option<String>,
    /// Where its files are here, when its program stores other paths: as found
    /// last, checked at startup ([`SourceConfig::revalidate`]).
    #[serde(skip_serializing_if = "PathRules::is_empty")]
    pub paths: PathRules,
}

impl SourceConfig {
    /// Drop path rules whose folder of ours has gone (a drive unplugged for good, a
    /// folder renamed): the next read looks for the files again. True if any went.
    pub fn revalidate(&mut self) -> bool {
        let before = self.paths.len();
        self.paths.retain(|ours, _| Path::new(ours).is_dir());
        self.paths.len() != before
    }
}

/// A place tracks come from.
#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub id: SourceId,
    pub kind: SourceKind,
    /// For showing: a folder's name, "Rekordbox", "Rekordbox USB (STICK)" …
    pub name: String,
    /// Its folder or library file; `None`: not set yet.
    pub location: Option<PathBuf>,
    pub config: SourceConfig,
    /// Its tracks are listed.
    pub enabled: bool,
}

/// A source to add: what [`Source`] is before the Store gives it an id.
#[derive(Clone, Debug, PartialEq)]
pub struct NewSource {
    pub kind: SourceKind,
    pub name: String,
    pub location: Option<PathBuf>,
    pub config: SourceConfig,
}

impl NewSource {
    /// A music folder, named after itself.
    pub fn folder(path: PathBuf) -> NewSource {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        NewSource {
            kind: SourceKind::Folder,
            name,
            location: Some(path),
            config: SourceConfig::default(),
        }
    }
}

/// Streaming services the pane lists as coming (not sources yet).
pub const STREAMING: [&str; 3] = ["SoundCloud", "Beatport", "Spotify"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_round_trip_and_know_their_programs() {
        for k in SourceKind::ALL {
            assert_eq!(SourceKind::parse(k.as_str()), Some(k));
        }
        assert!(SourceKind::Mixxx.found_by_itself());
        assert!(!SourceKind::Folder.found_by_itself());
        assert!(!SourceKind::RekordboxXml.found_by_itself());
        assert!(SourceKind::RekordboxUsb.has_memory_cues());
        assert!(!SourceKind::Serato.has_memory_cues());
    }

    #[test]
    fn revalidating_drops_rules_whose_folder_has_gone() {
        let here = std::env::temp_dir().to_string_lossy().into_owned();
        let mut c = SourceConfig {
            volume_root: None,
            paths: PathRules::from([
                (here.clone(), "C:/M".into()),
                ("/no/such/dir".into(), "D:/".into()),
            ]),
        };
        assert!(c.revalidate());
        assert_eq!(c.paths, PathRules::from([(here, "C:/M".into())]));
        assert!(!c.revalidate(), "nothing more to drop");
        assert_eq!(
            serde_json::to_string(&SourceConfig::default()).unwrap(),
            "{}"
        );
    }

    #[test]
    fn a_folder_is_named_after_itself() {
        let s = NewSource::folder("/home/me/Music".into());
        assert_eq!((s.kind, s.name.as_str()), (SourceKind::Folder, "Music"));
    }
}
