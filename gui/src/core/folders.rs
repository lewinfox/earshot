//! Where earshot keeps its files: the only place that decides. On Linux these are
//! the XDG base directories, each under `earshot`:
//!
//! | | Linux (default) | What's in it |
//! |---|---|---|
//! | data | `~/.local/share/earshot` | the library database (settings too) |
//! | config | `~/.config/earshot` | the keymap, the user's controller mappings |
//! | cache | `~/.cache/earshot` | stems, tempo lock's working files, numba's cache |
//! | state | `~/.local/state/earshot` | logs, debug exports, the window's size and place |
//!
//! `$XDG_DATA_HOME` and the rest move them. macOS and Windows get their own places
//! (`~/Library/Application Support/io.github.lewinfox.earshot`, `~/Library/Caches/…`).
//! Nothing goes anywhere else in the home folder.
//!
//! `EARSHOT_DATA_DIR` puts all four in that one folder (to try things on a copy).
//! Tests do the same with [`Folders::all_in`], since they run side by side in one
//! process.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use directories::{BaseDirs, ProjectDirs};

/// The four folders, as the app was started with them (`Services`).
#[derive(Clone, Debug, PartialEq)]
pub struct Folders {
    pub data: PathBuf,
    pub config: PathBuf,
    pub cache: PathBuf,
    pub state: PathBuf,
}

impl Folders {
    /// Where the real app keeps things.
    pub fn real() -> Folders {
        Folders {
            data: data_folder(),
            config: config_folder(),
            cache: cache_folder(),
            state: state_folder(),
        }
    }

    /// All four in one folder (tests).
    #[cfg(test)]
    pub fn all_in(dir: &Path) -> Folders {
        Folders {
            data: dir.to_owned(),
            config: dir.to_owned(),
            cache: dir.to_owned(),
            state: dir.to_owned(),
        }
    }

    /// The file eframe keeps the window's size and place in.
    pub fn window_state(&self) -> PathBuf {
        self.state.join("window.ron")
    }
}

/// Things to keep: the library database.
pub fn data_folder() -> PathBuf {
    pick(|p| p.data_dir().to_owned())
}

/// What the user edits: the keymap, controller mappings.
pub fn config_folder() -> PathBuf {
    pick(|p| p.config_dir().to_owned())
}

/// What can be made again: stems, working files. Backups skip it; cleaners may empty it.
pub fn cache_folder() -> PathBuf {
    pick(|p| p.cache_dir().to_owned())
}

/// What's worth keeping between runs but not backing up: logs, the window's place.
/// (Only Linux has a state folder; elsewhere it's the local data folder.)
pub fn state_folder() -> PathBuf {
    pick(|p| p.state_dir().unwrap_or(p.data_local_dir()).to_owned())
}

/// `EARSHOT_DATA_DIR`, else the platform's folder for earshot, else here.
fn pick(folder: impl Fn(&ProjectDirs) -> PathBuf) -> PathBuf {
    if let Some(d) = std::env::var_os("EARSHOT_DATA_DIR") {
        return PathBuf::from(d);
    }
    // "earshot" on Linux, "io.github.lewinfox.earshot" on macOS (as Apple expects),
    // "lewinfox\earshot" on Windows.
    ProjectDirs::from("io.github", "lewinfox", "earshot")
        .map(|p| folder(&p))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Move what earlier versions kept elsewhere into `to`, once: everything was in one
/// folder named `io.github.lewinfox.earshot` in the data folder, and eframe kept the
/// window's place in a folder of its own. Nothing already in the new places is
/// overwritten. Not with `EARSHOT_DATA_DIR`, which was always one folder. Returns
/// what it did, for the log (which can only start once its folder has moved).
pub fn adopt_old(to: &Folders) -> Vec<String> {
    let mut said = vec![];
    if std::env::var_os("EARSHOT_DATA_DIR").is_some() {
        return said;
    }
    let Some(base) = BaseDirs::new() else {
        return said;
    };
    let old = base.data_dir().join("io.github.lewinfox.earshot");
    adopt_from(&old, to, &mut said);
    // eframe's own default: `<data>/earshot/app.ron`, `earshot\data\app.ron` on Windows.
    let eframe = if cfg!(windows) {
        base.data_dir().join("earshot").join("data")
    } else {
        base.data_dir().join("earshot")
    };
    let window = eframe.join("app.ron");
    if window.is_file() {
        let dest = to.window_state();
        said.push(report(&window, &dest, move_to(&window, &dest)));
        if ![&to.data, &to.config, &to.cache, &to.state].contains(&&eframe) {
            let _ = fs::remove_dir(&eframe);
        }
    }
    said
}

/// [`adopt_old`] from the old folder `old`, then remove it if that emptied it.
fn adopt_from(old: &Path, to: &Folders, said: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(old) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let into = match name.to_str().unwrap_or("") {
            "keymap.txt" | "controllers" => &to.config,
            "stems" => &to.cache,
            "logs" | "debug" => &to.state,
            // Tempo lock's leftovers from the last run, which it clears at start.
            "tempo-lock" => {
                let _ = fs::remove_dir_all(e.path());
                continue;
            }
            // The library database (and its -wal, -shm) and anything else.
            _ => &to.data,
        };
        let dest = into.join(&name);
        if dest != e.path() {
            said.push(report(&e.path(), &dest, move_to(&e.path(), &dest)));
        }
    }
    let _ = fs::remove_dir(old);
}

/// Move a file or folder to `dest`, unless something is there already. Across
/// filesystems (~/.cache can be its own) it's copied, then removed.
fn move_to(src: &Path, dest: &Path) -> io::Result<bool> {
    if dest.exists() {
        return Ok(false);
    }
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir)?;
    }
    if fs::rename(src, dest).is_ok() {
        return Ok(true);
    }
    copy_all(src, dest)?;
    if src.is_dir() {
        fs::remove_dir_all(src)?;
    } else {
        fs::remove_file(src)?;
    }
    Ok(true)
}

fn copy_all(src: &Path, dest: &Path) -> io::Result<()> {
    if src.is_dir() {
        fs::create_dir_all(dest)?;
        for e in fs::read_dir(src)? {
            let e = e?;
            copy_all(&e.path(), &dest.join(e.file_name()))?;
        }
        Ok(())
    } else {
        fs::copy(src, dest).map(|_| ())
    }
}

fn report(src: &Path, dest: &Path, r: io::Result<bool>) -> String {
    let (src, dest) = (src.display(), dest.display());
    match r {
        Ok(true) => format!("moved {src} to {dest}"),
        Ok(false) => format!("left {src} where it is: {dest} is there already"),
        Err(e) => format!("couldn't move {src} to {dest}: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("earshot-folders-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn on_linux_each_is_an_xdg_folder_named_earshot() {
        let p = ProjectDirs::from("io.github", "lewinfox", "earshot").unwrap();
        for (dir, xdg) in [
            (p.data_dir(), ".local/share/earshot"),
            (p.config_dir(), ".config/earshot"),
            (p.cache_dir(), ".cache/earshot"),
            (p.state_dir().unwrap(), ".local/state/earshot"),
        ] {
            assert!(dir.ends_with("earshot"), "{}", dir.display());
            // As named, unless the user's $XDG_*_HOME moves it.
            if std::env::var_os("XDG_DATA_HOME").is_none()
                && std::env::var_os("XDG_CONFIG_HOME").is_none()
                && std::env::var_os("XDG_CACHE_HOME").is_none()
                && std::env::var_os("XDG_STATE_HOME").is_none()
            {
                assert!(
                    dir.ends_with(xdg),
                    "{} should end with {xdg}",
                    dir.display()
                );
            }
        }
    }

    #[test]
    fn the_old_folder_is_shared_out_and_removed() {
        let root = scratch("adopt");
        let old = root.join("old");
        for d in ["stems/ab", "logs", "controllers", "tempo-lock/7", "debug"] {
            fs::create_dir_all(old.join(d)).unwrap();
        }
        for f in [
            "library.db",
            "library.db-wal",
            "keymap.txt",
            "stems/ab/x.f32",
            "logs/a.log",
        ] {
            fs::write(old.join(f), f).unwrap();
        }
        let to = Folders {
            data: root.join("data"),
            config: root.join("config"),
            cache: root.join("cache"),
            state: root.join("state"),
        };
        adopt_from(&old, &to, &mut vec![]);

        for f in [
            "data/library.db",
            "data/library.db-wal",
            "config/keymap.txt",
        ] {
            assert!(root.join(f).is_file(), "{f}");
        }
        assert!(root.join("config/controllers").is_dir());
        assert_eq!(
            fs::read_to_string(root.join("cache/stems/ab/x.f32")).unwrap(),
            "stems/ab/x.f32"
        );
        assert!(root.join("state/logs/a.log").is_file());
        assert!(root.join("state/debug").is_dir());
        assert!(
            !root.join("cache/tempo-lock").exists(),
            "leftovers aren't kept"
        );
        assert!(!old.exists(), "the old folder is gone");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn nothing_already_in_the_new_place_is_overwritten() {
        let root = scratch("keep");
        let old = root.join("old");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("library.db"), "old").unwrap();
        let to = Folders::all_in(&root.join("new"));
        fs::create_dir_all(&to.data).unwrap();
        fs::write(to.data.join("library.db"), "new").unwrap();
        adopt_from(&old, &to, &mut vec![]);
        assert_eq!(
            fs::read_to_string(to.data.join("library.db")).unwrap(),
            "new"
        );
        assert_eq!(
            fs::read_to_string(old.join("library.db")).unwrap(),
            "old",
            "left, not lost"
        );
        fs::remove_dir_all(&root).unwrap();
    }
}
