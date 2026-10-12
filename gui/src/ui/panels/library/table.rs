//! The one track table, for the library, collections and the suggestions: columns
//! from [`COLUMNS`] ([`ColumnDef`]), picked, ordered and sized per view
//! ([`TableState`], saved in `UiPrefs`); sort by any column with a field, filter by
//! any of them (status and colour by picking, the rest by typing what follows
//! `field:` in a search); multi-select by track id; drag the selection anywhere
//! ([`DragPayload`]); drag rows to a new place in a playlist.
//!
//! The table draws rows it's given; the owner works them out (filters, sort) from
//! [`TableState::sort`] and [`TableState::filter`], again when [`TableOut::changed`].

use std::collections::{BTreeSet, HashMap};

use egui::{Align, Color32, CornerRadius, RichText, Sense, Stroke, Ui, pos2, vec2};
use egui_extras::{Column, TableBuilder};
use serde::{Deserialize, Serialize};

use super::drag::{self, DragPayload, Origin};
use super::tracks::{Media, MenuAt, status_mark, title_cell, track_menu};
use crate::core::command::Command;
use crate::core::edit::{self, EditCmd};
use crate::core::index::LibraryCmd;
use crate::core::query::{self, Dir, Expr, Field, Op, Sort, Value};
use crate::core::selection::{Mods, Selection};
use crate::core::suggest::Scored;
use crate::core::track::{Status, Track, TrackId};
use crate::ui::panels::CoreView;
use crate::ui::theme::{CUE_PALETTE, THEME, rgb};
use crate::ui::util;
use crate::ui::widgets::key_chip;

// ---- columns --------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ColumnId {
    /// The row's place in the list (a playlist's order).
    Index,
    Status,
    Wave,
    Title,
    Artist,
    Album,
    Genre,
    Bpm,
    Key,
    Time,
    Energy,
    Rating,
    Colour,
    Tags,
    Comment,
    File,
    /// Suggestions: the tempo change needed.
    Change,
    /// Suggestions: the score, 0-100.
    Score,
    /// Suggestions: a bar per part of the score.
    Parts,
}

/// What a cell draws from.
pub struct Cell<'a> {
    pub t: &'a Track,
    pub v: CoreView<'a>,
    pub media: &'a mut Media,
    /// The row's suggestion score, in the suggestions.
    pub score: Option<&'a Scored>,
    /// The row's place in the list.
    pub row: usize,
    /// Rows are tall enough for the artist under the title.
    pub two_lines: bool,
    /// The list's selection and rows, for an edit made in a cell (it applies to
    /// the selection when the row is in it).
    pub sel: &'a Selection,
    pub rows: &'a [TrackId],
    pub out: &'a mut Vec<Command>,
}

impl Cell<'_> {
    /// The tracks an edit in this cell applies to.
    pub fn targets(&self) -> Vec<TrackId> {
        if self.sel.contains(self.t.id) {
            self.sel.in_order(self.rows)
        } else {
            vec![self.t.id]
        }
    }
}

/// One column: its header, the field it sorts and filters by, its size and how a
/// cell is drawn.
pub struct ColumnDef {
    pub id: ColumnId,
    /// In the header (may be empty).
    pub title: &'static str,
    /// In the columns menu.
    pub name: &'static str,
    /// Sorts and filters by this (`None`: neither, by field).
    pub field: Option<Field>,
    /// Starting width.
    pub width: f32,
    /// Always this wide (not resizable).
    pub fixed: bool,
    pub cell: fn(&mut Ui, &mut Cell<'_>),
}

const fn col(
    id: ColumnId,
    title: &'static str,
    name: &'static str,
    field: Option<Field>,
    width: f32,
    fixed: bool,
    cell: fn(&mut Ui, &mut Cell<'_>),
) -> ColumnDef {
    ColumnDef {
        id,
        title,
        name,
        field,
        width,
        fixed,
        cell,
    }
}

/// Every column, in the columns menu's order.
pub const COLUMNS: [ColumnDef; 19] = [
    col(
        ColumnId::Index,
        "#",
        "# (list order)",
        None,
        30.0,
        true,
        index_cell,
    ),
    col(
        ColumnId::Status,
        "",
        "Analysis status",
        Some(Field::Status),
        34.0,
        true,
        |ui, c| status_mark(ui, c.t, c.v.lib.analysis(c.t.id)),
    ),
    col(ColumnId::Wave, "", "Waveform", None, 100.0, true, wave_cell),
    col(
        ColumnId::Title,
        "Title",
        "Title",
        Some(Field::Title),
        200.0,
        false,
        |ui, c| title_cell(ui, c.t, c.v.history.was_played(c.t.id), c.two_lines),
    ),
    col(
        ColumnId::Artist,
        "Artist",
        "Artist",
        Some(Field::Artist),
        200.0,
        false,
        |ui, c| muted(ui, &c.t.artist),
    ),
    col(
        ColumnId::Album,
        "Album",
        "Album",
        Some(Field::Album),
        160.0,
        false,
        |ui, c| muted(ui, &c.t.album),
    ),
    col(
        ColumnId::Genre,
        "Genre",
        "Genre",
        Some(Field::Genre),
        110.0,
        false,
        |ui, c| muted(ui, &c.t.genre),
    ),
    col(
        ColumnId::Bpm,
        "BPM",
        "BPM",
        Some(Field::Bpm),
        56.0,
        false,
        bpm_cell,
    ),
    col(
        ColumnId::Key,
        "Key",
        "Key",
        Some(Field::Key),
        40.0,
        false,
        |ui, c| {
            if let Some(k) = c.t.key {
                key_chip(ui, k);
            }
        },
    ),
    col(
        ColumnId::Time,
        "Time",
        "Time",
        Some(Field::Duration),
        44.0,
        false,
        |ui, c| muted(ui, &c.t.duration.map(util::mmss).unwrap_or_default()),
    ),
    col(
        ColumnId::Energy,
        "Energy",
        "Energy (1-10)",
        Some(Field::Energy),
        50.0,
        false,
        |ui, c| {
            muted(
                ui,
                &c.t.energy_level()
                    .map_or(String::new(), |e| format!("{e:.0}")),
            )
        },
    ),
    col(
        ColumnId::Rating,
        "Rating",
        "Rating",
        Some(Field::Rating),
        64.0,
        false,
        rating_cell,
    ),
    col(
        ColumnId::Colour,
        "",
        "Colour",
        Some(Field::Colour),
        22.0,
        true,
        colour_cell,
    ),
    col(
        ColumnId::Tags,
        "Tags",
        "Tags",
        Some(Field::Tag),
        140.0,
        false,
        |ui, c| muted(ui, &c.t.tags.join(", ")),
    ),
    col(
        ColumnId::Comment,
        "Comment",
        "Comment",
        Some(Field::Comment),
        160.0,
        false,
        |ui, c| muted(ui, &c.t.comment),
    ),
    col(
        ColumnId::File,
        "File",
        "File name",
        Some(Field::Path),
        160.0,
        false,
        |ui, c| {
            let name = c.t.path.rsplit('/').next().unwrap_or(&c.t.path);
            muted(ui, name)
        },
    ),
    col(
        ColumnId::Change,
        "",
        "Tempo change",
        None,
        48.0,
        true,
        change_cell,
    ),
    col(ColumnId::Score, "", "Score", None, 28.0, true, |ui, c| {
        if let Some(s) = c.score {
            ui.label(RichText::new(format!("{:.0}", s.total * 100.0)).strong());
        }
    }),
    col(
        ColumnId::Parts,
        "",
        "Score parts",
        None,
        54.0,
        true,
        |ui, c| {
            if let Some(s) = c.score {
                bars(ui, s);
            }
        },
    ),
];

pub fn def(id: ColumnId) -> &'static ColumnDef {
    COLUMNS
        .iter()
        .find(|c| c.id == id)
        .expect("every column has a def")
}

/// The columns any library list can show.
pub const LIBRARY_COLUMNS: [ColumnId; 16] = [
    ColumnId::Index,
    ColumnId::Status,
    ColumnId::Wave,
    ColumnId::Colour,
    ColumnId::Title,
    ColumnId::Artist,
    ColumnId::Album,
    ColumnId::Genre,
    ColumnId::Bpm,
    ColumnId::Key,
    ColumnId::Time,
    ColumnId::Energy,
    ColumnId::Rating,
    ColumnId::Tags,
    ColumnId::Comment,
    ColumnId::File,
];

/// The suggestions' columns: the library's and the score's.
pub const SUGGEST_COLUMNS: [ColumnId; 18] = [
    ColumnId::Status,
    ColumnId::Wave,
    ColumnId::Colour,
    ColumnId::Title,
    ColumnId::Artist,
    ColumnId::Album,
    ColumnId::Genre,
    ColumnId::Bpm,
    ColumnId::Key,
    ColumnId::Time,
    ColumnId::Energy,
    ColumnId::Rating,
    ColumnId::Tags,
    ColumnId::Comment,
    ColumnId::File,
    ColumnId::Change,
    ColumnId::Score,
    ColumnId::Parts,
];

// ---- per-view state --------------------------------------------------------------------

/// The kinds of list a table can show; each keeps its own columns and sort.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TableView {
    /// All tracks.
    Library,
    /// A folder on disk.
    Explorer,
    Playlist,
    Smart,
    Crate,
    /// A Flow's tracks, as a list.
    Flow,
    /// Everything in a collection folder.
    Folder,
    Suggest,
    /// Tracks that share their audio, a group at a time.
    Duplicates,
}

impl TableView {
    pub fn available(self) -> &'static [ColumnId] {
        match self {
            TableView::Suggest => &SUGGEST_COLUMNS,
            _ => &LIBRARY_COLUMNS,
        }
    }

    /// How a new view starts.
    pub fn default_state(self) -> TableState {
        use ColumnId::*;
        let ids: &[ColumnId] = match self {
            TableView::Suggest => &[Title, Change, Key, Score, Parts],
            TableView::Playlist | TableView::Flow => {
                &[Index, Status, Wave, Title, Artist, Bpm, Key, Time]
            }
            TableView::Duplicates => &[Status, Title, Artist, Bpm, Time, File],
            _ => &[Status, Wave, Title, Artist, Bpm, Key, Time],
        };
        TableState {
            columns: ids.iter().map(|&id| (id, def(id).width)).collect(),
            sort: None,
            filters: vec![],
            sized: BTreeSet::new(),
            fitted: 0,
        }
    }
}

/// A filter on one column.
#[derive(Clone, Debug, PartialEq)]
pub enum Filter {
    /// What would follow `field:` in a search ("deep", "120-126", ">=4").
    Text(String),
    /// Any of these statuses.
    Status(BTreeSet<Status>),
    /// Any of these colours (`None`: no colour).
    Colour(BTreeSet<Option<u32>>),
}

impl Filter {
    fn is_empty(&self) -> bool {
        match self {
            Filter::Text(s) => s.trim().is_empty(),
            Filter::Status(s) => s.is_empty(),
            Filter::Colour(s) => s.is_empty(),
        }
    }

    /// As a query term on `field`.
    fn expr(&self, field: Field) -> Result<Expr, query::ParseError> {
        Ok(match self {
            Filter::Text(s) => {
                let s = s.trim();
                // Comparisons go straight after the name ("bpm>=128"); anything else
                // after a colon.
                let sep = if s.starts_with(['<', '>', '=']) {
                    ""
                } else {
                    ":"
                };
                let quoted = if s.contains(char::is_whitespace) && !s.starts_with('"') {
                    format!("\"{s}\"")
                } else {
                    s.to_string()
                };
                query::parse(&format!("{}{sep}{quoted}", field.name()))?.expr
            }
            Filter::Status(set) => Expr::Or(
                set.iter()
                    .map(|s| Expr::Cmp(Field::Status, Op::Eq, Value::Status(*s)))
                    .collect(),
            ),
            Filter::Colour(set) => Expr::Or(
                set.iter()
                    .map(|c| match c {
                        Some(c) => Expr::Cmp(Field::Colour, Op::Eq, Value::Num(*c as f64)),
                        None => {
                            Expr::Not(Box::new(Expr::Cmp(Field::Colour, Op::Ge, Value::Num(0.0))))
                        }
                    })
                    .collect(),
            ),
        })
    }

    /// For the chip above the table.
    fn summary(&self) -> String {
        match self {
            Filter::Text(s) => s.trim().to_string(),
            Filter::Status(set) => set
                .iter()
                .map(|s| s.label())
                .collect::<Vec<_>>()
                .join(" or "),
            Filter::Colour(set) => set
                .iter()
                .map(|c| colour_name(*c))
                .collect::<Vec<_>>()
                .join(" or "),
        }
    }
}

/// One table's columns, sort and filters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TableState {
    /// The columns shown, in order, with their widths (points).
    pub columns: Vec<(ColumnId, f32)>,
    /// Sorted by this column (`None`: the list's own order).
    pub sort: Option<(ColumnId, Dir)>,
    /// Filters by column, all of which must match. Not kept between runs.
    #[serde(skip)]
    pub filters: Vec<(ColumnId, Filter)>,
    /// Columns sized by hand. The text columns not in it fit the library's longest
    /// value ([`auto_widths`]).
    pub sized: BTreeSet<ColumnId>,
    /// Bumped by "Fit columns to contents", so egui forgets the dragged widths.
    #[serde(skip)]
    pub fitted: u32,
}

impl Default for TableState {
    fn default() -> TableState {
        TableView::Library.default_state()
    }
}

impl TableState {
    /// The sort for `core::query` (empty: the list's own order).
    pub fn sort(&self) -> Sort {
        match self.sort {
            Some((c, dir)) => match def(c).field {
                Some(f) => Sort(vec![(f, dir)]),
                None => Sort::default(),
            },
            None => Sort::default(),
        }
    }

    /// Every column filter as one query term, or what's wrong with one of them.
    pub fn filter(&self) -> Result<Expr, String> {
        let mut all = vec![];
        for (c, f) in &self.filters {
            let d = def(*c);
            let Some(field) = d.field else { continue };
            if f.is_empty() {
                continue;
            }
            all.push(f.expr(field).map_err(|e| format!("{}: {e}", d.name))?);
        }
        Ok(Expr::And(all))
    }

    pub fn has_filters(&self) -> bool {
        self.filters.iter().any(|(_, f)| !f.is_empty())
    }

    fn filter_of(&self, c: ColumnId) -> Option<&Filter> {
        self.filters
            .iter()
            .find(|(id, f)| *id == c && !f.is_empty())
            .map(|(_, f)| f)
    }

    /// The column's filter, made (empty, of the right kind) if it hasn't one.
    pub fn filter_mut(&mut self, c: ColumnId) -> &mut Filter {
        if let Some(i) = self.filters.iter().position(|(id, _)| *id == c) {
            return &mut self.filters[i].1;
        }
        let f = match def(c).field {
            Some(Field::Status) => Filter::Status(BTreeSet::new()),
            Some(Field::Colour) => Filter::Colour(BTreeSet::new()),
            _ => Filter::Text(String::new()),
        };
        self.filters.push((c, f));
        &mut self.filters.last_mut().unwrap().1
    }

    /// A header click: up, down, then back to the list's own order. The "#" column
    /// goes straight back to it.
    pub fn click_sort(&mut self, c: ColumnId) {
        if def(c).field.is_none() {
            self.sort = None;
            return;
        }
        self.sort = match self.sort {
            Some((s, Dir::Asc)) if s == c => Some((c, Dir::Desc)),
            Some((s, Dir::Desc)) if s == c => None,
            _ => Some((c, Dir::Asc)),
        };
    }

    /// Show or hide a column (shown: at the end, at its default width).
    pub fn toggle_column(&mut self, c: ColumnId) {
        if let Some(i) = self.columns.iter().position(|(id, _)| *id == c) {
            if self.columns.len() > 1 {
                self.columns.remove(i);
                self.filters.retain(|(id, _)| *id != c);
                if self.sort.is_some_and(|(s, _)| s == c) {
                    self.sort = None;
                }
            }
        } else {
            self.columns.push((c, def(c).width));
        }
    }

    fn shows(&self, c: ColumnId) -> bool {
        self.columns.iter().any(|(id, _)| *id == c)
    }
}

/// Every view's table state, kept in `UiPrefs`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tables(pub HashMap<TableView, TableState>);

impl Tables {
    pub fn get_mut(&mut self, v: TableView) -> &mut TableState {
        self.0.entry(v).or_insert_with(|| v.default_state())
    }
}

// ---- the table ---------------------------------------------------------------------

/// One frame of a track table. Build it, then [`TrackTable::show`].
pub struct TrackTable<'a> {
    /// Unique among tables on screen.
    pub id: &'static str,
    pub rows: &'a [TrackId],
    pub state: &'a mut TableState,
    /// Which columns it can show.
    pub available: &'static [ColumnId],
    pub sel: &'a mut Selection,
    /// Where drags from it come from.
    pub origin: Origin,
    /// For the track menu's "Remove from".
    pub menu: MenuAt,
    pub row_height: f32,
    /// Rows can be dragged to a new place (a playlist shown in its own order).
    pub reorder: bool,
    /// Tracks from elsewhere can be dropped on it (a crate, playlist or Flow).
    pub takes_drops: bool,
    /// Suggestion scores by track, for the score columns.
    pub scores: Option<&'a HashMap<TrackId, Scored>>,
    pub scroll_to: Option<usize>,
    /// Highlight the selected rows (the suggestions only do while focused).
    pub show_selection: bool,
}

/// What happened in the table this frame.
#[derive(Default, Debug)]
pub struct TableOut {
    /// A row was clicked: the list takes focus.
    pub clicked: bool,
    /// The sort or a filter changed: work the rows out again.
    pub changed: bool,
    /// Rows at these places were dragged to before this place.
    pub moved: Option<(Vec<usize>, usize)>,
    /// Tracks from another list were dropped, before this place (`None`: not over a
    /// row, so at the end).
    pub dropped: Option<(Vec<TrackId>, Option<usize>)>,
}

/// What a row asked for, carried out after the table is drawn.
enum RowAct {
    Click(TrackId, Mods),
    RightClick(TrackId),
    Drag(TrackId),
}

impl TrackTable<'_> {
    pub fn show(
        self,
        ui: &mut Ui,
        v: &CoreView,
        media: &mut Media,
        out: &mut Vec<Command>,
    ) -> TableOut {
        let TrackTable {
            id,
            rows,
            state,
            available,
            sel,
            origin,
            menu,
            row_height,
            reorder,
            takes_drops,
            scores,
            scroll_to,
            show_selection,
        } = self;
        let mut res = TableOut::default();
        res.changed |= filter_chips(ui, state);

        let auto = auto_widths(ui.ctx(), v.lib);
        // Hand-sized columns keep their width; the text ones fit their contents and
        // the rest start at their usual width. Then the spare room goes to the text
        // columns not sized by hand, so the table fills the pane.
        let natural: Vec<(ColumnId, f32)> = state
            .columns
            .iter()
            .map(|&(c, w)| {
                let w = match auto.get(&c) {
                    _ if state.sized.contains(&c) => w,
                    Some(&a) => a,
                    None => def(c).width,
                };
                (c, w)
            })
            .collect();
        let gaps = ui.spacing().item_spacing.x * natural.len() as f32
            + ui.spacing().scroll.allocated_width();
        let filled = fill(&natural, ui.available_width() - gaps, |c| {
            auto.contains_key(&c) && !state.sized.contains(&c)
        });
        // Kept as they are while a column is dragged: a new layout would end the drag.
        let keep = ui.id().with((id, "filled"));
        let dragging = ui.input(|i| i.pointer.primary_down());
        let shown: Vec<(ColumnId, f32)> = match ui.data(|d| d.get_temp(keep)) {
            Some(old) if dragging => old,
            _ => {
                ui.data_mut(|d| d.insert_temp(keep, filled.clone()));
                filled
            }
        };
        // A different set of columns is a different table to egui (its saved widths
        // are by position), and so are new fitted widths, so they take effect.
        let salt: Vec<(ColumnId, u32)> = shown
            .iter()
            .map(|&(c, w)| {
                (
                    c,
                    if state.sized.contains(&c) {
                        0
                    } else {
                        w as u32
                    },
                )
            })
            .collect();
        let mut table = TableBuilder::new(ui)
            .id_salt((id, salt, state.fitted))
            .striped(true)
            .sense(Sense::click_and_drag())
            .cell_layout(egui::Layout::left_to_right(Align::Center));
        for &(c, w) in &shown {
            let d = def(c);
            table = table.column(if d.fixed {
                Column::exact(d.width)
            } else {
                Column::initial(w).at_least(24.0).resizable(true).clip(true)
            });
        }
        if let Some(r) = scroll_to {
            table = table.scroll_to_row(r, Some(Align::Center));
        }

        let mut header_click: Option<ColumnId> = None;
        let mut widths: Vec<(ColumnId, f32)> = vec![];
        let mut acts: Vec<RowAct> = vec![];
        let mut row_rects: Vec<(usize, egui::Rect)> = vec![];
        let two_lines = row_height > 30.0;
        let state_now = state.clone();
        let mut menu_state: Option<TableState> = None;
        let body = table
            .header(20.0, |mut h| {
                for &(c, _) in &shown {
                    // The column's width (not the header's contents').
                    let mut col_w = 0.0;
                    h.col(|ui| {
                        col_w = ui.max_rect().width();
                        let r =
                            ui.interact(ui.max_rect(), ui.id().with(("header", c)), Sense::click());
                        header_label(ui, c, &state_now);
                        if r.clicked() {
                            header_click = Some(c);
                        }
                        r.on_hover_cursor(egui::CursorIcon::PointingHand)
                            .context_menu(|ui| {
                                let s = menu_state.get_or_insert_with(|| state_now.clone());
                                header_menu(ui, c, s, available, v);
                            });
                    });
                    widths.push((c, col_w));
                }
            })
            .body(|body| {
                body.rows(row_height, rows.len(), |mut row| {
                    let i = row.index();
                    let tid = rows[i];
                    let Some(t) = v.lib.get(tid) else { return };
                    row.set_selected(show_selection && sel.contains(tid));
                    for &(c, _) in &shown {
                        row.col(|ui| {
                            let mut cell = Cell {
                                t,
                                v: *v,
                                media: &mut *media,
                                score: scores.and_then(|s| s.get(&tid)),
                                row: i,
                                two_lines,
                                sel,
                                rows,
                                out: &mut *out,
                            };
                            (def(c).cell)(ui, &mut cell);
                        });
                    }
                    let resp = row.response();
                    row_rects.push((i, resp.rect));
                    if resp.clicked() {
                        let m = resp.ctx.input(|i| i.modifiers);
                        acts.push(RowAct::Click(
                            tid,
                            Mods {
                                shift: m.shift,
                                command: m.command,
                            },
                        ));
                    }
                    if resp.double_clicked() {
                        out.push(LibraryCmd::Load(tid).into());
                    }
                    if resp.secondary_clicked() {
                        acts.push(RowAct::RightClick(tid));
                    }
                    if resp.drag_started() {
                        acts.push(RowAct::Drag(tid));
                    }
                    if resp.dragged() {
                        resp.ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
                    }
                    resp.context_menu(|ui| {
                        let ids = if sel.contains(tid) {
                            sel.in_order(rows)
                        } else {
                            vec![tid]
                        };
                        track_menu(ui, &ids, v, menu, out);
                    });
                });
            });
        let area = body.inner_rect;

        // The header's changes.
        if let Some(s) = menu_state.filter(|s| *s != state_now) {
            *state = s;
            res.changed = true;
        }
        if let Some(c) = header_click {
            state.click_sort(c);
            res.changed = true;
        }
        // Widths as dragged (not as a new table first lays itself out).
        let dragging = ui.input(|i| i.pointer.primary_down());
        for (c, w) in widths.into_iter().filter(|_| dragging) {
            // Dragged: its width moved from what it was given.
            let given = shown.iter().find(|e| e.0 == c).map_or(w, |e| e.1);
            if (given - w).abs() <= 0.5 {
                continue;
            }
            if let Some(e) = state.columns.iter_mut().find(|e| e.0 == c)
                && !def(c).fixed
            {
                e.1 = w;
                state.sized.insert(c);
            }
        }

        // The rows'.
        for a in acts {
            match a {
                RowAct::Click(tid, m) => {
                    sel.click(tid, rows, m);
                    res.clicked = true;
                }
                RowAct::RightClick(tid) => {
                    sel.right_click(tid);
                    res.clicked = true;
                }
                RowAct::Drag(tid) => {
                    if !sel.contains(tid) {
                        sel.select_only(tid);
                    }
                    let ids = sel.in_order(rows);
                    egui::DragAndDrop::set_payload(ui.ctx(), DragPayload::Tracks { ids, origin });
                }
            }
        }

        // Dropping on the table: reorder our own rows, or take others'.
        if let Some(p) = drag::payload(ui.ctx())
            && let DragPayload::Tracks { ids, origin: from } = p.as_ref()
            && let Some(at) = ui.ctx().pointer_hover_pos().filter(|p| area.contains(*p))
        {
            let ours = *from == origin;
            if (ours && reorder) || (!ours && takes_drops) {
                let place = drop_place(&row_rects, at.y);
                let line_y = match place {
                    Some(p) => row_rects
                        .iter()
                        .find(|(i, _)| *i == p)
                        .map(|(_, r)| r.top())
                        .or_else(|| row_rects.last().map(|(_, r)| r.bottom())),
                    None => row_rects.last().map(|(_, r)| r.bottom()),
                };
                let painter = ui.painter_at(area);
                if ours || place.is_some() {
                    if let Some(y) = line_y {
                        painter.line_segment(
                            [pos2(area.left(), y), pos2(area.right(), y)],
                            Stroke::new(2.0_f32, THEME.accent),
                        );
                    }
                } else {
                    painter.rect_stroke(
                        area,
                        3,
                        Stroke::new(1.0_f32, THEME.accent),
                        egui::StrokeKind::Inside,
                    );
                }
                if ui.input(|i| i.pointer.any_released()) {
                    egui::DragAndDrop::clear_payload(ui.ctx());
                    if ours {
                        let picked: Vec<usize> = (0..rows.len())
                            .filter(|&i| ids.contains(&rows[i]))
                            .collect();
                        let to = place.unwrap_or(rows.len());
                        res.moved = Some((picked, to));
                    } else {
                        res.dropped = Some((ids.clone(), place));
                    }
                }
            }
        }
        res
    }
}

/// The place a drop at height `y` goes before: the row whose top half it's over, the
/// one after for the bottom half. `None` below the last row drawn.
fn drop_place(rows: &[(usize, egui::Rect)], y: f32) -> Option<usize> {
    for (i, r) in rows {
        if y < r.center().y {
            return Some(*i);
        }
        if y < r.bottom() {
            return Some(i + 1);
        }
    }
    None
}

/// The header's text: the name, a sort arrow, in the accent colour when filtered.
fn header_label(ui: &mut Ui, c: ColumnId, s: &TableState) {
    let d = def(c);
    let arrow = match s.sort {
        Some((sc, Dir::Asc)) if sc == c => " ^",
        Some((sc, Dir::Desc)) if sc == c => " v",
        _ => "",
    };
    let colour = if s.filter_of(c).is_some() {
        THEME.accent
    } else {
        THEME.muted
    };
    let text = RichText::new(format!("{}{arrow}", d.title))
        .size(11.0)
        .color(colour);
    let tip = match d.field {
        Some(_) => format!(
            "{}: click to sort, right-click to filter or pick columns",
            d.name
        ),
        None => format!("{}: right-click to pick columns", d.name),
    };
    ui.add(egui::Label::new(text).selectable(false).truncate())
        .on_hover_text(tip);
}

/// Right-click on a header: sort, filter by this column, pick the columns.
fn header_menu(ui: &mut Ui, c: ColumnId, s: &mut TableState, available: &[ColumnId], v: &CoreView) {
    let d = def(c);
    if let Some(field) = d.field {
        ui.horizontal(|ui| {
            for (label, dir) in [("Sort up", Dir::Asc), ("Sort down", Dir::Desc)] {
                if ui
                    .selectable_label(s.sort == Some((c, dir)), label)
                    .clicked()
                {
                    s.sort = Some((c, dir));
                }
            }
            if ui
                .selectable_label(s.sort.is_none(), "List order")
                .clicked()
            {
                s.sort = None;
            }
        });
        ui.separator();
        ui.label(
            RichText::new(format!("Show only, by {}", d.name.to_lowercase())).color(THEME.muted),
        );
        match s.filter_mut(c) {
            Filter::Text(text) => {
                let hint = match field {
                    Field::Bpm => "120-126  or  >=128",
                    Field::Key => "8A  or  Am",
                    Field::Duration => "<5:00",
                    Field::Energy => "7-10",
                    Field::Rating => ">=4",
                    _ => "words",
                };
                let r = ui.add(
                    egui::TextEdit::singleline(text)
                        .hint_text(hint)
                        .desired_width(180.0),
                );
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    ui.close();
                }
            }
            Filter::Status(set) => {
                for st in Status::ALL {
                    let mut on = set.contains(&st);
                    if ui.checkbox(&mut on, st.label()).changed() {
                        if on {
                            set.insert(st);
                        } else {
                            set.remove(&st);
                        }
                    }
                }
            }
            Filter::Colour(set) => {
                let mut colours: BTreeSet<Option<u32>> =
                    v.lib.tracks().iter().map(|t| t.colour).collect();
                colours.insert(None);
                for col in colours {
                    let mut on = set.contains(&col);
                    ui.horizontal(|ui| {
                        swatch(ui, col);
                        if ui.checkbox(&mut on, colour_name(col)).changed() {
                            if on {
                                set.insert(col);
                            } else {
                                set.remove(&col);
                            }
                        }
                    });
                }
            }
        }
        if s.filter_of(c).is_some() && ui.button("Clear this filter").clicked() {
            s.filters.retain(|(id, _)| *id != c);
            ui.close();
        }
        ui.separator();
    } else if c == ColumnId::Index && ui.button("List order").clicked() {
        s.sort = None;
        ui.close();
    }
    ui.menu_button("Columns", |ui| {
        for &a in available {
            let mut on = s.shows(a);
            if ui.checkbox(&mut on, def(a).name).changed() {
                s.toggle_column(a);
            }
        }
    });
    if !s.sized.is_empty() && ui.button("Fit columns to contents").clicked() {
        s.sized.clear();
        s.fitted += 1;
        ui.close();
    }
    if s.has_filters() && ui.button("Clear all filters").clicked() {
        s.filters.clear();
        ui.close();
    }
    // An empty filter is made each frame the menu shows one; don't keep it.
    s.filters.retain(|(_, f)| !f.is_empty());
}

/// The filters in use, as chips over the table; clicking one clears it. True if
/// one was cleared.
fn filter_chips(ui: &mut Ui, s: &mut TableState) -> bool {
    if !s.has_filters() {
        return false;
    }
    let error = s.filter().err();
    let mut clear: Option<ColumnId> = None;
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Only:").size(11.0).color(THEME.muted));
        for (c, f) in &s.filters {
            if f.is_empty() {
                continue;
            }
            let text = format!("{} {}  x", def(*c).name, f.summary());
            if THEME
                .pill(ui, &text, THEME.accent)
                .interact(Sense::click())
                .on_hover_text("Click to clear (right-click the column's header to change it)")
                .clicked()
            {
                clear = Some(*c);
            }
        }
        if let Some(e) = &error {
            ui.label(RichText::new(e).size(11.0).color(THEME.bad));
        }
    });
    if let Some(c) = clear {
        s.filters.retain(|(id, _)| *id != c);
        return true;
    }
    false
}

// ---- cells ---------------------------------------------------------------------------

/// Widen the columns `grows` picks, each by its share of their width, so all of
/// them add up to `room`. Never narrows anything.
fn fill(
    natural: &[(ColumnId, f32)],
    room: f32,
    grows: impl Fn(ColumnId) -> bool,
) -> Vec<(ColumnId, f32)> {
    let used: f32 = natural.iter().map(|c| c.1).sum();
    let growing: f32 = natural.iter().filter(|c| grows(c.0)).map(|c| c.1).sum();
    let spare = (room - used).max(0.0);
    natural
        .iter()
        .map(|&(c, w)| match growing > 0.0 && grows(c) {
            true => (c, (w + spare * w / growing).floor()),
            false => (c, w),
        })
        .collect()
}

/// The widest a column grows to fit its contents.
const AUTO_MAX: f32 = 420.0;

/// The columns that fit their contents.
const AUTO: [ColumnId; 6] = [
    ColumnId::Title,
    ColumnId::Artist,
    ColumnId::Album,
    ColumnId::Genre,
    ColumnId::Comment,
    ColumnId::File,
];

/// The text an [`AUTO`] column shows.
fn cell_text(c: ColumnId, t: &Track) -> &str {
    match c {
        ColumnId::Title => &t.title,
        ColumnId::Artist => &t.artist,
        ColumnId::Album => &t.album,
        ColumnId::Genre => &t.genre,
        ColumnId::Comment => &t.comment,
        ColumnId::File => t.path.rsplit('/').next().unwrap_or(&t.path),
        _ => "",
    }
}

/// Each [`AUTO`] column's width to fit the library's longest value in it (and its
/// header), up to [`AUTO_MAX`]. Worked out again when the library changes.
pub fn auto_widths(
    ctx: &egui::Context,
    lib: &crate::core::index::LibraryIndex,
) -> HashMap<ColumnId, f32> {
    let id = egui::Id::new("table-auto-widths");
    if let Some((rev, w)) = ctx.data(|d| d.get_temp::<(u64, HashMap<ColumnId, f32>)>(id))
        && rev == lib.rev()
    {
        return w;
    }
    let font = egui::TextStyle::Body.resolve(&ctx.style());
    let width = |s: &str| {
        ctx.fonts_mut(|f| {
            f.layout_no_wrap(s.to_string(), font.clone(), Color32::WHITE)
                .size()
                .x
        })
    };
    let mut out = HashMap::new();
    for c in AUTO {
        // The longest few by characters: in a proportional font the widest is one of
        // those, near enough.
        let mut texts: Vec<&str> = lib.tracks().iter().map(|t| cell_text(c, t)).collect();
        texts.sort_unstable_by_key(|t| std::cmp::Reverse(t.chars().count()));
        texts.dedup();
        let widest = texts.iter().take(20).map(|t| width(t)).fold(0.0, f32::max);
        // Room for the sort arrow beside the header, and the cell's margins.
        let header = width(def(c).title) + 24.0;
        out.insert(c, (widest + 12.0).max(header).max(40.0).min(AUTO_MAX));
    }
    ctx.data_mut(|d| d.insert_temp(id, (lib.rev(), out.clone())));
    out
}

fn muted(ui: &mut Ui, s: &str) {
    ui.add(
        egui::Label::new(RichText::new(s).color(THEME.muted))
            .truncate()
            .selectable(false),
    );
}

fn index_cell(ui: &mut Ui, c: &mut Cell) {
    muted(ui, &(c.row + 1).to_string());
}

fn wave_cell(ui: &mut Ui, c: &mut Cell) {
    if let Some(tex) = c.media.thumb(ui.ctx(), c.t.id) {
        ui.add(egui::Image::new((tex, vec2(96.0, 18.0))));
    }
}

fn bpm_cell(ui: &mut Ui, c: &mut Cell) {
    let t = c.t;
    let bpm = t
        .bpm
        .map(|b| format!("{b:.2}"))
        .unwrap_or_else(|| "-".into());
    let text = RichText::new(bpm).color(THEME.muted);
    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
        ui.add(
            egui::Label::new(if t.analysed() { text } else { text.italics() }).selectable(false),
        )
        .on_hover_text(if t.analysed() {
            "Analysed"
        } else {
            "From the file's tag (not analysed)"
        });
    });
}

fn change_cell(ui: &mut Ui, c: &mut Cell) {
    let Some(s) = c.score else { return };
    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
        let ch = s
            .tempo_change
            .map(|c| match c * 100.0 {
                // No "-0.0%" for a change too small to show.
                p if p.abs() < 0.05 => "0.0%".to_string(),
                p => format!("{p:+.1}%"),
            })
            .unwrap_or_default();
        ui.add(egui::Label::new(RichText::new(ch).size(11.0).color(THEME.muted)).selectable(false))
            .on_hover_text("Tempo change needed (allowing half or double time)");
    });
}

fn colour_cell(ui: &mut Ui, c: &mut Cell) {
    if c.t.colour.is_some() {
        swatch(ui, c.t.colour);
    }
}

/// A small square of the track's colour (an outline for none).
fn swatch(ui: &mut Ui, c: Option<u32>) {
    let (rect, resp) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
    match c {
        Some(c) => ui.painter().rect_filled(rect, 2, rgb(c)),
        None => ui.painter().rect_stroke(
            rect.shrink(0.5),
            2,
            Stroke::new(1.0_f32, THEME.line),
            egui::StrokeKind::Inside,
        ),
    };
    resp.on_hover_text(colour_name(c));
}

/// A colour's name in the cue palette, else its hex.
fn colour_name(c: Option<u32>) -> String {
    match c {
        None => "No colour".into(),
        Some(c) => CUE_PALETTE
            .iter()
            .find(|(p, _)| *p == c)
            .map_or_else(|| format!("#{c:06x}"), |(_, n)| n.to_string()),
    }
}

/// The stars; a click rates the track (or the selection, when it's in it), as an
/// undoable edit.
fn rating_cell(ui: &mut Ui, c: &mut Cell) {
    if let Some(n) = stars(ui, c.t.rating.unwrap_or(0)) {
        let tracks = c.targets();
        let value = (n > 0).then_some(edit::Value::Rating(n));
        c.out.push(
            EditCmd::Set {
                tracks,
                values: vec![(edit::Field::Rating, value)],
            }
            .into(),
        );
    }
}

/// Five stars, `n` of them filled.
/// Clicking star k returns k (1-5), or 0 for the one already set (to clear it).
pub fn stars(ui: &mut Ui, n: u8) -> Option<u8> {
    let size = 11.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(size * 5.0 + 4.0, 16.0), Sense::click());
    let p = ui.painter();
    for i in 0..5u8 {
        let c = pos2(rect.left() + size * (i as f32 + 0.5), rect.center().y);
        let pts: Vec<egui::Pos2> = (0..10)
            .map(|k| {
                let r = if k % 2 == 0 { size * 0.48 } else { size * 0.2 };
                let a = std::f32::consts::PI * (k as f32 / 5.0) - std::f32::consts::FRAC_PI_2;
                c + vec2(a.cos(), a.sin()) * r
            })
            .collect();
        if i < n {
            // A star is convex from its centre: a fan of triangles fills it.
            let mut mesh = egui::Mesh::default();
            mesh.colored_vertex(c, THEME.warn);
            for q in &pts {
                mesh.colored_vertex(*q, THEME.warn);
            }
            for k in 0..10u32 {
                mesh.add_triangle(0, 1 + k, 1 + (k + 1) % 10);
            }
            p.add(mesh);
        } else {
            p.add(egui::Shape::closed_line(
                pts,
                Stroke::new(1.0_f32, THEME.line),
            ));
        }
    }
    let resp = resp.on_hover_text(match n {
        0 => "No rating (click a star to rate)".to_string(),
        n => format!("{n} of 5 (click a star to change it)"),
    });
    let at = resp.interact_pointer_pos().filter(|_| resp.clicked())?;
    let k = (((at.x - rect.left()) / size).floor() as i32 + 1).clamp(1, 5) as u8;
    Some(if k == n { 0 } else { k })
}

const BARS: [(&str, Color32); 3] = [
    ("key", THEME.score[0]),
    ("tempo", THEME.score[1]),
    ("energy", THEME.score[2]),
];

/// One small bar per part of the score, with the numbers on hover.
fn bars(ui: &mut Ui, s: &Scored) {
    let (rect, resp) = ui.allocate_exact_size(vec2(50.0, 18.0), Sense::hover());
    // A part that couldn't be scored (no key, say) shows as an empty bar.
    let values = BARS.map(|(name, _)| s.part(name));
    let h = 4.0;
    for (i, ((_, colour), v)) in BARS.iter().zip(values).enumerate() {
        let y = rect.top() + 2.0 + i as f32 * (h + 1.5);
        let track = egui::Rect::from_min_size(pos2(rect.left(), y), vec2(rect.width(), h));
        ui.painter()
            .rect_filled(track, CornerRadius::same(2), THEME.line);
        let mut fill = track;
        fill.set_width(track.width() * v.unwrap_or(0.0).clamp(0.0, 1.0) as f32);
        ui.painter()
            .rect_filled(fill, CornerRadius::same(2), *colour);
    }
    resp.on_hover_text(
        BARS.iter()
            .zip(values)
            .map(|((name, _), v)| match v {
                Some(v) => format!("{name} {:.0}%", v * 100.0),
                None => format!("{name} unknown"),
            })
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spare_room_goes_to_the_growing_columns_by_their_width() {
        use ColumnId::*;
        let natural = [(Title, 200.0), (Artist, 100.0), (Bpm, 56.0)];
        let grows = |c| c != Bpm;
        assert_eq!(
            fill(&natural, 656.0, grows),
            [(Title, 400.0), (Artist, 200.0), (Bpm, 56.0)]
        );
        // Too little room: nothing narrows.
        assert_eq!(fill(&natural, 100.0, grows), natural);
        // Nothing to grow: left as it is.
        assert_eq!(fill(&natural, 900.0, |_| false), natural);
    }

    #[test]
    fn column_filters_become_query_terms() {
        let mut s = TableView::Library.default_state();
        assert_eq!(s.filter(), Ok(Expr::And(vec![])));
        *s.filter_mut(ColumnId::Bpm) = Filter::Text("120-126".into());
        *s.filter_mut(ColumnId::Rating) = Filter::Text(">=4".into());
        *s.filter_mut(ColumnId::Artist) = Filter::Text("daft punk".into());
        if let Filter::Status(set) = s.filter_mut(ColumnId::Status) {
            set.extend([Status::Busy, Status::Attention]);
        }
        let want = query::parse(
            "bpm:120-126 rating>=4 artist:\"daft punk\" (status:busy OR status:attention)",
        )
        .unwrap()
        .expr;
        assert_eq!(s.filter(), Ok(want));
        assert!(s.has_filters());
        *s.filter_mut(ColumnId::Key) = Filter::Text("H".into());
        assert!(s.filter().unwrap_err().starts_with("Key"));
    }

    #[test]
    fn colour_filter_and_sort_clicks() {
        let mut s = TableView::Library.default_state();
        if let Filter::Colour(set) = s.filter_mut(ColumnId::Colour) {
            set.insert(None);
            set.insert(Some(0x10));
        }
        let cx = ();
        let mut l = crate::core::track::lib_track(1, "a", "b");
        let e = s.filter().unwrap();
        assert!(query::eval(&e, &Track::new(l.clone()), &cx), "no colour");
        l.colour = Some(0x10);
        assert!(query::eval(&e, &Track::new(l.clone()), &cx));
        l.colour = Some(0x20);
        assert!(!query::eval(&e, &Track::new(l), &cx));

        s.click_sort(ColumnId::Bpm);
        assert_eq!(s.sort(), Sort(vec![(Field::Bpm, Dir::Asc)]));
        s.click_sort(ColumnId::Bpm);
        assert_eq!(s.sort(), Sort(vec![(Field::Bpm, Dir::Desc)]));
        s.click_sort(ColumnId::Bpm);
        assert_eq!(s.sort(), Sort::default());
        s.click_sort(ColumnId::Status);
        assert_eq!(s.sort(), Sort(vec![(Field::Status, Dir::Asc)]));
        s.click_sort(ColumnId::Index);
        assert_eq!(s.sort, None, "# is the list's own order");
    }

    #[test]
    fn columns_come_and_go() {
        let mut s = TableView::Library.default_state();
        assert!(!s.shows(ColumnId::Rating));
        s.toggle_column(ColumnId::Rating);
        assert_eq!(
            s.columns.last(),
            Some(&(ColumnId::Rating, def(ColumnId::Rating).width))
        );
        s.sort = Some((ColumnId::Rating, Dir::Asc));
        *s.filter_mut(ColumnId::Rating) = Filter::Text("4".into());
        s.toggle_column(ColumnId::Rating);
        assert!(!s.shows(ColumnId::Rating) && s.sort.is_none() && !s.has_filters());
        // Every column a view can show has a def, and the defaults are among them.
        for v in [TableView::Library, TableView::Playlist, TableView::Suggest] {
            for (c, _) in v.default_state().columns {
                assert!(v.available().contains(&c), "{v:?} {c:?}");
            }
        }
    }

    #[test]
    fn drops_land_between_rows() {
        let r = |i: usize| {
            (
                i,
                egui::Rect::from_min_size(pos2(0.0, i as f32 * 20.0), vec2(100.0, 20.0)),
            )
        };
        let rows = [r(3), r(4), r(5)];
        assert_eq!(drop_place(&rows, 61.0), Some(3), "top half of row 3");
        assert_eq!(drop_place(&rows, 75.0), Some(4), "bottom half");
        assert_eq!(drop_place(&rows, 119.0), Some(6));
        assert_eq!(drop_place(&rows, 130.0), None, "below them all");
    }
}
