//! Prepare's Export tab: write playlists to another DJ program's library or a USB
//! stick (`core::export`). From: earshot's playlists, crates and Flows, or another
//! library from the Sources pane (all of it, or some playlists). To: a Rekordbox or
//! Serato stick, Serato or Mixxx on this computer, or a Rekordbox XML. "Convert this
//! stick" is the shortcut for a whole stick to the other format, on the same stick:
//! the old library is moved aside. Anything else goes onto a stick only when it's
//! ready for that kind of library (`Drive::needs_format`), else it's formatted first.

use std::collections::BTreeSet;
use std::path::PathBuf;

use egui::{RichText, Ui};

use super::library::external::{ExternalUi, Status};
use super::{Cx, Panel};
use crate::core::collections::{Collection, CollectionId, CollectionKind, Collections, Section};
use crate::core::drives::Drive;
use crate::core::export::{
    Export, ExportCmd, ExportEvent, ExportOptions, From, Target, TargetKind, Written, in_the_way,
    picked,
};
use crate::core::external::{ExternalCmd, Node, NodeKind, NodePath};
use crate::core::sources::{Source, SourceId, SourceKind};
use crate::ui::theme::THEME;
use crate::ui::widgets::{Button, Note, busy_line, heading, muted};

/// What the export takes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum FromChoice {
    #[default]
    Earshot,
    Library(SourceId),
}

pub struct ExportUi {
    from: FromChoice,
    /// earshot's collections ticked.
    picked: BTreeSet<CollectionId>,
    /// Another library's folders and playlists ticked; none: all of it.
    nodes: BTreeSet<NodePath>,
    kind: TargetKind,
    /// The stick (USB kinds).
    drive: Option<PathBuf>,
    /// Where the XML goes.
    xml: Option<PathBuf>,
    options: ExportOptions,
    /// The user agreed to move another program's library on the stick aside.
    replace: bool,
    /// Add to the library on the stick, rather than replace it.
    merge: bool,
    /// An export running, and what it's doing.
    running: Option<String>,
    report: Option<Result<Written, String>>,
}

impl Default for ExportUi {
    fn default() -> Self {
        ExportUi {
            from: FromChoice::default(),
            picked: BTreeSet::new(),
            nodes: BTreeSet::new(),
            kind: TargetKind::RekordboxUsb,
            drive: None,
            xml: None,
            options: ExportOptions::for_kind(TargetKind::RekordboxUsb),
            replace: false,
            merge: true,
            running: None,
            report: None,
        }
    }
}

impl ExportUi {
    pub fn event(&mut self, e: ExportEvent) {
        match e {
            ExportEvent::Picked(p) => match self.kind {
                TargetKind::RekordboxXml => self.xml = Some(p),
                _ => self.set_drive(Some(p)),
            },
            ExportEvent::Progress(m) => {
                if let Some(r) = &mut self.running {
                    *r = m;
                }
            }
            ExportEvent::Done(r) => {
                self.running = None;
                self.replace = false;
                self.report = Some(r);
            }
        }
    }

    fn set_kind(&mut self, kind: TargetKind) {
        if kind != self.kind {
            self.kind = kind;
            self.options = ExportOptions::for_kind(kind);
            self.replace = false;
        }
    }

    fn set_drive(&mut self, drive: Option<PathBuf>) {
        if drive != self.drive {
            self.drive = drive;
            self.replace = false;
        }
    }

    /// Set up converting a stick's library (`source`) to the other format.
    pub fn convert(&mut self, source: &Source) {
        let (Some(kind), Some(drive)) = (TargetKind::converting(source.kind), stick_of(source))
        else {
            return;
        };
        self.from = FromChoice::Library(source.id);
        self.nodes.clear();
        self.set_kind(kind);
        self.set_drive(Some(drive));
    }
}

/// The tab: what to export on the left, where to on the right.
pub struct ExportPanel<'a> {
    pub tab: &'a mut ExportUi,
    pub external: &'a mut ExternalUi,
}

impl Panel for ExportPanel<'_> {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let libraries = self.external.libraries();
        ui.columns(2, |cols| {
            egui::ScrollArea::vertical()
                .id_salt("export-from")
                .show(&mut cols[0], |ui| {
                    heading(ui, "From");
                    self.from(ui, &libraries, cx);
                });
            egui::ScrollArea::vertical()
                .id_salt("export-to")
                .show(&mut cols[1], |ui| {
                    heading(ui, "To");
                    self.to(ui, &libraries, cx);
                });
        });
    }
}

impl ExportPanel<'_> {
    // ---- from ---------------------------------------------------------------------

    fn from(&mut self, ui: &mut Ui, libraries: &[Source], cx: &mut Cx) {
        let tab = &mut *self.tab;
        let name = |c: FromChoice| match c {
            FromChoice::Earshot => "earshot".to_string(),
            FromChoice::Library(id) => libraries
                .iter()
                .find(|s| s.id == id)
                .map_or_else(|| "?".into(), |s| s.name.clone()),
        };
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("export-from-source")
                .selected_text(name(tab.from))
                .show_ui(ui, |ui| {
                    let mut choice = tab.from;
                    ui.selectable_value(&mut choice, FromChoice::Earshot, "earshot");
                    for s in libraries {
                        ui.selectable_value(&mut choice, FromChoice::Library(s.id), &s.name);
                    }
                    if choice != tab.from {
                        tab.from = choice;
                        tab.nodes.clear();
                    }
                });
            if ui
                .button("Add a stick or library…")
                .on_hover_text("It's added to the Sources pane too")
                .clicked()
            {
                cx.send(ExternalCmd::Pick);
            }
        });
        ui.add_space(4.0);
        match tab.from {
            FromChoice::Earshot => {
                let cs = cx.core.collections;
                if tab.picked.is_empty() {
                    muted(
                        ui,
                        "Tick the playlists, crates, Flows or folders to export.",
                    );
                }
                for section in Section::ALL {
                    if cs.children_in(section, None).is_empty() {
                        continue;
                    }
                    ui.label(RichText::new(section.label()).color(THEME.muted));
                    collections(ui, cs, section, None, &mut tab.picked);
                }
            }
            FromChoice::Library(id) => {
                let Some(source) = libraries.iter().find(|s| s.id == id).cloned() else {
                    tab.from = FromChoice::Earshot;
                    return;
                };
                if let Some(kind) = TargetKind::converting(source.kind)
                    && let Some(drive) = stick_of(&source)
                    && ui
                        .button(format!("Convert this stick to {}", short(kind)))
                        .on_hover_text(
                            "All of it, written in the other format on the same stick. The \
                             library it has now is moved aside on the stick, not deleted.",
                        )
                        .clicked()
                {
                    tab.nodes.clear();
                    tab.set_kind(kind);
                    tab.set_drive(Some(drive));
                }
                self.external.ensure_read(&source, cx.out);
                match self.external.status(id) {
                    None => {}
                    Some(Status::Reading(m)) => {
                        busy_line(ui, m, "Reading…");
                    }
                    Some(Status::Failed(e)) => {
                        ui.add(Note::danger(e));
                    }
                    Some(Status::Ready(lib)) => {
                        let mut all = tab.nodes.is_empty();
                        if ui
                            .checkbox(&mut all, format!("All of it ({} tracks)", lib.tracks.len()))
                            .clicked()
                            && all
                        {
                            tab.nodes.clear();
                        }
                        for (i, n) in lib.playlists.iter().enumerate() {
                            nodes(ui, n, vec![i], &mut tab.nodes);
                        }
                    }
                }
            }
        }
    }

    // ---- to -----------------------------------------------------------------------

    fn to(&mut self, ui: &mut Ui, libraries: &[Source], cx: &mut Cx) {
        let tab = &mut *self.tab;
        egui::ComboBox::from_id_salt("export-to-kind")
            .selected_text(tab.kind.label())
            .show_ui(ui, |ui| {
                let mut kind = tab.kind;
                for k in TargetKind::ALL {
                    ui.selectable_value(&mut kind, k, k.label());
                }
                tab.set_kind(kind);
            });
        ui.add_space(4.0);
        let mut target = None;
        let mut in_way: Vec<SourceKind> = vec![];
        let mut needs_format: Option<Drive> = None;
        // Converting a stick's own library, on it: the old one is moved aside.
        let from_stick = match tab.from {
            FromChoice::Library(id) => libraries.iter().find(|s| s.id == id).and_then(stick_of),
            FromChoice::Earshot => None,
        };
        match tab.kind {
            k if k.on_drive() => {
                ui.horizontal(|ui| {
                    ui.label("Stick:");
                    if ui
                        .small_button("Other folder…")
                        .on_hover_text("A drive that isn't listed, or a folder to write into")
                        .clicked()
                    {
                        cx.send(ExportCmd::PickFolder);
                    }
                });
                let drives: &[Drive] = cx.core.drives;
                if drives.is_empty() {
                    muted(ui, "No USB sticks plugged in.");
                }
                let mut chosen = tab.drive.clone();
                for d in drives {
                    let free = d.free_bytes as f64 / 1e9;
                    let mut text = format!("{} ({free:.1} GB free)", d.label);
                    if !d.libraries.is_empty() {
                        let names: Vec<_> = d.libraries.iter().map(|k| short_kind(*k)).collect();
                        text += &format!(" — has {}", names.join(", "));
                    }
                    if ui.radio(chosen.as_ref() == Some(&d.path), text).clicked() {
                        chosen = Some(d.path.clone());
                    }
                    if chosen.as_ref() == Some(&d.path) {
                        if let Some(n) = d.notes() {
                            muted(ui, n);
                        }
                        if !d.writable {
                            ui.add(Note::danger("Not writable"));
                        }
                        if from_stick.as_ref() == Some(&d.path) {
                            in_way = in_the_way(k, d);
                        } else if let Some(why) = d.needs_format(k.stick_kind().unwrap()) {
                            ui.add(Note::warning(format!("Not ready for this library. {why}")));
                            needs_format = Some(d.clone());
                        }
                    }
                }
                if let Some(p) = &chosen
                    && !drives.iter().any(|d| &d.path == p)
                {
                    let _ = ui.radio(true, p.display().to_string());
                }
                tab.set_drive(chosen);
                target = tab.drive.clone().map(|path| Target {
                    kind: k,
                    path,
                    volume_root: None,
                });
            }
            TargetKind::RekordboxXml => {
                ui.horizontal(|ui| {
                    let shown = tab
                        .xml
                        .as_ref()
                        .map_or_else(|| "Not chosen".into(), |p| p.display().to_string());
                    ui.label(shown);
                    if ui.button("Choose…").clicked() {
                        cx.send(ExportCmd::PickXml);
                    }
                });
                target = tab.xml.clone().map(|path| Target {
                    kind: TargetKind::RekordboxXml,
                    path,
                    volume_root: None,
                });
            }
            k => {
                let program = k.program().expect("a program's library");
                match libraries.iter().find(|s| s.kind == program) {
                    Some(s) => {
                        muted(
                            ui,
                            s.location
                                .as_deref()
                                .map(|p| p.display().to_string())
                                .unwrap_or_default(),
                        );
                        muted(
                            ui,
                            format!(
                                "Updates {}'s own library (its files are backed up first). \
                                 Close {} before exporting.",
                                program.label(),
                                program.label()
                            ),
                        );
                        target = s.location.clone().map(|path| Target {
                            kind: k,
                            path,
                            volume_root: s.config.volume_root.clone(),
                        });
                    }
                    None => {
                        muted(
                            ui,
                            format!(
                                "{} isn't set up: find it in the Sources pane first.",
                                program.label()
                            ),
                        );
                    }
                }
            }
        }

        ui.add_space(6.0);
        options(ui, tab.kind, &mut tab.options);
        if tab.kind.on_drive() {
            ui.checkbox(&mut tab.merge, "Keep what's on the stick (add to it)")
                .on_hover_text("Without it, the stick's library holds only what's exported");
        }

        if let Some(d) = &needs_format {
            ui.add_space(6.0);
            if ui
                .add(Button::secondary("Format this stick…"))
                .on_hover_text("Erase it and make it FAT32; then export")
                .clicked()
            {
                self.external.open_format(d);
            }
        }
        if !in_way.is_empty() {
            ui.add_space(6.0);
            let names: Vec<_> = in_way.iter().map(|k| short_kind(*k)).collect();
            ui.add(Note::warning(format!(
                "This stick has a {} library. A stick holds one DJ library.",
                names.join(" and ")
            )));
            ui.checkbox(
                &mut tab.replace,
                "Move it aside (kept on the stick, renamed with .earshot-<time>)",
            );
        }

        ui.add_space(8.0);
        let from = match tab.from {
            FromChoice::Earshot => (!tab.picked.is_empty()).then(|| {
                let ids: Vec<_> = tab.picked.iter().copied().collect();
                From::Earshot(picked(cx.core.collections, &ids))
            }),
            FromChoice::Library(id) => {
                libraries
                    .iter()
                    .find(|s| s.id == id)
                    .map(|s| From::Library {
                        source: s.clone(),
                        nodes: (!tab.nodes.is_empty()).then(|| tab.nodes.iter().cloned().collect()),
                    })
            }
        };
        let blocked = if tab.running.is_some() {
            Some("An export is running")
        } else if from.is_none() {
            Some("Pick what to export")
        } else if target.is_none() {
            Some("Pick where to export to")
        } else if needs_format.is_some() {
            Some("Format the stick first")
        } else if !in_way.is_empty() && !tab.replace {
            Some("The stick has another library on it")
        } else {
            None
        };
        let go = ui
            .add_enabled(blocked.is_none(), Button::primary("Export"))
            .on_disabled_hover_text(blocked.unwrap_or_default());
        if go.clicked()
            && let (Some(from), Some(target)) = (from, target)
        {
            tab.running = Some(String::new());
            tab.report = None;
            cx.send(ExportCmd::Run(Box::new(Export {
                from,
                target,
                options: tab.options,
                replace: tab.replace && !in_way.is_empty(),
                // Serato and Mixxx on this computer are updated in place anyway.
                merge: tab.merge && tab.kind.on_drive(),
            })));
        }
        if let Some(m) = &tab.running {
            busy_line(ui, m, "Starting…");
        }
        match &tab.report {
            None => {}
            Some(Err(e)) => {
                ui.add(Note::danger(format!("Export failed: {e}")));
            }
            Some(Ok(w)) => report(ui, w),
        }
    }
}

/// The options that matter to `kind`.
fn options(ui: &mut Ui, kind: TargetKind, o: &mut ExportOptions) {
    match kind {
        TargetKind::RekordboxUsb => {
            ui.checkbox(&mut o.copy_missing, "Copy tracks onto the stick");
            ui.checkbox(&mut o.waveforms, "Measure waveforms")
                .on_hover_text("Slower; without it, players show placeholder waveforms");
            ui.checkbox(&mut o.transcode, "Convert Ogg, Opus and the like to MP3")
                .on_hover_text("Players can't read them");
            ui.checkbox(&mut o.xml, "Add a rekordbox.xml")
                .on_hover_text("For importing the stick into rekordbox on a computer");
        }
        TargetKind::SeratoUsb | TargetKind::Serato => {
            if kind == TargetKind::SeratoUsb {
                ui.checkbox(&mut o.copy_missing, "Copy tracks onto the stick");
            }
            ui.checkbox(
                &mut o.file_tags,
                "Write cues and grids into the audio files",
            )
            .on_hover_text("Serato keeps them there: without this, it won't see them");
        }
        TargetKind::Mixxx => {
            ui.checkbox(&mut o.overwrite, "Replace cues and grids Mixxx already has");
        }
        TargetKind::RekordboxXml => {}
    }
}

fn report(ui: &mut Ui, w: &Written) {
    let mut text = format!("Exported {} tracks in {} playlists.", w.tracks, w.playlists);
    if w.skipped > 0 {
        text += &format!(
            " {} left out: their audio isn't on this computer.",
            w.skipped
        );
    }
    ui.add(Note::success(text));
    for m in &w.moved {
        muted(ui, format!("The old library is now at {}", m.display()));
    }
    for line in &w.warnings {
        muted(ui, line);
    }
}

/// A collections tree with a tick box on each.
fn collections(
    ui: &mut Ui,
    cs: &Collections,
    section: Section,
    parent: Option<CollectionId>,
    picked: &mut BTreeSet<CollectionId>,
) {
    for c in cs.children_in(section, parent) {
        let tick = |ui: &mut Ui, picked: &mut BTreeSet<CollectionId>, c: &Collection| {
            let mut on = picked.contains(&c.id);
            let label = match c.kind {
                CollectionKind::Folder => format!("📁 {}", c.name),
                CollectionKind::Smart => format!("{} (smart)", c.name),
                _ => format!("{} ({})", c.name, c.tracks.len()),
            };
            if ui.checkbox(&mut on, label).changed() {
                if on {
                    picked.insert(c.id);
                } else {
                    picked.remove(&c.id);
                }
            }
        };
        if c.kind != CollectionKind::Folder {
            tick(ui, picked, c);
            continue;
        }
        let id = ui.make_persistent_id(("export-collection", c.id));
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false)
            .show_header(ui, |ui| tick(ui, picked, c))
            .body(|ui| collections(ui, cs, section, Some(c.id), picked));
    }
}

/// Another library's playlist tree with a tick box on each.
fn nodes(ui: &mut Ui, n: &Node, path: NodePath, ticked: &mut BTreeSet<NodePath>) {
    let tick = |ui: &mut Ui, ticked: &mut BTreeSet<NodePath>| {
        let mut on = ticked.contains(&path);
        let label = match n.kind {
            NodeKind::Folder => format!("📁 {}", n.name),
            _ => format!("{} ({})", n.name, n.tracks.len()),
        };
        if ui.checkbox(&mut on, label).changed() {
            if on {
                ticked.insert(path.clone());
            } else {
                ticked.remove(&path);
            }
        }
    };
    if n.kind != NodeKind::Folder || n.children.is_empty() {
        tick(ui, ticked);
        return;
    }
    let id = ui.make_persistent_id(("export-node", &path));
    egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false)
        .show_header(ui, |ui| tick(ui, ticked))
        .body(|ui| {
            for (i, c) in n.children.iter().enumerate() {
                let mut p = path.clone();
                p.push(i);
                nodes(ui, c, p, ticked);
            }
        });
}

/// The stick a library is on, if it's on one.
pub fn stick_of(s: &Source) -> Option<PathBuf> {
    match s.kind {
        SourceKind::RekordboxUsb => s.location.clone(),
        SourceKind::SeratoUsb => {
            // A stick's Serato library is relative to the stick; the computer's to
            // `/` (or `C:/`).
            let root = PathBuf::from(s.config.volume_root.as_deref()?);
            (root.is_dir() && root.parent().is_some()).then_some(root)
        }
        _ => None,
    }
}

fn short(kind: TargetKind) -> &'static str {
    match kind {
        TargetKind::SeratoUsb | TargetKind::Serato => "Serato",
        TargetKind::Mixxx => "Mixxx",
        TargetKind::RekordboxUsb | TargetKind::RekordboxXml => "Rekordbox",
    }
}

/// A stick library's kind, for showing.
pub fn short_kind(k: SourceKind) -> &'static str {
    match k {
        SourceKind::RekordboxUsb | SourceKind::RekordboxDb | SourceKind::RekordboxXml => {
            "Rekordbox"
        }
        SourceKind::Serato | SourceKind::SeratoUsb => "Serato",
        SourceKind::Mixxx => "Mixxx",
        SourceKind::Folder => "folder",
    }
}
