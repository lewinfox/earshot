//! The library: the same in both modes. An icon stack on the left picks the source
//! (what we're operating on), shown in the pane beside it; the track list (or the
//! Flow canvas) fills the middle; an icon stack on the right opens side panes such
//! as the suggestions.
//!
//! To add a source: a variant of `core::view::Source`, an item in [`SOURCES`], and an
//! arm in [`LibraryPane::source`]. A side pane: a variant of `core::view::Aside`, an
//! item in [`ASIDES`] and an arm in [`LibraryPane::aside`].

pub mod browser;
pub mod collections;
pub mod suggest;
pub mod tracks;

use egui::Ui;

use self::browser::{LibraryUi, Scope};
use self::collections::{CollectionsUi, TreeCtx};
use self::suggest::SuggestUi;
use self::tracks::Media;
use super::flow::FlowUi;
use super::{Cx, Panel, resized};
use crate::core::collections::CollectionCmd;
use crate::core::command::{Command, ViewCmd};
use crate::core::view::{Aside, Mode, Pane, Region, Source};
use crate::ui::widgets::{Opens, StackItem, icon_stack, icon_stack::WIDTH};

/// The left icon stack.
const SOURCES: [StackItem<Source>; 1] = [StackItem::new(
    Source::Browser,
    "🎵",
    "Library: all tracks, collections and folders",
)];

/// The right icon stack.
const ASIDES: [StackItem<Aside>; 1] = [StackItem::new(
    Aside::Suggestions,
    "💡",
    "Suggestions: what could play next",
)];

pub struct LibraryPane {
    /// The folder tree and the track list.
    pub browser: LibraryUi,
    pub collections: CollectionsUi,
    pub suggest: SuggestUi,
    pub flow: FlowUi,
    /// Waveform thumbnails and cover art for every list.
    pub media: Media,
    /// Which list the keyboard and the controller's browse knob move through.
    pub pane: Pane,
}

impl LibraryPane {
    pub fn new(flow: FlowUi) -> LibraryPane {
        LibraryPane {
            browser: LibraryUi::default(),
            collections: CollectionsUi::default(),
            suggest: SuggestUi::default(),
            flow,
            media: Media::default(),
            pane: Pane::Library,
        }
    }

    /// Full-screen Flow: the decks and side panes hide while planning.
    pub fn planning(&self) -> bool {
        self.flow.open && self.flow.full
    }

    /// The left pane: the source picked in the icon stack.
    fn source(&mut self, ui: &mut Ui, source: Source, cx: &mut Cx) {
        match source {
            Source::Browser => {
                // What the collection tree needs from the track list, read before it's
                // borrowed for drawing.
                let (search, selected) = (self.browser.search.clone(), self.browser.selected());
                let open = if self.flow.open {
                    self.flow.flow
                } else {
                    match self.browser.scope() {
                        Scope::Collection(id) => Some(*id),
                        _ => None,
                    }
                };
                let tree = TreeCtx {
                    collections: cx.core.collections,
                    open,
                    search: &search,
                    selected: &selected,
                };
                let colls = &mut self.collections;
                let mut coll_cmds: Vec<CollectionCmd> = vec![];
                self.browser.tree(ui, &cx.core, cx.out, |ui| {
                    colls.show(ui, &tree, &mut coll_cmds)
                });
                cx.out
                    .extend(coll_cmds.into_iter().map(Command::Collection));
            }
        }
    }

    /// A pane on the right, opened from its icon stack.
    fn aside(&mut self, ui: &mut Ui, aside: Aside, cx: &mut Cx) {
        match aside {
            Aside::Suggestions => {
                let focus = self.pane == Pane::Suggest;
                if self.suggest.show(ui, &cx.core, focus, cx.out) {
                    cx.send(ViewCmd::FocusPane(Pane::Suggest));
                }
            }
        }
    }

    /// The middle: the track list, or the Flow canvas.
    fn middle(&mut self, ui: &mut Ui, cx: &mut Cx) {
        ui.horizontal(|ui| {
            if ui.selectable_label(!self.flow.open, "Tracks").clicked() {
                cx.send(ViewCmd::ShowFlow(false));
            }
            if ui
                .selectable_label(self.flow.open, "Flow")
                .on_hover_text("Plan a set: add tracks, then grow it from suggestions")
                .clicked()
            {
                cx.send(ViewCmd::ShowFlow(true));
            }
        });
        ui.separator();
        if self.flow.open {
            let preview = cx.engine.preview;
            self.flow
                .show(ui, &cx.core, &mut self.media, preview, cx.out);
        } else if self.browser.list(ui, &cx.core, &mut self.media, cx.out) {
            cx.send(ViewCmd::FocusPane(Pane::Library));
        }
    }
}

impl Panel for LibraryPane {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let layout = cx.prefs.layout().clone();
        let show_sides = !self.planning();
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
            .show_animated_inside(ui, show_sides, |ui| {
                if let Some(s) = icon_stack(ui, &SOURCES, layout.source, Opens::Right) {
                    cx.send(ViewCmd::Source(s));
                }
            });
        if let Some(source) = layout.source {
            let r = egui::SidePanel::left(id("source"))
                .resizable(true)
                .default_width(layout.sizes.source)
                .width_range(140.0..=600.0)
                .show_animated_inside(ui, show_sides, |ui| self.source(ui, source, cx));
            if let Some(r) = r {
                resized(
                    ui.ctx(),
                    r.response.rect.width(),
                    Region::Source,
                    layout.sizes.source,
                    cx,
                );
            }
        }
        egui::SidePanel::right(id("aside-stack"))
            .resizable(false)
            .exact_width(WIDTH + 6.0)
            .frame(stack_frame)
            .show_animated_inside(ui, show_sides, |ui| {
                if let Some(a) = icon_stack(ui, &ASIDES, layout.aside, Opens::Left) {
                    cx.send(ViewCmd::Aside(a));
                }
            });
        if let Some(aside) = layout.aside {
            let r = egui::SidePanel::right(id("aside"))
                .resizable(true)
                .default_width(layout.sizes.aside)
                .width_range(240.0..=700.0)
                .show_animated_inside(ui, show_sides, |ui| self.aside(ui, aside, cx));
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
