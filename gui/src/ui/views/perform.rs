//! Perform: the decks' waveforms stacked across the top, the decks either side of
//! the mixer, and the library below. Two decks, or four (the 2/4 toggle in the top
//! bar): then each side of the mixer holds two smaller decks, one above the other.

use earshot_engine::DeckId;
use egui::{Margin, Ui, vec2};

use super::{Panels, library, row_frame};
use crate::core::view::Region;
use crate::ui::panels::deck::{DeckPanel, Side, WavePanel};
use crate::ui::panels::mixer::{self, MixerPanel};
use crate::ui::panels::{Cx, Panel, resized};

/// A smaller deck's height, while four are shown.
const COMPACT_H: f32 = 150.0;
/// Between the decks stacked on one side.
const STACK_GAP: f32 = 4.0;

pub fn show(ctx: &egui::Context, panels: &mut Panels, cx: &mut Cx) {
    let decks = cx.prefs.decks();
    let compact = decks.len() > 2;
    let waves_h = cx.prefs.perform.sizes.waves;
    let r = egui::TopBottomPanel::top("waves-perform")
        .resizable(true)
        .default_height(waves_h)
        .height_range(60.0..=520.0)
        .frame(row_frame(cx, Margin::same(6)))
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            let n = decks.len() as f32;
            let height = (ui.available_height() - 3.0 * (n - 1.0)) / n;
            for &d in &decks {
                let deck = &mut panels.decks[d.index()];
                WavePanel { deck, height }.ui(ui, cx);
            }
        });
    resized(ctx, r.response.rect.height(), Region::Waves, waves_h, cx);
    // Fixed heights, so the decks can't be stretched by a height egui remembered from
    // an older layout: the mixer's, or two smaller decks'.
    let deck_h = if compact {
        COMPACT_H
    } else {
        mixer::HEIGHT + 12.0
    };
    let per_side = decks.len().div_ceil(2) as f32;
    let height = (deck_h * per_side + STACK_GAP * (per_side - 1.0)).max(mixer::HEIGHT + 12.0);
    egui::TopBottomPanel::top("decks-perform")
        .exact_height(height + 8.0)
        .frame(row_frame(cx, Margin::symmetric(6, 4)))
        .show(ctx, |ui| {
            // Decks on the crossfader's left, the mixer, then the rest.
            let side = |d: &DeckId| {
                if mixer::on_left(*d) {
                    Side::Left
                } else {
                    Side::Right
                }
            };
            let mixer_w = mixer::width(&decks) + 16.0;
            let width = (ui.available_width() - mixer_w - 12.0) / 2.0;
            let column = |ui: &mut Ui, s: Side, panels: &mut Panels, cx: &mut Cx| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = STACK_GAP;
                    for d in decks.iter().filter(|d| side(d) == s) {
                        let deck = &mut panels.decks[d.index()];
                        DeckPanel {
                            deck,
                            width,
                            height: deck_h,
                            side: s,
                            compact,
                        }
                        .ui(ui, cx);
                    }
                });
            };
            ui.horizontal_top(|ui| {
                column(ui, Side::Left, panels, cx);
                ui.allocate_ui(vec2(mixer_w, 0.0), |ui| {
                    MixerPanel {
                        decks: &decks,
                        width: mixer_w,
                    }
                    .ui(ui, cx)
                });
                column(ui, Side::Right, panels, cx);
            });
        });
    library(ctx, panels, cx);
}
