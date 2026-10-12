//! The tempo column on a deck's outside edge: SYNC (right-click to lead), Q and
//! KEY over the tempo fader, its nudge buttons and its range.

use earshot_engine::{DeckCmd as EngineCmd, TempoRange};
use egui::{Ui, vec2};

use super::{DeckInfo, DeckUi, TEMPO_W};
use crate::core::bus::{DeckControl, deck_letter};
use crate::core::command::{Command, MixerCmd};
use crate::ui::theme::THEME;
use crate::ui::widgets::{Fader, HwButton, Range, held};

/// The tempo fader, in the registry's units (-1..1 across the deck's range), and
/// the range markings beside it (it runs minus at the top, like Mixxx's).
fn tempo_fader(range: TempoRange) -> (&'static Range, &'static [(f64, &'static str)]) {
    const fn fader(format: fn(f64) -> String) -> Range {
        Range {
            name: "Tempo",
            min: -1.0,
            max: 1.0,
            default: 0.0,
            centre: Some(0.0),
            format,
        }
    }
    fn pct(rate: f64) -> String {
        format!("{:+.2} %", rate * 100.0)
    }
    const SIX: Range = fader(|v| pct(TempoRange::Six.rate(v)));
    const TEN: Range = fader(|v| pct(TempoRange::Ten.rate(v)));
    const SIXTEEN: Range = fader(|v| pct(TempoRange::Sixteen.rate(v)));
    const WIDE: Range = fader(|v| pct(TempoRange::Wide.rate(v)));
    match range {
        TempoRange::Six => (&SIX, &[(0.0, "-6"), (0.5, "0"), (1.0, "+6")]),
        TempoRange::Ten => (&TEN, &[(0.0, "-10"), (0.5, "0"), (1.0, "+10")]),
        TempoRange::Sixteen => (&SIXTEEN, &[(0.0, "-16"), (0.5, "0"), (1.0, "+16")]),
        TempoRange::Wide => (&WIDE, &[(0.0, "0"), (0.5, "1x"), (1.0, "2.5")]),
    }
}

impl DeckUi {
    /// BPM and tempo readouts, SYNC, and the tempo fader with nudge buttons.
    /// SYNC, Q and KEY over the tempo fader, `height` tall in all.
    pub(super) fn tempo(
        &mut self,
        ui: &mut Ui,
        info: &DeckInfo,
        out: &mut Vec<Command>,
        height: f32,
    ) {
        let s = self.state;
        ui.vertical(|ui| {
            ui.set_width(TEMPO_W);
            ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
            let following = match s.following.and_then(deck_letter) {
                Some(l) if s.sync => format!("\nFollowing deck {l}"),
                _ => String::new(),
            };
            let leading = if info.leader {
                "\nThe sync leader: synced decks follow this one"
            } else {
                ""
            };
            // SYNC, and beside it the sync leader's star.
            let lead = if info.leader { None } else { Some(self.id) };
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let r = ui
                    .add(
                        HwButton::new("SYNC", self.colour(), vec2(TEMPO_W - 28.0, 20.0))
                            .font(10.0)
                            .lit(s.sync),
                    )
                    .on_hover_text(format!(
                        "Match the leader's tempo and beat{following}{leading}"
                    ));
                if held(ui, &r) == Some(true) {
                    self.control(out, DeckControl::Sync, 1.0);
                }
                let r = ui
                    .add(
                        HwButton::new("★", self.colour(), vec2(24.0, 20.0))
                            .font(11.0)
                            .lit(info.leader),
                    )
                    .on_hover_text(if info.leader {
                        "The sync leader: synced decks follow this one. Click to stop leading"
                    } else {
                        "Make this deck the sync leader: synced decks follow it"
                    });
                if r.clicked() {
                    out.push(Command::Mixer(MixerCmd::SyncLeader(lead)));
                }
            });
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let r = ui
                    .add(
                        HwButton::new("Q", THEME.accent, vec2(24.0, 18.0))
                            .font(10.0)
                            .lit(s.quantize),
                    )
                    .on_hover_text(
                        "Quantise: snap this deck's cues, loops and hot cues to the beat",
                    );
                if held(ui, &r) == Some(true) {
                    self.control(out, DeckControl::Quantize, 1.0);
                }
                let r = ui
                    .add(
                        HwButton::new("KEY", THEME.accent, vec2(TEMPO_W - 28.0, 18.0))
                            .font(9.0)
                            .lit(s.keylock),
                    )
                    .on_hover_text("Keylock: the tempo fader changes the speed but not the key");
                if held(ui, &r) == Some(true) {
                    self.control(out, DeckControl::Keylock, 1.0);
                }
            });
            // The fader gets exactly what's left, so it never stretches the deck.
            let h = height - ui.min_rect().height() - ui.spacing().item_spacing.y;
            ui.horizontal_top(|ui| {
                ui.add_space(16.0); // the fader's range markings
                let (fader, marks) = tempo_fader(s.range);
                // The cap is where the fader was left; until it's moved past the
                // tempo (marked), it doesn't change it.
                let mut v = s.fader;
                let target = (!s.fader_live).then(|| s.range.fader(s.rate));
                let r = ui.add(
                    Fader::vertical(fader, &mut v, vec2(26.0, h))
                        .inverted(true)
                        .ticks(17)
                        .marks(marks)
                        .target(target)
                        .colour(self.colour()),
                );
                if r.changed() {
                    self.control(out, DeckControl::Tempo, v);
                }
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    for (label, control, tip) in [
                        ("−", DeckControl::NudgeDown, "Nudge slower (hold)"),
                        ("+", DeckControl::NudgeUp, "Nudge faster (hold)"),
                    ] {
                        let r = ui
                            .add(HwButton::new(label, THEME.text, vec2(24.0, 24.0)).font(14.0))
                            .on_hover_text(tip);
                        if let Some(d) = held(ui, &r) {
                            self.control(out, control, d as u8 as f64);
                        }
                    }
                    // The fader's range: click for the next, right-click to pick one.
                    let r = ui
                        .add(HwButton::new(s.range.label(), THEME.text, vec2(24.0, 18.0)).font(8.0))
                        .on_hover_text("Tempo range: ±6, ±10, ±16 % or WIDE (0 to 256 % speed)");
                    if r.clicked() {
                        self.control(out, DeckControl::TempoRangeNext, 1.0);
                    }
                    r.context_menu(|ui| {
                        for range in TempoRange::ALL {
                            if ui.radio(s.range == range, range.label()).clicked() {
                                self.engine(out, EngineCmd::TempoRange(range));
                                ui.close();
                            }
                        }
                    });
                });
            });
        });
    }
}
