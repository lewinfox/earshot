//! The library: one component, the same in both modes (and beside the Flow canvas).
//! An icon stack on the left picks the source (what we're operating on: all tracks,
//! playlists, crates, Flows, folders on disk, other libraries), shown in the pane
//! beside it; the track table fills the middle; an icon stack on the right opens
//! side panes such as the suggestions.
//!
//! One list has focus at a time ([`LibraryPane::focused_pane`]), each with its own
//! selection: the keyboard and the controller's browse knob move through it, "load
//! selected" loads from it, and the suggestions' *Selected* follows it.
//!
//! The browse knob reaches every pane, left to right ([`Level`]): the sources (the
//! icon stack), the source's tree (its folders, playlists, crates and Flows), the
//! track list, then the suggestions. Turning it moves the highlight within one;
//! Next (pressing it) goes into what's highlighted (a source's tree, a folder's
//! contents, a playlist's tracks) and Prev comes back out (closing the folder).
//! Whatever is highlighted shows straight away. A Flow, Manage and the other
//! libraries' panes are as far as it goes: there only Prev does anything.
//!
//! Shared pieces for other panels: [`table::TrackTable`] (any list of tracks),
//! [`drag::DragPayload`] (dragging tracks and collections, with a ghost),
//! [`tracks::track_menu`] (the one right-click menu).
//!
//! The other sources (DJ programs' libraries, USB sticks, Manage) come after
//! [`VIEWS`], from `external::panes`. To add a view of earshot's library: a variant
//! of `core::view::Source`, an item in [`VIEWS`], and an arm in
//! [`LibraryPane::source`]. A side pane: a variant of `core::view::Aside`, an
//! item in [`ASIDES`] and an arm in [`LibraryPane::aside`].

pub mod browser;
pub mod collections;
pub mod drag;
pub mod external;
pub mod suggest;
pub mod table;
pub mod tracks;

use egui::{RichText, Ui};

use self::browser::{AllRow, LibraryUi, Scope};
use self::collections::{CollectionsUi, TreeCtx};
use self::external::{ExternalUi, Middle, NodeKey};
use self::suggest::SuggestUi;
use self::table::{TableView, Tables};
use self::tracks::Media;
use super::flow::FlowUi;
use super::{Cx, Panel, resized};
use crate::core::collections::{CollectionCmd, CollectionId, CollectionKind, Collections, Section};
use crate::core::command::{Command, ViewCmd};
use crate::core::external::NodePath;
use crate::core::outside::is_outside;
use crate::core::sources::STREAMING;
use crate::core::sources::SourceId;
use crate::core::track::TrackId;
use crate::core::view::{Aside, Mode, Pane, Region, Source};
use crate::ui::theme::{fs, sized_panel, text_scale};
use crate::ui::widgets::{Opens, StackItem, icon_stack, icon_stack::WIDTH, icon_stack_with_menu};

/// The left icon stack's views of earshot's own library (the other sources follow).
const VIEWS: [StackItem<Source>; 4] = [
    StackItem::new(Source::AllTracks, "🎵", "All tracks"),
    StackItem::new(Source::Playlists, "☰", "Playlists and smart playlists"),
    StackItem::new(Source::Crates, "📦", "Crates"),
    StackItem::new(Source::Flows, "🔀", "Flows: sets planned on a canvas"),
];

/// The right icon stack.
const ASIDES: [StackItem<Aside>; 1] = [StackItem::new(
    Aside::Suggestions,
    "💡",
    "Suggestions: what could play next",
)];

/// The list a track was loaded from, so "Show in library" can go back to it: the
/// same file can be in many lists.
#[derive(Clone, Debug, PartialEq)]
pub enum LoadedFrom {
    /// The track list: the left pane that was open, and what the list showed.
    List {
        source: Option<Source>,
        scope: Scope,
    },
    /// Another library's list (a DJ program's, or one on a USB stick): the left
    /// pane, the library and the playlist or folder in it (`None`: all its tracks).
    Elsewhere {
        pane: Source,
        library: SourceId,
        node: Option<NodePath>,
    },
}

/// Where the browse knob is (see the module's notes).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Level {
    /// The left icon stack.
    Sources,
    /// The source's tree, on [`LibraryPane::tree_cursor`].
    Tree,
    /// A list: [`LibraryPane::focused_pane`].
    #[default]
    List,
    /// Inside something it can't step through (a Flow, Manage, another library's
    /// pane): only Prev.
    Inside,
}

/// A row of a source's tree.
#[derive(Clone, Debug, PartialEq)]
pub enum TreeKey {
    /// A playlist, crate, Flow or folder of them.
    Collection(CollectionId),
    /// One of All tracks' lists.
    All(AllRow),
    /// Another library's All tracks, playlist or folder.
    Node(NodeKey),
}

/// What Next does on a tree row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Into {
    /// Opens it, onto its first row.
    Folder,
    /// Into its track list.
    List,
    /// Into it, with only Prev to come back (a Flow).
    Inside,
}

#[derive(Clone, Debug)]
struct TreeRow {
    key: TreeKey,
    parent: Option<TreeKey>,
    into: Into,
}

/// The browse knob's highlight round a row: an outline, brought into view when it
/// has just moved.
pub(super) fn outline(ui: &Ui, r: &egui::Response, on: bool, scroll: bool) {
    if on {
        ui.painter().rect_stroke(
            r.rect.expand(1.0),
            3.0,
            crate::ui::theme::THEME.cursor_stroke(),
            egui::StrokeKind::Outside,
        );
        if scroll {
            r.scroll_to_me(None);
        }
    }
}

/// A move of the browse knob, done when the library is next drawn (it has the
/// sources and trees then).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nav {
    Turn(i32),
    Next,
    Prev,
}

pub struct LibraryPane {
    /// The track list and the Explorer tree.
    pub browser: LibraryUi,
    pub collections: CollectionsUi,
    /// Other DJ programs' libraries.
    pub external: ExternalUi,
    pub suggest: SuggestUi,
    /// Waveform thumbnails and cover art for every list.
    pub media: Media,
    /// Every table's columns and sort, by view (saved in `UiPrefs`).
    pub tables: Tables,
    /// The list with focus.
    pub focused_pane: Pane,
    /// Where the browse knob is.
    pub level: Level,
    /// The tree row highlighted at [`Level::Tree`].
    pub tree_cursor: Option<TreeKey>,
    /// Moves waiting for the next draw.
    pub nav: Vec<Nav>,
    /// The knob just moved: bring its highlight into view.
    scroll: bool,
    /// The source open when last drawn.
    source: Option<Source>,
}

impl LibraryPane {
    pub fn new(tables: Tables) -> LibraryPane {
        LibraryPane {
            browser: LibraryUi::default(),
            collections: CollectionsUi::default(),
            external: ExternalUi::default(),
            suggest: SuggestUi::default(),
            media: Media::default(),
            tables,
            focused_pane: Pane::Library,
            level: Level::List,
            tree_cursor: None,
            nav: vec![],
            scroll: false,
            source: None,
        }
    }

    /// The middle lists another library's tracks (not earshot's list).
    fn external_list(&self) -> bool {
        self.source.and_then(|s| self.external.middle(s)) == Some(Middle::Tracks)
    }

    /// Into a list, onto its first track.
    fn into_list(&mut self, pane: Pane) {
        self.focus(pane);
        match pane {
            Pane::Suggest => self.suggest.focus_top(),
            _ if self.external_list() => self.external.focus_top(),
            _ => self.browser.focus_top(),
        }
    }

    /// A list was picked (clicked, or reached by the knob): it has the knob.
    pub fn focus(&mut self, pane: Pane) {
        self.focused_pane = pane;
        self.level = Level::List;
    }

    /// The open source's tree for the knob, top to bottom: each row, its folder, and
    /// what Next does on it. Empty when there's no tree (Manage, a stick, a library
    /// not read yet).
    fn tree_rows(&self, source: Source, collections: &Collections) -> Vec<TreeRow> {
        if let Some(section) = source.section() {
            return self
                .collections
                .visible(collections, section)
                .into_iter()
                .filter_map(|(id, parent)| {
                    let into = match collections.get(id)?.kind {
                        CollectionKind::Folder => Into::Folder,
                        CollectionKind::Flow => Into::Inside,
                        _ => Into::List,
                    };
                    Some(TreeRow {
                        key: TreeKey::Collection(id),
                        parent: parent.map(TreeKey::Collection),
                        into,
                    })
                })
                .collect();
        }
        match source {
            Source::AllTracks => self
                .browser
                .nav_rows()
                .into_iter()
                .map(|(row, parent)| TreeRow {
                    into: if row == AllRow::History {
                        Into::Folder
                    } else {
                        Into::List
                    },
                    key: TreeKey::All(row),
                    parent: parent.map(TreeKey::All),
                })
                .collect(),
            Source::Program(_) | Source::Library(_) => self
                .external
                .pane_library(source)
                .map_or(vec![], |id| self.external.nav_rows(id))
                .into_iter()
                .map(|(key, parent, folder)| TreeRow {
                    key: TreeKey::Node(key),
                    parent: parent.map(TreeKey::Node),
                    into: if folder { Into::Folder } else { Into::List },
                })
                .collect(),
            _ => vec![],
        }
    }

    /// Show what a tree row lists, as clicking it does.
    fn show_row(&mut self, key: &TreeKey, cx: &mut Cx) {
        match key {
            TreeKey::Collection(id) => {
                let flow = cx
                    .core
                    .collections
                    .get(*id)
                    .is_some_and(|c| c.kind == CollectionKind::Flow);
                cx.send(Command::Collection(if flow && cx.prefs.flow_tab() {
                    CollectionCmd::Open(*id)
                } else {
                    CollectionCmd::Show(*id)
                }));
            }
            TreeKey::All(row) => {
                let table = self.tables.get_mut(TableView::Library);
                self.browser.show_row(row, table, cx.out);
                self.browser.nav_cursor = Some(row.clone());
            }
            TreeKey::Node(node) => self.external.open_node(node, cx.out),
        }
    }

    /// Open or close a folder row.
    fn set_folder_open(&mut self, key: &TreeKey, open: bool) {
        match key {
            TreeKey::Collection(id) => self.collections.set_open(*id, open),
            TreeKey::Node((id, Some(path))) => self.external.set_node_open(*id, path.clone(), open),
            // The History's sessions show while it's listed.
            TreeKey::All(_) | TreeKey::Node(_) => {}
        }
    }

    /// Put the knob on a tree row and show it.
    fn tree_to(&mut self, key: TreeKey, cx: &mut Cx) {
        self.level = Level::Tree;
        self.show_row(&key, cx);
        self.tree_cursor = Some(key);
    }

    /// Do the knob's moves. `items`: the left icon stack.
    fn navigate(&mut self, items: &[StackItem<Source>], cx: &mut Cx) {
        let navs = std::mem::take(&mut self.nav);
        self.scroll = !navs.is_empty();
        for nav in navs {
            let source = cx.prefs.layout().source;
            let rows = source.map_or(vec![], |s| self.tree_rows(s, cx.core.collections));
            let at = self
                .tree_cursor
                .as_ref()
                .and_then(|c| rows.iter().position(|r| &r.key == c));
            match (self.level, nav) {
                (Level::List, Nav::Turn(n)) => self.browse(n),
                (Level::Sources, Nav::Turn(n)) => {
                    let ids: Vec<Source> =
                        items.iter().filter(|i| i.enabled).map(|i| i.id).collect();
                    let at = source.and_then(|s| ids.iter().position(|i| *i == s));
                    let to = match at {
                        Some(i) => (i as i32 + n).clamp(0, ids.len() as i32 - 1) as usize,
                        None => 0,
                    };
                    if let Some(s) = ids.get(to) {
                        cx.send(ViewCmd::Source(Some(*s)));
                        self.tree_cursor = None;
                    }
                }
                (Level::Sources, Nav::Next) => {
                    if let Some(first) = rows.first() {
                        self.tree_to(first.key.clone(), cx);
                    } else if !matches!(source, None | Some(Source::Program(_) | Source::Library(_))) {
                        // Manage, a stick: nothing to step through. (A library not read
                        // yet: nothing until it has its tree.)
                        self.level = Level::Inside;
                    }
                }
                (Level::Sources, Nav::Prev) => {}
                (Level::Tree, Nav::Turn(n)) => {
                    let to = match at {
                        Some(i) => (i as i32 + n).clamp(0, rows.len() as i32 - 1) as usize,
                        None => 0,
                    };
                    if let Some(r) = rows.get(to) {
                        self.tree_to(r.key.clone(), cx);
                    }
                }
                (Level::Tree, Nav::Next) => {
                    let Some(i) = at else { continue };
                    let row = rows[i].clone();
                    match row.into {
                        Into::Folder => {
                            self.set_folder_open(&row.key, true);
                            self.show_row(&row.key, cx);
                            // Its first row, now it's open.
                            let rows = self.tree_rows(source.unwrap(), cx.core.collections);
                            let first = rows
                                .iter()
                                .find(|r| r.parent.as_ref() == Some(&row.key))
                                .map(|r| r.key.clone());
                            if let Some(k) = first {
                                self.tree_to(k, cx);
                            }
                        }
                        Into::Inside => self.level = Level::Inside,
                        Into::List => self.into_list(Pane::Library),
                    }
                }
                (Level::Tree, Nav::Prev) => match at.and_then(|i| rows[i].parent.clone()) {
                    Some(folder) => {
                        self.set_folder_open(&folder, false);
                        self.tree_to(folder, cx);
                    }
                    None => self.level = Level::Sources,
                },
                (Level::List, Nav::Next) => {
                    let suggestions = cx.prefs.flow_tab()
                        || cx.prefs.layout().aside == Some(Aside::Suggestions);
                    if self.focused_pane != Pane::Suggest && suggestions {
                        self.into_list(Pane::Suggest);
                    }
                }
                (Level::List, Nav::Prev) => match self.focused_pane {
                    Pane::Suggest => self.focus(Pane::Library),
                    Pane::Library | Pane::Flow => self.back_out(at.is_some()),
                },
                (Level::Inside, Nav::Prev) => self.back_out(at.is_some()),
                (Level::Inside, _) => {}
            }
        }
        // The sub-panes draw the highlight.
        let on = |k: &TreeKey| self.level == Level::Tree && self.tree_cursor.as_ref() == Some(k);
        self.browser.nav_cursor = match &self.tree_cursor {
            Some(k @ TreeKey::All(row)) if on(k) => Some(row.clone()),
            _ => None,
        };
        self.external.nav_cursor = match &self.tree_cursor {
            Some(k @ TreeKey::Node(node)) if on(k) => Some(node.clone()),
            _ => None,
        };
        self.browser.nav_scroll = self.scroll;
        self.external.nav_scroll = self.scroll;
    }

    /// Out of a list or a Flow: to the tree row it came from, or the sources.
    fn back_out(&mut self, tree: bool) {
        self.level = if tree {
            Level::Tree
        } else {
            Level::Sources
        };
    }

    /// The focused list's focused track: what "load selected" loads and the
    /// suggestions' *Selected* is. `flow`: the Flow canvas, for its selected card.
    pub fn focused_track(&self, flow: &FlowUi) -> Option<TrackId> {
        match self.focused_pane {
            Pane::Library if self.external_list() => self.external.focused(),
            Pane::Library => self.browser.focused(),
            Pane::Suggest => self.suggest.focused(),
            Pane::Flow => flow.selected_track().or(self.browser.focused()),
        }
    }

    /// The list with focus, as where a track being loaded now comes from. `source`:
    /// the left pane open; `flow`: the Flow canvas. `None`: the suggestions, or a
    /// Flow not saved yet.
    pub fn here(&self, source: Option<Source>, flow: &FlowUi) -> Option<LoadedFrom> {
        match self.focused_pane {
            Pane::Library => match source.and_then(|s| Some((s, self.external.middle(s)?))) {
                Some((pane, Middle::Tracks)) => {
                    let (library, node) = self.external.open_list()?;
                    Some(LoadedFrom::Elsewhere {
                        pane,
                        library,
                        node,
                    })
                }
                // A stick's own page lists no tracks.
                Some((_, Middle::Stick)) => None,
                None => Some(LoadedFrom::List {
                    source,
                    scope: self.browser.scope().clone(),
                }),
            },
            Pane::Flow => flow.flow.map(|id| LoadedFrom::List {
                source: Some(Source::Flows),
                scope: Scope::Collection(id),
            }),
            Pane::Suggest => None,
        }
    }

    /// Go back to where `track` was loaded from (All tracks if not known) and
    /// select it there. `source`: the left pane, changed to the one to show. Returns
    /// what to tell the user, if it couldn't go back there.
    pub fn show_track(
        &mut self,
        track: TrackId,
        from: Option<LoadedFrom>,
        source: &mut Option<Source>,
        out: &mut Vec<Command>,
    ) -> Option<String> {
        let mut note = None;
        let (pane, scope) = match from {
            Some(LoadedFrom::Elsewhere {
                pane,
                library,
                node,
            }) => {
                if !self.external.pane_gone(pane) && self.external.reveal(library, node, track, out)
                {
                    *source = Some(pane);
                    self.focus(Pane::Library);
                    return None;
                }
                let what = match pane {
                    Source::Stick(_) => "The USB stick it was loaded from isn't plugged in",
                    _ => "The library it was loaded from isn't there any more",
                };
                // Only that library had it: nowhere else to show it.
                if is_outside(track) {
                    return Some(format!("{what}, so it can't be shown."));
                }
                note = Some(format!("{what}: showing it in All tracks."));
                (Some(Source::AllTracks), Scope::All)
            }
            Some(LoadedFrom::List { source, scope }) => (source, scope),
            None => (Some(Source::AllTracks), Scope::All),
        };
        self.browser.set_scope(scope);
        self.browser.reveal(track);
        self.focus(Pane::Library);
        *source = pane;
        note
    }

    /// Move the focused list's selection (the keyboard, the browse knob).
    pub fn browse(&mut self, delta: i32) {
        match self.focused_pane {
            Pane::Library if self.external_list() => self.external.move_focus(delta),
            Pane::Library | Pane::Flow => self.browser.move_focus(delta),
            Pane::Suggest => self.suggest.move_focus(delta),
        }
    }

    /// The left pane: the source picked in the icon stack.
    fn source(&mut self, ui: &mut Ui, source: Source, cx: &mut Cx) {
        if let Some(section) = source.section() {
            // The Flow on the canvas, while the Flow editor is showing.
            let open = if cx.prefs.flow_tab() {
                cx.core.open_flow
            } else {
                match self.browser.scope() {
                    Scope::Collection(id) => Some(*id),
                    _ => None,
                }
            };
            let search = self.browser.search.clone();
            let sends = std::cell::RefCell::new(vec![]);
            let tree = TreeCtx {
                collections: cx.core.collections,
                open,
                cursor: match (&self.tree_cursor, self.level) {
                    (Some(TreeKey::Collection(id)), Level::Tree) => Some(*id),
                    _ => None,
                },
                scroll: self.scroll,
                search: &search,
                sticks: cx.core.drives,
                sends: &sends,
            };
            let mut cmds: Vec<CollectionCmd> = vec![];
            self.collections.show(ui, section, &tree, &mut cmds);
            for (id, drive, kind) in sends.into_inner() {
                let what = crate::ui::panels::library::external::Sending::Earshot(id);
                self.external.send_to_stick(&drive, what, kind);
            }
            cx.out.extend(cmds.into_iter().map(Command::Collection));
            // Another library's playlist dropped here: import it.
            if section != Section::Flows {
                self.external.import_drop(ui, ui.max_rect());
            }
            return;
        }
        match source {
            Source::AllTracks => {
                let table = self.tables.get_mut(TableView::Library);
                if self.browser.all_tracks(ui, &cx.core, table, cx.out)
                    && let Some(row) = self.browser.nav_cursor.clone()
                {
                    // Clicked: the knob is there.
                    self.level = Level::Tree;
                    self.tree_cursor = Some(TreeKey::All(row));
                }
            }
            Source::Manage => self.manage(ui, cx),
            Source::Program(_) | Source::Library(_) | Source::Stick(_) => {
                if self.external.pane_gone(source) {
                    cx.send(ViewCmd::Source(None));
                    return;
                }
                let before = self.external.nav_cursor.clone();
                self.external.pane(ui, source, &cx.core, cx.out);
                // A row clicked: the knob is there.
                if let Some(node) = self.external.nav_cursor.clone().filter(|n| Some(n) != before.as_ref()) {
                    self.level = Level::Tree;
                    self.tree_cursor = Some(TreeKey::Node(node));
                }
            }
            Source::Playlists | Source::Crates | Source::Flows => {
                unreachable!("sections are drawn above")
            }
        }
    }

    /// Managing sources: the music folders (picking one lists its files in the
    /// middle), adding libraries, and streaming services to come.
    fn manage(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let before = self.browser.scope().clone();
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                heading(ui, cx, "Music folders");
                self.browser.folder_tree(ui, &cx.core, cx.out);
                ui.add_space(10.0);
                heading(ui, cx, "Other libraries");
                self.external.manage(ui, cx.out);
                ui.add_space(10.0);
                heading(ui, cx, "Streaming");
                for name in STREAMING {
                    ui.label(RichText::new(format!("{name}  ·  coming")).color(cx.theme.muted));
                }
            });
        // A folder was picked: it's what the middle lists now.
        if *self.browser.scope() != before && matches!(self.browser.scope(), Scope::Folder(_)) {
            self.external.close();
        }
    }

    /// A pane on the right, opened from its icon stack.
    fn aside(&mut self, ui: &mut Ui, aside: Aside, cx: &mut Cx) {
        match aside {
            Aside::Suggestions => {
                let focus = self.focused_pane == Pane::Suggest;
                let table = self.tables.get_mut(TableView::Suggest);
                if self
                    .suggest
                    .show(ui, &cx.core, focus, &mut self.media, table, cx.out)
                {
                    cx.send(ViewCmd::FocusPane(Pane::Suggest));
                }
            }
        }
    }

    /// The middle: the track list (another library's, while one is picked there).
    fn middle(&mut self, ui: &mut Ui, cx: &mut Cx) {
        match cx
            .prefs
            .layout()
            .source
            .and_then(|s| self.external.middle(s))
        {
            Some(Middle::Stick) => return self.external.stick_view(ui, &cx.core, cx.out),
            Some(Middle::Tracks) => {
                let table = self.tables.get_mut(TableView::External);
                if self
                    .external
                    .list(ui, &cx.core, &mut self.media, table, cx.out)
                {
                    cx.send(ViewCmd::FocusPane(Pane::Library));
                }
                return;
            }
            None => {}
        }
        let table = self.tables.get_mut(self.browser.view(&cx.core));
        if self
            .browser
            .list(ui, &cx.core, &mut self.media, table, cx.out)
        {
            cx.send(ViewCmd::FocusPane(Pane::Library));
        }
    }

    /// Draw it on the left of a Prepare tool (Flow, Tempo lock), without its own side
    /// panes. One width for every tool, so the library stays put when switching.
    pub fn show_beside(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let saved = cx.prefs.prepare.sizes.flow_library;
        let r = egui::SidePanel::left(sized_panel("tool-library"))
            .resizable(true)
            .default_width(fs(saved))
            .width_range(fs(320.0)..=fs(1400.0))
            .frame(egui::Frame::new().fill(cx.theme.bg))
            .show_inside(ui, |ui| {
                // The panel keeps the width its contents take: all of it.
                ui.expand_to_include_rect(ui.max_rect());
                self.show(ui, cx, false)
            });
        let w = r.response.rect.width() / text_scale();
        resized(ui.ctx(), w, Region::FlowLibrary, saved, cx);
    }

    /// Draw it, with or without the right icon stack and its panes (`asides`; the
    /// Flow editor has its own).
    pub fn show(&mut self, ui: &mut Ui, cx: &mut Cx, asides: bool) {
        let layout = cx.prefs.layout().clone();
        self.source = layout.source;
        // The import and add-to-stick windows, wherever their drop was.
        self.external.set_music(&self.browser.folders);
        self.external.windows(ui.ctx(), &cx.core, cx.out);
        // Panel ids per mode, so each mode keeps its own sizes.
        let id = |name: &str| match cx.prefs.mode {
            Mode::Perform => format!("{name}-perform"),
            Mode::Prepare => format!("{name}-prepare"),
            Mode::Controller => format!("{name}-controller"),
        };
        let mut items: Vec<StackItem<Source>> = VIEWS.into_iter().collect();
        items.extend(self.external.stack_items(&cx.core));
        self.navigate(&items, cx);
        let stack_frame = egui::Frame::new()
            .fill(cx.theme.bg)
            .inner_margin(egui::Margin::symmetric(3, 6));
        egui::SidePanel::left(id("source-stack"))
            .resizable(false)
            .exact_width(WIDTH + 6.0)
            .frame(stack_frame)
            .show_inside(ui, |ui| {
                let (external, core, out) = (&mut self.external, &cx.core, &mut *cx.out);
                let cursor = self.level == Level::Sources;
                let picked = icon_stack_with_menu(
                    ui,
                    &items,
                    layout.source,
                    Opens::Right,
                    cursor,
                    |ui, s| {
                        if let Source::Stick(id) = s {
                            external.stick_menu(ui, id, core, out);
                        }
                    },
                );
                if let Some(s) = picked {
                    cx.send(ViewCmd::Source(s));
                }
            });
        if let Some(source) = layout.source {
            let r = egui::SidePanel::left(sized_panel(id("source")))
                .resizable(true)
                .default_width(fs(layout.sizes.source))
                .width_range(fs(140.0)..=fs(600.0))
                .show_inside(ui, |ui| self.source(ui, source, cx));
            resized(
                ui.ctx(),
                r.response.rect.width() / text_scale(),
                Region::Source,
                layout.sizes.source,
                cx,
            );
        }
        egui::SidePanel::right(id("aside-stack"))
            .resizable(false)
            .exact_width(WIDTH + 6.0)
            .frame(stack_frame)
            .show_animated_inside(ui, asides, |ui| {
                if let Some(a) = icon_stack(ui, &ASIDES, layout.aside, Opens::Left) {
                    cx.send(ViewCmd::Aside(a));
                }
            });
        if let Some(aside) = layout.aside {
            let r = egui::SidePanel::right(sized_panel(id("aside")))
                .resizable(true)
                .default_width(fs(layout.sizes.aside))
                .width_range(fs(240.0)..=fs(700.0))
                .show_animated_inside(ui, asides, |ui| self.aside(ui, aside, cx));
            if let Some(r) = r {
                resized(
                    ui.ctx(),
                    r.response.rect.width() / text_scale(),
                    Region::Aside,
                    layout.sizes.aside,
                    cx,
                );
            }
        }
        // Room on the right: right against a panel's resize handle, the list's
        // scroll bar and the handle take the hover in turns and its highlight
        // flickers.
        let frame = egui::Frame::central_panel(ui.style()).inner_margin(egui::Margin {
            left: 8,
            right: 8 + HANDLE_GAP,
            top: 8,
            bottom: 8,
        });
        egui::CentralPanel::default()
            .frame(frame)
            .show_inside(ui, |ui| self.middle(ui, cx));
    }
}

/// How far the list's scroll bar keeps from the resize handle on its right: more
/// than the handle's grab radius plus the pointer's search radius.
const HANDLE_GAP: i8 = 4;

/// A section's heading in a pane.
fn heading(ui: &mut Ui, cx: &Cx, title: &str) {
    ui.label(
        RichText::new(title.to_uppercase())
            .size(fs(10.0))
            .color(cx.theme.muted),
    );
    ui.add_space(2.0);
}

impl Panel for LibraryPane {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        self.show(ui, cx, true);
    }
}
