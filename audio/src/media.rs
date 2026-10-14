//! Where a track's audio is, and getting its bytes: everything that reads audio
//! (decoding, tags, cover art, the audio hash) goes through [`Location::open`], so a
//! new kind of place (a remote object, a streaming service) is one more variant here
//! and nothing else changes.
//!
//! Stored in the database as bytes ([`Location::to_bytes`]): a local path's raw bytes,
//! or a URL (`scheme://…`).

use std::fmt;
use std::fs::File;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};

/// Bytes to read a track from: seekable, so formats with their index at the end
/// (MP4) and the audio hash's ranges work.
pub trait Media: Read + Seek + Send + Sync {
    /// Its length in bytes, if known.
    fn len(&self) -> Option<u64>;
}

impl Media for File {
    fn len(&self) -> Option<u64> {
        self.metadata().ok().map(|m| m.len())
    }
}

/// Where a track's audio is.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Location {
    /// A file on this computer.
    File(PathBuf),
    /// Anywhere else, by URL: `s3://bucket/key`, `https://…`, a streaming service's
    /// own scheme. Not readable yet: [`Location::open`] says so.
    Url(String),
}

impl Location {
    /// From its stored form: a URL if it starts `scheme://`, else a local path.
    pub fn from_bytes(b: Vec<u8>) -> Location {
        if let Some(i) = b.windows(3).position(|w| w == b"://")
            && i > 0
            && b[..i]
                .iter()
                .all(|c| c.is_ascii_alphanumeric() || b"+-.".contains(c))
            && let Ok(url) = String::from_utf8(b.clone())
        {
            return Location::Url(url);
        }
        Location::File(path_from_bytes(b))
    }

    /// Its stored form (see [`Location::from_bytes`]).
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Location::File(p) => path_to_bytes(p),
            Location::Url(u) => u.as_bytes().to_vec(),
        }
    }

    /// The local file, if it is one.
    pub fn as_file(&self) -> Option<&Path> {
        match self {
            Location::File(p) => Some(p),
            Location::Url(_) => None,
        }
    }

    /// Its last path segment ("Song.mp3").
    pub fn file_name(&self) -> String {
        match self {
            Location::File(p) => p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            Location::Url(u) => {
                let path = u.split(['?', '#']).next().unwrap_or(u);
                path.rsplit('/').next().unwrap_or(path).to_string()
            }
        }
    }

    /// The file name's extension, lower case ("mp3"): what kind of audio it is.
    pub fn extension(&self) -> Option<String> {
        let name = self.file_name();
        let (stem, ext) = name.rsplit_once('.')?;
        (!stem.is_empty() && !ext.is_empty()).then(|| ext.to_lowercase())
    }

    /// Open it for reading.
    pub fn open(&self) -> Result<Box<dyn Media>, String> {
        match self {
            Location::File(p) => File::open(p)
                .map(|f| Box::new(f) as Box<dyn Media>)
                .map_err(|e| format!("{}: {e}", p.display())),
            Location::Url(u) => Err(format!("{u}: reading audio from a URL isn't supported yet")),
        }
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Location::File(p) => write!(f, "{}", p.display()),
            Location::Url(u) => f.write_str(u),
        }
    }
}

impl From<PathBuf> for Location {
    fn from(p: PathBuf) -> Location {
        Location::File(p)
    }
}

impl From<&Path> for Location {
    fn from(p: &Path) -> Location {
        Location::File(p.to_path_buf())
    }
}

/// A path's raw bytes (lossy where the OS has no byte form).
pub fn path_to_bytes(p: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        p.as_os_str().as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        p.to_string_lossy().as_bytes().to_vec()
    }
}

/// A path from its raw bytes ([`path_to_bytes`]).
pub fn path_from_bytes(b: Vec<u8>) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_vec(b))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(&b).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_form_round_trips_and_tells_urls_from_paths() {
        for l in [
            Location::File("/music/a b/Song.MP3".into()),
            Location::File("relative/odd://name.flac".into()),
            Location::Url("s3://bucket/house/track.flac".into()),
            Location::Url("https://example.com/x/y.m4a?sig=1".into()),
        ] {
            assert_eq!(Location::from_bytes(l.to_bytes()), l);
        }
        let u = Location::Url("https://example.com/x/y.M4A?sig=1".into());
        assert_eq!(
            (u.file_name().as_str(), u.extension()),
            ("y.M4A", Some("m4a".into()))
        );
        assert_eq!(Location::File("/m/.hidden".into()).extension(), None);
    }

    #[test]
    fn files_open_and_urls_say_they_cant_yet() {
        let p = std::env::temp_dir().join(format!("earshot-media-{}.bin", std::process::id()));
        std::fs::write(&p, b"12345").unwrap();
        let mut m = Location::File(p.clone()).open().unwrap();
        assert_eq!(m.len(), Some(5));
        let mut s = String::new();
        m.read_to_string(&mut s).unwrap();
        assert_eq!(s, "12345");
        std::fs::remove_file(&p).unwrap();
        let e = Location::Url("s3://b/k.mp3".into()).open().err().unwrap();
        assert!(e.contains("isn't supported yet"), "{e}");
    }
}
