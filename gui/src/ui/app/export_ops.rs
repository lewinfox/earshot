//! USB sticks (`core::drives`) and writing to other DJ programs' libraries and
//! sticks (`core::export`). Jobs run on threads of their own and hold the stick
//! they use ([`Uses`]), so its light is red until they're done and it's flushed;
//! what comes of them arrives as `Event::Export`, for the Export tab and the stick
//! view.

use std::path::Path;

use super::*;
use crate::core::drives::{self, Drive, Hold};
use crate::core::export::{self, ExportCmd, ExportEvent};
use crate::core::sources::{NewSource, SourceConfig};

impl EarshotApp {
    /// The stick `p` is on, held for a job.
    pub(super) fn hold_drive(&self, p: &Path) -> Option<Hold> {
        self.drives
            .iter()
            .find(|d| d.holds(p))
            .map(|d| self.uses.hold(&d.path))
    }

    /// The sticks plugged in changed: each library on one becomes a source (once),
    /// listed under its stick.
    pub(super) fn drives_changed(&mut self, list: Vec<Drive>) {
        for d in &list {
            for &kind in &d.libraries {
                let (location, volume_root) = d.library_location(kind);
                let known = self
                    .sources
                    .iter()
                    .any(|s| s.kind == kind && s.location.as_deref() == Some(location.as_path()));
                if known || !self.sticks_added.insert((kind, location.clone())) {
                    continue;
                }
                self.lib.store.send(StoreCmd::AddSource(NewSource {
                    kind,
                    name: d.label.clone(),
                    location: Some(location),
                    config: SourceConfig {
                        volume_root,
                        ..SourceConfig::default()
                    },
                }));
            }
        }
        self.panels.library.external.set_drives(list.clone());
        self.drives = list;
    }

    pub(super) fn export_event(&mut self, e: ExportEvent) {
        self.panels.library.external.export_event(&e);
        self.panels.export.event(e);
    }

    pub(super) fn export_cmd(&mut self, c: ExportCmd) {
        let sink = self.sink.clone();
        let send = move |e: ExportEvent| sink(Event::Export(e));
        match c {
            ExportCmd::PickFolder => {
                std::thread::spawn(move || {
                    if let Some(dir) = rfd::FileDialog::new()
                        .set_title("The stick, or a folder to write into")
                        .pick_folder()
                    {
                        send(ExportEvent::Picked(dir));
                    }
                });
            }
            ExportCmd::PickXml => {
                std::thread::spawn(move || {
                    if let Some(file) = rfd::FileDialog::new()
                        .set_title("Save the Rekordbox XML as")
                        .set_file_name("rekordbox.xml")
                        .add_filter("Rekordbox XML", &["xml"])
                        .save_file()
                    {
                        send(ExportEvent::Picked(file));
                    }
                });
            }
            ExportCmd::Convert(source) => {
                self.set_mode(Mode::Prepare);
                self.dispatch(ViewCmd::Tool(crate::core::view::Tool::Export).into());
                self.panels.export.convert(&source);
            }
            ExportCmd::Run(x) => {
                let roots = self.roots();
                let reader = self.lib.store.reader.clone();
                let mut holds: Vec<Hold> = self.hold_drive(&x.target.path).into_iter().collect();
                // Reading from another stick holds that one too.
                if let export::From::Library { source, .. } = &x.from
                    && let Some(at) = &source.location
                    && !self
                        .drives
                        .iter()
                        .any(|d| d.holds(at) && d.holds(&x.target.path))
                {
                    holds.extend(self.hold_drive(at));
                }
                crate::core::log::note(&format!(
                    "export to {:?} {} (replace: {}, merge: {}), {:?}",
                    x.target.kind,
                    x.target.path.display(),
                    x.replace,
                    x.merge,
                    x.options
                ));
                let sink = self.sink.clone();
                let to = self
                    .drives
                    .iter()
                    .find(|d| d.holds(&x.target.path))
                    .map_or_else(|| x.target.kind.label().to_string(), |d| d.label.clone());
                let job = self.jobs.start(format!("Exporting to {to}"));
                std::thread::spawn(move || {
                    let progress = |m: String| {
                        job.report(&m);
                        send(ExportEvent::Progress(m))
                    };
                    let result = export::run(&reader, &x, &roots, progress);
                    drop(job);
                    match &result {
                        Ok(w) => crate::core::log::note(&format!("exported: {w:?}")),
                        Err(e) => crate::core::log::note(&format!("export failed: {e}")),
                    }
                    // Flushed (red until then), and what's on the sticks looked at again.
                    drop(holds);
                    sink(Event::Drives(drives::list()));
                    send(ExportEvent::Done(result));
                });
            }
        }
    }
}
