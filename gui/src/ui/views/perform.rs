//! Perform: the decks' waveforms stacked across the top, the decks either side of
//! the mixer, the effect units in a strip under them (when shown), and the library
//! below. Two decks, or four (the 2/4 toggle in the top
//! bar): then each side of the mixer holds two smaller decks, one above the other.

use earshot_engine::DeckId;
use egui::{Margin, Ui};
use egui_extras::{Size, StripBuilder};

use super::{Panels, library, row_frame};
use crate::core::view::Region;
use crate::ui::panels::deck::{DeckLayout, DeckPanel, Side, WavePanel, min_height};
use crate::ui::panels::mixer::{self, MixerPanel};
use crate::ui::panels::{Cx, Panel, resized};

/// Between the decks stacked on one side.
const STACK_GAP: f32 = 4.0;

pub fn show(ctx: &egui::Context, panels: &mut Panels, cx: &mut Cx) {
    decks_rows(ctx, panels, cx, "perform");
    library(ctx, panels, cx);
}

/// The waveforms' row and the decks' row (with the mixer), as Perform draws them.
/// Controller mode draws the same. `mode` keeps each mode's panel ids apart.
pub fn decks_rows(ctx: &egui::Context, panels: &mut Panels, cx: &mut Cx, mode: &str) {
    let decks = cx.prefs.decks();
    let compact = decks.len() > 2;
    let waves_h = cx.prefs.layout().sizes.waves;
    let r = egui::TopBottomPanel::top(format!("waves-{mode}"))
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
    // One height for the row, the decks' or the mixer's, whichever needs more: the
    // mixer fills it, and each side's decks share it (their controls grow to fill).
    // Worked out here, so nothing can be stretched by a height egui remembered from
    // an older layout.
    let per_side = decks.len().div_ceil(2) as f32;
    let gaps = STACK_GAP * (per_side - 1.0);
    let height = (min_height(compact) * per_side + gaps).max(mixer::HEIGHT + 12.0);
    let deck_h = (height - gaps) / per_side;
    egui::TopBottomPanel::top(format!("decks-{mode}"))
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
            let show_mixer = cx.prefs.mixer_shown();
            let column = |ui: &mut Ui, s: Side, panels: &mut Panels, cx: &mut Cx| {
                let width = ui.available_width();
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = STACK_GAP;
                    for d in decks.iter().filter(|d| side(d) == s) {
                        let deck = &mut panels.decks[d.index()];
                        DeckPanel {
                            deck,
                            layout: DeckLayout {
                                width,
                                height: deck_h,
                                side: s,
                                compact,
                                tempo_fader: show_mixer,
                            },
                        }
                        .ui(ui, cx);
                    }
                });
            };
            // The mixer at its own width; the decks share the rest, so nothing can be
            // pushed off the edge.
            let mut strip = StripBuilder::new(ui).size(Size::remainder());
            if show_mixer {
                strip = strip.size(Size::exact(mixer::width(&decks) + 16.0));
            }
            strip.size(Size::remainder()).horizontal(|mut strip| {
                strip.cell(|ui| column(ui, Side::Left, panels, cx));
                if show_mixer {
                    strip.cell(|ui| {
                        let width = ui.available_width();
                        MixerPanel {
                            decks: &decks,
                            width,
                            height,
                        }
                        .ui(ui, cx)
                    });
                }
                strip.cell(|ui| column(ui, Side::Right, panels, cx));
            });
        });
}
