//! Controller: Perform's decks and mixer across the top (so the controller can be
//! played as it would be in a set), then four tabs: Mapping (map each message to a
//! command, by pressing it), History (the messages as they came in), Send (send the
//! controller a message typed in, for testing) and the library (to try the browse
//! knob and load buttons).

use super::perform::decks_rows;
use super::{Panels, body, library, pane_frame, tab_bar};
use crate::ui::flex::Tui;
use crate::ui::panels::controller::{ControllerTab, HistoryPanel, MappingPanel, SendPanel};
use crate::ui::panels::{Cx, Panel};
use crate::ui::widgets::{Tab, tool_tabs};

const TABS: [Tab<ControllerTab>; 4] = [
    Tab {
        id: ControllerTab::Mapping,
        name: "Mapping",
        tip: "Every message the controller sends, and the command it runs",
    },
    Tab {
        id: ControllerTab::History,
        name: "History",
        tip: "The messages as they came in, newest first",
    },
    Tab {
        id: ControllerTab::Send,
        name: "Send",
        tip: "Send the controller a message you type, to see what it does",
    },
    Tab {
        id: ControllerTab::Library,
        name: "Library",
        tip: "The library, to try the browse knob and load buttons",
    },
];

pub fn show(t: &mut Tui, panels: &mut Panels, cx: &mut Cx) {
    decks_rows(t, panels, cx);
    let current = panels.controller.tab;
    let mut picked = None;
    tab_bar(t, cx.theme, |ui| {
        picked = tool_tabs(ui, &TABS, current);
    });
    if let Some(tab) = picked {
        panels.controller.tab = tab;
    }
    match current {
        ControllerTab::Library => library(t, panels, cx),
        tab => {
            body(t, pane_frame(cx.theme), |ui| {
                let c = &mut panels.controller;
                match tab {
                    ControllerTab::History => HistoryPanel { tab: c }.ui(ui, cx),
                    ControllerTab::Send => SendPanel { tab: c }.ui(ui, cx),
                    _ => MappingPanel { tab: c }.ui(ui, cx),
                }
            });
        }
    }
}
