//! The mixer between the decks, laid out like Mixxx's: each channel has gain, high /
//! mid / low EQ and filter knobs, a headphone cue button and a fader with a level
//! meter; the main meters and the crossfader sit in the middle, main and headphone
//! knobs on the right. Channels on the crossfader's left side sit left of the middle,
//! the others mirrored on the right.
//!
//! Every control is bound to its entry in the control registry: it shows the engine's
//! current value (so keyboard and MIDI moves show) and sends the registry's
//! [`Command`] when changed.

use earshot_engine::loudness::TARGET_LUFS;
use earshot_engine::{DeckId, EQ_HIGH, EQ_LOW, EQ_MID, EngineSnapshot, MasterParam, XfSide};
use egui::{Rect, Response, Sense, Ui, vec2};

use super::controls::{Fader, HwButton, Knob, Meter, Range, held};
use super::util;
use crate::core::bus::{Control, DeckControl};
use crate::core::command::Command;

// Layout, in points from the mixer's top left. Columns from the left: deck A's knobs,
// its gain / cue / fader, its meter, the main meters, then deck B mirrored, then the
// main and headphone knobs.
const KNOB_COL: f32 = 34.0;
const FADER_COL: f32 = 30.0;
const VU_W: f32 = 7.0;
const CENTRE_W: f32 = 30.0;
const MASTER_GAP: f32 = 10.0;
const ROW: f32 = 40.0;
const KNOB: f32 = 24.0;
/// Headphone cue buttons, under the gain knobs.
const PFL_TOP: f32 = ROW + 2.0;
const PFL_H: f32 = 18.0;
/// Channel faders and meters.
const FADER_TOP: f32 = PFL_TOP + PFL_H + 6.0;
const FADER_BOTTOM: f32 = 4.0 * ROW;
const XF_TOP: f32 = FADER_BOTTOM + 6.0;
const XF_H: f32 = 20.0;
const STRIP_W: f32 = KNOB_COL + FADER_COL + VU_W;
pub const HEIGHT: f32 = XF_TOP + XF_H;

/// The mixer's width for these channels.
pub fn width(decks: &[DeckId]) -> f32 {
    channels_w(decks.len()) + MASTER_GAP + KNOB_COL
}

fn channels_w(n: usize) -> f32 {
    n as f32 * STRIP_W + CENTRE_W
}

/// Which side of the middle a deck's channel (and the deck itself) sits: the side of
/// the crossfader it starts on, so decks 1 and 3 left, 2 and 4 right.
pub fn on_left(deck: DeckId) -> bool {
    XfSide::default_for(deck) == XfSide::Left
}

/// A knob or fader bound to `control`: shows the engine's value (the default without
/// an engine) and sends what the user changes.
fn bound(
    ui: &mut Ui,
    shared: Option<&EngineSnapshot>,
    control: Control,
    out: &mut Vec<Command>,
    add: impl FnOnce(&mut Ui, &Range, &mut f64) -> Response,
) -> Response {
    let range = control.range().expect("a knob or fader");
    let mut v = shared.and_then(|s| control.get(s)).unwrap_or(range.default);
    let r = add(ui, &range, &mut v);
    if r.changed() {
        out.extend(control.command(v));
    }
    r
}

/// `decks`: the channels, in order.
pub fn show(
    ui: &mut Ui,
    shared: Option<&EngineSnapshot>,
    decks: &[DeckId],
    out: &mut Vec<Command>,
) {
    let width = width(decks);
    let (area, _) = ui.allocate_exact_size(vec2(width, HEIGHT), Sense::hover());
    let at =
        |x: f32, y: f32, w: f32, h: f32| Rect::from_min_size(area.min + vec2(x, y), vec2(w, h));
    let row = |x: f32, r: usize| at(x, r as f32 * ROW, KNOB_COL, ROW);
    let left: Vec<DeckId> = decks.iter().copied().filter(|d| on_left(*d)).collect();
    let right: Vec<DeckId> = decks.iter().copied().filter(|d| !on_left(*d)).collect();
    let main_x = left.len() as f32 * STRIP_W;
    let channels_w = channels_w(decks.len());

    let strips = left
        .iter()
        .enumerate()
        .map(|(i, d)| (*d, i as f32 * STRIP_W, true))
        .chain(
            right
                .iter()
                .enumerate()
                .map(|(i, d)| (*d, main_x + CENTRE_W + i as f32 * STRIP_W, false)),
        );
    for (deck, base, left_side) in strips {
        let colour = util::deck_colour(deck);
        let ch = |c| Control::Deck(deck, c);
        // Knobs on the outside, then the fader, then the meter by the middle.
        let (knob_x, fader_x, vu_x) = if left_side {
            (base, base + KNOB_COL, base + KNOB_COL + FADER_COL)
        } else {
            (base + VU_W + FADER_COL, base + VU_W, base)
        };
        // High, mid, low, filter down the outside, like Mixxx.
        for (r, band, label) in [(0, EQ_HIGH, "HIGH"), (1, EQ_MID, "MID"), (2, EQ_LOW, "LOW")] {
            bound(
                ui,
                shared,
                ch(DeckControl::Eq(band)),
                out,
                |ui, range, v| {
                    ui.put(
                        row(knob_x, r),
                        Knob::new(range, v)
                            .diameter(KNOB)
                            .label(label)
                            .colour(colour),
                    )
                },
            );
        }
        bound(ui, shared, ch(DeckControl::Filter), out, |ui, range, v| {
            ui.put(
                row(knob_x, 3),
                Knob::new(range, v)
                    .diameter(KNOB)
                    .label("FILTER")
                    .colour(colour),
            )
        });
        // Gain over the headphone cue button over the fader.
        let r = bound(ui, shared, ch(DeckControl::Trim), out, |ui, range, v| {
            ui.put(
                at(fader_x, 0.0, FADER_COL, ROW),
                Knob::new(range, v)
                    .diameter(KNOB)
                    .label("GAIN")
                    .colour(colour),
            )
        });
        if r.hovered() {
            let auto = shared.map_or(1.0, |s| s.deck(deck).auto_gain);
            r.on_hover_text(format!(
                "Auto-gain for this track: {:+.1} dB (brings it to {:.0} LUFS)",
                20.0 * auto.log10(),
                TARGET_LUFS
            ));
        }
        let pfl = shared.is_some_and(|s| s.deck(deck).pfl);
        let r = ui
            .put(
                at(fader_x + 2.0, PFL_TOP, FADER_COL - 4.0, PFL_H),
                HwButton::new("CUE", colour, vec2(FADER_COL - 4.0, PFL_H))
                    .font(9.0)
                    .lit(pfl),
            )
            .on_hover_text("Headphone cue (PFL)");
        if held(ui, &r) == Some(true) {
            out.extend(ch(DeckControl::Pfl).command(1.0));
        }
        let fader_rect = at(fader_x, FADER_TOP, FADER_COL, FADER_BOTTOM - FADER_TOP);
        bound(ui, shared, ch(DeckControl::Volume), out, |ui, range, v| {
            ui.put(
                fader_rect,
                Fader::vertical(range, v, vec2(FADER_COL - 8.0, fader_rect.height()))
                    .colour(colour),
            )
        });
        let level = shared.map_or(0.0, |s| s.deck(deck).vu);
        ui.put(
            at(vu_x, FADER_TOP, VU_W, FADER_BOTTOM - FADER_TOP),
            Meter::new(level, vec2(VU_W - 2.0, FADER_BOTTOM - FADER_TOP)),
        );
    }

    // Main meters, left and right, between the channels.
    for side in 0..2 {
        let level = shared.map_or(0.0, |s| s.mixer.vu_main[side]);
        ui.put(
            at(
                main_x + CENTRE_W / 2.0 - VU_W + side as f32 * VU_W,
                FADER_TOP,
                VU_W,
                FADER_BOTTOM - FADER_TOP,
            ),
            Meter::new(level, vec2(VU_W - 1.0, FADER_BOTTOM - FADER_TOP)),
        );
    }
    // "MAIN", or a red "LIMIT" while (and a moment after) the limiter turns the
    // master down by more than a little.
    let main_label = at(main_x, PFL_TOP, CENTRE_W, PFL_H);
    let now = ui.input(|i| i.time);
    let held = egui::Id::new("earshot-limit-held");
    if shared.is_some_and(|s| s.mixer.limiting > 0.5) {
        ui.ctx().data_mut(|d| d.insert_temp(held, now));
    }
    let limiting = ui
        .ctx()
        .data(|d| d.get_temp::<f64>(held))
        .is_some_and(|t| now - t < 1.0);
    let r = ui.interact(main_label, ui.id().with("main-label"), Sense::hover());
    r.on_hover_text(
        "The master limiter keeps the output under full scale; LIMIT lights while it's working",
    );
    ui.painter().text(
        main_label.center(),
        egui::Align2::CENTER_CENTER,
        if limiting { "LIMIT" } else { "MAIN" },
        egui::FontId::proportional(9.0),
        if limiting { util::BAD } else { util::MUTED },
    );

    let xf_w = channels_w - 2.0 * KNOB_COL;
    let crossfader = Control::Master(MasterParam::Crossfader);
    bound(ui, shared, crossfader, out, |ui, range, v| {
        ui.put(
            at(KNOB_COL, XF_TOP, xf_w, XF_H),
            Fader::horizontal(range, v, vec2(xf_w, XF_H)).ticks(9),
        )
    });

    // Main and headphone knobs on the right.
    let mx = channels_w + MASTER_GAP;
    for (r, (param, label)) in [
        (MasterParam::Gain, "MAIN"),
        (MasterParam::Balance, "BAL"),
        (MasterParam::HeadGain, "HEAD"),
        (MasterParam::HeadMix, "MIX"),
    ]
    .into_iter()
    .enumerate()
    {
        bound(ui, shared, Control::Master(param), out, |ui, range, v| {
            ui.put(row(mx, r), Knob::new(range, v).diameter(KNOB).label(label))
        });
    }
    // Separator between the channels and the main section.
    let x = area.left() + channels_w + MASTER_GAP / 2.0;
    ui.painter().line_segment(
        [
            egui::pos2(x, area.top()),
            egui::pos2(x, area.top() + FADER_BOTTOM),
        ],
        egui::Stroke::new(1.0_f32, util::LINE),
    );
}
