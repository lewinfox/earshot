//! Prepare: one player. Its waveform across the top with the cue pane on the right
//! (as in Rekordbox), then the tool tabs (what we're doing to the track) over the
//! tool's body, and the library below.
//!
//! To add a tool: a variant of `core::view::Tool`, an entry in [`TOOLS`] and an arm
//! in [`tool`].

use earshot_engine::DeckId;
use egui::{Margin, Ui, vec2};

use super::{Panels, library, row_frame};
use crate::core::command::ViewCmd;
use crate::core::view::{Region, Tool};
use crate::ui::panels::deck::{CUE_PANE_W, CuePane, PreparePlayer, WavePanel};
use crate::ui::panels::{Cx, Panel, mixer, resized};
use crate::ui::widgets::{Tab, tool_tabs};

const TOOLS: [Tab<Tool>; 2] = [
    Tab {
        id: Tool::CuesGrid,
        name: "Cues & Grid",
        tip: "Cues, loops, beat jumps and the beat grid",
    },
    Tab {
        id: Tool::TempoLock,
        name: "Tempo lock",
        tip: "Straighten a track's tempo (coming soon)",
    },
];

/// The tab bar's height, and a little room under the player (it runs a few points
/// taller than Perform's decks).
const TABS_H: f32 = 34.0;

pub fn show(ctx: &egui::Context, panels: &mut Panels, cx: &mut Cx) {
    let planning = panels.library.planning();
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
    // The player row is as tall as Perform's decks, plus the tabs.
    let height = mixer::HEIGHT + 12.0;
    egui::TopBottomPanel::top("tools-prepare")
        .exact_height(height + 8.0 + TABS_H)
        .frame(row_frame(cx, Margin::symmetric(6, 4)))
        .show_animated(ctx, !planning, |ui| {
            let current = cx.prefs.prepare.tool;
            ui.allocate_ui(vec2(ui.available_width(), 22.0), |ui| {
                if let Some(t) = tool_tabs(ui, &TOOLS, current) {
                    cx.send(ViewCmd::Tool(t));
                }
            });
            tool(ui, current, height, panels, cx);
        });
    library(ctx, panels, cx);
}

/// The tool tab's body, `height` tall.
fn tool(ui: &mut Ui, tool: Tool, height: f32, panels: &mut Panels, cx: &mut Cx) {
    match tool {
        Tool::CuesGrid => {
            let deck = &mut panels.decks[DeckId::PREPARE.index()];
            let width = ui.available_width();
            PreparePlayer {
                deck,
                width,
                height,
            }
            .ui(ui, cx);
        }
        Tool::TempoLock => {
            ui.label(
                egui::RichText::new(
                    "Tempo lock is on its way: analyse a track's tempo, \
                     straighten it and save the result.",
                )
                .color(cx.theme.muted),
            );
        }
    }
}
