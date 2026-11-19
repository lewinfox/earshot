//! The mixer between the decks, laid out like Mixxx's: each channel has gain, high /
//! mid / low EQ and filter knobs, a headphone cue button, a fader with a level
//! meter and its crossfader side (L, THRU or R) under the fader; the master meters and
//! the crossfader sit in the middle, master and headphone knobs in a row across the
//! top. The faders and meters stretch to fill the height it's given. Channels
//! on the crossfader's left side (decks 1 and 3) sit left of the middle, the others
//! mirrored on the right. Two channels or four, as many as decks are shown.
//!
//! Every control is bound to its entry in the control registry: it shows the engine's
//! current value (so keyboard and MIDI moves show) and sends the registry's
//! [`Command`] when changed.

use earshot_engine::loudness::TARGET_LUFS;
use earshot_engine::{
    DeckCmd as EngineCmd, DeckId, EQ_HIGH, EQ_LOW, EQ_MID, EngineSnapshot, MasterParam, XfSide,
};
use egui::{Rect, Response, Sense, Ui, vec2};

use crate::core::bus::{Control, DeckControl};
use crate::core::command::{Command, DeckCmd};
use crate::ui::panels::{Cx, Panel};
use crate::ui::theme::THEME;
use crate::ui::widgets::{Fader, HwButton, Knob, Meter, Range, held};

// Layout, in points from the channels' top left (under the master and headphone
// knobs' row). Columns from the left: deck A's knobs, its gain / cue / fader, its
// meter, the master meters, then deck B mirrored.
const KNOB_COL: f32 = 34.0;
const FADER_COL: f32 = 30.0;
const VU_W: f32 = 7.0;
const CENTRE_W: f32 = 30.0;
pub(super) const ROW: f32 = 40.0;
pub(super) const KNOB: f32 = 24.0;
/// The master and headphone knobs' row, over the channels, and the gap under it.
const MASTER_H: f32 = ROW + 2.0;
/// Headphone cue buttons, under the gain knobs.
const PFL_TOP: f32 = ROW + 2.0;
pub(super) const PFL_H: f32 = 18.0;
/// The master meters' label ("MASTER" / "LIMIT"), over them.
const MASTER_LABEL_H: f32 = 12.0;
/// Channel faders and meters, down to (at the least) the fourth knob row's bottom.
const FADER_TOP: f32 = PFL_TOP + PFL_H + 6.0;
const MIN_FADER_BOTTOM: f32 = 4.0 * ROW;
/// The crossfader, under the faders.
const XF_GAP: f32 = 6.0;
const XF_H: f32 = 20.0;
const STRIP_W: f32 = KNOB_COL + FADER_COL + VU_W;
/// The least height it needs at its design size (it fills more if given it).
const HEIGHT: f32 = MASTER_H + MIN_FADER_BOTTOM + XF_GAP + XF_H;
/// Its frame's margins and border round all that, across and down.
const FRAME: egui::Vec2 = vec2(16.0, 12.0);

/// The mixer's sizes at a scale: the design sizes above, times it. The layout picks
/// the scale (`ui::layout::shell`), so the mixer is as tall as the decks beside it.
struct Dims {
    s: f32,
    knob_col: f32,
    fader_col: f32,
    vu_w: f32,
    centre_w: f32,
    row: f32,
    knob: f32,
    master_h: f32,
    pfl_top: f32,
    pfl_h: f32,
    master_label_h: f32,
    fader_top: f32,
    xf_gap: f32,
    xf_h: f32,
    strip_w: f32,
}

impl Dims {
    fn at(s: f32) -> Dims {
        Dims {
            s,
            knob_col: KNOB_COL * s,
            fader_col: FADER_COL * s,
            vu_w: VU_W * s,
            centre_w: CENTRE_W * s,
            row: ROW * s,
            knob: KNOB * s,
            master_h: MASTER_H * s,
            pfl_top: PFL_TOP * s,
            pfl_h: PFL_H * s,
            master_label_h: MASTER_LABEL_H * s,
            fader_top: FADER_TOP * s,
            xf_gap: XF_GAP * s,
            xf_h: XF_H * s,
            strip_w: STRIP_W * s,
        }
    }

    /// A caption's size: scaled, but never too small to read.
    fn font(&self, size: f32) -> f32 {
        (size * self.s).max(7.0)
    }

    fn channels_w(&self, n: usize) -> f32 {
        channels_w(n) * self.s
    }
}

/// The mixer's size for this many channels at `scale`, with its frame.
pub fn outer_size(decks: usize, scale: f32) -> egui::Vec2 {
    vec2(channels_w(decks), HEIGHT) * scale + FRAME
}

fn channels_w(n: usize) -> f32 {
    n as f32 * STRIP_W + CENTRE_W
}

/// Which side of the middle a deck's channel (and the deck itself) sits: the side of
/// the crossfader it starts on, so decks 1 and 3 left, 2 and 4 right.
pub fn on_left(deck: DeckId) -> bool {
    XfSide::default_for(deck) == XfSide::Left
}

fn shown_id(control: Control) -> egui::Id {
    egui::Id::new(("display-only", control.name().0))
}

/// A display-only knob on the controller is at `value`: [`bound`] shows that.
/// What the controller shows for a display-only control, if it has sent any.
pub fn shown(ctx: &egui::Context, control: Control) -> Option<f64> {
    ctx.data(|d| d.get_temp(shown_id(control)))
}

pub fn set_shown(ctx: &egui::Context, control: Control, value: f64) {
    ctx.data_mut(|d| d.insert_temp(shown_id(control), value));
    ctx.request_repaint();
}

fn waiting_id(control: Control) -> egui::Id {
    egui::Id::new(("takeover-waiting", control.name().0))
}

/// Where the controller's knob for `control` is, while the control waits for it to
/// reach its value (soft takeover after a hand switches decks: `core::midi::Route`).
pub fn waiting(ctx: &egui::Context, control: Control) -> Option<f64> {
    ctx.data(|d| d.get_temp(waiting_id(control)))
}

/// Mark (or with `None`, stop marking) where the controller's knob for `control` is.
pub fn set_waiting(ctx: &egui::Context, control: Control, at: Option<f64>) {
    ctx.data_mut(|d| match at {
        Some(v) => d.insert_temp(waiting_id(control), v),
        None => d.remove::<f64>(waiting_id(control)),
    });
    ctx.request_repaint();
}

/// The controls of a channel that take over softly, for `deck`.
pub fn takeover_controls(deck: DeckId) -> impl Iterator<Item = Control> {
    [
        DeckControl::Volume,
        DeckControl::Trim,
        DeckControl::Filter,
        DeckControl::Eq(EQ_LOW),
        DeckControl::Eq(EQ_MID),
        DeckControl::Eq(EQ_HIGH),
    ]
    .into_iter()
    .map(move |c| Control::Deck(deck, c))
}

/// A knob or fader bound to `control`: shows the engine's value (the default without
/// an engine) and sends what the user changes. One the controller acts on itself (a
/// display-only mapping) shows the controller's knob, outlined, and can't be moved
/// here.
pub(super) fn bound(
    ui: &mut Ui,
    shared: Option<&EngineSnapshot>,
    control: Control,
    out: &mut Vec<Command>,
    add: impl FnOnce(&mut Ui, &Range, &mut f64) -> Response,
) -> Response {
    let range = control.range().expect("a knob or fader");
    let shown: Option<f64> = ui.ctx().data(|d| d.get_temp(shown_id(control)));
    let mut v = shown
        .or_else(|| shared.and_then(|s| control.get(s)))
        .unwrap_or(range.default);
    let r = add(ui, &range, &mut v);
    if shown.is_some() {
        ui.painter().rect_stroke(
            r.rect.expand(1.0),
            3.0,
            egui::Stroke::new(1.0_f32, THEME.muted),
            egui::StrokeKind::Outside,
        );
        return r.on_hover_text("Set on the controller (display only): earshot shows it");
    }
    if r.changed() {
        out.extend(control.command(v));
    }
    r
}

/// `decks`: the channels, in order. `scale`: the size to draw at. `height`: what to
/// fill (at least its own height at that scale).
pub fn show(
    ui: &mut Ui,
    shared: Option<&EngineSnapshot>,
    decks: &[DeckId],
    out: &mut Vec<Command>,
    scale: f32,
    height: f32,
) {
    let d = Dims::at(scale);
    let width = d.channels_w(decks.len());
    let height = height.max(HEIGHT * scale);
    let (area, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    // The faders take the height to spare.
    let fader_bottom = height - d.master_h - d.xf_gap - d.xf_h;
    let xf_side_top = fader_bottom - d.pfl_h;
    let xf_top = fader_bottom + d.xf_gap;
    let top = area.min + vec2(0.0, d.master_h);
    let at = |x: f32, y: f32, w: f32, h: f32| Rect::from_min_size(top + vec2(x, y), vec2(w, h));
    let row = |x: f32, r: usize| at(x, r as f32 * d.row, d.knob_col, d.row);
    let left: Vec<DeckId> = decks.iter().copied().filter(|d| on_left(*d)).collect();
    let right: Vec<DeckId> = decks.iter().copied().filter(|d| !on_left(*d)).collect();
    let main_x = left.len() as f32 * d.strip_w;
    let channels_w = d.channels_w(decks.len());

    let strips = left
        .iter()
        .enumerate()
        .map(|(i, deck)| (*deck, i as f32 * d.strip_w, true))
        .chain(
            right
                .iter()
                .enumerate()
                .map(|(i, deck)| (*deck, main_x + d.centre_w + i as f32 * d.strip_w, false)),
        );
    for (deck, base, left_side) in strips {
        let colour = THEME.deck_colour(deck);
        let ch = |c| Control::Deck(deck, c);
        // Knobs on the outside, then the fader, then the meter by the middle.
        let (knob_x, fader_x, vu_x) = if left_side {
            (base, base + d.knob_col, base + d.knob_col + d.fader_col)
        } else {
            (base + d.vu_w + d.fader_col, base + d.vu_w, base)
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
                            .diameter(d.knob)
                            .label(label)
                            .colour(colour)
                            .target(waiting(ui.ctx(), ch(DeckControl::Eq(band)))),
                    )
                },
            );
        }
        bound(ui, shared, ch(DeckControl::Filter), out, |ui, range, v| {
            ui.put(
                row(knob_x, 3),
                Knob::new(range, v)
                    .diameter(d.knob)
                    .label("FILTER")
                    .colour(colour)
                    .target(waiting(ui.ctx(), ch(DeckControl::Filter))),
            )
        });
        // Gain over the headphone cue button over the fader.
        let r = bound(ui, shared, ch(DeckControl::Trim), out, |ui, range, v| {
            ui.put(
                at(fader_x, 0.0, d.fader_col, d.row),
                Knob::new(range, v)
                    .diameter(d.knob)
                    .label("GAIN")
                    .colour(colour)
                    .target(waiting(ui.ctx(), ch(DeckControl::Trim))),
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
                at(fader_x + 2.0, d.pfl_top, d.fader_col - 4.0, d.pfl_h),
                HwButton::new("CUE", colour, vec2(d.fader_col - 4.0, d.pfl_h))
                    .font(d.font(9.0))
                    .lit(pfl),
            )
            .on_hover_text("Headphone cue (PFL)");
        if held(ui, &r) == Some(true) {
            out.extend(ch(DeckControl::Pfl).command(1.0));
        }
        let fader_rect = at(
            fader_x,
            d.fader_top,
            d.fader_col,
            xf_side_top - 2.0 - d.fader_top,
        );
        bound(ui, shared, ch(DeckControl::Volume), out, |ui, range, v| {
            ui.put(
                fader_rect,
                Fader::vertical(range, v, vec2(d.fader_col - 8.0, fader_rect.height()))
                    .colour(colour)
                    .target(waiting(ui.ctx(), ch(DeckControl::Volume))),
            )
        });
        // Which side of the crossfader the channel is on: click for the next.
        let side = shared.map_or(XfSide::default_for(deck), |s| s.deck(deck).crossfader);
        let (label, tip) = match side {
            XfSide::Left => ("L", "On the crossfader's left"),
            XfSide::Thru => ("THRU", "Not on the crossfader: always heard"),
            XfSide::Right => ("R", "On the crossfader's right"),
        };
        let r = ui
            .put(
                at(fader_x + 2.0, xf_side_top, d.fader_col - 4.0, d.pfl_h),
                HwButton::new(label, colour, vec2(d.fader_col - 4.0, d.pfl_h))
                    .font(d.font(8.0))
                    .lit(side != XfSide::Thru),
            )
            .on_hover_text(format!("{tip}. Click: L, THRU, R"));
        if held(ui, &r) == Some(true) {
            let next = match side {
                XfSide::Left => XfSide::Thru,
                XfSide::Thru => XfSide::Right,
                XfSide::Right => XfSide::Left,
            };
            out.push(Command::Deck(
                deck,
                DeckCmd::Engine(EngineCmd::Crossfader(next)),
            ));
        }
        let level = shared.map_or(0.0, |s| s.deck(deck).vu);
        ui.put(
            at(vu_x, d.fader_top, d.vu_w, fader_bottom - d.fader_top),
            Meter::new(level, vec2(d.vu_w - 2.0, fader_bottom - d.fader_top)),
        );
    }

    // Master cue, level with the channels' CUE buttons: the master mix in the
    // headphones.
    let master_cue = Control::Master(MasterParam::MasterCue);
    // Display only: the controller's own button, shown, and not pressed here.
    let shown = shown(ui.ctx(), master_cue);
    let lit = shown.map_or_else(|| shared.is_some_and(|s| s.mixer.master_cue), |v| v > 0.5);
    let r = ui
        .put(
            at(main_x + 1.0, d.pfl_top, d.centre_w - 2.0, d.pfl_h),
            HwButton::new("CUE", THEME.accent, vec2(d.centre_w - 2.0, d.pfl_h))
                .font(d.font(9.0))
                .lit(lit),
        )
        .on_hover_text("Master cue: the master mix in the headphones");
    if held(ui, &r) == Some(true) && shown.is_none() {
        out.extend(master_cue.command(1.0));
    }
    // Master meters, left and right, between the channels, under their label.
    let meter_top = d.fader_top + d.master_label_h;
    for side in 0..2 {
        let level = shared.map_or(0.0, |s| s.mixer.vu_main[side]);
        ui.put(
            at(
                main_x + d.centre_w / 2.0 - d.vu_w + side as f32 * d.vu_w,
                meter_top,
                d.vu_w,
                fader_bottom - meter_top,
            ),
            Meter::new(level, vec2(d.vu_w - 1.0, fader_bottom - meter_top)),
        );
    }
    // "MASTER", or a red "LIMIT" while (and a moment after) the limiter turns the
    // master down by more than a little.
    let master_label = at(main_x, d.fader_top - 2.0, d.centre_w, d.master_label_h);
    let now = ui.input(|i| i.time);
    let held = egui::Id::new("earshot-limit-held");
    if shared.is_some_and(|s| s.mixer.limiting > 0.5) {
        ui.ctx().data_mut(|d| d.insert_temp(held, now));
    }
    let limiting = ui
        .ctx()
        .data(|d| d.get_temp::<f64>(held))
        .is_some_and(|t| now - t < 1.0);
    let r = ui.interact(master_label, ui.id().with("master-label"), Sense::hover());
    r.on_hover_text(
        "The master limiter keeps the output under full scale; LIMIT lights while it's working",
    );
    ui.painter().text(
        master_label.center(),
        egui::Align2::CENTER_CENTER,
        if limiting { "LIMIT" } else { "MASTER" },
        egui::FontId::proportional(d.font(9.0)),
        if limiting { THEME.bad } else { THEME.muted },
    );

    let xf_w = channels_w - 2.0 * d.knob_col;
    let crossfader = Control::Master(MasterParam::Crossfader);
    bound(ui, shared, crossfader, out, |ui, range, v| {
        ui.put(
            at(d.knob_col, xf_top, xf_w, d.xf_h),
            Fader::horizontal(range, v, vec2(xf_w, d.xf_h)).ticks(9),
        )
    });

    // Master and headphone knobs in a row across the top, spread over the width.
    let masters = [
        (MasterParam::Gain, "MASTER"),
        (MasterParam::Balance, "BAL"),
        (MasterParam::HeadGain, "HEAD"),
        (MasterParam::HeadMix, "MIX"),
    ];
    let step = channels_w / masters.len() as f32;
    for (i, (param, label)) in masters.into_iter().enumerate() {
        let x = i as f32 * step + (step - d.knob_col) / 2.0;
        let rect = Rect::from_min_size(area.min + vec2(x, 0.0), vec2(d.knob_col, d.row));
        bound(ui, shared, Control::Master(param), out, |ui, range, v| {
            ui.put(rect, Knob::new(range, v).diameter(d.knob).label(label))
        });
    }
    // Separator between them and the channels.
    let y = area.top() + d.master_h - 2.0;
    ui.painter().line_segment(
        [egui::pos2(area.left(), y), egui::pos2(area.right(), y)],
        egui::Stroke::new(1.0_f32, THEME.line),
    );
}

/// The mixer for `decks`, in its frame, drawn at `scale` and `height` tall with the
/// frame (as tall as the decks beside it): both from `ui::layout::DeckRow`.
pub struct MixerPanel<'a> {
    pub decks: &'a [DeckId],
    pub scale: f32,
    pub height: f32,
}

impl Panel for MixerPanel<'_> {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        cx.theme.panel_frame().show(ui, |ui| {
            show(
                ui,
                cx.engine.snap.as_ref(),
                self.decks,
                cx.out,
                self.scale,
                self.height - FRAME.y,
            );
        });
    }
}
