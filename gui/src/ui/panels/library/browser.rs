//! The library browser: the folder tree on the left (collections, then the
//! filesystem Explorer, like Rekordbox's) and the track list. Tracks can be
//! multi-selected (Ctrl/Shift-click, Ctrl+A), dragged onto a deck or a collection,
//! double-clicked to load, and right-clicked for the track menu. The list shows the
//! whole library, an Explorer folder, or a collection, filtered by the search box
//! (`core::query`) and sorted by a column or the search's `sort:`.

use std::collections::HashMap;

use egui::{Align, RichText, Sense, Ui, vec2};
use egui_extras::{Column, TableBuilder};

use super::tracks::{Media, MenuAt, status_mark, title_cell, track_menu};
use crate::core::collections::{CollectionCmd, CollectionId, CollectionKind};
use crate::core::command::Command;
use crate::core::index::LibraryCmd;
use crate::core::library::{self, Subdir};
use crate::core::query::{self, Dir, Expr, Field, Query, Sort};
use crate::core::selection::{Mods, Selection};
use crate::core::track::TrackId;
use crate::ui::panels::CoreView;
use crate::ui::theme::THEME;
use crate::ui::util;
use crate::ui::widgets::key_chip;

/// What the track list shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Scope {
    #[default]
    All,
    /// An Explorer (filesystem) folder.
    Folder(String),
    /// A crate, playlist, smart playlist, Flow, or everything in a folder of them.
    Collection(CollectionId),
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
    /// From the column headers; the search's `sort:` wins over it.
    sort: Sort,
    /// The rows shown, in order.
    shown: Vec<TrackId>,
    /// (library, collections, this pane) revisions `shown` was worked out for.
    built_for: Option<(u64, u64, u64)>,
    /// Bumped when the search, scope or sort change.
    rev: u64,
    /// Unanalysed tracks among `shown`, for the header button.
    unanalysed: Vec<TrackId>,
    sel: Selection,
    scroll_to: Option<usize>,
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

    /// The track with keyboard focus (for "load selected").
    pub fn focused(&self) -> Option<TrackId> {
        self.sel.focused()
    }

    /// The selected tracks, in list order.
    pub fn selected(&self) -> Vec<TrackId> {
        self.sel.in_order(&self.shown)
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

    /// Work the rows out again if the library, the collections, the search, the
    /// scope or the sort changed.
    pub fn refresh(&mut self, v: &CoreView) {
        let stamp = (v.lib.rev(), v.collections.rev(), self.rev);
        if self.built_for == Some(stamp) {
            return;
        }
        self.built_for = Some(stamp);
        let mut q = self.query.clone();
        if q.sort.0.is_empty() {
            q.sort = self.sort.clone();
        }
        let within: Option<Vec<TrackId>> = match &self.scope {
            Scope::All => None,
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
                    let mut seen = std::collections::HashSet::new();
                    Some(
                        v.collections
                            .all()
                            .iter()
                            .filter(|i| {
                                i.id != c.id
                                    && i.parent.is_some_and(|p| v.collections.is_within(p, c.id))
                            })
                            .flat_map(|i| i.tracks.iter().copied())
                            .filter(|t| seen.insert(*t))
                            .collect(),
                    )
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

    // ---- folder tree ----------------------------------------------------------------------

    /// `collections` draws the collections between *All tracks* and the Explorer.
    pub fn tree(
        &mut self,
        ui: &mut Ui,
        v: &CoreView,
        out: &mut Vec<Command>,
        collections: impl FnOnce(&mut Ui),
    ) {
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                if ui
                    .selectable_label(self.scope == Scope::All, "All tracks")
                    .clicked()
                {
                    self.set_scope(Scope::All);
                }
                ui.add_space(6.0);
                collections(ui);
                ui.add_space(6.0);
                ui.label(RichText::new("EXPLORER").size(10.0).color(THEME.muted));
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

    // ---- track list -----------------------------------------------------------------------

    fn title(&self, v: &CoreView) -> String {
        match &self.scope {
            Scope::All => "All tracks".into(),
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
        out: &mut Vec<Command>,
    ) -> bool {
        self.refresh(v);
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
        self.refresh(v);
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
        let mut table = TableBuilder::new(ui)
            .striped(true)
            .sense(Sense::click_and_drag())
            .cell_layout(egui::Layout::left_to_right(Align::Center))
            .column(Column::exact(34.0))
            .column(Column::exact(100.0))
            .column(Column::remainder().at_least(160.0).clip(true))
            .column(
                Column::initial(200.0)
                    .at_least(80.0)
                    .resizable(true)
                    .clip(true),
            )
            .column(Column::exact(56.0))
            .column(Column::exact(40.0))
            .column(Column::exact(44.0));
        if let Some(r) = self.scroll_to.take() {
            table = table.scroll_to_row(r, Some(Align::Center));
        }
        let shown = std::mem::take(&mut self.shown);
        let mut clicked: Option<(TrackId, egui::Modifiers)> = None;
        let mut right_clicked: Option<TrackId> = None;
        let mut sort_by: Option<Field> = None;
        let at = MenuAt {
            viewing: match self.scope {
                Scope::Collection(id) => Some(id),
                _ => None,
            },
        };
        let effective = if self.query.sort.0.is_empty() {
            &self.sort
        } else {
            &self.query.sort
        };
        table
            .header(20.0, |mut h| {
                for (name, field) in [
                    ("", None),
                    ("", None),
                    ("Title", Some(Field::Title)),
                    ("Artist", Some(Field::Artist)),
                    ("BPM", Some(Field::Bpm)),
                    ("Key", Some(Field::Key)),
                    ("Time", Some(Field::Duration)),
                ] {
                    h.col(|ui| {
                        let arrow = match effective.0.first() {
                            Some((f, d)) if Some(*f) == field => match d {
                                Dir::Asc => " ^",
                                Dir::Desc => " v",
                            },
                            _ => "",
                        };
                        let text = RichText::new(format!("{name}{arrow}"))
                            .size(11.0)
                            .color(THEME.muted);
                        match field {
                            Some(f) => {
                                if ui
                                    .add(egui::Label::new(text).sense(Sense::click()))
                                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                                    .clicked()
                                {
                                    sort_by = Some(f);
                                }
                            }
                            None => {
                                ui.label(text);
                            }
                        }
                    });
                }
            })
            .body(|body| {
                body.rows(26.0, shown.len(), |mut row| {
                    let id = shown[row.index()];
                    let Some(t) = v.lib.get(id) else { return };
                    row.set_selected(self.sel.contains(id));
                    row.col(|ui| status_mark(ui, t, v.lib.analysis(id)));
                    row.col(|ui| {
                        if let Some(tex) = media.thumb(ui.ctx(), id) {
                            ui.add(egui::Image::new((tex, vec2(96.0, 18.0))));
                        }
                    });
                    row.col(|ui| title_cell(ui, t, v.history.was_played(id), false));
                    row.col(|ui| {
                        ui.add(
                            egui::Label::new(RichText::new(&t.artist).color(THEME.muted))
                                .truncate()
                                .selectable(false),
                        );
                    });
                    row.col(|ui| {
                        let bpm = t
                            .bpm
                            .map(|b| format!("{b:.2}"))
                            .unwrap_or_else(|| "-".into());
                        let text = RichText::new(bpm).color(THEME.muted);
                        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                            ui.add(
                                egui::Label::new(if t.analysed() { text } else { text.italics() })
                                    .selectable(false),
                            )
                            .on_hover_text(if t.analysed() {
                                "Analysed"
                            } else {
                                "From the file's tag (not analysed)"
                            });
                        });
                    });
                    row.col(|ui| {
                        if let Some(k) = t.key {
                            key_chip(ui, k);
                        }
                    });
                    row.col(|ui| {
                        ui.label(
                            RichText::new(t.duration.map(util::mmss).unwrap_or_default())
                                .color(THEME.muted),
                        );
                    });
                    let resp = row.response();
                    if resp.clicked() {
                        clicked = Some((id, resp.ctx.input(|i| i.modifiers)));
                    }
                    if resp.double_clicked() {
                        out.push(LibraryCmd::Load(id).into());
                    }
                    if resp.secondary_clicked() {
                        right_clicked = Some(id);
                    }
                    resp.dnd_set_drag_payload(id);
                    resp.context_menu(|ui| {
                        let sel = if self.sel.contains(id) {
                            self.sel.in_order(&shown)
                        } else {
                            vec![id]
                        };
                        track_menu(ui, &sel, v, at, out);
                    });
                });
            });
        self.shown = shown;
        if let Some(id) = right_clicked {
            self.sel.right_click(id);
        }
        if let Some((id, m)) = clicked {
            let mods = Mods {
                shift: m.shift,
                command: m.command,
            };
            self.sel.click(id, &self.shown, mods);
        }
        if let Some(f) = sort_by {
            // Up, down, then back to the list's own order.
            self.sort = match self.sort.0.first() {
                Some(&(g, Dir::Asc)) if g == f => Sort(vec![(f, Dir::Desc)]),
                Some(&(g, Dir::Desc)) if g == f => Sort::default(),
                _ => Sort(vec![(f, Dir::Asc)]),
            };
            self.rev += 1;
        }
        clicked.is_some() || right_clicked.is_some()
    }
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
