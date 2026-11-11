//! Controller: Perform's decks and mixer across the top (so the controller can be
//! played as it would be in a set), then two tabs: the library (to try the browse
//! knob and load buttons) and the Mapping tab (what each message is, and whether
//! earshot can do it).

use egui::{Margin, vec2};

use super::perform::decks_rows;
use super::{Panels, library, row_frame};
use crate::ui::panels::controller::{ControllerTab, MappingPanel};
use crate::ui::panels::{Cx, Panel};
use crate::ui::widgets::{Tab, tool_tabs};

const TABS: [Tab<ControllerTab>; 2] = [
    Tab {
        id: ControllerTab::Mapping,
        name: "Mapping",
        tip: "Every message the controller sends, looked up in its Mixxx mapping",
    },
    Tab {
        id: ControllerTab::Library,
        name: "Library",
        tip: "The library, to try the browse knob and load buttons",
    },
];

pub fn show(ctx: &egui::Context, panels: &mut Panels, cx: &mut Cx) {
    decks_rows(ctx, panels, cx, "controller");
    let current = panels.controller.tab;
    egui::TopBottomPanel::top("tabs-controller")
        .exact_height(30.0)
        .frame(row_frame(cx, Margin::symmetric(6, 4)))
        .show(ctx, |ui| {
            ui.allocate_ui(vec2(ui.available_width(), 22.0), |ui| {
                if let Some(t) = tool_tabs(ui, &TABS, current) {
                    panels.controller.tab = t;
                }
            });
        });
    match current {
        ControllerTab::Library => library(ctx, panels, cx),
        ControllerTab::Mapping => {
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(cx.theme.bg).inner_margin(6))
                .show(ctx, |ui| {
                    MappingPanel {
                        tab: &mut panels.controller,
                    }
                    .ui(ui, cx)
                });
        }
    }
}
