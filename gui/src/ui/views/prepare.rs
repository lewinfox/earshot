//! Prepare: one player. Its waveform across the top with the cue pane on the right
//! (as in Rekordbox), its controls under that, then the tool tabs (what we're doing
//! to the track), each with the library on its left: `| library | tool |` (Flow adds
//! its suggestions on the right).
//!
//! To add a tool: a variant of `core::view::Tool`, an entry in [`TOOLS`] and an arm
//! in [`tool`].

use earshot_engine::DeckId;
use egui::{Margin, Ui, vec2};

use super::{Panels, row_frame};
use crate::core::command::ViewCmd;
use crate::core::view::{Region, Tool};
use crate::ui::panels::deck::{CUE_PANE_W, CuePane, PreparePlayer, WavePanel};
use crate::ui::panels::flow::FlowTab;
use crate::ui::panels::tempo_lock::TempoLockPanel;
use crate::ui::panels::{Cx, Panel, mixer, resized};
use crate::ui::widgets::{Tab, tool_tabs};

const TOOLS: [Tab<Tool>; 2] = [
    Tab {
        id: Tool::TempoLock,
        name: "Tempo lock",
        tip: "Chart a track's tempo, straighten it to one BPM and save the result",
    },
    Tab {
        id: Tool::Flow,
        name: "Flow",
        tip: "Plan a set: add tracks, then grow it from suggestions",
    },
];

pub fn show(ctx: &egui::Context, panels: &mut Panels, cx: &mut Cx) {
    let current = cx.prefs.prepare.tool;
    // The Flow editor's full screen hides the player.
    let planning = current == Tool::Flow && panels.flow.full;
    let waves_h = cx.prefs.prepare.sizes.waves;
    let r = egui::TopBottomPanel::top("waves-prepare")
        .resizable(true)
        .default_height(waves_h)
        .height_range(60.0..=520.0)
        .frame(row_frame(cx, Margin::same(6)))
        .show_animated(ctx, !planning, |ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            let (w, height) = (ui.available_width(), ui.available_height());
            let wave_w = (w - CUE_PANE_W - 6.0).max(w / 2.0);
            let deck = &mut panels.decks[DeckId::PREPARE.index()];
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.allocate_ui(vec2(wave_w, height), |ui| {
                    WavePanel {
                        deck: &mut *deck,
                        height,
                    }
                    .ui(ui, cx);
                });
                CuePane(deck).ui(ui, cx);
            });
        });
    if let Some(r) = r {
        resized(ctx, r.response.rect.height(), Region::Waves, waves_h, cx);
    }
    // The player's controls (hot cues or the grid editor, loops, beat jumps, tempo),
    // as tall as Perform's decks.
    let height = mixer::HEIGHT + 12.0;
    egui::TopBottomPanel::top("player-prepare")
        .exact_height(height + 8.0)
        .frame(row_frame(cx, Margin::symmetric(6, 4)))
        .show_animated(ctx, !planning, |ui| {
            let deck = &mut panels.decks[DeckId::PREPARE.index()];
            let width = ui.available_width();
            PreparePlayer {
                deck,
                width,
                height,
            }
            .ui(ui, cx);
        });
    egui::TopBottomPanel::top("tools-prepare")
        .exact_height(30.0)
        .frame(row_frame(cx, Margin::symmetric(6, 4)))
        .show(ctx, |ui| {
            ui.allocate_ui(vec2(ui.available_width(), 22.0), |ui| {
                if let Some(t) = tool_tabs(ui, &TOOLS, current) {
                    cx.send(ViewCmd::Tool(t));
                }
            });
        });
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(cx.theme.bg))
        .show(ctx, |ui| match current {
            Tool::Flow => FlowTab {
                flow: &mut panels.flow,
                library: &mut panels.library,
            }
            .ui(ui, cx),
            _ => {
                panels.library.show_beside(ui, cx);
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().fill(cx.theme.bg).inner_margin(6))
                    .show_inside(ui, |ui| tool(ui, current, panels, cx));
            }
        });
}

/// A tool's body, right of the library.
fn tool(ui: &mut Ui, tool: Tool, panels: &mut Panels, cx: &mut Cx) {
    match tool {
        // Drawn by its own tab.
        Tool::Flow => {}
        Tool::TempoLock => TempoLockPanel {
            tab: &mut panels.tempo_lock,
            roots: &panels.library.browser.folders,
        }
        .ui(ui, cx),
    }
}
