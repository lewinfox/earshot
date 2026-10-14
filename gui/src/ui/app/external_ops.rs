//! Other DJ programs' libraries (`core::external`): finding, reading and importing
//! them, each on a thread of its own; what comes of it arrives as
//! `Event::External`, for the library pane's "Other libraries". Where the user put
//! each library, and the path rules that find its files, are kept in the settings
//! (`settings::EXTERNAL`).

use std::path::PathBuf;

use super::*;
use crate::core::external::{self, ExternalCmd, ExternalEvent, Imported, PROGRAMS};

impl EarshotApp {
    pub(super) fn external_cmd(&mut self, c: ExternalCmd) {
        let sink = self.sink.clone();
        let send = move |e: ExternalEvent| sink(Event::External(e));
        match c {
            ExternalCmd::Find => {
                std::thread::spawn(move || {
                    send(ExternalEvent::Found {
                        at: None,
                        result: external::find(None),
                    })
                });
            }
            ExternalCmd::Pick => {
                let sink = self.sink.clone();
                std::thread::spawn(move || {
                    if let Some(dir) = rfd::FileDialog::new()
                        .set_title("A DJ library's folder, or a drive")
                        .pick_folder()
                    {
                        sink(Event::Command(ExternalCmd::Detect(dir).into()));
                    }
                });
            }
            ExternalCmd::Detect(at) => {
                std::thread::spawn(move || {
                    let result = external::find(Some(&at));
                    send(ExternalEvent::Found {
                        at: Some(at),
                        result,
                    })
                });
            }
            ExternalCmd::SetLocation { format } => {
                let sink = self.sink.clone();
                let name = program_name(&format).to_string();
                std::thread::spawn(move || {
                    if let Some(at) = rfd::FileDialog::new()
                        .set_title(format!("Where is the {name} library?"))
                        .pick_folder()
                    {
                        sink(Event::Command(ExternalCmd::Locate { format, at }.into()));
                    }
                });
            }
            ExternalCmd::Locate { format, at } => {
                std::thread::spawn(move || {
                    let result = external::find(Some(&at)).and_then(|found| {
                        found
                            .into_iter()
                            .find(|s| s.format == format)
                            .ok_or_else(|| {
                                format!("No {} library in {}", program_name(&format), at.display())
                            })
                    });
                    send(ExternalEvent::Located { format, result })
                });
            }
            ExternalCmd::Forget(source) => {
                self.settings.external.locations.retain(|s| *s != source);
                self.save_external();
            }
            ExternalCmd::Read(source) => {
                let (roots, paths) = (self.roots(), self.path_rules(&source));
                std::thread::spawn(move || {
                    let s = source.clone();
                    let result = external::read(&source, &roots, &paths, false, |message| {
                        send(ExternalEvent::Progress {
                            source: s.clone(),
                            message,
                        })
                    });
                    send(ExternalEvent::Read {
                        source,
                        result: result.map(Arc::new),
                    })
                });
            }
            ExternalCmd::Import {
                source,
                target,
                options,
                read,
            } => {
                let (roots, paths) = (self.roots(), self.path_rules(&source));
                let (store, hasher) = (self.lib.store.clone(), self.hasher.clone());
                std::thread::spawn(move || {
                    let s = source.clone();
                    let progress = |message| {
                        send(ExternalEvent::Progress {
                            source: s.clone(),
                            message,
                        })
                    };
                    let result = (|| -> Result<Imported, String> {
                        // Cues and grids weren't read for browsing: read again, and
                        // show what was read (its cues counted).
                        let lib = match read {
                            Some(lib) if !options.needs_cues() => lib,
                            _ => {
                                let lib = Arc::new(external::read(
                                    &source, &roots, &paths, true, progress,
                                )?);
                                send(ExternalEvent::Read {
                                    source: source.clone(),
                                    result: Ok(lib.clone()),
                                });
                                lib
                            }
                        };
                        let (mut save, missing) = external::plan(&source, &lib, &target, options);
                        send(ExternalEvent::Progress {
                            source: source.clone(),
                            message: format!("reading {} files' tags", save.tracks.len()),
                        });
                        let files: Vec<PathBuf> =
                            save.tracks.iter().map(|t| t.file.clone()).collect();
                        save.files =
                            library::changed_files(&store, &files).map_err(|e| e.to_string())?;
                        let mut done = store.import(save).map_err(|e| e.to_string())?;
                        done.missing = missing;
                        Ok(done)
                    })();
                    // New files need their audio hashes worked out.
                    let _ = hasher.send(());
                    send(ExternalEvent::Imported { source, result })
                });
            }
        }
    }

    /// Keep what an event says about where libraries and their files are, then pass
    /// it to the pane.
    pub(super) fn external_event(&mut self, e: ExternalEvent) {
        if let ExternalEvent::Read {
            source,
            result: Ok(lib),
        } = &e
        {
            for w in &lib.warnings {
                eprintln!("reading {}: {w}", source.name);
            }
        }
        let ext = &mut self.settings.external;
        let changed = match &e {
            // Added with Add…: listed from now on.
            ExternalEvent::Found {
                at: Some(_),
                result: Ok(found),
            } => {
                let before = ext.locations.len();
                for s in found {
                    if !ext.locations.contains(s) {
                        ext.locations.push(s.clone());
                    }
                }
                ext.locations.len() != before
            }
            ExternalEvent::Located {
                format,
                result: Ok(source),
            } => {
                ext.locations.retain(|s| s.format != *format);
                ext.locations.push(source.clone());
                true
            }
            ExternalEvent::Read {
                source,
                result: Ok(lib),
            } => {
                let key = source.key();
                if lib.paths.is_empty() {
                    ext.paths.remove(&key).is_some()
                } else if ext.paths.get(&key) != Some(&lib.paths) {
                    ext.paths.insert(key, lib.paths.clone());
                    true
                } else {
                    false
                }
            }
            _ => false,
        };
        if changed {
            self.save_external();
        }
        self.panels.library.external.event(e);
    }

    /// Save the settings for other libraries, and show the pane where they are.
    pub(super) fn save_external(&mut self) {
        let ext = &self.settings.external;
        self.lib.store.send(settings::save(settings::EXTERNAL, ext));
        self.panels
            .library
            .external
            .set_locations(ext.locations.clone());
    }

    /// A library's path rules, as last found.
    fn path_rules(&self, source: &external::Source) -> external::PathRules {
        self.settings
            .external
            .paths
            .get(&source.key())
            .cloned()
            .unwrap_or_default()
    }

    /// Where to start looking for files another program stores under other paths
    /// (it runs under Wine, or the library came from another computer): the library
    /// folders, and the system's Music folder. The search widens from there.
    fn roots(&self) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = self
            .panels
            .library
            .browser
            .folders
            .iter()
            .map(PathBuf::from)
            .collect();
        if let Some(music) =
            directories::UserDirs::new().and_then(|d| d.audio_dir().map(PathBuf::from))
            && !roots.contains(&music)
        {
            roots.push(music);
        }
        roots
    }
}

/// "Rekordbox", "Serato", "Mixxx" for a format, else the format.
pub(crate) fn program_name(format: &str) -> &str {
    PROGRAMS
        .iter()
        .find(|p| p.0 == format)
        .map_or(format, |p| p.1)
}
