//! Prepare: the same decks as Perform across the top, then the tool tabs (what we're
//! doing to the target track: the deck picked in the top bar), each with the library
//! on its left: `| library | tool |` (Flow adds its suggestions on the right).
//!
//! To add a tool: a variant of `core::view::Tool`, an entry in [`TOOLS`] and an arm
//! in [`tool`].

use egui::Ui;

use super::perform::decks_rows;
use super::{Panels, body, pane_frame, tab_bar};
use crate::core::command::ViewCmd;
use crate::core::view::Tool;
use crate::ui::panels::deck::CuePane;
use crate::ui::panels::export::ExportPanel;
use crate::ui::panels::flow::FlowTab;
use crate::ui::panels::stems::StemsPanel;
use crate::ui::panels::tempo_lock::TempoLockPanel;
use crate::ui::panels::trim::TrimPanel;
use crate::ui::panels::{Cx, Panel};
use crate::ui::widgets::{Tab, tool_tabs};

const TOOLS: [Tab<Tool>; 6] = [
    Tab {
        id: Tool::Cues,
        name: "Cues",
        tip: "The target track's memory cues and loops",
    },
    Tab {
        id: Tool::TempoLock,
        name: "Tempo lock",
        tip: "Chart a track's tempo, straighten it to one BPM and save the result",
    },
    Tab {
        id: Tool::Stems,
        name: "Stems",
        tip: "Drums, bass, vocals and other",
    },
    Tab {
        id: Tool::Trim,
        name: "Trim",
        tip: "Keep the part of the track between two points and save it as a new file",
    },
    Tab {
        id: Tool::Flow,
        name: "Flow",
        tip: "Plan a set: add tracks, then grow it from suggestions",
    },
    Tab {
        id: Tool::Export,
        name: "Export",
        tip: "Write playlists to a Rekordbox or Serato stick, Serato, Mixxx or Rekordbox XML, \
              or convert a stick to the other format",
    },
];

pub fn show(ctx: &egui::Context, panels: &mut Panels, cx: &mut Cx) {
    let current = cx.prefs.prepare.tool;
    decks_rows(ctx, panels, cx, "prepare");
    tab_bar(ctx, "tools-prepare", cx.theme, |ui| {
        if let Some(t) = tool_tabs(ui, &TOOLS, current) {
            cx.send(ViewCmd::Tool(t));
        }
    });
    body(
        ctx,
        egui::Frame::new().fill(cx.theme.bg),
        |ui| match current {
            Tool::Flow => FlowTab {
                flow: &mut panels.flow,
                library: &mut panels.library,
            }
            .ui(ui, cx),
            _ => {
                panels.library.show_beside(ui, cx);
                egui::CentralPanel::default()
                    .frame(pane_frame(cx.theme))
                    .show_inside(ui, |ui| tool(ui, current, panels, cx));
            }
        },
    );
}

/// A tool's body, right of the library.
fn tool(ui: &mut Ui, tool: Tool, panels: &mut Panels, cx: &mut Cx) {
    match tool {
        // Drawn by its own tab.
        Tool::Flow => {}
        Tool::Cues => CuePane(&mut panels.decks[cx.prefs.target().index()]).ui(ui, cx),
        Tool::TempoLock => TempoLockPanel {
            tab: &mut panels.tempo_lock,
            roots: &panels.library.browser.folders,
        }
        .ui(ui, cx),
        Tool::Stems => StemsPanel {
            tab: &mut panels.stems,
        }
        .ui(ui, cx),
        Tool::Trim => TrimPanel {
            tab: &mut panels.trim,
        }
        .ui(ui, cx),
        Tool::Export => ExportPanel {
            tab: &mut panels.export,
            external: &mut panels.library.external,
        }
        .ui(ui, cx),
    }
}
