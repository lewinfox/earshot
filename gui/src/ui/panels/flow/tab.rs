//! Prepare's Flow tab: `| library | canvas | suggestions |`. The library is the same
//! component as everywhere else (without its own side panes); the suggestions pop out
//! from an icon stack on the right, as in Rekordbox, and follow the selected card.

use egui::Ui;

use super::FlowUi;
use crate::core::command::ViewCmd;
use crate::core::view::{Aside, Pane, Region};
use crate::ui::panels::library::LibraryPane;
use crate::ui::panels::library::table::TableView;
use crate::ui::panels::{Cx, Panel, resized};
use crate::ui::widgets::{Opens, StackItem, icon_stack, icon_stack::WIDTH};

/// The right icon stack.
const ASIDES: [StackItem<Aside>; 1] = [StackItem::new(
    Aside::Suggestions,
    "💡",
    "Suggestions: what could follow the selected card",
)];

pub struct FlowTab<'a> {
    pub flow: &'a mut FlowUi,
    pub library: &'a mut LibraryPane,
}

impl Panel for FlowTab<'_> {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let layout = cx.prefs.prepare.clone();
        let stack_frame = egui::Frame::new()
            .fill(cx.theme.bg)
            .inner_margin(egui::Margin::symmetric(3, 6));
        egui::SidePanel::right("flow-aside-stack")
            .resizable(false)
            .exact_width(WIDTH + 6.0)
            .frame(stack_frame)
            .show_inside(ui, |ui| {
                if let Some(a) = icon_stack(ui, &ASIDES, layout.aside, Opens::Left) {
                    cx.send(ViewCmd::Aside(a));
                }
            });
        let r = egui::SidePanel::right("flow-aside")
            .resizable(true)
            .default_width(layout.sizes.aside)
            .width_range(240.0..=700.0)
            .show_animated_inside(ui, layout.aside.is_some(), |ui| match layout.aside {
                Some(Aside::Suggestions) | None => {
                    let lib = &mut *self.library;
                    let focus = lib.focused_pane == Pane::Suggest;
                    let table = lib.tables.get_mut(TableView::Suggest);
                    if lib
                        .suggest
                        .show(ui, &cx.core, focus, &mut lib.media, table, cx.out)
                    {
                        cx.send(ViewCmd::FocusPane(Pane::Suggest));
                    }
                }
            });
        if let Some(r) = r {
            let w = r.response.rect.width();
            resized(ui.ctx(), w, Region::Aside, layout.sizes.aside, cx);
        }
        let r = egui::SidePanel::left("flow-library")
            .resizable(true)
            .default_width(layout.sizes.flow_library)
            .width_range(320.0..=1400.0)
            .frame(egui::Frame::new().fill(cx.theme.bg))
            .show_inside(ui, |ui| {
                // The panel keeps the width its contents take: all of it.
                ui.expand_to_include_rect(ui.max_rect());
                self.library.show(ui, cx, false)
            });
        let w = r.response.rect.width();
        resized(
            ui.ctx(),
            w,
            Region::FlowLibrary,
            layout.sizes.flow_library,
            cx,
        );
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(cx.theme.bg).inner_margin(4))
            .show_inside(ui, |ui| {
                let preview = cx.engine.preview;
                let area = ui.available_rect_before_wrap();
                let media = &mut self.library.media;
                self.flow.show(ui, &cx.core, media, preview, cx.out);
                // A click on the canvas gives it focus (its selected card is what
                // "load selected" loads).
                if ui.input(|i| i.pointer.any_pressed())
                    && ui
                        .ctx()
                        .pointer_interact_pos()
                        .is_some_and(|p| area.contains(p))
                    && self.library.focused_pane != Pane::Flow
                {
                    cx.send(ViewCmd::FocusPane(Pane::Flow));
                }
            });
    }
}
