//! Perform's effect units, in the top bar: a row each (one, for now). A row is its
//! effect (a dropdown), its length in beats (◀ ▶), its level, what it's on (a deck
//! or the master mix), its tempo (the track's, or tapped in), its on button, and its release (the effect that rings out
//! when it's released, and a button to release it), in the colour of the deck it's on.
//!
//! Like the mixer, every control is bound to its entry in the control registry, so
//! keyboard and MIDI moves show here.

use earshot_engine::{EngineSnapshot, FX_UNITS, FxCmd, FxKind, FxRelease, FxTarget, FxUnitState};
use egui::{Response, RichText, Ui, vec2};

use super::mixer::bound;
use crate::core::bus::{Control, FxControl, deck_letter};
use crate::core::command::Command;
use crate::ui::theme::THEME;
use crate::ui::widgets::{HwButton, Knob, held};

/// Its buttons' height, and the level knob's size: the top bar's.
const H: f32 = 18.0;
const KNOB: f32 = 16.0;
/// What the effect's on, and the on button.
const TARGET_W: f32 = 44.0;
const ON_W: f32 = 32.0;
/// The TAP button, wide enough for "TAP 124".
const TAP_W: f32 = 48.0;
/// The ◀ and ▶ buttons, and the length between them.
const STEP: f32 = 16.0;
const BEATS_W: f32 = 30.0;

/// A length in beats as the units show it: `1/4`, `1/2`, `2`.
fn fmt_beats(b: f64) -> String {
    if b >= 1.0 {
        format!("{b}")
    } else {
        format!("1/{}", (1.0 / b).round())
    }
}

/// Every effect unit's row, side by side.
pub fn show(ui: &mut Ui, shared: Option<&EngineSnapshot>, out: &mut Vec<Command>) {
    for unit in 0..FX_UNITS {
        if unit > 0 {
            ui.add_space(16.0);
        }
        unit_row(ui, shared, unit, out);
    }
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
            vec2(w, H),
            HwButton::new(text, colour, vec2(w, H)).font(9.0),
        )
    };

    // "FX", numbered once there's more than one.
    let name = if FX_UNITS > 1 {
        format!("FX{}", unit + 1)
    } else {
        "FX".into()
    };
    ui.label(RichText::new(name).size(10.0).color(THEME.muted));
    // The effect: pick one, and the unit steps to it (it only knows "next").
    let mut kind = state.kind;
    egui::ComboBox::from_id_salt(("fx-kind", unit))
        .selected_text(state.kind.name())
        .width(64.0)
        .show_ui(ui, |ui| {
            for k in FxKind::ALL {
                ui.selectable_value(&mut kind, k, k.name());
            }
        })
        .response
        .on_hover_text("The effect");
    if kind != state.kind {
        let at = |k: FxKind| FxKind::ALL.iter().position(|x| *x == k).unwrap_or(0);
        let n = FxKind::ALL.len();
        for _ in 0..(at(kind) + n - at(state.kind)) % n {
            out.extend(fx(FxControl::Next).command(1.0));
        }
    }

    let r = button(ui, "◀", STEP).on_hover_text("Halve effect length");
    press(ui, &r, FxControl::BeatsDown, out);
    ui.add_sized(
        vec2(BEATS_W, H),
        egui::Label::new(
            RichText::new(fmt_beats(state.beats))
                .size(10.0)
                .color(THEME.text),
        ),
    )
    .on_hover_text("Effect length, in beats");
    let r = button(ui, "▶", STEP).on_hover_text("Double effect length");
    press(ui, &r, FxControl::BeatsUp, out);

    // The tempo: tapped ("TAP 124", with AUTO to go back), or the track's.
    let tap_text = match state.tap_bpm {
        Some(b) => format!("TAP {b:.0}"),
        None => "TAP".into(),
    };
    let r = ui
        .add_sized(
            vec2(TAP_W, H),
            HwButton::new(&tap_text, colour, vec2(TAP_W, H))
                .font(9.0)
                .lit(state.tap_bpm.is_some()),
        )
        .on_hover_text("Tap in time to set the effect's tempo, instead of the track's");
    press(ui, &r, FxControl::TempoTap, out);
    if state.tap_bpm.is_some() {
        let r = button(ui, "AUTO", ON_W).on_hover_text("Follow the track's tempo again");
        press(ui, &r, FxControl::TempoAuto, out);
    }

    bound(ui, shared, fx(FxControl::Level), out, |ui, range, v| {
        ui.add(Knob::new(range, v).diameter(KNOB).colour(colour))
    });

    let target = match state.target {
        FxTarget::Deck(d) => deck_letter(d).to_string(),
        FxTarget::Master => "MASTER".to_string(),
    };
    let r = ui
        .add_sized(
            vec2(TARGET_W, H),
            HwButton::new(&target, colour, vec2(TARGET_W, H))
                .font(9.0)
                .tinted(true),
        )
        .on_hover_text("What the effect is on: a deck, or the master mix. Click for the next");
    press(ui, &r, FxControl::Target, out);
    let r = ui
        .add_sized(
            vec2(ON_W, H),
            HwButton::new("ON", colour, vec2(ON_W, H))
                .font(9.0)
                .lit(state.enabled)
                .tinted(busy),
        )
        .on_hover_text("Effect on / off. An echo's repeats ring on after it's off");
    press(ui, &r, FxControl::On, out);

    // The release: what rings out, and the button that releases.
    let mut release = state.release;
    egui::ComboBox::from_id_salt(("fx-release", unit))
        .selected_text(state.release.name())
        .width(56.0)
        .show_ui(ui, |ui| {
            for r in FxRelease::ALL {
                ui.selectable_value(&mut release, r, r.name());
            }
        })
        .response
        .on_hover_text("The release effect: what rings out when the effect is released");
    if release != state.release {
        out.push(Command::Fx(unit, FxCmd::ReleaseKind(release)));
    }
    let r = button(ui, "REL", ON_W).on_hover_text("Release: the effect goes off, and the release effect rings out");
    press(ui, &r, FxControl::Release, out);
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
