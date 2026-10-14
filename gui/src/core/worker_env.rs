//! Where the Python worker is and how to run it.
//!
//! An installed earshot (a release) ships it beside its binary, ready to run
//! (`packaging/bundle.sh`): a Python with every package installed (`python/`), the
//! worker's code (`worker/`), the Beat This! checkpoints (`models/`) and the programs
//! the tempo lock tool runs (`bin/`, `lib/`). On a Mac the same folders can be in an
//! app's `Contents/Resources`. Nothing is set up on the user's computer.
//!
//! Without a bundle (a build from a checkout) it is the checkout's `worker/`, whose
//! `.venv` `worker/setup.sh` makes. `EARSHOT_WORKER_DIR` names another such folder.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// How to start the worker: `python -m <module>` in `dir`, with `envs` added.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkerEnv {
    pub python: PathBuf,
    pub dir: PathBuf,
    pub envs: Vec<(String, String)>,
    /// From a bundle (not a checkout).
    pub bundled: bool,
}

impl WorkerEnv {
    /// `EARSHOT_WORKER_DIR`, else the bundle beside this program, else this checkout's
    /// `worker/`.
    pub fn find() -> WorkerEnv {
        if let Some(dir) = std::env::var_os("EARSHOT_WORKER_DIR") {
            return WorkerEnv::checkout(PathBuf::from(dir));
        }
        if let Some(root) = std::env::current_exe()
            .ok()
            .and_then(|exe| bundle_root(&exe))
        {
            return WorkerEnv::bundle(&root, std::env::var_os("PATH"));
        }
        WorkerEnv::checkout(Path::new(env!("CARGO_MANIFEST_DIR")).join("../worker"))
    }

    /// A worker folder with its own `.venv` (`worker/setup.sh`).
    pub fn checkout(dir: PathBuf) -> WorkerEnv {
        let python = if cfg!(windows) {
            dir.join(".venv/Scripts/python.exe")
        } else {
            dir.join(".venv/bin/python")
        };
        WorkerEnv {
            python,
            dir,
            envs: vec![],
            bundled: false,
        }
    }

    /// The bundle in `root`. Its programs come before `path` (the system's), and its
    /// own folders are the only ones Python looks in: not the user's site-packages,
    /// and nothing written into the install (which may be read-only).
    pub fn bundle(root: &Path, path: Option<OsString>) -> WorkerEnv {
        let python = if cfg!(windows) {
            root.join("python/python.exe")
        } else {
            root.join("python/bin/python3")
        };
        let dir = root.join("worker");
        let path = std::env::join_paths(
            std::iter::once(root.join("bin")).chain(path.iter().flat_map(std::env::split_paths)),
        )
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
        let s = |p: &Path| p.to_string_lossy().into_owned();
        WorkerEnv {
            envs: vec![
                ("PYTHONPATH".into(), s(&dir)),
                ("PYTHONNOUSERSITE".into(), "1".into()),
                ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
                // Beat This! loads its checkpoints from torch's cache, found here.
                ("TORCH_HOME".into(), s(&root.join("models"))),
                ("PATH".into(), path),
            ],
            python,
            dir,
            bundled: true,
        }
    }

    /// Why it can't run, if it can't: what the user should do about it.
    pub fn problem(&self) -> Option<String> {
        if self.python.exists() {
            None
        } else if self.bundled {
            Some(format!(
                "the analysis worker is missing from this install ({}): reinstall earshot",
                self.python.display()
            ))
        } else {
            Some(format!(
                "the analysis worker isn't set up: run {}/setup.sh",
                self.dir
                    .canonicalize()
                    .unwrap_or(self.dir.clone())
                    .display()
            ))
        }
    }
}

/// The folder with the bundle, for a program at `exe`: beside it, or in a Mac app's
/// `Resources`.
fn bundle_root(exe: &Path) -> Option<PathBuf> {
    let here = exe.parent()?;
    [here.to_path_buf(), here.join("../Resources")]
        .into_iter()
        .find(|r| r.join("python").is_dir() && r.join("worker").is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bundle_beside_the_program_is_found_and_its_programs_come_first() {
        let root = std::env::temp_dir().join(format!("earshot-bundle-{}", std::process::id()));
        let exe = root.join("earshot-gui");
        assert_eq!(bundle_root(&exe), None, "no bundle yet");
        for d in ["python/bin", "worker"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        assert_eq!(bundle_root(&exe), Some(root.clone()));

        let env = WorkerEnv::bundle(&root, Some("/usr/bin:/bin".into()));
        let get = |k: &str| env.envs.iter().find(|(n, _)| n == k).unwrap().1.clone();
        assert_eq!(
            get("PATH"),
            format!("{}:/usr/bin:/bin", root.join("bin").display())
        );
        assert_eq!(get("PYTHONPATH"), root.join("worker").to_string_lossy());
        let missing = env.problem().unwrap();
        assert!(missing.contains("reinstall"), "{missing}");
        std::fs::write(root.join("python/bin/python3"), "").unwrap();
        assert_eq!(env.problem(), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_checkout_without_its_venv_says_to_run_setup() {
        let env = WorkerEnv::checkout("/no/such/worker".into());
        assert!(env.problem().unwrap().contains("setup.sh"));
        assert!(env.envs.is_empty());
    }
}
