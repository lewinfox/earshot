//! The library's track list and what it shows: every track, a folder on disk (the
//! Explorer tree, a source of its own), or a collection. Filtered by the search box
//! (`core::query`) and the columns' filters, sorted by a column or the search's
//! `sort:`. Drawn by the one [`TrackTable`], so it multi-selects, drags, reorders a
//! playlist's rows and has the track menu.

use std::collections::HashMap;

use egui::{Align, RichText, Ui};

use super::drag::Origin;
use super::table::{ColumnId, Filter, TableState, TableView, TrackTable};
use super::tracks::{Media, MenuAt};
use crate::core::collections::{CollectionCmd, CollectionId, CollectionKind};
use crate::core::command::Command;
use crate::core::index::LibraryCmd;
use crate::core::library::{self, Subdir};
use crate::core::query::{self, Expr, Query, Sort};
use crate::core::selection::Selection;
use crate::core::track::{Status, TrackId};
use crate::ui::panels::CoreView;
use crate::ui::theme::THEME;

/// What the track list shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Scope {
    #[default]
    All,
    /// An Explorer (filesystem) folder.
    Folder(String),
    /// A crate, playlist, smart playlist, Flow, or everything in a folder of them.
    Collection(CollectionId),
    /// Tracks that share their audio with another, a group at a time.
    Duplicates,
}

#[derive(Default)]
pub struct LibraryUi {
    /// The library folders (Explorer roots).
    pub folders: Vec<String>,
    scope: Scope,
    subdirs: HashMap<String, Vec<Subdir>>,
    pub search: String,
    /// The search as last parsed without a mistake.
    query: Query,
    /// What's wrong with the search as typed.
    error: Option<String>,
    /// The rows shown, in order.
    shown: Vec<TrackId>,
    /// What `shown` was worked out for: (library, collections, this pane) revisions,
    /// and the table's sort and filters.
    built_for: Option<((u64, u64, u64), Sort, Result<Expr, String>)>,
    /// Bumped when the search or scope change.
    rev: u64,
    /// Unanalysed tracks among `shown`, for the header button.
    unanalysed: Vec<TrackId>,
    sel: Selection,
    scroll_to: Option<usize>,
    /// Groups of tracks with the same audio, as last found (`LibraryCmd::FindDuplicates`).
    duplicates: Option<Vec<Vec<TrackId>>>,
    pub scanning: bool,
    pub search_focus: bool,
}

impl LibraryUi {
    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    pub fn set_scope(&mut self, scope: Scope) {
        if scope != self.scope {
            self.scope = scope;
            self.rev += 1;
        }
    }

    /// The groups of tracks with the same audio, as last found.
    #[cfg(test)]
    pub fn duplicates(&self) -> Option<&[Vec<TrackId>]> {
        self.duplicates.as_deref()
    }

    /// The groups of tracks with the same audio, as found.
    pub fn set_duplicates(&mut self, groups: Vec<Vec<TrackId>>) {
        self.duplicates = Some(groups);
        self.rev += 1;
    }

    /// The track with keyboard focus (for "load selected").
    pub fn focused(&self) -> Option<TrackId> {
        self.sel.focused()
    }

    pub fn move_focus(&mut self, delta: i32) {
        self.scroll_to = self.sel.move_focus(delta, &self.shown);
    }

    fn set_search(&mut self) {
        match query::parse(&self.search) {
            Ok(q) => {
                self.query = q;
                self.error = None;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
        self.rev += 1;
    }

    /// The kind of list the scope is, for its table's columns and sort.
    pub fn view(&self, v: &CoreView) -> TableView {
        match &self.scope {
            Scope::All => TableView::Library,
            Scope::Duplicates => TableView::Duplicates,
            Scope::Folder(_) => TableView::Explorer,
            Scope::Collection(id) => match v.collections.get(*id).map(|c| c.kind) {
                Some(CollectionKind::Playlist) => TableView::Playlist,
                Some(CollectionKind::Smart) => TableView::Smart,
                Some(CollectionKind::Crate) => TableView::Crate,
                Some(CollectionKind::Flow) => TableView::Flow,
                Some(CollectionKind::Folder) | None => TableView::Folder,
            },
        }
    }

    fn collection(&self) -> Option<CollectionId> {
        match self.scope {
            Scope::Collection(id) => Some(id),
            _ => None,
        }
    }

    /// Work the rows out again if the library, the collections, the search, the
    /// scope or the table's sort and filters changed.
    pub fn refresh(&mut self, v: &CoreView, table: &TableState) {
        let stamp = (v.lib.rev(), v.collections.rev(), self.rev);
        let (sort, filter) = (table.sort(), table.filter());
        if self
            .built_for
            .as_ref()
            .is_some_and(|b| b.0 == stamp && b.1 == sort && b.2 == filter)
        {
            return;
        }
        let mut q = self.query.clone();
        if let Ok(f) = &filter {
            q.expr = Expr::And(vec![q.expr, f.clone()]);
        }
        if q.sort.0.is_empty() {
            q.sort = sort.clone();
        }
        self.built_for = Some((stamp, sort, filter));
        let within: Option<Vec<TrackId>> = match &self.scope {
            Scope::All => None,
            // A group at a time, in the order found (unless sorted).
            Scope::Duplicates => Some(
                self.duplicates
                    .iter()
                    .flatten()
                    .flatten()
                    .copied()
                    .collect(),
            ),
            Scope::Folder(f) => {
                let dir = format!("{f}/");
                Some(
                    v.lib
                        .tracks()
                        .iter()
                        .filter(|t| t.path.starts_with(&dir))
                        .map(|t| t.id)
                        .collect(),
                )
            }
            Scope::Collection(id) => match v.collections.get(*id) {
                None => Some(vec![]),
                Some(c) if c.kind == CollectionKind::Smart => {
                    match query::parse(c.query.as_deref().unwrap_or("")) {
                        Ok(smart) => {
                            q.expr = Expr::And(vec![smart.expr, q.expr]);
                            if q.sort.0.is_empty() {
                                q.sort = smart.sort;
                            }
                            None
                        }
                        Err(_) => Some(vec![]),
                    }
                }
                Some(c) if c.kind == CollectionKind::Folder => {
                    Some(v.collections.tracks_inside(c.id))
                }
                Some(c) => Some(c.tracks.clone()),
            },
        };
        self.shown = v.lib.query(&q, v.collections, within.as_deref());
        self.sel.retain(&self.shown);
        self.unanalysed = self
            .shown
            .iter()
            .copied()
            .filter(|id| v.lib.get(*id).is_some_and(|t| !t.analysed()))
            .collect();
    }

    /// The list is a playlist in its own order, so its rows are its entries and can
    /// be dragged to a new place.
    fn reorderable(&self, v: &CoreView, table: &TableState) -> bool {
        self.collection()
            .and_then(|id| v.collections.get(id))
            .is_some_and(|c| c.kind == CollectionKind::Playlist)
            && self.query == Query::default()
            && table.sort.is_none()
            && !table.has_filters()
    }

    // ---- the Explorer -------------------------------------------------------------------

    /// The library folders on disk, as a tree.
    pub fn explorer(&mut self, ui: &mut Ui, v: &CoreView, out: &mut Vec<Command>) {
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.label(
                    RichText::new("FOLDERS ON DISK")
                        .size(10.0)
                        .color(THEME.muted),
                );
                if self.folders.is_empty() {
                    ui.label(
                        RichText::new("No folders yet. Menu > Import > Folder...")
                            .size(11.0)
                            .color(THEME.muted),
                    );
                }
                for root in self.folders.clone() {
                    let name = root.rsplit('/').next().unwrap_or(&root).to_string();
                    self.node(
                        ui,
                        &Subdir {
                            name,
                            path: root,
                            has_children: true,
                        },
                        true,
                        v,
                        out,
                    );
                }
                ui.add_space(6.0);
                if ui.button("Add a folder...").clicked() {
                    out.push(LibraryCmd::PickRoots.into());
                }
            });
    }

    fn label(&mut self, ui: &mut Ui, d: &Subdir, root: bool, v: &CoreView, out: &mut Vec<Command>) {
        let selected = matches!(&self.scope, Scope::Folder(f) if *f == d.path);
        let text = if root {
            RichText::new(&d.name).strong()
        } else {
            RichText::new(&d.name)
        };
        let r = ui.selectable_label(selected, text).on_hover_text(&d.path);
        if r.clicked() {
            self.set_scope(Scope::Folder(d.path.clone()));
        }
        r.context_menu(|ui| folder_menu(ui, &d.path, root, v, out));
    }

    fn node(&mut self, ui: &mut Ui, d: &Subdir, root: bool, v: &CoreView, out: &mut Vec<Command>) {
        if !d.has_children {
            ui.horizontal(|ui| {
                ui.add_space(ui.spacing().indent);
                self.label(ui, d, root, v, out);
            });
            return;
        }
        let id = ui.make_persistent_id(("folder", &d.path));
        let state =
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
        let header = state.show_header(ui, |ui| self.label(ui, d, root, v, out));
        header.body(|ui| {
            let kids = self
                .subdirs
                .entry(d.path.clone())
                .or_insert_with(|| library::list_subdirs(&d.path).unwrap_or_default())
                .clone();
            for k in &kids {
                self.node(ui, k, false, v, out);
            }
        });
    }

    // ---- all tracks ------------------------------------------------------------------------

    /// The All tracks source: every track, or only those with an analysis status
    /// (it sets the status column's filter), or the duplicates. True when one was
    /// clicked.
    pub fn all_tracks(
        &mut self,
        ui: &mut Ui,
        v: &CoreView,
        table: &mut TableState,
        out: &mut Vec<Command>,
    ) -> bool {
        let mut clicked = false;
        let mut count = [0usize; 4];
        for t in v.lib.tracks() {
            count[v.lib.status(t) as usize] += 1;
        }
        let only = match table.filters.iter().find(|(c, _)| *c == ColumnId::Status) {
            Some((_, Filter::Status(s))) if s.len() == 1 => s.first().copied(),
            _ => None,
        };
        let all = self.scope == Scope::All;
        if ui
            .selectable_label(
                all && only.is_none(),
                format!("All tracks  ({})", v.lib.tracks().len()),
            )
            .clicked()
        {
            self.set_scope(Scope::All);
            table.filters.retain(|(c, _)| *c != ColumnId::Status);
            clicked = true;
        }
        ui.add_space(4.0);
        ui.label(RichText::new("BY ANALYSIS").size(10.0).color(THEME.muted));
        for s in [Status::None, Status::Busy, Status::Attention, Status::Done] {
            let text = format!("{}  ({})", s.label(), count[s as usize]);
            if ui
                .selectable_label(all && only == Some(s), text)
                .on_hover_text(format!(
                    "Only tracks whose status is \"{}\" (in a search: status:{})",
                    s.label(),
                    s.name()
                ))
                .clicked()
            {
                self.set_scope(Scope::All);
                *table.filter_mut(ColumnId::Status) = Filter::Status([s].into());
                clicked = true;
            }
        }
        ui.add_space(4.0);
        ui.label(RichText::new("LIBRARY").size(10.0).color(THEME.muted));
        let dups = match (&self.duplicates, self.scope == Scope::Duplicates) {
            (Some(g), true) => format!("Duplicates  ({})", g.len()),
            _ => "Duplicates".to_string(),
        };
        if ui
            .selectable_label(self.scope == Scope::Duplicates, dups)
            .on_hover_text("Tracks whose audio is the same as another's")
            .clicked()
        {
            self.set_scope(Scope::Duplicates);
            // Found afresh each time.
            out.push(LibraryCmd::FindDuplicates.into());
            clicked = true;
        }
        clicked
    }

    // ---- the track list ---------------------------------------------------------------------

    fn title(&self, v: &CoreView) -> String {
        match &self.scope {
            Scope::All => "All tracks".into(),
            Scope::Duplicates => "Duplicates".into(),
            Scope::Folder(f) => f.rsplit('/').next().unwrap_or(f).into(),
            Scope::Collection(id) => v
                .collections
                .get(*id)
                .map_or_else(|| "(deleted)".into(), |c| c.name.clone()),
        }
    }

    /// Returns true when a row was clicked (so the list takes keyboard focus).
    pub fn list(
        &mut self,
        ui: &mut Ui,
        v: &CoreView,
        media: &mut Media,
        table: &mut TableState,
        out: &mut Vec<Command>,
    ) -> bool {
        self.refresh(v, table);
        ui.horizontal(|ui| {
            ui.label(RichText::new(self.title(v)).strong());
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                let n = self.unanalysed.len();
                let label = if n == 0 {
                    "All analysed".to_string()
                } else {
                    format!("Analyse all ({n})")
                };
                if ui
                    .add_enabled(n > 0, egui::Button::new(label))
                    .on_hover_text("Analyse every unanalysed track in this list (static mode)")
                    .clicked()
                {
                    let tracks = self.unanalysed.clone();
                    out.push(
                        LibraryCmd::Analyse {
                            tracks,
                            dynamic: false,
                        }
                        .into(),
                    );
                }
                let count = if self.scanning {
                    "scanning…".into()
                } else {
                    format!("{} tracks", self.shown.len())
                };
                ui.label(RichText::new(count).color(THEME.muted));
                let mut edit = egui::TextEdit::singleline(&mut self.search)
                    .hint_text("Search   bpm:120-126  key:8A  -tag:x  sort:-bpm   ( / )")
                    .desired_width(f32::INFINITY);
                if self.error.is_some() {
                    edit = edit.text_color(THEME.bad);
                }
                let mut search = ui.add(edit);
                if let Some(e) = &self.error {
                    search = search.on_hover_text(e);
                }
                if std::mem::take(&mut self.search_focus) {
                    search.request_focus();
                }
                if search.changed() {
                    self.set_search();
                }
            });
        });
        if self.scope == Scope::Duplicates {
            let text = match &self.duplicates {
                None => "Looking for tracks with the same audio…".to_string(),
                Some(g) if g.is_empty() => "No two tracks have the same audio.".to_string(),
                Some(g) => format!(
                    "{} with the same audio, one group after another. Right-click a \
                     track to show its file, or remove it from the library (the file \
                     stays on disk).",
                    match g.len() {
                        1 => "One group of tracks".to_string(),
                        n => format!("{n} groups of tracks"),
                    }
                ),
            };
            ui.label(RichText::new(text).color(THEME.muted));
        }
        if let Scope::Collection(id) = self.scope
            && let Some(c) = v
                .collections
                .get(id)
                .filter(|c| c.kind == CollectionKind::Smart)
        {
            ui.horizontal(|ui| {
                let q = c.query.as_deref().unwrap_or("");
                ui.label(RichText::new("Smart:").color(THEME.muted));
                ui.label(RichText::new(if q.is_empty() { "(every track)" } else { q }).monospace());
                if ui
                    .add_enabled(
                        self.error.is_none() && !self.search.trim().is_empty(),
                        egui::Button::new("Use the search"),
                    )
                    .on_hover_text("Save the search as this smart playlist's query")
                    .clicked()
                {
                    out.push(CollectionCmd::SetQuery(id, self.search.trim().to_string()).into());
                    self.search.clear();
                    self.set_search();
                }
            });
        }
        self.refresh(v, table);
        if self.folders.is_empty() && !self.scanning {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("No library folders yet. Menu > Import > Folder...")
                        .color(THEME.muted),
                )
            });
            return false;
        }

        let ctrl = ui.input(|i| i.modifiers.command);
        if ctrl && ui.input(|i| i.key_pressed(egui::Key::A)) && !ui.ctx().wants_keyboard_input() {
            self.sel.select_all(&self.shown);
        }
        let viewing = self.collection();
        let reorder = self.reorderable(v, table);
        let takes_drops = viewing
            .and_then(|id| v.collections.get(id))
            .is_some_and(|c| c.kind.takes_tracks());
        let shown = std::mem::take(&mut self.shown);
        let res = TrackTable {
            id: "library",
            rows: &shown,
            state: table,
            available: self.view(v).available(),
            sel: &mut self.sel,
            origin: viewing.map_or(Origin::Library, Origin::Collection),
            menu: MenuAt { viewing },
            row_height: 26.0,
            reorder,
            takes_drops,
            scores: None,
            scroll_to: self.scroll_to.take(),
            show_selection: true,
        }
        .show(ui, v, media, out);
        self.shown = shown;
        if let (Some((from, to)), Some(playlist)) = (res.moved, viewing) {
            out.push(CollectionCmd::MoveEntries { playlist, from, to }.into());
        }
        if let (Some((tracks, at)), Some(to)) = (res.dropped, viewing) {
            drop_into(to, tracks, at.filter(|_| reorder), v, out);
        }
        res.clicked
    }
}

/// Tracks dropped on a collection's list: added, and in a playlist shown in its own
/// order, moved to where they were dropped.
fn drop_into(
    to: CollectionId,
    tracks: Vec<TrackId>,
    at: Option<usize>,
    v: &CoreView,
    out: &mut Vec<Command>,
) {
    let end = v.collections.get(to).map_or(0, |c| c.tracks.len());
    let cmd = match at.filter(|&a| a < end) {
        Some(at) => CollectionCmd::Insert {
            playlist: to,
            tracks,
            at,
        },
        None => CollectionCmd::AddTracks { to, tracks },
    };
    out.push(cmd.into());
}

/// Right-click on an Explorer folder: analyse what's in it, or stop scanning it.
fn folder_menu(ui: &mut Ui, path: &str, root: bool, v: &CoreView, out: &mut Vec<Command>) {
    let prefix = format!("{path}/");
    let inside: Vec<_> = v
        .lib
        .tracks()
        .iter()
        .filter(|t| t.path.starts_with(&prefix))
        .collect();
    let fresh: Vec<TrackId> = inside
        .iter()
        .filter(|t| !t.analysed())
        .map(|t| t.id)
        .collect();
    let drifting: Vec<TrackId> = inside.iter().filter(|t| t.drifts).map(|t| t.id).collect();
    let all: Vec<TrackId> = inside.iter().map(|t| t.id).collect();
    let mut analyse = |ui: &mut Ui, tracks: Vec<TrackId>, dynamic: bool| {
        out.push(LibraryCmd::Analyse { tracks, dynamic }.into());
        ui.close();
    };
    if ui
        .add_enabled(
            !fresh.is_empty(),
            egui::Button::new(format!("Analyse {} unanalysed tracks", fresh.len())),
        )
        .clicked()
    {
        analyse(ui, fresh, false);
    }
    if ui
        .button(format!("Analyse all {} tracks again", all.len()))
        .clicked()
    {
        analyse(ui, all, false);
    }
    if ui
        .add_enabled(
            !drifting.is_empty(),
            egui::Button::new(format!(
                "Analyse {} drifting tracks (dynamic)",
                drifting.len()
            )),
        )
        .clicked()
    {
        analyse(ui, drifting, true);
    }
    if root {
        ui.separator();
        if ui.button("Remove from library").clicked() {
            out.push(LibraryCmd::RemoveRoot(path.to_string()).into());
            ui.close();
        }
    }
}
