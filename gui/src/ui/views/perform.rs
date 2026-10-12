//! Perform: the decks' waveforms stacked across the top, the decks either side of
//! the mixer, and the library below.

use earshot_engine::DeckId;
use egui::{Margin, vec2};

use super::{Panels, library, row_frame};
use crate::core::player::DECKS;
use crate::core::view::Region;
use crate::ui::panels::deck::{DeckPanel, Side, WavePanel};
use crate::ui::panels::mixer::{self, MixerPanel};
use crate::ui::panels::{Cx, Panel, resized};

pub fn show(ctx: &egui::Context, panels: &mut Panels, cx: &mut Cx) {
    let planning = panels.library.planning();
    let decks: Vec<DeckId> = DeckId::first(DECKS).collect();
    let waves_h = cx.prefs.perform.sizes.waves;
    let r = egui::TopBottomPanel::top("waves-perform")
        .resizable(true)
        .default_height(waves_h)
        .height_range(60.0..=520.0)
        .frame(row_frame(cx, Margin::same(6)))
        .show_animated(ctx, !planning, |ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            let n = decks.len() as f32;
            let height = (ui.available_height() - 3.0 * (n - 1.0)) / n;
            for &d in &decks {
                let deck = &mut panels.decks[d.index()];
                WavePanel { deck, height }.ui(ui, cx);
            }
        });
    if let Some(r) = r {
        resized(ctx, r.response.rect.height(), Region::Waves, waves_h, cx);
    }
    // A fixed height (the mixer's), so the decks can't be stretched by a height egui
    // remembered from an older layout.
    let height = mixer::HEIGHT + 12.0;
    egui::TopBottomPanel::top("decks-perform")
        .exact_height(height + 8.0)
        .frame(row_frame(cx, Margin::symmetric(6, 4)))
        .show_animated(ctx, !planning, |ui| {
            // Decks on the crossfader's left, the mixer, then the rest.
            let side = |d: &DeckId| {
                if mixer::on_left(*d) {
                    Side::Left
                } else {
                    Side::Right
                }
            };
            let mixer_w = mixer::width(&decks) + 16.0;
            let n = decks.len() as f32;
            let width = (ui.available_width() - mixer_w - 6.0 * n) / n;
            ui.horizontal_top(|ui| {
                for d in decks.iter().filter(|d| side(d) == Side::Left) {
                    let deck = &mut panels.decks[d.index()];
                    DeckPanel {
                        deck,
                        width,
                        height,
                        side: Side::Left,
                    }
                    .ui(ui, cx);
                }
                ui.allocate_ui(vec2(mixer_w, 0.0), |ui| {
                    MixerPanel {
                        decks: &decks,
                        width: mixer_w,
                    }
                    .ui(ui, cx)
                });
                for d in decks.iter().filter(|d| side(d) == Side::Right) {
                    let deck = &mut panels.decks[d.index()];
                    DeckPanel {
                        deck,
                        width,
                        height,
                        side: Side::Right,
                    }
                    .ui(ui, cx);
                }
            });
        });
    library(ctx, panels, cx);
}
