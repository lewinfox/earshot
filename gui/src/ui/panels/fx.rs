//! Perform's effect units, in a strip across the window under the decks: unit 1 on
//! the left half, unit 2 on the right. Each is a row: its effect, its length in beats
//! (◀ ▶), its level, what it's on (a deck or the main mix) and its on button, in the
//! colour of the deck it's on.
//!
//! Like the mixer, every control is bound to its entry in the control registry, so
//! keyboard and MIDI moves show here.

use earshot_engine::{EngineSnapshot, FX_UNITS, FxTarget, FxUnitState};
use egui::{Align, Layout, Response, RichText, Ui, vec2};

use super::mixer::{KNOB, PFL_H, ROW, bound};
use crate::core::bus::{Control, FxControl, deck_letter};
use crate::core::command::Command;
use crate::ui::panels::{Cx, Panel};
use crate::ui::theme::THEME;
use crate::ui::widgets::{HwButton, Knob, held};

/// The strip's height, inside its frame: a knob and its label.
pub const HEIGHT: f32 = ROW;
/// The effect's button, and what it's on.
const NAME_W: f32 = 64.0;
const TARGET_W: f32 = 48.0;
const ON_W: f32 = 40.0;
/// The ◀ and ▶ buttons, and the length between them.
const STEP: f32 = 16.0;
const BEATS_W: f32 = 32.0;

/// A length in beats as the units show it: `1/4`, `1/2`, `2`.
fn fmt_beats(b: f64) -> String {
    if b >= 1.0 {
        format!("{b}")
    } else {
        format!("1/{}", (1.0 / b).round())
    }
}

pub fn show(ui: &mut Ui, shared: Option<&EngineSnapshot>, out: &mut Vec<Command>) {
    let half = (ui.available_width() - 12.0) / FX_UNITS as f32;
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        for unit in 0..FX_UNITS {
            ui.allocate_ui_with_layout(
                vec2(half, HEIGHT),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.set_width(half);
                    unit_row(ui, shared, unit, out);
                },
            );
        }
    });
}

fn unit_row(ui: &mut Ui, shared: Option<&EngineSnapshot>, unit: usize, out: &mut Vec<Command>) {
    ui.spacing_mut().item_spacing.x = 6.0;
    let state = shared.map_or(FxUnitState::default_for(unit), |s| s.mixer.fx[unit]);
    let busy = shared.is_some_and(|s| s.mixer.fx_busy[unit]);
    let colour = match state.target {
        FxTarget::Deck(d) => THEME.deck_colour(d),
        FxTarget::Master => THEME.accent,
    };
    let fx = |c| Control::Fx(unit, c);
    let press = |ui: &Ui, r: &Response, c: FxControl, out: &mut Vec<Command>| {
        if held(ui, r) == Some(true) {
            out.extend(fx(c).command(1.0));
        }
    };
    let button = |ui: &mut Ui, text: &str, w: f32| {
        ui.add_sized(
            vec2(w, PFL_H),
            HwButton::new(text, colour, vec2(w, PFL_H)).font(9.0),
        )
    };

    ui.label(
        RichText::new(format!("FX{}", unit + 1))
            .size(10.0)
            .color(THEME.muted),
    );
    let name = state.kind.name().to_uppercase();
    let r = ui
        .add_sized(
            vec2(NAME_W, PFL_H),
            HwButton::new(&name, colour, vec2(NAME_W, PFL_H))
                .font(9.0)
                .tinted(true),
        )
        .on_hover_text("The effect. Click for the next one");
    press(ui, &r, FxControl::Next, out);

    let r = button(ui, "◀", STEP)
        .on_hover_text("Halve the length: the echo's repeats, the Trans cuts come twice as often");
    press(ui, &r, FxControl::BeatsDown, out);
    ui.add_sized(
        vec2(BEATS_W, PFL_H),
        egui::Label::new(
            RichText::new(fmt_beats(state.beats))
                .size(10.0)
                .color(THEME.text),
        ),
    )
    .on_hover_text("The length, in beats");
    let r = button(ui, "▶", STEP).on_hover_text("Double the length");
    press(ui, &r, FxControl::BeatsUp, out);

    bound(ui, shared, fx(FxControl::Level), out, |ui, range, v| {
        ui.add(
            Knob::new(range, v)
                .diameter(KNOB)
                .label("LEVEL")
                .colour(colour),
        )
    });

    let target = match state.target {
        FxTarget::Deck(d) => deck_letter(d).map_or("?".to_string(), |l| l.to_string()),
        FxTarget::Master => "MAIN".to_string(),
    };
    let r = ui
        .add_sized(
            vec2(TARGET_W, PFL_H),
            HwButton::new(&target, colour, vec2(TARGET_W, PFL_H))
                .font(9.0)
                .tinted(true),
        )
        .on_hover_text("What the effect is on: a deck, or the main mix. Click for the next");
    press(ui, &r, FxControl::Target, out);
    let r = ui
        .add_sized(
            vec2(ON_W, PFL_H),
            HwButton::new("ON", colour, vec2(ON_W, PFL_H))
                .font(9.0)
                .lit(state.enabled)
                .tinted(busy),
        )
        .on_hover_text("Effect on / off. An echo's repeats ring on after it's off");
    press(ui, &r, FxControl::On, out);
}

/// The effect units, in their frame, across the window.
pub struct FxPanel;

impl Panel for FxPanel {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        crate::ui::theme::normal_text(ui);
        cx.theme.panel_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_height(HEIGHT);
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
