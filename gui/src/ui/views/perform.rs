//! Perform: the decks' waveforms stacked across the top, the decks either side of
//! the mixer, and the library below. Two decks, or four (the 2/4 toggle in the top
//! bar): then each side of the mixer holds two smaller decks, one above the other.

use earshot_engine::DeckId;

use super::{Panels, library};
use crate::core::view::Region;
use crate::ui::flex::{self, Flex, Tui, TuiBuilderLogic, col, leaf, row};
use crate::ui::layout::{DECKS_SHARE, Edge, STACK_GAP, WAVE_GAP};
use crate::ui::panels::deck::{DeckLayout, Side, WavePanel};
use crate::ui::panels::mixer::{self, MixerPanel};
use crate::ui::panels::{Cx, Panel, drag_to_resize};

pub fn show(t: &mut Tui, panels: &mut Panels, cx: &mut Cx) {
    decks_rows(t, panels, cx);
    library(t, panels, cx);
}

/// The waveforms' row and the decks' row (with the mixer): the same in every mode.
pub fn decks_rows(t: &mut Tui, panels: &mut Panels, cx: &mut Cx) {
    let decks = cx.prefs.decks();
    let shell = cx.shell;
    let sp = &cx.theme.spacing;
    if shell.waves {
        // As tall as dragged; the waveforms share it.
        let style = leaf().h_pct(shell.waves_pct()).no_shrink();
        let frame = egui::Frame::new()
            .fill(cx.theme.bg)
            .inner_margin(sp.waves_margin);
        flex::pane(t, style, |ui| {
            flex::boxed(ui, frame, |ui| {
                let n = decks.len().max(1) as f32;
                let wave_h = ((ui.available_height() - (n - 1.0) * WAVE_GAP) / n).max(0.0);
                ui.spacing_mut().item_spacing.y = WAVE_GAP;
                for &d in &decks {
                    let deck = &mut panels.decks[d.index()];
                    WavePanel {
                        deck,
                        height: wave_h,
                    }
                    .ui(ui, cx);
                }
            });
            drag_to_resize(ui, Edge::Bottom, Region::Waves, shell.size.y, cx);
        });
    }
    if !shell.decks_row {
        return;
    }
    // `| left decks | mixer | right decks |`: the decks share what the mixer leaves,
    // each as tall as its width makes it (its aspect ratio), and the row is as tall
    // as they are (never more than its share of the window). The mixer stretches to
    // the row's height.
    let row_style = row()
        .gap(sp.item.x)
        .pad(sp.row_margin)
        .max_h_pct(DECKS_SHARE)
        .no_shrink();
    let tempo_fader = shell.mixer;
    t.style(row_style).add(|t| {
        let side = |d: &DeckId| {
            if mixer::on_left(*d) {
                Side::Left
            } else {
                Side::Right
            }
        };
        let column = |t: &mut Tui, s: Side, panels: &mut Panels, cx: &mut Cx| {
            t.style(col().grow().gap(STACK_GAP)).add(|t| {
                for d in decks.iter().filter(|d| side(d) == s) {
                    let deck = &mut panels.decks[d.index()];
                    let engine = cx.engine;
                    let info = engine.deck(deck.id);
                    deck.show(
                        t,
                        DeckLayout {
                            side: s,
                            tempo_fader,
                        },
                        info,
                        cx.out,
                    );
                }
            });
        };
        column(t, Side::Left, panels, cx);
        if shell.mixer {
            let size = mixer::outer_size(decks.len(), mixer::SCALE);
            flex::pane(t, leaf().w(size.x).min_h(size.y), |ui| {
                MixerPanel {
                    decks: &decks,
                    scale: mixer::SCALE,
                    height: ui.max_rect().height(),
                }
                .ui(ui, cx)
            });
        }
        column(t, Side::Right, panels, cx);
    });
}
