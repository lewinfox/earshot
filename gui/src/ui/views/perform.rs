//! Perform: the decks' waveforms stacked across the top, the decks either side of
//! the mixer, and the library below. Two decks, or four (the 2/4 toggle in the top
//! bar): then each side of the mixer holds two smaller decks, one above the other.

use earshot_engine::DeckId;
use egui::Ui;
use egui_extras::{Size, StripBuilder};

use super::{Panels, library, row_frame};
use crate::core::view::Region;
use crate::ui::layout::{Edge, STACK_GAP, WAVE_GAP};
use crate::ui::panels::deck::{DeckLayout, DeckPanel, Side, WavePanel};
use crate::ui::panels::mixer::{self, MixerPanel};
use crate::ui::panels::{Cx, Panel, drag_to_resize};

pub fn show(ctx: &egui::Context, panels: &mut Panels, cx: &mut Cx) {
    decks_rows(ctx, panels, cx, "perform");
    library(ctx, panels, cx);
}

/// The waveforms' row and the decks' row (with the mixer): the same in every mode.
/// `mode` keeps each mode's panel ids apart. Their heights, the decks' scale and
/// whether there's a mixer are the layout's (`cx.shell`); nothing is sized here.
pub fn decks_rows(ctx: &egui::Context, panels: &mut Panels, cx: &mut Cx, mode: &str) {
    let decks = cx.prefs.decks();
    let shell = cx.shell;
    let sp = &cx.theme.spacing;
    let waves_row = egui::TopBottomPanel::top(format!("waves-{mode}"))
        .exact_height(shell.waves_h)
        .frame(row_frame(cx, sp.waves_margin));
    show_row(ctx, waves_row, shell.waves_h > 0.0, |ui| {
        ui.spacing_mut().item_spacing.y = WAVE_GAP;
        for &d in &decks {
            let deck = &mut panels.decks[d.index()];
            WavePanel {
                deck,
                height: shell.wave_h,
            }
            .ui(ui, cx);
        }
        drag_to_resize(ui, Edge::Bottom, Region::Waves, shell.size.y, cx);
    });
    let row = shell.decks;
    let decks_row = egui::TopBottomPanel::top(format!("decks-{mode}"))
        .exact_height(shell.decks_h)
        .frame(row_frame(cx, sp.row_margin));
    show_row(ctx, decks_row, shell.decks_h > 0.0, |ui| {
        // Decks on the crossfader's left, the mixer, then the rest.
        let side = |d: &DeckId| {
            if mixer::on_left(*d) {
                Side::Left
            } else {
                Side::Right
            }
        };
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
                            height: row.deck_h,
                            side: s,
                            scale: row.scale,
                            tempo_fader: row.mixer.is_some(),
                        },
                    }
                    .ui(ui, cx);
                }
            });
        };
        // The mixer at its own width; the decks share the rest.
        let mut strip = StripBuilder::new(ui).size(Size::remainder());
        if let Some((_, width)) = row.mixer {
            strip = strip.size(Size::exact(width));
        }
        strip.size(Size::remainder()).horizontal(|mut strip| {
            strip.cell(|ui| column(ui, Side::Left, panels, cx));
            if let Some((scale, _)) = row.mixer {
                strip.cell(|ui| {
                    MixerPanel {
                        decks: &decks,
                        scale,
                        height: row.height,
                    }
                    .ui(ui, cx)
                });
            }
            strip.cell(|ui| column(ui, Side::Right, panels, cx));
        });
    });
}

/// A row of the shell, there or not at once (not slid in or out): a change of layout
/// lands whole, in one frame.
fn show_row(
    ctx: &egui::Context,
    row: egui::TopBottomPanel,
    shown: bool,
    add: impl FnOnce(&mut Ui),
) {
    if shown {
        row.show(ctx, add);
    }
}
