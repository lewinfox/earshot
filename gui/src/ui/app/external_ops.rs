//! Other DJ programs' libraries (`core::external`): finding, reading and importing
//! them, each on a thread of its own; what comes of it arrives as
//! `Event::External`. Libraries found or set become sources (`core::sources`), and a
//! read's path rules are kept in its source's settings.

use std::path::PathBuf;

use super::*;
use crate::core::external::{self, ExternalCmd, ExternalEvent, Imported};
use crate::core::sources::SourceConfig;

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
                        .set_title("A USB stick, or a folder with a Rekordbox XML")
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
            ExternalCmd::SetLocation { kind } => {
                let sink = self.sink.clone();
                std::thread::spawn(move || {
                    if let Some(at) = rfd::FileDialog::new()
                        .set_title(format!("Where is the {} library?", kind.label()))
                        .pick_folder()
                    {
                        sink(Event::Command(ExternalCmd::Locate { kind, at }.into()));
                    }
                });
            }
            ExternalCmd::Locate { kind, at } => {
                std::thread::spawn(move || {
                    let result = external::find(Some(&at)).and_then(|found| {
                        found
                            .into_iter()
                            .find(|f| f.kind() == Some(kind))
                            .ok_or_else(|| {
                                format!("No {} library in {}", kind.label(), at.display())
                            })
                    });
                    send(ExternalEvent::Located { result })
                });
            }
            ExternalCmd::Read(source) => {
                let roots = self.roots();
                std::thread::spawn(move || {
                    let id = source.id;
                    let result = external::read(&source, &roots, false, |message| {
                        send(ExternalEvent::Progress {
                            source: id,
                            message,
                        })
                    });
                    send(ExternalEvent::Read {
                        source: id,
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
                let roots = self.roots();
                let (store, hasher) = (self.lib.store.clone(), self.hasher.clone());
                std::thread::spawn(move || {
                    let id = source.id;
                    let progress = |message| {
                        send(ExternalEvent::Progress {
                            source: id,
                            message,
                        })
                    };
                    let result = (|| -> Result<Imported, String> {
                        // Cues and grids weren't read for browsing: read again, and
                        // show what was read (its cues counted).
                        let lib = match read {
                            Some(lib) if !options.needs_cues() => lib,
                            _ => {
                                let lib =
                                    Arc::new(external::read(&source, &roots, true, progress)?);
                                send(ExternalEvent::Read {
                                    source: id,
                                    result: Ok(lib.clone()),
                                });
                                lib
                            }
                        };
                        let (mut save, missing) = external::plan(&source, &lib, &target, options);
                        send(ExternalEvent::Progress {
                            source: id,
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
                    send(ExternalEvent::Imported { source: id, result })
                });
            }
        }
    }

    /// Libraries found or set become sources; a read's path rules go into its
    /// source's settings. Then the pane hears of it.
    pub(super) fn external_event(&mut self, e: ExternalEvent) {
        match &e {
            // In the usual places: a program's library, unless it has one (or had,
            // and the user removed it).
            ExternalEvent::Found {
                at: None,
                result: Ok(found),
            } => {
                for f in found {
                    let Some(new) = f.to_source() else { continue };
                    let known = self.sources.iter().any(|s| s.kind == new.kind);
                    if new.kind.found_by_itself() && !known {
                        self.lib.store.send(StoreCmd::AddSource(new));
                    }
                }
            }
            // Added with Add…: every library there.
            ExternalEvent::Found {
                at: Some(_),
                result: Ok(found),
            } => {
                for new in found.iter().filter_map(|f| f.to_source()) {
                    self.lib.store.send(StoreCmd::AddSource(new));
                }
            }
            ExternalEvent::Located { result: Ok(found) } => {
                if let Some(new) = found.to_source() {
                    self.lib.store.send(StoreCmd::AddSource(new));
                }
            }
            ExternalEvent::Read {
                source,
                result: Ok(lib),
            } => {
                for w in &lib.warnings {
                    eprintln!("reading source {source}: {w}");
                }
                if let Some(s) = self.sources.iter().find(|s| s.id == *source)
                    && s.config.paths != lib.paths
                {
                    let mut s = s.clone();
                    s.config = SourceConfig {
                        paths: lib.paths.clone(),
                        ..s.config
                    };
                    self.lib.store.send(StoreCmd::UpdateSource(s));
                }
            }
            _ => {}
        }
        self.panels.library.external.event(e);
    }

    /// Where to start looking for files another program stores under other paths
    /// (it runs under Wine, or the library came from another computer): the music
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
