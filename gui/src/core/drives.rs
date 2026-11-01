//! USB sticks and other removable drives: which are plugged in (the mount table,
//! which [`watch`] waits on: the kernel wakes it when a drive is mounted or
//! unmounted, so sticks come and go as they're plugged in and out), which DJ
//! libraries are on them, whether earshot is using one (so
//! it isn't safe to pull out), copying tracks off one, and ejecting it.
//!
//! A drive is "in use" while a job holds it ([`Uses::hold`]): an export writing to
//! it, an import reading from it. When the last hold goes, the drive's filesystem is
//! flushed (`syncfs`), so everything earshot wrote is on the stick before the light
//! goes green.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use earshot_engine::media::Location;

use super::hash;
use super::sources::SourceKind;
use super::store::Reader;

/// How long [`watch`] waits without news before looking anyway: a library written
/// onto a stick by another program doesn't change the mount table. (earshot looks
/// again itself after writing to one.)
const IDLE: Duration = Duration::from_secs(30);

/// Where desktops mount removable drives.
const ROOTS: [&str; 4] = ["/media", "/run/media", "/mnt", "/Volumes"];

/// Filesystems that are never drives.
const VIRTUAL: [&str; 14] = [
    "proc",
    "sysfs",
    "devtmpfs",
    "devpts",
    "cgroup",
    "cgroup2",
    "overlay",
    "autofs",
    "binfmt_misc",
    "debugfs",
    "tracefs",
    "securityfs",
    "pstore",
    "tmpfs",
];

/// A removable drive, mounted.
#[derive(Clone, Debug, PartialEq)]
pub struct Drive {
    /// Where it's mounted.
    pub path: PathBuf,
    /// Its name (the mount point's last part, which is the volume label).
    pub label: String,
    /// The block device (`/dev/sdb1`), for ejecting.
    pub device: String,
    pub fstype: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub writable: bool,
    /// The DJ libraries on it: [`SourceKind::RekordboxUsb`], [`SourceKind::SeratoUsb`].
    pub libraries: Vec<SourceKind>,
}

impl Drive {
    /// What's worth knowing about its filesystem, for DJ players.
    pub fn notes(&self) -> Option<&'static str> {
        match self.fstype.as_str() {
            "ntfs" | "ntfs3" => Some(
                "NTFS: rekordbox won't write to it, macOS only reads it and no player reads it. \
                 Use FAT32, or exFAT for a laptop only.",
            ),
            "exfat" | "fuseblk" => Some(
                "exFAT (or another FUSE filesystem): fine on a laptop and a CDJ-3000; \
                 CDJ-2000NXS2 and older players only read FAT32.",
            ),
            _ => None,
        }
    }

    /// The library of `kind` on it, as a source would have it: where, and Serato's
    /// volume root.
    pub fn library_location(&self, kind: SourceKind) -> (PathBuf, Option<String>) {
        match kind {
            SourceKind::SeratoUsb => (
                self.path.join("_Serato_"),
                Some(self.path.to_string_lossy().into_owned()),
            ),
            _ => (self.path.clone(), None),
        }
    }

    /// Which stick it is, for the icon stack (its mount point, hashed: the same
    /// stick plugged in again usually gets the same one).
    pub fn id(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::hash::DefaultHasher::new();
        self.path.hash(&mut h);
        h.finish()
    }

    /// `p` is on this drive.
    pub fn holds(&self, p: &Path) -> bool {
        p.starts_with(&self.path)
    }
}

/// The DJ libraries in a folder (a drive's root).
pub fn libraries_at(root: &Path) -> Vec<SourceKind> {
    let mut out = vec![];
    let rekordbox = ["PIONEER", ".PIONEER"].iter().any(|d| {
        let rb = root.join(d).join("rekordbox");
        rb.join("export.pdb").is_file() || rb.join("exportLibrary.db").is_file()
    });
    if rekordbox {
        out.push(SourceKind::RekordboxUsb);
    }
    if root.join("_Serato_").join("database V2").is_file() {
        out.push(SourceKind::SeratoUsb);
    }
    out
}

/// `/proc/mounts` writes spaces and the like as octal (`\040`).
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 3 < b.len()
            && let Ok(n) = u8::from_str_radix(&s[i + 1..i + 4], 8)
        {
            out.push(n);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The removable drives in a mount table (`/proc/self/mounts`'s text): mounts below
/// one of [`ROOTS`] (not a root's own mount), as (device, mount point, fstype).
fn parse_mounts(text: &str) -> Vec<(String, PathBuf, String)> {
    text.lines()
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            let (device, at, fstype) = (f.next()?, unescape(f.next()?), f.next()?);
            let at = PathBuf::from(at);
            let below_root = ROOTS
                .iter()
                .any(|r| at.starts_with(r) && at != Path::new(r));
            (below_root && !VIRTUAL.contains(&fstype))
                .then(|| (device.to_owned(), at, fstype.to_owned()))
        })
        .collect()
}

/// Total and free bytes, and whether we may write to it.
fn space(at: &Path) -> (u64, u64, bool) {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(at.as_os_str().as_bytes()) else {
        return (0, 0, false);
    };
    // SAFETY: `c` is a valid C string; `st` is plain data statvfs fills in.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::statvfs(c.as_ptr(), &mut st) } == 0;
    let writable = unsafe { libc::access(c.as_ptr(), libc::W_OK) } == 0;
    if !ok {
        return (0, 0, writable);
    }
    let block = st.f_frsize as u64;
    (
        st.f_blocks as u64 * block,
        st.f_bavail as u64 * block,
        writable,
    )
}

/// The removable drives mounted now.
pub fn list() -> Vec<Drive> {
    let mounts = match fs::read_to_string("/proc/self/mounts") {
        Ok(text) => parse_mounts(&text),
        // macOS: everything under /Volumes but the system disk's link.
        Err(_) => fs::read_dir("/Volumes")
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| !p.is_symlink() && p.is_dir())
            .map(|p| (String::new(), p, String::new()))
            .collect(),
    };
    mounts
        .into_iter()
        .map(|(device, path, fstype)| {
            let (total_bytes, free_bytes, writable) = space(&path);
            Drive {
                label: path.file_name().map_or_else(
                    || path.display().to_string(),
                    |n| n.to_string_lossy().into(),
                ),
                libraries: libraries_at(&path),
                device,
                fstype,
                total_bytes,
                free_bytes,
                writable,
                path,
            }
        })
        .collect()
}

/// Watch the drives on a thread of its own; `changed` hears the whole list each
/// time it differs (and once at the start). On Linux the thread sleeps until the
/// kernel says the mount table changed (`/proc/self/mounts` is readable as
/// "priority" data then), or for [`IDLE`]; elsewhere it looks every 2 s.
pub fn watch(changed: impl Fn(Vec<Drive>) + Send + 'static) {
    std::thread::Builder::new()
        .name("earshot-drives".into())
        .spawn(move || {
            use std::os::fd::AsRawFd;
            let mounts = fs::File::open("/proc/self/mounts").ok();
            let mut last: Option<Vec<Drive>> = None;
            loop {
                let mut now = list();
                // Free space moves while something writes; a MB is enough to show.
                for d in &mut now {
                    d.free_bytes = d.free_bytes / 1_000_000 * 1_000_000;
                }
                if last.as_ref() != Some(&now) {
                    last = Some(now.clone());
                    changed(now);
                }
                match &mounts {
                    Some(f) => {
                        let mut p = libc::pollfd {
                            fd: f.as_raw_fd(),
                            events: libc::POLLPRI | libc::POLLERR,
                            revents: 0,
                        };
                        // SAFETY: one valid pollfd.
                        unsafe { libc::poll(&mut p, 1, IDLE.as_millis() as i32) };
                        // Reading this same open file to the end clears the news
                        // (`list` reads its own copy).
                        use std::io::{Read, Seek};
                        let mut f = f;
                        let _ = f.seek(io::SeekFrom::Start(0));
                        let _ = f.read_to_end(&mut vec![]);
                    }
                    None => std::thread::sleep(Duration::from_secs(2)),
                }
            }
        })
        .expect("start the drive watcher");
}

/// Write everything cached for the filesystem at `at` to the drive.
pub fn flush(at: &Path) {
    use std::os::fd::AsRawFd;
    if let Ok(f) = fs::File::open(at) {
        // SAFETY: a valid open file descriptor.
        unsafe { libc::syncfs(f.as_raw_fd()) };
    }
}

/// The drives jobs are using, shared between the UI and the jobs' threads.
#[derive(Clone, Default)]
pub struct Uses(Arc<Mutex<HashMap<PathBuf, usize>>>);

impl Uses {
    /// Mark `drive` in use until the returned hold is dropped (then it's flushed).
    pub fn hold(&self, drive: &Path) -> Hold {
        *self.0.lock().unwrap().entry(drive.to_owned()).or_default() += 1;
        Hold {
            uses: self.clone(),
            drive: drive.to_owned(),
        }
    }

    /// Something is using `drive`: it isn't safe to remove.
    pub fn busy(&self, drive: &Path) -> bool {
        self.0.lock().unwrap().get(drive).is_some_and(|&n| n > 0)
    }
}

/// A drive in use: see [`Uses::hold`].
pub struct Hold {
    uses: Uses,
    drive: PathBuf,
}

impl Drop for Hold {
    fn drop(&mut self) {
        // Flushed while still held, so the light is red until it's done.
        flush(&self.drive);
        let mut m = self.uses.0.lock().unwrap();
        if let Some(n) = m.get_mut(&self.drive) {
            *n -= 1;
            if *n == 0 {
                m.remove(&self.drive);
            }
        }
    }
}

/// Unmount a drive and power it off, so it can be pulled out. Blocking.
pub fn eject(d: &Drive) -> Result<(), String> {
    flush(&d.path);
    let run = |cmd: &str, args: &[&str]| -> Result<(), String> {
        let out = Command::new(cmd)
            .args(args)
            .output()
            .map_err(|e| format!("{cmd}: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
        }
    };
    if cfg!(target_os = "macos") {
        return run("diskutil", &["eject", &d.path.to_string_lossy()]);
    }
    if d.device.starts_with("/dev/") {
        run(
            "udisksctl",
            &["unmount", "--no-user-interaction", "-b", &d.device],
        )?;
        // Unmounted is safe to pull out; powering off is a courtesy some drives refuse.
        let _ = run(
            "udisksctl",
            &["power-off", "--no-user-interaction", "-b", &d.device],
        );
        return Ok(());
    }
    run("gio", &["mount", "-e", &d.path.to_string_lossy()])
}

// ---- copying tracks off a stick ---------------------------------------------------------

/// How a copy went.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Copied {
    /// Copied into the music folder.
    pub copied: usize,
    /// Already in earshot (the same audio) or already copied: not copied again.
    pub already: usize,
    /// Couldn't be copied, with why.
    pub failed: Vec<String>,
}

/// Where a stick's file goes: `dest/<stick label>/<its path on the stick>`, without
/// Rekordbox's `Contents` folder at the front.
pub fn copy_path(file: &Path, stick: &Path, label: &str, dest: &Path) -> PathBuf {
    let rel = file.strip_prefix(stick).unwrap_or(file);
    let rel = rel.strip_prefix("Contents").unwrap_or(rel);
    let rel: PathBuf = rel
        .components()
        .filter(|c| matches!(c, std::path::Component::Normal(_)))
        .collect();
    dest.join(label).join(rel)
}

/// Copy `files` (on the stick at `stick`) into the music folder `dest`. Each is
/// checked against earshot's tracks first: one with the same audio is used instead
/// of a copy. Returns where each file is now (the copy, or earshot's own).
pub fn copy_in(
    reader: &Reader,
    files: &[PathBuf],
    stick: &Path,
    label: &str,
    dest: &Path,
    mut progress: impl FnMut(String),
) -> (HashMap<PathBuf, PathBuf>, Copied) {
    let mut out = HashMap::new();
    let mut report = Copied::default();
    for (i, f) in files.iter().enumerate() {
        progress(format!("Copying {}/{}", i + 1, files.len()));
        let to = copy_path(f, stick, label, dest);
        let size = |p: &Path| fs::metadata(p).map(|m| m.len()).ok();
        if to.is_file() && size(&to) == size(f) {
            report.already += 1;
            out.insert(f.clone(), to);
            continue;
        }
        let known = hash::content_hash(&Location::File(f.clone()))
            .ok()
            .and_then(|h| reader.file_with_hash(&h).ok().flatten());
        if let Some(ours) = known {
            report.already += 1;
            out.insert(f.clone(), ours);
            continue;
        }
        match copy_one(f, &to) {
            Ok(()) => {
                report.copied += 1;
                out.insert(f.clone(), to);
            }
            Err(e) => report.failed.push(format!("{}: {e}", f.display())),
        }
    }
    (out, report)
}

/// Copy through a temporary name, so a half-copied file never looks like a track.
fn copy_one(from: &Path, to: &Path) -> io::Result<()> {
    if let Some(dir) = to.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = to.with_extension("earshot-part");
    fs::copy(from, &tmp)?;
    fs::rename(&tmp, to)
}

/// One entry of a folder on a stick, for the file browser.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub dir: bool,
    pub size: u64,
    /// An audio file earshot plays.
    pub audio: bool,
}

/// What's in a folder: folders first, then files, each by name; hidden ones left out.
pub fn read_dir(dir: &Path) -> io::Result<Vec<Entry>> {
    let mut out: Vec<Entry> = fs::read_dir(dir)?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                return None;
            }
            let meta = e.metadata().ok()?;
            let path = e.path();
            let audio = !meta.is_dir() && super::library::is_audio(&path);
            Some(Entry {
                name,
                dir: meta.is_dir(),
                size: meta.len(),
                audio,
                path,
            })
        })
        .collect();
    out.sort_by(|a, b| (!a.dir, a.name.to_lowercase()).cmp(&(!b.dir, b.name.to_lowercase())));
    Ok(out)
}

/// The audio files in and below `paths` (files, or folders walked through).
pub fn audio_files(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = vec![];
    let mut stack: Vec<PathBuf> = paths.iter().rev().cloned().collect();
    while let Some(p) = stack.pop() {
        if p.is_dir() {
            if let Ok(entries) = read_dir(&p) {
                stack.extend(entries.into_iter().rev().map(|e| e.path));
            }
        } else if super::library::is_audio(&p) {
            out.push(p);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty folder of its own for a test.
    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("earshot-drives-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn sticks_are_mounts_below_the_usual_places() {
        let text = "\
/dev/nvme0n1p2 / ext4 rw 0 0
proc /proc proc rw 0 0
/dev/sdb1 /media/lewin/MY\\040STICK vfat rw 0 0
tmpfs /run/media tmpfs rw 0 0
/dev/sdc1 /run/media/lewin/SERATO exfat rw 0 0
/dev/sdd1 /mnt ext4 rw 0 0
";
        let m = parse_mounts(text);
        assert_eq!(
            m,
            [
                (
                    "/dev/sdb1".into(),
                    "/media/lewin/MY STICK".into(),
                    "vfat".into()
                ),
                (
                    "/dev/sdc1".into(),
                    "/run/media/lewin/SERATO".into(),
                    "exfat".into()
                ),
            ]
        );
    }

    #[test]
    fn libraries_are_found_by_their_files() {
        let dir = scratch("libraries");
        assert!(libraries_at(&dir).is_empty());
        fs::create_dir_all(dir.join("PIONEER/rekordbox")).unwrap();
        fs::write(dir.join("PIONEER/rekordbox/export.pdb"), b"").unwrap();
        fs::create_dir_all(dir.join("_Serato_")).unwrap();
        fs::write(dir.join("_Serato_/database V2"), b"").unwrap();
        assert_eq!(
            libraries_at(&dir),
            [SourceKind::RekordboxUsb, SourceKind::SeratoUsb]
        );
    }

    #[test]
    fn copies_go_under_the_sticks_name_without_contents() {
        let p = copy_path(
            Path::new("/media/s/STICK/Contents/Artist/Album/a.mp3"),
            Path::new("/media/s/STICK"),
            "STICK",
            Path::new("/home/me/Music"),
        );
        assert_eq!(p, Path::new("/home/me/Music/STICK/Artist/Album/a.mp3"));
    }

    #[test]
    fn a_hold_marks_the_drive_busy_until_dropped() {
        let dir = scratch("hold");
        let uses = Uses::default();
        let h = uses.hold(&dir);
        let h2 = uses.hold(&dir);
        assert!(uses.busy(&dir));
        drop(h);
        assert!(uses.busy(&dir));
        drop(h2);
        assert!(!uses.busy(&dir));
    }
}
