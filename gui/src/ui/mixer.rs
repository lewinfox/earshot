//! The mixer between the decks, laid out like Mixxx's: each channel has gain, high /
//! mid / low EQ and filter knobs, a headphone cue button and a fader with a level
//! meter; the main meters and the crossfader sit in the middle, main and headphone
//! knobs on the right.
//!
//! Every control shows the engine's current value (so keyboard and MIDI moves show)
//! and sends changes through the control bus as an [`Action`].

use std::sync::atomic::Ordering::Relaxed;

use earshot_engine::Shared;
use earshot_engine::loudness::TARGET_LUFS;
use egui::{Rect, Response, Sense, Ui, vec2};

use super::controls::{
    Fader, HwButton, Knob, Meter, Range, fmt_balance, fmt_db, fmt_filter, fmt_percent,
};
use super::deck::Action;
use super::util;
use crate::core::controls::{CHANNELS, EQ_GROUPS, FILTER_GROUPS, value};

pub const GAIN: Range = Range {
    name: "Gain",
    min: 0.0,
    max: 4.0,
    default: 1.0,
    centre: Some(1.0),
    format: fmt_db,
};
/// Low, mid, high: Mixxx's `parameter1..3`.
pub const EQ: [Range; 3] = [
    Range {
        name: "Low",
        ..GAIN
    },
    Range {
        name: "Mid",
        ..GAIN
    },
    Range {
        name: "High",
        ..GAIN
    },
];
const EQ_KEYS: [&str; 3] = ["parameter1", "parameter2", "parameter3"];
pub const FILTER: Range = Range {
    name: "Filter",
    min: 0.0,
    max: 1.0,
    default: 0.5,
    centre: Some(0.5),
    format: fmt_filter,
};
pub const VOLUME: Range = Range {
    name: "Volume",
    min: 0.0,
    max: 1.0,
    default: 1.0,
    centre: None,
    format: fmt_percent,
};
pub const CROSSFADER: Range = Range {
    name: "Crossfader",
    min: -1.0,
    max: 1.0,
    default: 0.0,
    centre: Some(0.0),
    format: fmt_balance,
};
pub const MAIN_GAIN: Range = Range {
    name: "Main level",
    min: 0.0,
    max: 5.0,
    default: 1.0,
    centre: Some(1.0),
    format: fmt_db,
};
pub const BALANCE: Range = Range {
    name: "Balance",
    ..CROSSFADER
};
pub const HEAD_GAIN: Range = Range {
    name: "Headphone level",
    ..MAIN_GAIN
};
pub const HEAD_MIX: Range = Range {
    name: "Headphone mix",
    min: -1.0,
    max: 1.0,
    default: -1.0,
    centre: None,
    format: |v| {
        format!(
            "{:.0} % cue, {:.0} % main",
            (1.0 - v) * 50.0,
            (v + 1.0) * 50.0
        )
    },
};

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
const CHANNELS_W: f32 = 2.0 * (KNOB_COL + FADER_COL + VU_W) + CENTRE_W;
pub const WIDTH: f32 = CHANNELS_W + MASTER_GAP + KNOB_COL;
pub const HEIGHT: f32 = XF_TOP + XF_H;

/// One control bound to `group key`: shows the engine's value (the default without
/// an engine) and sends what the user changes.
fn bound(
    ui: &mut Ui,
    shared: Option<&Shared>,
    (group, key): (&'static str, &'static str),
    default: f64,
    actions: &mut Vec<Action>,
    add: impl FnOnce(&mut Ui, &mut f64) -> Response,
) {
    let mut v = shared.and_then(|s| value(s, group, key)).unwrap_or(default);
    if add(ui, &mut v).changed() {
        actions.push(Action::Control {
            group,
            key: key.into(),
            value: v,
        });
    }
}

pub fn show(ui: &mut Ui, shared: Option<&Shared>, actions: &mut Vec<Action>) {
    let (area, _) = ui.allocate_exact_size(vec2(WIDTH, HEIGHT), Sense::hover());
    let at =
        |x: f32, y: f32, w: f32, h: f32| Rect::from_min_size(area.min + vec2(x, y), vec2(w, h));
    let row = |x: f32, r: usize| at(x, r as f32 * ROW, KNOB_COL, ROW);

    for d in 0..2 {
        let group = CHANNELS[d];
        let colour = util::DECK[d];
        // Deck A's knobs on the far left; deck B's mirrored on the right.
        let (knob_x, fader_x, vu_x) = if d == 0 {
            (0.0, KNOB_COL, KNOB_COL + FADER_COL)
        } else {
            let base = KNOB_COL + FADER_COL + VU_W + CENTRE_W;
            (base + VU_W + FADER_COL, base + VU_W, base)
        };
        // High, mid, low, filter down the outside, like Mixxx.
        for (r, band) in [(0, 2), (1, 1), (2, 0)] {
            let label = ["LOW", "MID", "HIGH"][band];
            bound(
                ui,
                shared,
                (EQ_GROUPS[d], EQ_KEYS[band]),
                1.0,
                actions,
                |ui, v| {
                    ui.put(
                        row(knob_x, r),
                        Knob::new(&EQ[band], v)
                            .diameter(KNOB)
                            .label(label)
                            .colour(colour),
                    )
                },
            );
        }
        bound(
            ui,
            shared,
            (FILTER_GROUPS[d], "super1"),
            FILTER.default,
            actions,
            |ui, v| {
                ui.put(
                    row(knob_x, 3),
                    Knob::new(&FILTER, v)
                        .diameter(KNOB)
                        .label("FILTER")
                        .colour(colour),
                )
            },
        );
        // Gain over the headphone cue button over the fader.
        bound(
            ui,
            shared,
            (group, "pregain"),
            GAIN.default,
            actions,
            |ui, v| {
                let auto = shared.map_or(1.0, |s| s.decks[d].auto_gain.get());
                ui.put(
                    at(fader_x, 0.0, FADER_COL, ROW),
                    Knob::new(&GAIN, v)
                        .diameter(KNOB)
                        .label("GAIN")
                        .colour(colour),
                )
                .on_hover_text(format!(
                    "Auto-gain for this track: {:+.1} dB (brings it to {:.0} LUFS)",
                    20.0 * auto.log10(),
                    TARGET_LUFS
                ))
            },
        );
        let pfl = shared.is_some_and(|s| s.decks[d].pfl.load(Relaxed));
        let r = ui
            .put(
                at(fader_x + 2.0, PFL_TOP, FADER_COL - 4.0, PFL_H),
                HwButton::new("CUE", colour, vec2(FADER_COL - 4.0, PFL_H))
                    .font(9.0)
                    .lit(pfl),
            )
            .on_hover_text("Headphone cue (PFL)");
        if r.clicked() {
            actions.push(Action::Control {
                group,
                key: "pfl".into(),
                value: if pfl { 0.0 } else { 1.0 },
            });
        }
        let fader_rect = at(fader_x, FADER_TOP, FADER_COL, FADER_BOTTOM - FADER_TOP);
        bound(
            ui,
            shared,
            (group, "volume"),
            VOLUME.default,
            actions,
            |ui, v| {
                ui.put(
                    fader_rect,
                    Fader::vertical(&VOLUME, v, vec2(FADER_COL - 8.0, fader_rect.height()))
                        .colour(colour),
                )
            },
        );
        let level = shared.map_or(0.0, |s| s.decks[d].vu.get());
        ui.put(
            at(vu_x, FADER_TOP, VU_W, FADER_BOTTOM - FADER_TOP),
            Meter::new(level, vec2(VU_W - 2.0, FADER_BOTTOM - FADER_TOP)),
        );
    }

    // Main meters, left and right, between the channels.
    let main_x = KNOB_COL + FADER_COL + VU_W;
    for side in 0..2 {
        let level = shared.map_or(0.0, |s| s.vu_main[side].get());
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
    if shared.is_some_and(|s| s.limiting.get() > 0.5) {
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

    let xf_w = CHANNELS_W - 2.0 * KNOB_COL;
    bound(
        ui,
        shared,
        ("[Master]", "crossfader"),
        0.0,
        actions,
        |ui, v| {
            ui.put(
                at(KNOB_COL, XF_TOP, xf_w, XF_H),
                Fader::horizontal(&CROSSFADER, v, vec2(xf_w, XF_H)).ticks(9),
            )
        },
    );

    // Main and headphone knobs on the right.
    let mx = CHANNELS_W + MASTER_GAP;
    for (r, (key, range, label)) in [
        ("gain", &MAIN_GAIN, "MAIN"),
        ("balance", &BALANCE, "BAL"),
        ("headGain", &HEAD_GAIN, "HEAD"),
        ("headMix", &HEAD_MIX, "MIX"),
    ]
    .into_iter()
    .enumerate()
    {
        bound(
            ui,
            shared,
            ("[Master]", key),
            range.default,
            actions,
            |ui, v| ui.put(row(mx, r), Knob::new(range, v).diameter(KNOB).label(label)),
        );
    }
    // Separator between the channels and the main section.
    let x = area.left() + CHANNELS_W + MASTER_GAP / 2.0;
    ui.painter().line_segment(
        [
            egui::pos2(x, area.top()),
            egui::pos2(x, area.top() + FADER_BOTTOM),
        ],
        egui::Stroke::new(1.0_f32, util::LINE),
    );
}
