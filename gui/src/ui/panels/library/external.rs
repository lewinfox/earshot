//! The DJ software in the library's Sources pane: Rekordbox, Serato and Mixxx
//! libraries, USB sticks and Rekordbox XML (sources read through cratelib,
//! `core::external`). The three programs are always listed (set up or not: "Set
//! location…"), then the libraries added with "Add…", each with its playlists and
//! crates once read; the middle lists the tracks of the one picked. Right-click a
//! library, folder, playlist, crate or track for "Import to earshot…", which asks
//! what to bring (grid, hot cues, memory cues, overwrite) in a small window.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{RichText, Ui};
use egui_extras::{Column, TableBuilder};

use crate::core::collections::{CollectionCmd, Section};
use crate::core::command::{Command, ViewCmd};
use crate::core::external::{
    ExtLibrary, ExtTrack, ExternalCmd, ExternalEvent, ImportOptions, Imported, Node, NodeKind,
    NodePath, Target,
};
use crate::core::index::LibraryCmd;
use crate::core::sources::{Source, SourceId, SourceKind};
use crate::core::track::TrackId;
use crate::core::view::Source as PaneSource;
use crate::ui::panels::CoreView;
use crate::ui::theme::THEME;

/// How far reading a library has got.
enum Status {
    Reading(String),
    Ready(Arc<ExtLibrary>),
    Failed(String),
}

/// How an import went, shown until the next one.
struct Report {
    ok: bool,
    text: String,
    /// The collection it made, to go to.
    shown_in: Option<(i64, Section)>,
}

/// The import window, open.
struct Dialog {
    source: Source,
    target: Target,
    options: ImportOptions,
}

#[derive(Default)]
pub struct ExternalUi {
    /// Every source, as the Store has them (the libraries are drawn from these).
    sources: Vec<Source>,
    /// Looked in the usual places yet (done the first time the pane shows).
    searched: bool,
    /// What the last search said, if it failed.
    find_error: Option<String>,
    /// Each library's read.
    status: HashMap<SourceId, Status>,
    /// What the middle lists: a library, and a node in it (`None`: all its tracks).
    open: Option<(SourceId, Option<NodePath>)>,
    dialog: Option<Dialog>,
    /// An import running, and what it's doing.
    importing: Option<(SourceId, String)>,
    /// How the last import went.
    report: Option<Report>,
    /// The selected row (an external track id).
    selected: Option<String>,
    /// Our tracks by path, and the library revision it was made for.
    by_path: (u64, HashMap<String, TrackId>),
}

impl ExternalUi {
    /// The sources, as the Store has them now. A library moved elsewhere is read
    /// again when next opened.
    pub fn set_sources(&mut self, sources: Vec<Source>) {
        for new in &sources {
            let moved = self
                .sources
                .iter()
                .any(|old| old.id == new.id && old.location != new.location);
            if moved {
                self.status.remove(&new.id);
            }
        }
        self.sources = sources;
        let listed = |id| self.sources.iter().any(|s| s.id == id && s.enabled);
        if self.open.as_ref().is_some_and(|o| !listed(o.0)) {
            self.open = None;
        }
    }

    fn source(&self, id: SourceId) -> Option<&Source> {
        self.sources.iter().find(|s| s.id == id)
    }

    /// What the pane lists: each program, with its library if it has one switched
    /// on, then the other libraries.
    fn entries(&self) -> Vec<(SourceKind, Option<Source>)> {
        let on = |k: SourceKind| {
            self.sources
                .iter()
                .find(|s| s.kind == k && s.enabled && s.location.is_some())
                .cloned()
        };
        let mut out: Vec<_> = SourceKind::PROGRAMS.iter().map(|&k| (k, on(k))).collect();
        out.extend(
            self.sources
                .iter()
                .filter(|s| s.kind.is_library() && !s.kind.is_program() && s.enabled)
                .map(|s| (s.kind, Some(s.clone()))),
        );
        out
    }

    /// Take in what came of an `ExternalCmd` (the app keeps the locations first).
    pub fn event(&mut self, e: ExternalEvent) {
        match e {
            ExternalEvent::Found { at: None, result } => self.find_error = result.err(),
            ExternalEvent::Found {
                at: Some(at),
                result,
            } => {
                self.find_error = match result {
                    Ok(list) if list.is_empty() => {
                        Some(format!("No DJ library in {}", at.display()))
                    }
                    Ok(_) => None,
                    Err(e) => Some(e),
                }
            }
            ExternalEvent::Located { result, .. } => {
                self.find_error = result.err();
            }
            ExternalEvent::Progress { source, message } => {
                if let Some((s, m)) = &mut self.importing
                    && *s == source
                {
                    *m = message;
                } else if let Some(Status::Reading(m)) = self.status.get_mut(&source) {
                    *m = message;
                }
            }
            ExternalEvent::Read { source, result } => {
                let status = match result {
                    Ok(lib) => Status::Ready(lib),
                    Err(e) => Status::Failed(e),
                };
                self.status.insert(source, status);
            }
            ExternalEvent::Imported { source, result } => {
                if self.importing.as_ref().is_some_and(|i| i.0 == source) {
                    self.importing = None;
                }
                let name = self.source(source).map_or("a library", |s| s.name.as_str());
                self.report = Some(match result {
                    Ok(r) => Report {
                        ok: r.tracks > 0 || r.missing == 0,
                        text: summary(name, &r),
                        shown_in: r.shown_in,
                    },
                    Err(e) => Report {
                        ok: false,
                        text: format!("Importing from {name} failed: {e}"),
                        shown_in: None,
                    },
                });
            }
        }
    }

    /// The DJ software section of the Sources pane: each library, and its
    /// playlists once read.
    pub fn section(&mut self, ui: &mut Ui, out: &mut Vec<Command>) {
        if !self.searched {
            self.searched = true;
            out.push(ExternalCmd::Find.into());
        }
        if let Some(e) = &self.find_error {
            ui.label(RichText::new(e).size(11.0).color(THEME.bad));
        }
        for (kind, source) in self.entries() {
            match source {
                Some(source) => self.library(ui, &source, out),
                None => unset(ui, kind, out),
            }
        }
        ui.horizontal(|ui| {
            if ui
                .button("Add…")
                .on_hover_text("A USB stick, or a folder with a Rekordbox XML")
                .clicked()
            {
                out.push(ExternalCmd::Pick.into());
            }
            if ui
                .button("Look again")
                .on_hover_text("Look for libraries in each program's usual place")
                .clicked()
            {
                out.push(ExternalCmd::Find.into());
            }
        });
        self.status_box(ui, out);
        self.dialog(ui.ctx(), out);
    }

    /// What an import is doing, or how the last one went, in a box: where its
    /// playlists went, with a button to go there.
    fn status_box(&mut self, ui: &mut Ui, out: &mut Vec<Command>) {
        if self.importing.is_none() && self.report.is_none() {
            return;
        }
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if let Some((_, m)) = &self.importing {
                ui.horizontal(|ui| {
                    ui.spinner();
                    let m = if m.is_empty() { "starting" } else { m };
                    ui.label(format!("Importing… {m}"));
                });
                return;
            }
            let Some(r) = &self.report else { return };
            let (mark, colour) = if r.ok {
                ("✔", THEME.ok)
            } else {
                ("⚠", THEME.bad)
            };
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(mark).color(colour));
                ui.label(&r.text);
            });
            let mut dismiss = false;
            ui.horizontal(|ui| {
                if let Some((id, section)) = r.shown_in
                    && ui.button("Show it").clicked()
                {
                    let pane = match section {
                        Section::Crates => PaneSource::Crates,
                        _ => PaneSource::Playlists,
                    };
                    out.push(ViewCmd::Source(Some(pane)).into());
                    out.push(CollectionCmd::Open(id).into());
                }
                dismiss = ui.small_button("Dismiss").clicked();
            });
            if dismiss {
                self.report = None;
            }
        });
    }

    /// Stop listing a library's tracks in the middle (a folder was picked).
    pub fn close(&mut self) {
        self.open = None;
    }

    /// A library and (opened) its playlists.
    fn library(&mut self, ui: &mut Ui, source: &Source, out: &mut Vec<Command>) {
        let id = ui.make_persistent_id(("external", source.id));
        let state =
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
        let is_open = state.is_open();
        let header = state.show_header(ui, |ui| {
            let selected = self.open == Some((source.id, None));
            let r = ui
                .selectable_label(selected, RichText::new(&source.name).strong())
                .on_hover_text(
                    source
                        .location
                        .as_deref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                );
            if r.clicked() {
                self.show(source, None, out);
            }
            r.context_menu(|ui| {
                self.import_item(ui, source, Target::Library);
                if ui.button("Read again").clicked() {
                    self.status.remove(&source.id);
                    self.read(source, out);
                    ui.close();
                }
                ui.separator();
                if source.kind.is_program() && ui.button("Set location…").clicked() {
                    out.push(ExternalCmd::SetLocation { kind: source.kind }.into());
                    ui.close();
                }
                if ui.button("Remove").clicked() {
                    out.push(LibraryCmd::RemoveSource(source.id).into());
                    ui.close();
                }
            });
        });
        if is_open {
            self.read_once(source, out);
        }
        header.body(|ui| match self.status.get(&source.id) {
            None => {}
            Some(Status::Reading(m)) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new(if m.is_empty() { "Reading…" } else { m }).size(11.0));
                });
            }
            Some(Status::Failed(e)) => {
                ui.label(RichText::new(e).size(11.0).color(THEME.bad));
            }
            Some(Status::Ready(lib)) => {
                let lib = lib.clone();
                if lib.playlists.is_empty() {
                    ui.label(RichText::new("No playlists").size(11.0).color(THEME.muted));
                }
                for (i, n) in lib.playlists.iter().enumerate() {
                    self.node(ui, source, n, vec![i], out);
                }
            }
        });
    }

    fn node(
        &mut self,
        ui: &mut Ui,
        source: &Source,
        n: &Node,
        path: NodePath,
        out: &mut Vec<Command>,
    ) {
        if n.kind != NodeKind::Folder || n.children.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(ui.spacing().indent);
                self.node_label(ui, source, n, &path, out);
            });
            return;
        }
        let id = ui.make_persistent_id(("external", source.id, &path));
        let header =
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false)
                .show_header(ui, |ui| self.node_label(ui, source, n, &path, out));
        header.body(|ui| {
            for (i, c) in n.children.iter().enumerate() {
                let mut p = path.clone();
                p.push(i);
                self.node(ui, source, c, p, out);
            }
        });
    }

    fn node_label(
        &mut self,
        ui: &mut Ui,
        source: &Source,
        n: &Node,
        path: &NodePath,
        out: &mut Vec<Command>,
    ) {
        let selected = self.open == Some((source.id, Some(path.clone())));
        let icon = match n.kind {
            NodeKind::Folder => "📁",
            NodeKind::Playlist => "☰",
            NodeKind::Crate => "📦",
        };
        let r = ui.selectable_label(selected, format!("{icon} {}", n.name));
        if r.clicked() {
            self.show(source, Some(path.clone()), out);
        }
        r.context_menu(|ui| self.import_item(ui, source, Target::Node(path.clone())));
    }

    fn import_item(&mut self, ui: &mut Ui, source: &Source, target: Target) {
        let busy = self.importing.is_some();
        if ui
            .add_enabled(!busy, egui::Button::new("Import to earshot…"))
            .on_disabled_hover_text("An import is running")
            .clicked()
        {
            self.dialog = Some(Dialog {
                source: source.clone(),
                target,
                options: ImportOptions {
                    memory_cues: source.kind.has_memory_cues(),
                    ..ImportOptions::default()
                },
            });
            ui.close();
        }
    }

    fn read_once(&mut self, source: &Source, out: &mut Vec<Command>) {
        if !self.status.contains_key(&source.id) {
            self.read(source, out);
        }
    }

    fn read(&mut self, source: &Source, out: &mut Vec<Command>) {
        self.status
            .insert(source.id, Status::Reading(String::new()));
        out.push(ExternalCmd::Read(source.clone()).into());
    }

    /// List a library's tracks, or a node's, in the middle.
    fn show(&mut self, source: &Source, node: Option<NodePath>, out: &mut Vec<Command>) {
        self.read_once(source, out);
        self.open = Some((source.id, node));
        self.selected = None;
    }

    /// Something is picked to list in the middle.
    pub fn showing(&self) -> bool {
        self.open.is_some()
    }

    /// The middle: the picked library's or node's tracks.
    pub fn list(&mut self, ui: &mut Ui, v: &CoreView, out: &mut Vec<Command>) {
        let Some((id, node)) = self.open.clone() else {
            return;
        };
        let Some(source) = self.source(id).cloned() else {
            return;
        };
        let Some(Status::Ready(lib)) = self.status.get(&id) else {
            ui.label(RichText::new("Reading…").color(THEME.muted));
            return;
        };
        let lib = lib.clone();
        if v.lib.rev() != self.by_path.0 || self.by_path.1.is_empty() {
            self.by_path = (
                v.lib.rev(),
                v.lib
                    .tracks()
                    .iter()
                    .map(|t| (t.path.clone(), t.id))
                    .collect(),
            );
        }
        let tracks = lib.tracks_in(node.as_deref());
        let missing = tracks.iter().filter(|t| t.file.is_none()).count();
        let title = match node.as_deref().and_then(|p| lib.node(p)) {
            Some(n) => format!("{} › {}", source.name, n.name),
            None => source.name.clone(),
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).strong());
            let mut info = format!("{} tracks", tracks.len());
            if missing > 0 {
                info += &format!(", {missing} not on this computer");
            }
            ui.label(RichText::new(info).color(THEME.muted));
            let target = node.clone().map_or(Target::Library, Target::Node);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.import_button(ui, &source, target);
            });
        });
        if !lib.warnings.is_empty() {
            let n = lib.warnings.len();
            let text = format!(
                "ⓘ {n} warning{} while reading",
                if n == 1 { "" } else { "s" }
            );
            ui.add(
                egui::Label::new(RichText::new(text).size(11.0).color(THEME.warn))
                    .selectable(false)
                    .sense(egui::Sense::hover()),
            )
            .on_hover_ui(|ui| {
                ui.set_max_width(600.0);
                for w in &lib.warnings {
                    ui.label(w);
                }
            });
        }
        self.status_box(ui, out);
        ui.separator();
        let row_h = 20.0;
        TableBuilder::new(ui)
            .striped(true)
            .sense(egui::Sense::click())
            .column(Column::exact(18.0))
            .column(Column::remainder().at_least(120.0).clip(true))
            .column(Column::initial(160.0).at_least(60.0).clip(true))
            .column(Column::exact(56.0))
            .column(Column::exact(44.0))
            .column(Column::exact(40.0))
            .header(row_h, |mut h| {
                for name in ["", "Title", "Artist", "BPM", "Key", "Cues"] {
                    h.col(|ui| {
                        ui.label(RichText::new(name).size(11.0).color(THEME.muted));
                    });
                }
            })
            .body(|body| {
                body.rows(row_h, tracks.len(), |mut row| {
                    let t = tracks[row.index()];
                    let ours = self.ours(t);
                    row.set_selected(self.selected.as_deref() == Some(t.id.as_str()));
                    let (mark, tip) = match (ours, &t.file) {
                        (Some(_), _) => ("●", "In earshot"),
                        (None, Some(_)) => ("○", "Not in earshot yet"),
                        (None, None) => ("✕", "Not on this computer"),
                    };
                    row.col(|ui| {
                        ui.label(RichText::new(mark).color(THEME.muted))
                            .on_hover_text(tip);
                    });
                    row.col(|ui| {
                        let title = if t.title.is_empty() {
                            t.display()
                        } else {
                            t.title.clone()
                        };
                        let text = RichText::new(title);
                        let text = if t.file.is_none() {
                            text.color(THEME.muted)
                        } else {
                            text
                        };
                        ui.label(text).on_hover_text(&t.location);
                    });
                    row.col(|ui| {
                        ui.label(&t.artist);
                    });
                    row.col(|ui| {
                        ui.label(t.bpm.map(|b| format!("{b:.2}")).unwrap_or_default());
                    });
                    row.col(|ui| {
                        ui.label(t.key.as_deref().unwrap_or(""));
                    });
                    row.col(|ui| {
                        if !t.cues.is_empty() {
                            ui.label(t.cues.len().to_string());
                        }
                    });
                    let r = row.response();
                    if r.clicked() {
                        self.selected = Some(t.id.clone());
                    }
                    if r.double_clicked()
                        && let Some(id) = ours
                    {
                        out.push(LibraryCmd::Load(id).into());
                    }
                    r.context_menu(|ui| {
                        self.selected = Some(t.id.clone());
                        if let Some(id) = ours
                            && ui.button("Load").clicked()
                        {
                            out.push(LibraryCmd::Load(id).into());
                            ui.close();
                        }
                        if t.file.is_some() {
                            self.import_item(ui, &source, Target::Track(t.id.clone()));
                        } else {
                            ui.label(RichText::new("Not on this computer").color(THEME.muted));
                        }
                    });
                });
            });
    }

    fn import_button(&mut self, ui: &mut Ui, source: &Source, target: Target) {
        let busy = self.importing.is_some();
        if ui
            .add_enabled(!busy, egui::Button::new("Import to earshot…"))
            .clicked()
        {
            self.dialog = Some(Dialog {
                source: source.clone(),
                options: ImportOptions {
                    memory_cues: source.kind.has_memory_cues(),
                    ..ImportOptions::default()
                },
                target,
            });
        }
    }

    /// Our track with this track's file, if there is one.
    fn ours(&self, t: &ExtTrack) -> Option<TrackId> {
        let f = t.file.as_ref()?;
        self.by_path.1.get(f.to_string_lossy().as_ref()).copied()
    }

    /// The import window, while it's open.
    fn dialog(&mut self, ctx: &egui::Context, out: &mut Vec<Command>) {
        let Some(d) = &mut self.dialog else { return };
        let what = match (&d.target, self.status.get(&d.source.id)) {
            (Target::Library, _) => format!("Everything in {}", d.source.name),
            (Target::Node(p), Some(Status::Ready(lib))) => lib
                .node(p)
                .map_or_else(|| "?".into(), |n| format!("\"{}\"", n.name)),
            (Target::Track(id), Some(Status::Ready(lib))) => lib
                .track(id)
                .map_or_else(|| "?".into(), |t| format!("\"{}\"", t.display())),
            _ => String::new(),
        };
        let (mut go, mut close) = (false, false);
        let mut open = true;
        egui::Window::new("Import to earshot")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(what);
                ui.label(
                    RichText::new(
                        "Tracks whose files are on this computer, with their title, artist, \
                         BPM, key and the like, and the playlists and crates they're in.",
                    )
                    .size(11.0)
                    .color(THEME.muted),
                );
                ui.add_space(6.0);
                let o = &mut d.options;
                ui.checkbox(&mut o.grid, "Beat grid")
                    .on_hover_text("Becomes the track's grid; the one it had is kept, to go back to");
                ui.checkbox(&mut o.hot_cues, "Hot cues")
                    .on_hover_text("Into empty pads (with Overwrite, into every pad)");
                let memory = d.source.kind.has_memory_cues();
                if !memory {
                    o.memory_cues = false;
                }
                ui.add_enabled(memory, egui::Checkbox::new(&mut o.memory_cues, "Memory cues"))
                    .on_disabled_hover_text("Only Rekordbox has memory cues");
                ui.checkbox(&mut o.overwrite, "Overwrite existing metadata")
                    .on_hover_text(
                        "Replace what earshot has (your own edits too), and set hot cues over \
                         earshot's. Without it, imports only fill gaps.",
                    );
                if o.needs_cues() && d.source.kind == SourceKind::Serato {
                    ui.label(
                        RichText::new("Serato keeps cues and grids in the audio files: reading them takes a while.")
                            .size(11.0)
                            .color(THEME.muted),
                    );
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    go = ui.button("Import").clicked();
                    close = ui.button("Cancel").clicked();
                });
            });
        if go {
            let d = self.dialog.take().expect("open");
            let lib = match self.status.get(&d.source.id) {
                Some(Status::Ready(lib)) => Some(lib.clone()),
                _ => None,
            };
            self.importing = Some((d.source.id, String::new()));
            self.report = None;
            out.push(
                ExternalCmd::Import {
                    source: d.source,
                    target: d.target,
                    options: d.options,
                    read: lib,
                }
                .into(),
            );
        } else if close || !open {
            self.dialog = None;
        }
    }
}

/// A program whose library isn't known yet: right-click to say where it is.
fn unset(ui: &mut Ui, kind: SourceKind, out: &mut Vec<Command>) {
    ui.horizontal(|ui| {
        ui.add_space(ui.spacing().indent);
        let r = ui
            .add(
                egui::Label::new(
                    RichText::new(format!("{}  ·  not set", kind.label())).color(THEME.muted),
                )
                .sense(egui::Sense::click()),
            )
            .on_hover_text("Right-click › Set location…");
        let set = |out: &mut Vec<Command>| out.push(ExternalCmd::SetLocation { kind }.into());
        if r.double_clicked() {
            set(out);
        }
        r.context_menu(|ui| {
            if ui.button("Set location…").clicked() {
                set(out);
                ui.close();
            }
        });
    });
}

/// "Imported 120 tracks (3 not on this computer), 4 playlists, 80 grids, 300 cues."
fn summary(name: &str, r: &Imported) -> String {
    if r.tracks == 0 && r.missing > 0 {
        return format!(
            "Nothing imported from {name}: none of its {} tracks' files were found on this \
             computer.",
            r.missing
        );
    }
    let mut parts = vec![format!("{} tracks", r.tracks)];
    if r.collections > 0 {
        parts.push(format!("{} playlists and crates", r.collections));
    }
    if r.grids > 0 {
        parts.push(format!("{} grids", r.grids));
    }
    if r.cues > 0 {
        parts.push(format!("{} cues", r.cues));
    }
    let mut s = format!("Imported from {name}: {}.", parts.join(", "));
    if r.missing > 0 {
        s += &format!(" {} tracks aren't on this computer.", r.missing);
    }
    s
}
