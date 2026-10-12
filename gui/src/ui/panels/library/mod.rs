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
//! Shared pieces for other panels: [`table::TrackTable`] (any list of tracks),
//! [`drag::DragPayload`] (dragging tracks and collections, with a ghost),
//! [`tracks::track_menu`] (the one right-click menu).
//!
//! To add a source: a variant of `core::view::Source`, an item in [`SOURCES`], and an
//! arm in [`LibraryPane::source`]. A side pane: a variant of `core::view::Aside`, an
//! item in [`ASIDES`] and an arm in [`LibraryPane::aside`].

pub mod browser;
pub mod collections;
pub mod drag;
pub mod suggest;
pub mod table;
pub mod tracks;

use egui::{RichText, Ui};

use self::browser::{LibraryUi, Scope};
use self::collections::{CollectionsUi, TreeCtx};
use self::suggest::SuggestUi;
use self::table::{TableView, Tables};
use self::tracks::Media;
use super::flow::FlowUi;
use super::{Cx, Panel, resized};
use crate::core::collections::CollectionCmd;
use crate::core::command::{Command, ViewCmd};
use crate::core::track::TrackId;
use crate::core::view::{Aside, Mode, Pane, Region, Source};
use crate::ui::widgets::{Opens, StackItem, icon_stack, icon_stack::WIDTH};

/// A placeholder (greyed out) in the left stack.
const fn later(id: Source, icon: &'static str, tip: &'static str) -> StackItem<Source> {
    StackItem {
        id,
        icon,
        tip,
        enabled: false,
    }
}

/// The left icon stack.
const SOURCES: [StackItem<Source>; 9] = [
    StackItem::new(Source::AllTracks, "🎵", "All tracks"),
    StackItem::new(Source::Playlists, "☰", "Playlists and smart playlists"),
    StackItem::new(Source::Crates, "📦", "Crates"),
    StackItem::new(Source::Flows, "🔀", "Flows: sets planned on a canvas"),
    StackItem::new(Source::Explorer, "📁", "Folders on disk"),
    StackItem::new(
        Source::External,
        "🖴",
        "Other libraries: Rekordbox, Mixxx, drives (coming)",
    ),
    later(Source::SoundCloud, "☁", "SoundCloud (coming)"),
    later(Source::Beatport, "🛒", "Beatport (coming)"),
    later(Source::Spotify, "🎧", "Spotify (coming)"),
];

/// The right icon stack.
const ASIDES: [StackItem<Aside>; 1] = [StackItem::new(
    Aside::Suggestions,
    "💡",
    "Suggestions: what could play next",
)];

pub struct LibraryPane {
    /// The track list and the Explorer tree.
    pub browser: LibraryUi,
    pub collections: CollectionsUi,
    pub suggest: SuggestUi,
    /// Waveform thumbnails and cover art for every list.
    pub media: Media,
    /// Every table's columns and sort, by view (saved in `UiPrefs`).
    pub tables: Tables,
    /// The list with focus.
    pub focused_pane: Pane,
}

impl LibraryPane {
    pub fn new(tables: Tables) -> LibraryPane {
        LibraryPane {
            browser: LibraryUi::default(),
            collections: CollectionsUi::default(),
            suggest: SuggestUi::default(),
            media: Media::default(),
            tables,
            focused_pane: Pane::Library,
        }
    }

    /// The focused list's focused track: what "load selected" loads and the
    /// suggestions' *Selected* is. `flow`: the Flow canvas, for its selected card.
    pub fn focused_track(&self, flow: &FlowUi) -> Option<TrackId> {
        match self.focused_pane {
            Pane::Library => self.browser.focused(),
            Pane::Suggest => self.suggest.focused(),
            Pane::Flow => flow.selected_track().or(self.browser.focused()),
        }
    }

    /// Move the focused list's selection (the keyboard, the browse knob).
    pub fn browse(&mut self, delta: i32) {
        match self.focused_pane {
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
            let tree = TreeCtx {
                collections: cx.core.collections,
                open,
                search: &search,
            };
            let mut cmds: Vec<CollectionCmd> = vec![];
            self.collections.show(ui, section, &tree, &mut cmds);
            cx.out.extend(cmds.into_iter().map(Command::Collection));
            return;
        }
        match source {
            Source::AllTracks => {
                let table = self.tables.get_mut(TableView::Library);
                self.browser.all_tracks(ui, &cx.core, table, cx.out);
            }
            Source::Explorer => self.browser.explorer(ui, &cx.core, cx.out),
            Source::External => placeholder(
                ui,
                cx,
                "Other libraries",
                "Rekordbox and Mixxx libraries, and drives, will show here once \
                 earshot can read them (through cratelib).",
            ),
            Source::SoundCloud | Source::Beatport | Source::Spotify => {
                placeholder(ui, cx, "Streaming", "Coming later.")
            }
            Source::Playlists | Source::Crates | Source::Flows => {
                unreachable!("sections are drawn above")
            }
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

    /// The middle: the track list.
    fn middle(&mut self, ui: &mut Ui, cx: &mut Cx) {
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
        let r = egui::SidePanel::left("tool-library")
            .resizable(true)
            .default_width(saved)
            .width_range(320.0..=1400.0)
            .frame(egui::Frame::new().fill(cx.theme.bg))
            .show_inside(ui, |ui| {
                // The panel keeps the width its contents take: all of it.
                ui.expand_to_include_rect(ui.max_rect());
                self.show(ui, cx, false)
            });
        let w = r.response.rect.width();
        resized(ui.ctx(), w, Region::FlowLibrary, saved, cx);
    }

    /// Draw it, with or without the right icon stack and its panes (`asides`; the
    /// Flow editor has its own).
    pub fn show(&mut self, ui: &mut Ui, cx: &mut Cx, asides: bool) {
        let layout = cx.prefs.layout().clone();
        // Panel ids per mode, so each mode keeps its own sizes.
        let id = |name: &str| match cx.prefs.mode {
            Mode::Perform => format!("{name}-perform"),
            Mode::Prepare => format!("{name}-prepare"),
        };
        let stack_frame = egui::Frame::new()
            .fill(cx.theme.bg)
            .inner_margin(egui::Margin::symmetric(3, 6));
        egui::SidePanel::left(id("source-stack"))
            .resizable(false)
            .exact_width(WIDTH + 6.0)
            .frame(stack_frame)
            .show_inside(ui, |ui| {
                if let Some(s) = icon_stack(ui, &SOURCES, layout.source, Opens::Right) {
                    cx.send(ViewCmd::Source(s));
                }
            });
        if let Some(source) = layout.source {
            let r = egui::SidePanel::left(id("source"))
                .resizable(true)
                .default_width(layout.sizes.source)
                .width_range(140.0..=600.0)
                .show_inside(ui, |ui| self.source(ui, source, cx));
            resized(
                ui.ctx(),
                r.response.rect.width(),
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
            let r = egui::SidePanel::right(id("aside"))
                .resizable(true)
                .default_width(layout.sizes.aside)
                .width_range(240.0..=700.0)
                .show_animated_inside(ui, asides, |ui| self.aside(ui, aside, cx));
            if let Some(r) = r {
                resized(
                    ui.ctx(),
                    r.response.rect.width(),
                    Region::Aside,
                    layout.sizes.aside,
                    cx,
                );
            }
        }
        egui::CentralPanel::default().show_inside(ui, |ui| self.middle(ui, cx));
    }
}

/// A source that isn't built yet.
fn placeholder(ui: &mut Ui, cx: &Cx, title: &str, text: &str) {
    ui.label(
        RichText::new(title.to_uppercase())
            .size(10.0)
            .color(cx.theme.muted),
    );
    ui.add_space(4.0);
    ui.label(RichText::new(text).color(cx.theme.muted));
}

impl Panel for LibraryPane {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        self.show(ui, cx, true);
    }
}
