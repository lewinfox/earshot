//! The DJ software in the library's Sources pane: Rekordbox, Serato and Mixxx
//! libraries, USB sticks and Rekordbox XML (sources read through cratelib,
//! `core::external`). The three programs are always listed (set up or not: "Set
//! location…"), then the libraries added with "Add…", each with its playlists and
//! crates once read; the middle lists the tracks of the one picked. Right-click a
//! library, folder, playlist, crate or track for "Import to earshot…", which asks
//! what to bring (grid, hot cues, memory cues, overwrite) in a small window.

mod panes;
pub use panes::Middle;
mod sticks;
pub use sticks::{Sending, send_menu};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{RichText, Ui};

use crate::core::collections::{CollectionCmd, Section};
use crate::core::command::{Command, ViewCmd};
use crate::core::drives::Drive;
use crate::core::external::{
    ExtLibrary, ExternalCmd, ExternalEvent, ImportOptions, Imported, Node, NodeKind, NodePath,
    Target,
};
use crate::core::index::LibraryCmd;
use crate::core::query::{Expr, Query, Sort};
use crate::core::selection::Selection;
use crate::core::sources::{Source, SourceId, SourceKind};
use crate::core::track::TrackId;
use crate::core::view::Source as PaneSource;
use crate::ui::panels::CoreView;
use crate::ui::panels::library::drag::DragPayload;
use crate::ui::panels::library::drag::Origin;
use crate::ui::panels::library::table::{TableState, TableView, TrackTable};
use crate::ui::panels::library::tracks::{Media, MenuAt};
use crate::ui::theme::{THEME, fs};
use crate::ui::widgets::{self, Button, Choice, Note, busy_line, go_cancel, muted};

/// How far reading a library has got.
pub enum Status {
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

/// What the list's rows were worked out for: the index, the library and node shown,
/// the sort and the filters.
type RowsKey = (u64, SourceId, Option<NodePath>, Sort, Result<Expr, String>);

/// The import window, open.
struct Dialog {
    source: Source,
    target: Target,
    options: ImportOptions,
    /// A stick's: the music folder its files are copied into.
    copy_to: Option<String>,
}

/// A row of a library's tree, for the browse knob: the library, and a playlist or
/// folder in it (`None`: its All tracks).
pub type NodeKey = (SourceId, Option<NodePath>);

#[derive(Default)]
pub struct ExternalUi {
    /// The browse knob's highlight in a library's tree, and whether it just moved.
    pub nav_cursor: Option<NodeKey>,
    pub nav_scroll: bool,
    /// The folders open in the trees.
    open_nodes: std::collections::HashSet<(SourceId, NodePath)>,
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
    /// Its tracks earshot can play, by library: their id there → ours.
    ids: HashMap<SourceId, HashMap<String, TrackId>>,
    /// The list's rows, and what they were worked out for.
    rows: Option<(RowsKey, Vec<TrackId>)>,
    sel: Selection,
    /// A track to select and scroll to once the list has it ([`Self::reveal`]).
    reveal: Option<TrackId>,
    /// The row to scroll to on the next frame (the knob moved).
    scroll_to: Option<usize>,
    /// Focus the first track once the list has rows ([`Self::focus_top`]).
    want_top: bool,
    /// The USB sticks plugged in, and the stick views' state (`sticks.rs`).
    drives: Vec<Drive>,
    stick: sticks::StickUi,
    /// earshot's music folders, where copies off a stick go.
    music: Vec<String>,
    /// Another library's tracks put in a playlist, crate or Flow: asking whether to
    /// import them first (then carry on with the command), and "don't ask again".
    confirm: Option<(Vec<TrackId>, Command, bool)>,
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
        let listed = |id| self.listed(id);
        if self.open.as_ref().is_some_and(|o| !listed(o.0)) {
            self.open = None;
        }
    }

    /// The library a program's or library's pane shows (`None`: not set up, or a
    /// stick's, which can hold several).
    pub fn pane_library(&self, pane: PaneSource) -> Option<SourceId> {
        match pane {
            PaneSource::Program(kind) => self
                .entries()
                .into_iter()
                .find(|(k, _)| *k == kind)
                .and_then(|(_, s)| s.map(|s| s.id)),
            PaneSource::Library(id) => Some(id),
            _ => None,
        }
    }

    /// A library's tree top to bottom (not inside closed folders): each row, its
    /// folder, and whether it's a folder. Empty until it has been read.
    pub fn nav_rows(&self, id: SourceId) -> Vec<(NodeKey, Option<NodeKey>, bool)> {
        fn walk(
            ui: &ExternalUi,
            id: SourceId,
            nodes: &[Node],
            parent: Option<NodePath>,
            out: &mut Vec<(NodeKey, Option<NodeKey>, bool)>,
        ) {
            for (i, n) in nodes.iter().enumerate() {
                let mut path = parent.clone().unwrap_or_default();
                path.push(i);
                let folder = n.kind == NodeKind::Folder;
                out.push(((id, Some(path.clone())), parent.clone().map(|p| (id, Some(p))), folder));
                if folder && ui.open_nodes.contains(&(id, path.clone())) {
                    walk(ui, id, &n.children, Some(path), out);
                }
            }
        }
        let Some(Status::Ready(lib)) = self.status.get(&id) else {
            return vec![];
        };
        let mut out = vec![((id, None), None, false)];
        walk(self, id, &lib.playlists, None, &mut out);
        out
    }

    /// Open or close a folder in a library's tree.
    pub fn set_node_open(&mut self, id: SourceId, path: NodePath, open: bool) {
        if open {
            self.open_nodes.insert((id, path));
        } else {
            self.open_nodes.remove(&(id, path));
        }
    }

    /// List a library's tracks, or a node's, in the middle (as clicking it does).
    pub fn open_node(&mut self, key: &NodeKey, out: &mut Vec<Command>) {
        if let Some(source) = self.source(key.0).cloned() {
            self.show(&source, key.1.clone(), out);
            self.nav_cursor = Some(key.clone());
        }
    }

    fn source(&self, id: SourceId) -> Option<&Source> {
        self.sources.iter().find(|s| s.id == id)
    }

    /// A stick library's source shows under its stick while that's plugged in, and
    /// not at all otherwise (unless it's a copy of a stick in a folder).
    fn on_a_stick(&self, s: &Source) -> Option<&Drive> {
        let at = s.location.as_deref()?;
        self.drives.iter().find(|d| d.holds(at))
    }

    fn hidden(&self, s: &Source) -> bool {
        s.kind.on_stick()
            && (self.on_a_stick(s).is_some() || !s.location.as_deref().is_some_and(Path::exists))
    }

    /// It's listed somewhere: switched on, and not a stick's while it's unplugged.
    fn listed(&self, id: SourceId) -> bool {
        self.sources.iter().any(|s| {
            s.id == id
                && s.enabled
                && (!s.kind.on_stick() || s.location.as_deref().is_some_and(Path::exists))
        })
    }

    /// What the DJ software section lists: each program, with its library if it has
    /// one switched on, then the other libraries (those on sticks are under their
    /// sticks).
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
                .filter(|s| !self.hidden(s))
                .map(|s| (s.kind, Some(s.clone()))),
        );
        out
    }

    /// The libraries listed, for the Export tab: each program's that's set up, then
    /// the others.
    pub fn libraries(&self) -> Vec<Source> {
        let mut out: Vec<Source> = self.entries().into_iter().filter_map(|(_, s)| s).collect();
        out.extend(
            self.sources
                .iter()
                .filter(|s| s.enabled && s.kind.on_stick() && self.on_a_stick(s).is_some())
                .cloned(),
        );
        out
    }

    /// The sticks plugged in now.
    pub fn set_drives(&mut self, drives: Vec<Drive>) {
        self.drives = drives;
        let listed = |id| self.listed(id);
        if self.open.as_ref().is_some_and(|o| !listed(o.0)) {
            self.open = None;
        }
        self.stick.drives_changed(&self.drives);
    }

    /// earshot's music folders.
    pub fn set_music(&mut self, folders: &[String]) {
        if self.music != folders {
            self.music = folders.to_vec();
        }
    }

    /// How reading a library has gone, if it has been asked for.
    pub fn status(&self, id: SourceId) -> Option<&Status> {
        self.status.get(&id)
    }

    /// Read a library, unless it has been.
    pub fn ensure_read(&mut self, source: &Source, out: &mut Vec<Command>) {
        self.read_once(source, out);
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
            ExternalEvent::Relist(_) => {}
            ExternalEvent::CopyProgress(_)
            | ExternalEvent::Copied(_)
            | ExternalEvent::Ejected { .. }
            | ExternalEvent::Formatted { .. } => self.stick.event(e),
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

    /// The windows the pane opens: importing, and adding to a stick.
    pub fn windows(&mut self, ctx: &egui::Context, v: &CoreView, out: &mut Vec<Command>) {
        self.import_window(ctx, out);
        self.confirm_window(ctx, out);
        self.to_stick_window(ctx, v, out);
        self.format_window(ctx, v, out);
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
                busy_line(ui, m, "Importing…");
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
        self.stick.open = None;
    }

    /// A library's pane body: its name (click for all its tracks; drag it, or
    /// right-click it) and its playlists, read when first shown.
    fn library(&mut self, ui: &mut Ui, source: &Source, out: &mut Vec<Command>) {
        self.read_once(source, out);
        let selected = self.open == Some((source.id, None));
        let count = match self.status.get(&source.id) {
            Some(Status::Ready(lib)) => format!(" ({})", lib.tracks.len()),
            _ => String::new(),
        };
        let r = ui
            .selectable_label(
                selected,
                RichText::new(format!("All tracks{count}")).strong(),
            )
            .interact(egui::Sense::click_and_drag());
        r.dnd_set_drag_payload(DragPayload::External {
            source: source.id,
            path: vec![],
            name: source.name.clone(),
        });
        let r = r.on_hover_text(
            source
                .location
                .as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        );
        super::outline(
            ui,
            &r,
            self.nav_cursor == Some((source.id, None)),
            self.nav_scroll,
        );
        if r.clicked() {
            self.show(source, None, out);
            self.nav_cursor = Some((source.id, None));
        }
        r.context_menu(|ui| {
            self.import_item(ui, source, Target::Library);
            if ui.button("Read again").clicked() {
                self.status.remove(&source.id);
                self.read(source, out);
                ui.close();
            }
            ui.separator();
            if source.kind.is_program() && ui.button("Change location…").clicked() {
                out.push(ExternalCmd::SetLocation { kind: source.kind }.into());
                ui.close();
            }
            if !source.kind.on_stick() && ui.button("Remove").clicked() {
                out.push(LibraryCmd::RemoveSource(source.id).into());
                ui.close();
            }
        });
        match self.status.get(&source.id) {
            None => {}
            Some(Status::Reading(m)) => {
                busy_line(ui, m, "Reading…");
            }
            Some(Status::Failed(e)) => {
                ui.add(Note::danger(e).small());
            }
            Some(Status::Ready(lib)) => {
                let lib = lib.clone();
                if lib.playlists.is_empty() {
                    muted(ui, "No playlists");
                }
                for (i, n) in lib.playlists.iter().enumerate() {
                    self.node(ui, source, n, vec![i], out);
                }
            }
        }
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
        let mut state =
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
        state.set_open(self.open_nodes.contains(&(source.id, path.clone())));
        let header = state.show_header(ui, |ui| self.node_label(ui, source, n, &path, out));
        header.body(|ui| {
            for (i, c) in n.children.iter().enumerate() {
                let mut p = path.clone();
                p.push(i);
                self.node(ui, source, c, p, out);
            }
        });
        // Its arrow clicked: keep what it says.
        if let Some(s) = egui::collapsing_header::CollapsingState::load(ui.ctx(), id) {
            self.set_node_open(source.id, path.clone(), s.is_open());
        }
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
        let r = ui
            .selectable_label(selected, format!("{icon} {}", n.name))
            .interact(egui::Sense::click_and_drag());
        r.dnd_set_drag_payload(DragPayload::External {
            source: source.id,
            path: path.clone(),
            name: n.name.clone(),
        });
        super::outline(
            ui,
            &r,
            self.nav_cursor == Some((source.id, Some(path.clone()))),
            self.nav_scroll,
        );
        if r.clicked() {
            self.show(source, Some(path.clone()), out);
            self.nav_cursor = Some((source.id, Some(path.clone())));
        }
        r.context_menu(|ui| {
            self.import_item(ui, source, Target::Node(path.clone()));
            // A stick's own playlist isn't sent to that stick.
            let others: Vec<Drive> = self
                .drives
                .iter()
                .filter(|d| !source.location.as_deref().is_some_and(|at| d.holds(at)))
                .cloned()
                .collect();
            if let Some((drive, kind)) = send_menu(ui, &others) {
                let what = Sending::Library {
                    source: source.clone(),
                    path: path.clone(),
                    name: n.name.clone(),
                };
                self.send_to_stick(&drive, what, kind);
            }
        });
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
                copy_to: source
                    .kind
                    .on_stick()
                    .then(|| self.music.first().cloned().unwrap_or_default()),
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

    /// The list's focused track: what "load selected" loads from it.
    pub fn focused(&self) -> Option<TrackId> {
        self.sel.focused()
    }

    /// Move the list's focus (the keyboard, the browse knob).
    pub fn move_focus(&mut self, delta: i32) {
        let rows = self.rows.as_ref().map(|(_, r)| r.clone()).unwrap_or_default();
        self.scroll_to = self.sel.move_focus(delta, &rows);
    }

    /// Put the focus on the list's first track, once it has one.
    pub fn focus_top(&mut self) {
        self.want_top = true;
    }

    /// What the list shows: a library, and a node in it (`None`: all its tracks).
    pub fn open_list(&self) -> Option<(SourceId, Option<NodePath>)> {
        self.open.clone()
    }

    /// List library `id`'s `node` again and select `track` there. False if the
    /// library isn't listed any more (its stick unplugged, say).
    pub fn reveal(
        &mut self,
        id: SourceId,
        node: Option<NodePath>,
        track: TrackId,
        out: &mut Vec<Command>,
    ) -> bool {
        let Some(source) = self.source(id).cloned().filter(|_| self.listed(id)) else {
            return false;
        };
        self.show(&source, node, out);
        self.reveal = Some(track);
        true
    }

    fn show(&mut self, source: &Source, node: Option<NodePath>, out: &mut Vec<Command>) {
        self.read_once(source, out);
        self.open = Some((source.id, node));
        self.stick.open = None;
    }

    /// The middle: the picked library's or node's tracks. True when a row was
    /// clicked (the list takes focus).
    pub fn list(
        &mut self,
        ui: &mut Ui,
        v: &CoreView,
        media: &mut Media,
        table: &mut TableState,
        out: &mut Vec<Command>,
    ) -> bool {
        let Some((id, node)) = self.open.clone() else {
            return false;
        };
        let Some(source) = self.source(id).cloned() else {
            return false;
        };
        let Some(Status::Ready(lib)) = self.status.get(&id) else {
            ui.label(RichText::new("Reading…").color(THEME.muted));
            return false;
        };
        let lib = lib.clone();
        let tracks = lib.tracks_in(node.as_deref());
        let ids = self.ids.get(&id).cloned();
        let ids = ids.as_ref();
        let missing = tracks
            .iter()
            .filter(|t| ids.is_none_or(|m| !m.contains_key(&t.id)))
            .count();
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
                egui::Label::new(RichText::new(text).size(fs(11.0)).color(THEME.warn))
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
        // The rows: its tracks earshot can play (ours, or outside ones), sorted and
        // filtered as the table says; worked out again when any of that changes.
        let (sort, filter) = (table.sort(), table.filter());
        let key = (v.lib.rev(), id, node.clone(), sort.clone(), filter.clone());
        if self.rows.as_ref().is_none_or(|(k, _)| *k != key) {
            let within: Vec<TrackId> = tracks
                .iter()
                .filter_map(|t| ids.and_then(|m| m.get(&t.id)).copied())
                .collect();
            let q = Query {
                expr: filter.clone().unwrap_or(Expr::ALL),
                sort: sort.clone(),
            };
            let rows = v.lib.query(&q, v.collections, Some(&within));
            self.sel.retain(&rows);
            self.rows = Some((key, rows));
        }
        let rows = self
            .rows
            .as_ref()
            .map(|(_, r)| r.clone())
            .unwrap_or_default();
        // A track to show: select it and scroll there, clearing filters that hide it.
        let mut scroll_to = self.scroll_to.take();
        if self.want_top && !rows.is_empty() {
            self.want_top = false;
            self.sel = Selection::default();
            scroll_to = self.sel.move_focus(0, &rows);
        }
        if let Some(t) = self.reveal {
            if let Some(i) = rows.iter().position(|&r| r == t) {
                self.sel.select_only(t);
                scroll_to = Some(i);
                self.reveal = None;
            } else if table.has_filters() {
                table.filters.clear();
                ui.ctx().request_repaint();
            } else {
                self.reveal = None; // not in that list any more
            }
        }
        TrackTable {
            id: "external",
            rows: &rows,
            state: table,
            available: TableView::External.available(),
            sel: &mut self.sel,
            origin: Origin::Library,
            menu: MenuAt::default(),
            row_height: 26.0,
            reorder: false,
            takes_drops: false,
            scores: None,
            scroll_to,
            show_selection: true,
        }
        .show(ui, v, media, out)
        .clicked
    }

    /// Ask whether to import another library's tracks before `then` (which puts
    /// them in a playlist, crate or Flow).
    pub fn confirm_import(&mut self, tracks: Vec<TrackId>, then: Command) {
        self.confirm = Some((tracks, then, false));
    }

    fn confirm_window(&mut self, ctx: &egui::Context, out: &mut Vec<Command>) {
        let Some((tracks, _, remember)) = &mut self.confirm else {
            return;
        };
        let n = tracks
            .iter()
            .filter(|&&t| crate::core::outside::is_outside(t))
            .count();
        let choice = widgets::dialog(ctx, "Import to earshot?", |ui| {
            ui.label(if n == 1 {
                "That track isn't in earshot's library yet. Playlists, crates and Flows \
                 hold earshot's tracks, so it has to be imported first."
                    .to_string()
            } else {
                format!(
                    "{n} of those tracks aren't in earshot's library yet. Playlists, \
                     crates and Flows hold earshot's tracks, so they have to be imported \
                     first."
                )
            });
            muted(
                ui,
                "Their grids, cues and details come too. A USB stick's are copied \
                 into your first music folder.",
            );
            ui.checkbox(remember, "Don't ask again");
            go_cancel(ui, Button::primary("Import"), true, "")
        });
        match choice {
            Choice::Go(()) => {
                let (tracks, then, remember) = self.confirm.take().expect("open");
                out.push(
                    ExternalCmd::ImportNow {
                        tracks,
                        then: Box::new(then),
                        remember,
                    }
                    .into(),
                );
            }
            Choice::Close => self.confirm = None,
            Choice::Stay => {}
        }
    }

    /// The import window for these tracks of a library (by their ids there).
    pub fn ask_import(&mut self, source: SourceId, tracks: Vec<String>) {
        if let Some(s) = self.source(source).cloned() {
            self.open_import(&s, Target::Tracks(tracks));
        }
    }

    /// The tracks of a library earshot can play: its id there → ours (an earshot
    /// track with the same file, or an outside one).
    pub fn set_ids(&mut self, source: SourceId, ids: HashMap<String, TrackId>) {
        self.ids.insert(source, ids);
        self.rows = None;
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
                copy_to: source
                    .kind
                    .on_stick()
                    .then(|| self.music.first().cloned().unwrap_or_default()),
                target,
            });
        }
    }

    /// Our track with this track's file, if there is one.
    /// The import window, while it's open.
    fn import_window(&mut self, ctx: &egui::Context, out: &mut Vec<Command>) {
        let Some(d) = &mut self.dialog else { return };
        let what = match (&d.target, self.status.get(&d.source.id)) {
            (Target::Library, _) => format!("Everything in {}", d.source.name),
            (Target::Node(p), Some(Status::Ready(lib))) => lib
                .node(p)
                .map_or_else(|| "?".into(), |n| format!("\"{}\"", n.name)),
            (Target::Tracks(ids), Some(Status::Ready(lib))) => match ids.as_slice() {
                [one] => lib
                    .track(one)
                    .map_or_else(|| "?".into(), |t| format!("\"{}\"", t.display())),
                many => format!("{} tracks from {}", many.len(), d.source.name),
            },
            _ => String::new(),
        };
        let choice = widgets::dialog(ctx, "Import to earshot", |ui| {
            ui.label(what);
            muted(
                ui,
                "Tracks whose files are on this computer, with their title, artist, \
                 BPM, key and the like, and the playlists and crates they're in.",
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
            ui.add_enabled(
                memory,
                egui::Checkbox::new(&mut o.memory_cues, "Memory cues"),
            )
            .on_disabled_hover_text("Only Rekordbox has memory cues");
            ui.checkbox(&mut o.overwrite, "Overwrite existing metadata")
                .on_hover_text(
                    "Replace what earshot has (your own edits too), and set hot cues over \
                     earshot's. Without it, imports only fill gaps.",
                );
            if o.needs_cues() && d.source.kind == SourceKind::Serato {
                muted(
                    ui,
                    "Serato keeps cues and grids in the audio files: reading them takes a while.",
                );
            }
            let mut ready = true;
            if let Some(to) = &mut d.copy_to {
                ui.add_space(6.0);
                ui.label("Copy the files into:");
                if self.music.is_empty() {
                    ui.add(Note::danger(
                        "Add a music folder first (Sources › Folders).",
                    ));
                    ready = false;
                } else {
                    if !self.music.contains(to) {
                        *to = self.music[0].clone();
                    }
                    egui::ComboBox::from_id_salt("import-copy-to")
                        .selected_text(to.as_str())
                        .show_ui(ui, |ui| {
                            for m in &self.music {
                                ui.selectable_value(to, m.clone(), m);
                            }
                        });
                    muted(
                        ui,
                        "In a folder named after the stick. Tracks earshot has \
                         already (the same audio) aren't copied again.",
                    );
                }
            }
            go_cancel(ui, Button::primary("Import"), ready, "")
        });
        match choice {
            Choice::Go(()) => {
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
                        copy_to: d.copy_to.map(PathBuf::from),
                    }
                    .into(),
                );
            }
            Choice::Close => self.dialog = None,
            Choice::Stay => {}
        }
    }
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
    if let Some(c) = &r.copied {
        s += &format!(" {}", sticks::copied_text(c));
    }
    s
}
