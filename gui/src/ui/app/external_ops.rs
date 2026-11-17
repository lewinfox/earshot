//! Other DJ programs' libraries (`core::external`): finding, reading and importing
//! them, each on a thread of its own; what comes of it arrives as
//! `Event::External`. Libraries found or set become sources (`core::sources`), and a
//! read's path rules are kept in its source's settings.

use std::collections::HashMap;
use std::path::PathBuf;

use super::*;
use crate::core::drives::{self, Drive};
use crate::core::external::{self, ExternalCmd, ExternalEvent, Imported};
use crate::core::outside::OutsideTrack;
use crate::core::sources::{Source, SourceConfig, SourceId};
use crate::core::track::TrackId;
use crate::ui::panels::library::external::Status;

impl EarshotApp {
    pub(super) fn external_cmd(&mut self, c: ExternalCmd) {
        let sink = self.sink.clone();
        let send = move |e: ExternalEvent| sink(Event::External(e));
        match c {
            ExternalCmd::Find => {
                let job = self.jobs.start("Looking for DJ libraries");
                crate::core::background::spawn("earshot-external", move || {
                    let _job = job;
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
                crate::core::background::spawn("earshot-external", move || {
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
                crate::core::background::spawn("earshot-external", move || {
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
                let job = self.jobs.start(format!("Reading {}", source.name));
                crate::core::background::spawn("earshot-external", move || {
                    let id = source.id;
                    let result = external::read(&source, &roots, false, |message| {
                        job.report(&message);
                        send(ExternalEvent::Progress {
                            source: id,
                            message,
                        })
                    });
                    let ok = result.is_ok();
                    send(ExternalEvent::Read {
                        source: id,
                        result: result.map(Arc::new),
                    });
                    // Then again with cues and grids, for playing its tracks: slower
                    // (Serato keeps them in the audio files), so the list comes first.
                    job.say("cues and beat grids");
                    job.uncounted();
                    if ok && let Ok(lib) = external::read(&source, &roots, true, |m| job.report(&m))
                    {
                        send(ExternalEvent::Read {
                            source: id,
                            result: Ok(Arc::new(lib)),
                        });
                    }
                });
            }
            ExternalCmd::Import {
                source,
                target,
                options,
                read,
                copy_to,
            } => {
                let roots = self.roots();
                let (store, hasher) = (self.lib.store.clone(), self.hasher.clone());
                // Off a stick: copied into `copy_to` under the stick's name, and the
                // stick held (red) meanwhile.
                let stick = source.location.as_deref().and_then(|at| {
                    self.drives
                        .iter()
                        .find(|d| d.holds(at))
                        .map(|d| (d.path.clone(), d.label.clone()))
                });
                let hold = source
                    .location
                    .as_deref()
                    .and_then(|at| self.hold_drive(at));
                let sink = self.sink.clone();
                crate::core::log::note(&format!(
                    "import from {} ({:?}): {target:?}, {options:?}",
                    source.name, source.location
                ));
                let job = self.jobs.start(format!("Importing from {}", source.name));
                crate::core::background::spawn("earshot-external", move || {
                    let id = source.id;
                    let progress = |message: String| {
                        job.report(&message);
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
                                    Arc::new(external::read(&source, &roots, true, &progress)?);
                                send(ExternalEvent::Read {
                                    source: id,
                                    result: Ok(lib.clone()),
                                });
                                lib
                            }
                        };
                        let (mut save, missing) = external::plan(&source, &lib, &target, options);
                        let mut copied = None;
                        if let Some(to) = &copy_to {
                            let (stick, label) =
                                stick.clone().ok_or("the stick isn't plugged in any more")?;
                            let files: Vec<PathBuf> =
                                save.tracks.iter().map(|t| t.file.clone()).collect();
                            let (now_at, report) = drives::copy_in(
                                &store.reader,
                                &files,
                                &stick,
                                &label,
                                to,
                                |n, of| {
                                    progress(format!("Copying {}/{of}", n + 1));
                                    job.progress(n as u64, of as u64);
                                },
                            );
                            save.tracks.retain_mut(|t| match now_at.get(&t.file) {
                                Some(p) => {
                                    t.file = p.clone();
                                    true
                                }
                                None => false,
                            });
                            copied = Some(report);
                        }
                        progress(format!("reading {} files' tags", save.tracks.len()));
                        let files: Vec<PathBuf> =
                            save.tracks.iter().map(|t| t.file.clone()).collect();
                        save.files =
                            library::changed_files(&store, &files).map_err(|e| e.to_string())?;
                        let mut done = store.import(save).map_err(|e| e.to_string())?;
                        done.missing = missing;
                        done.copied = copied;
                        Ok(done)
                    })();
                    drop(hold);
                    // The copies are in a music folder: it takes them as its own.
                    if copy_to.is_some() {
                        sink(Event::Command(LibraryCmd::Rescan.into()));
                    }
                    // New files need their audio hashes worked out.
                    hasher.send(());
                    send(ExternalEvent::Imported { source: id, result })
                });
            }
            ExternalCmd::CopyIn { files, drive, to } => self.copy_in(files, drive, to),
            ExternalCmd::ImportThen { tracks, then } => {
                if self.settings.import_without_asking {
                    self.import_outside(tracks, *then);
                } else {
                    self.panels.library.external.confirm_import(tracks, *then);
                }
            }
            ExternalCmd::ImportNow {
                tracks,
                then,
                remember,
            } => {
                if remember {
                    self.settings.import_without_asking = true;
                    self.lib.store.send(crate::core::settings::save(
                        settings::IMPORT_WITHOUT_ASKING,
                        &true,
                    ));
                }
                self.import_outside(tracks, *then);
            }
            ExternalCmd::AskImport(ids) => {
                // A list holds one library's tracks; the first track's library it is.
                let outside = &self.lib.store.outside;
                let tracks: Vec<_> = ids.iter().filter_map(|&id| outside.get(id)).collect();
                if let Some(first) = tracks.first() {
                    let source = first.source;
                    let ext = tracks
                        .iter()
                        .filter(|t| t.source == source)
                        .map(|t| t.external_id.clone())
                        .collect();
                    self.panels.library.external.ask_import(source, ext);
                }
            }
            ExternalCmd::Format { drive, label, then } => {
                crate::core::log::note(&format!(
                    "format {} ({}) as {label}",
                    drive.path.display(),
                    drive.device
                ));
                let hold = self.uses.hold(&drive.path);
                let sink = self.sink.clone();
                let job = self.jobs.start(format!("Formatting {}", drive.label));
                crate::core::background::spawn("earshot-external", move || {
                    let result = drives::format(&drive, &label);
                    drop(job);
                    match &result {
                        Ok(at) => crate::core::log::note(&format!("formatted: {}", at.display())),
                        Err(e) => crate::core::log::note(&format!("format failed: {e}")),
                    }
                    drop(hold);
                    // The stick as it is now, before anything is written to it.
                    sink(Event::Drives(drives::list()));
                    send(ExternalEvent::Formatted {
                        drive: drive.path,
                        result,
                        then,
                    });
                });
            }
            ExternalCmd::Eject(drive) => {
                let job = self.jobs.start(format!("Ejecting {}", drive.label));
                crate::core::background::spawn("earshot-external", move || {
                    let result = drives::eject(&drive);
                    drop(job);
                    send(ExternalEvent::Ejected {
                        drive: drive.path,
                        result,
                    })
                });
            }
        }
    }

    /// Copy files off a stick into a music folder, then scan it.
    fn copy_in(&mut self, files: Vec<PathBuf>, drive: Drive, to: PathBuf) {
        let reader = self.lib.store.reader.clone();
        let hold = self.uses.hold(&drive.path);
        let sink = self.sink.clone();
        let job = self.jobs.start(format!("Copying from {}", drive.label));
        crate::core::background::spawn("earshot-external", move || {
            let send = |e| sink(Event::External(e));
            let files = drives::audio_files(&files);
            let (_, report) =
                drives::copy_in(&reader, &files, &drive.path, &drive.label, &to, |n, of| {
                    job.progress(n as u64, of as u64);
                    send(ExternalEvent::CopyProgress(format!(
                        "Copying {}/{of}",
                        n + 1
                    )))
                });
            drop(job);
            drop(hold);
            sink(Event::Command(LibraryCmd::Rescan.into()));
            send(ExternalEvent::Copied(Ok(report)));
        });
    }

    /// Libraries found or set become sources; a read's path rules go into its
    /// source's settings. Then the pane hears of it.
    pub(crate) fn external_event(&mut self, e: ExternalEvent) {
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
            ExternalEvent::Relist(source) => {
                if let Some(Status::Ready(lib)) = self.panels.library.external.status(*source) {
                    let lib = lib.clone();
                    self.list_outside(*source, &lib);
                }
            }
            ExternalEvent::Imported { source, result } => match result {
                Ok(r) => {
                    crate::core::log::note(&format!("import from source {source}: {r:?}"));
                    // Its tracks imported are earshot's now, in its list too.
                    if let Some(Status::Ready(lib)) = self.panels.library.external.status(*source) {
                        let lib = lib.clone();
                        self.list_outside(*source, &lib);
                    }
                }
                Err(e) => {
                    crate::core::log::note(&format!("import from source {source} failed: {e}"))
                }
            },
            ExternalEvent::Read {
                source,
                result: Err(e),
            } => crate::core::log::note(&format!("reading source {source} failed: {e}")),
            ExternalEvent::Read {
                source,
                result: Ok(lib),
            } => {
                let missing = lib.tracks.iter().filter(|t| t.file.is_none()).count();
                crate::core::log::note(&format!(
                    "read source {source}: {} tracks ({missing} not found here), path rules {:?}",
                    lib.tracks.len(),
                    lib.paths
                ));
                for w in &lib.warnings {
                    eprintln!("reading source {source}: {w}");
                }
                self.list_outside(*source, lib);
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
        let e = match e {
            ExternalEvent::Formatted {
                drive,
                result: Ok(at),
                then: Some(mut x),
            } => {
                x.target.path = at.clone();
                self.panels.library.external.writing_to(&at);
                self.dispatch(crate::core::export::ExportCmd::Run(x).into());
                ExternalEvent::Formatted {
                    drive,
                    result: Ok(at),
                    then: None,
                }
            }
            e => e,
        };
        self.panels.library.external.event(e);
    }

    /// A library's tracks become usable like earshot's: each one earshot has (the same
    /// file) is that track; the others are outside tracks (`core::outside`), in the
    /// index but not the library.
    fn list_outside(&mut self, source: SourceId, lib: &external::ExtLibrary) {
        let on_stick = self
            .sources
            .iter()
            .find(|s| s.id == source)
            .is_some_and(|s| s.kind.on_stick());
        let outside = self.lib.store.outside.clone();
        let reader = self.lib.store.reader.clone();
        let (mut ids, mut rows) = (HashMap::new(), vec![]);
        for t in &lib.tracks {
            let Some(file) = &t.file else { continue };
            if let Ok(Some(id)) = reader.track_at(file) {
                ids.insert(t.id.clone(), id);
            } else if let Some(id) = outside.register(source, t, on_stick) {
                ids.insert(t.id.clone(), id);
                rows.extend(outside.get(id).map(|o| o.lib_track()));
            }
        }
        self.index.put_outside(rows);
        self.panels.library.external.set_ids(source, ids);
    }

    /// Import outside tracks (each from the library it was read from: its grid, cues
    /// and metadata come too; a stick's is copied into the first music folder), then
    /// carry out `then` with them as earshot's tracks.
    fn import_outside(&mut self, tracks: Vec<TrackId>, then: Command) {
        let outside = self.lib.store.outside.clone();
        let mut jobs: Vec<(Source, Arc<external::ExtLibrary>, Vec<OutsideTrack>)> = vec![];
        for t in tracks.iter().filter_map(|&id| outside.get(id)) {
            if let Some(job) = jobs.iter_mut().find(|j| j.0.id == t.source) {
                job.2.push(t);
                continue;
            }
            let source = self.sources.iter().find(|s| s.id == t.source).cloned();
            let lib = match self.panels.library.external.status(t.source) {
                Some(Status::Ready(lib)) => Some(lib.clone()),
                _ => None,
            };
            if let (Some(source), Some(lib)) = (source, lib) {
                jobs.push((source, lib, vec![t]));
            }
        }
        let music = self
            .panels
            .library
            .browser
            .folders
            .first()
            .map(PathBuf::from);
        let sticks: Vec<(PathBuf, String)> = self
            .drives
            .iter()
            .map(|d| (d.path.clone(), d.label.clone()))
            .collect();
        let holds: Vec<_> = jobs
            .iter()
            .filter_map(|j| j.0.location.as_deref())
            .filter_map(|at| self.hold_drive(at))
            .collect();
        let (store, hasher, sink) = (
            self.lib.store.clone(),
            self.hasher.clone(),
            self.sink.clone(),
        );
        let job = self.jobs.start("Importing tracks");
        crate::core::background::spawn("earshot-external", move || {
            let mut now: HashMap<TrackId, TrackId> = HashMap::new();
            let mut copied_any = false;
            for (source, lib, tracks) in jobs {
                let ext: Vec<String> = tracks.iter().map(|t| t.external_id.clone()).collect();
                let options = external::ImportOptions {
                    memory_cues: source.kind.has_memory_cues(),
                    ..external::ImportOptions::default()
                };
                let (mut save, _) =
                    external::plan(&source, &lib, &external::Target::Tracks(ext), options);
                let mut moved: HashMap<PathBuf, PathBuf> = HashMap::new();
                if source.kind.on_stick() {
                    let stick = source
                        .location
                        .as_deref()
                        .and_then(|at| sticks.iter().find(|(p, _)| at.starts_with(p)));
                    let (Some((stick, label)), Some(to)) = (stick, &music) else {
                        crate::core::log::note("import from a stick: no stick or no music folder");
                        continue;
                    };
                    let files: Vec<PathBuf> = save.tracks.iter().map(|t| t.file.clone()).collect();
                    job.say(format!("copying from {label}"));
                    let (at, _) =
                        drives::copy_in(&store.reader, &files, stick, label, to, |n, of| {
                            job.progress(n as u64, of as u64)
                        });
                    for t in &mut save.tracks {
                        if let Some(p) = at.get(&t.file) {
                            moved.insert(t.file.clone(), p.clone());
                            t.file = p.clone();
                        }
                    }
                    copied_any = true;
                }
                let files: Vec<PathBuf> = save.tracks.iter().map(|t| t.file.clone()).collect();
                let result = library::changed_files(&store, &files)
                    .map_err(|e| e.to_string())
                    .and_then(|f| {
                        save.files = f;
                        store.import(save).map_err(|e| e.to_string())
                    });
                if let Err(e) = result {
                    crate::core::log::note(&format!("importing from {}: {e}", source.name));
                    continue;
                }
                for t in &tracks {
                    let at = moved.get(&t.file).unwrap_or(&t.file);
                    if let Ok(Some(id)) = store.reader.track_at(at) {
                        now.insert(t.id, id);
                    }
                }
                sink(Event::External(ExternalEvent::Relist(source.id)));
            }
            drop(holds);
            hasher.send(());
            if copied_any {
                sink(Event::Command(LibraryCmd::Rescan.into()));
            }
            sink(Event::Command(
                then.map_tracks(|id| now.get(&id).copied().unwrap_or(id)),
            ));
        });
    }

    /// Where to start looking for files another program stores under other paths
    /// (it runs under Wine, or the library came from another computer): the music
    /// folders, and the system's Music folder. The search widens from there.
    pub(super) fn roots(&self) -> Vec<PathBuf> {
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
