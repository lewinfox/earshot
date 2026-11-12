//! The Trim tab's half in the shell: the save dialog, the cut (on a thread of its own),
//! scanning the new file in, and the trim points over Prepare's waveform.

use std::path::{Path, PathBuf};

use super::*;
use crate::core::trim::{self, Source, TrimCmd, TrimEvent};
use crate::core::view::Tool;
use crate::ui::panels::deck::wave::Layer;
use crate::ui::panels::trim::TrimPoints;


impl EarshotApp {
    pub(super) fn trim_cmd(&mut self, c: TrimCmd) {
        match c {
            TrimCmd::SaveAs { track, start, end } => {
                let Some(src) = self.trim_source(track) else {
                    return;
                };
                // The dialog blocks, so on its own thread; where's picked comes back
                // as a command.
                let sink = self.sink.clone();
                std::thread::spawn(move || {
                    let mut d = rfd::FileDialog::new()
                        .set_title("Save the trimmed track")
                        .set_file_name(trim::default_name(&src.path));
                    if let Some(dir) = src.path.parent() {
                        d = d.set_directory(dir);
                    }
                    if let Some(to) = d.save_file() {
                        let cmd = TrimCmd::Save {
                            track,
                            start,
                            end,
                            to,
                        };
                        sink(Event::Command(cmd.into()));
                    }
                });
            }
            TrimCmd::Save {
                track,
                start,
                end,
                to,
            } => {
                let Some(src) = self.trim_source(track) else {
                    return;
                };
                self.panels.trim.busy = true;
                self.panels.trim.note = None;
                let in_library = self
                    .panels
                    .library
                    .browser
                    .folders
                    .iter()
                    .any(|f| to.starts_with(f));
                let job = self.jobs.start(format!("Trimming {}", src.title));
                let (store, hasher, sink) = (
                    self.lib.store.clone(),
                    self.hasher.clone(),
                    self.sink.clone(),
                );
                crate::core::background::spawn("earshot-trim", move || {
                    let _job = job;
                    let done = trim::cut(&src, start, end, &to).and_then(|()| {
                        if in_library {
                            library::scan_file(&store, &to).map_err(|e| e.to_string())?;
                            hasher.send(());
                        }
                        Ok(())
                    });
                    sink(Event::Trim(match done {
                        Ok(()) => TrimEvent::Saved {
                            track,
                            path: to,
                            in_library,
                        },
                        Err(error) => TrimEvent::Failed { track, error },
                    }));
                });
            }
        }
    }

    pub(super) fn trim_event(&mut self, e: TrimEvent) {
        if let TrimEvent::Saved {
            path,
            in_library: false,
            ..
        } = &e
        {
            let text = format!(
                "Saved {}. It isn't in a library folder, so it isn't in the library.",
                path.display()
            );
            self.notify(crate::core::notice::Level::Info, text);
        }
        self.panels.trim.on_event(e);
    }

    /// Once a frame: the trim points over Prepare's waveform, while the Trim tab is open.
    pub(super) fn sync_trim(&mut self) {
        let track = self
            .frame
            .snap
            .as_ref()
            .and_then(|s| s.deck(self.prefs.target()).track_id);
        let tab = &self.panels.trim;
        let layer = (self.prefs.mode == crate::core::view::Mode::Prepare
            && self.prefs.prepare.tool == Tool::Trim
            && track.is_some()
            && tab.track == track)
            .then(|| {
                Arc::new(TrimPoints {
                    start: tab.start,
                    end: tab.end,
                }) as Arc<dyn Layer>
            });
        self.panels.decks[self.prefs.target().index()].set_overlay("trim", layer);
    }

    /// The file behind `track`, from the library.
    fn trim_source(&mut self, track: i64) -> Option<Source> {
        let Some(t) = self.index.get(track) else {
            self.panels.trim.note = Some(Err("That track isn't in the library.".into()));
            return None;
        };
        let path = PathBuf::from(&t.path);
        if !Path::new(&path).is_file() {
            self.panels.trim.note = Some(Err(format!("{} isn't there.", path.display())));
            return None;
        }
        let duration = self
            .frame
            .snap
            .as_ref()
            .map(|s| s.deck(self.prefs.target()))
            .filter(|d| d.track_id == Some(track))
            .map_or(f64::INFINITY, |d| d.duration);
        Some(Source {
            path,
            title: t.title.clone(),
            duration,
        })
    }
}
