//! Perform's effect units, beside the mixer: a column per unit, top to bottom its
//! effect, its length in beats (◀ ▶), its level, what it's on (a deck or the main
//! mix) and its on button. Each column takes the colour of the deck it's on.
//!
//! Like the mixer, every control is bound to its entry in the control registry, so
//! keyboard and MIDI moves show here.

use earshot_engine::{EngineSnapshot, FX_UNITS, FxTarget, FxUnitState};
use egui::{Align2, FontId, Rect, Response, Sense, Ui, pos2, vec2};

use super::mixer::{FADER_BOTTOM, HEIGHT, KNOB, PFL_H, ROW, bound};
use crate::core::bus::{Control, FxControl, deck_letter};
use crate::core::command::Command;
use crate::ui::panels::{Cx, Panel};
use crate::ui::theme::THEME;
use crate::ui::widgets::{HwButton, Knob, held};

/// One unit's column.
const COL: f32 = 46.0;
const BUTTON: f32 = COL - 4.0;
/// The ◀ and ▶ buttons.
const STEP: f32 = 13.0;

/// The panel's width, inside its frame.
pub const WIDTH: f32 = FX_UNITS as f32 * COL;

/// A length in beats as the units show it: `1/4`, `1/2`, `2`.
fn fmt_beats(b: f64) -> String {
    if b >= 1.0 {
        format!("{b}")
    } else {
        format!("1/{}", (1.0 / b).round())
    }
}

pub fn show(ui: &mut Ui, shared: Option<&EngineSnapshot>, out: &mut Vec<Command>) {
    let (area, _) = ui.allocate_exact_size(vec2(WIDTH, HEIGHT), Sense::hover());
    for unit in 0..FX_UNITS {
        let col = Rect::from_min_size(
            area.min + vec2(unit as f32 * COL, 0.0),
            vec2(COL, FADER_BOTTOM),
        );
        column(ui, shared, unit, col, out);
        if unit > 0 {
            ui.painter().line_segment(
                [col.left_top(), col.left_bottom()],
                egui::Stroke::new(1.0_f32, THEME.line),
            );
        }
    }
}

fn column(
    ui: &mut Ui,
    shared: Option<&EngineSnapshot>,
    unit: usize,
    col: Rect,
    out: &mut Vec<Command>,
) {
    let state = shared.map_or(FxUnitState::default_for(unit), |s| s.mixer.fx[unit]);
    let busy = shared.is_some_and(|s| s.mixer.fx_busy[unit]);
    let colour = match state.target {
        FxTarget::Deck(d) => THEME.deck_colour(d),
        FxTarget::Master => THEME.accent,
    };
    let fx = |c| Control::Fx(unit, c);
    let x = col.left() + (COL - BUTTON) / 2.0;
    let rect = |dx: f32, y: f32, w: f32, h: f32| {
        Rect::from_min_size(pos2(x + dx, col.top() + y), vec2(w, h))
    };
    let press = |ui: &Ui, r: &Response, c: FxControl, out: &mut Vec<Command>| {
        if held(ui, r) == Some(true) {
            out.extend(fx(c).command(1.0));
        }
    };
    let caption = |ui: &Ui, y: f32, text: &str, size: f32| {
        ui.painter().text(
            pos2(col.center().x, col.top() + y),
            Align2::CENTER_CENTER,
            text,
            FontId::proportional(size),
            THEME.muted,
        );
    };

    // Row 1: the unit's name over its effect.
    caption(ui, 7.0, &format!("FX{}", unit + 1), 9.0);
    let name = state.kind.name().to_uppercase();
    let r = ui
        .put(
            rect(0.0, 16.0, BUTTON, PFL_H),
            HwButton::new(&name, colour, vec2(BUTTON, PFL_H))
                .font(9.0)
                .tinted(true),
        )
        .on_hover_text("The effect. Click for the next one");
    press(ui, &r, FxControl::Next, out);

    // Row 2: ◀ length ▶.
    let y = ROW + 11.0;
    caption(ui, y - 5.0, "BEATS", 7.0);
    let r = ui
        .put(
            rect(0.0, y, STEP, PFL_H),
            HwButton::new("◀", colour, vec2(STEP, PFL_H)).font(8.0),
        )
        .on_hover_text("Halve the length: the echo's repeats, the Trans cuts come twice as often");
    press(ui, &r, FxControl::BeatsDown, out);
    let r = ui
        .put(
            rect(BUTTON - STEP, y, STEP, PFL_H),
            HwButton::new("▶", colour, vec2(STEP, PFL_H)).font(8.0),
        )
        .on_hover_text("Double the length");
    press(ui, &r, FxControl::BeatsUp, out);
    ui.painter().text(
        pos2(col.center().x, col.top() + y + PFL_H / 2.0),
        Align2::CENTER_CENTER,
        fmt_beats(state.beats),
        FontId::proportional(10.0),
        THEME.text,
    );

    // Row 3: level.
    bound(ui, shared, fx(FxControl::Level), out, |ui, range, v| {
        ui.put(
            Rect::from_min_size(pos2(col.left(), col.top() + 2.0 * ROW), vec2(COL, ROW)),
            Knob::new(range, v)
                .diameter(KNOB)
                .label("LEVEL")
                .colour(colour),
        )
    });

    // Row 4: what it's on, and on / off.
    let target = match state.target {
        FxTarget::Deck(d) => deck_letter(d).map_or("?".to_string(), |l| l.to_string()),
        FxTarget::Master => "MAIN".to_string(),
    };
    let r = ui
        .put(
            rect(0.0, 3.0 * ROW, BUTTON, 16.0),
            HwButton::new(&target, colour, vec2(BUTTON, 16.0))
                .font(8.0)
                .tinted(true),
        )
        .on_hover_text("What the effect is on: a deck, or the main mix. Click for the next");
    press(ui, &r, FxControl::Target, out);
    let r = ui
        .put(
            rect(0.0, 3.0 * ROW + 19.0, BUTTON, PFL_H),
            HwButton::new("ON", colour, vec2(BUTTON, PFL_H))
                .font(9.0)
                .lit(state.enabled)
                .tinted(busy),
        )
        .on_hover_text("Effect on / off. An echo's repeats ring on after it's off");
    press(ui, &r, FxControl::On, out);
}

/// The effect units, in their frame.
pub struct FxPanel;

impl FxPanel {
    /// Its width, frame included.
    pub const WIDTH: f32 = WIDTH + 16.0;
}

impl Panel for FxPanel {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        crate::ui::theme::normal_text(ui);
        cx.theme.panel_frame().show(ui, |ui| {
            ui.set_width(WIDTH);
            show(ui, cx.engine.snap.as_ref(), cx.out);
        });
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn beat_lengths_read_as_fractions() {
        use super::fmt_beats as f;
        assert_eq!(f(1.0 / 16.0), "1/16");
        assert_eq!(f(0.5), "1/2");
        assert_eq!(f(1.0), "1");
        assert_eq!(f(16.0), "16");
    }
}
