//! The player page with a controller connected: its 8 performance pads, 4 by 2, each
//! labelled with what it does in the pad mode picked on the controller (Rekordbox
//! shows the same). They fill the page box: the room to spare is theirs, for long
//! labels. Clicking one does what the controller's pad does.
//!
//! - Hot cue: the cue's letter, its time and its label.
//! - Beat loop: a loop of 1/4, 1/2, 1, 2, 4, 8, 16 or 32 beats.
//! - Beat jump: back and forward in pairs, by 1, 2, 4 and 8 beats.
//! - The other modes are things earshot doesn't do yet: the pads are blank.

use egui::{Color32, Ui, vec2};

use super::design::*;
use super::loops::size_label;
use super::{DeckInfo, DeckUi};
use crate::core::bus::{DeckControl, hot_cue_letter};
use crate::core::command::Command;
use crate::core::view::PadMode;
use crate::ui::theme::THEME;
use crate::ui::util;
use crate::ui::widgets::{HwButton, held};

/// Beat loop mode's loops, pad by pad, in beats.
pub const PAD_LOOPS: [f64; 8] = [0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0];
/// Beat jump mode's jumps, pad by pad, in beats (negative: back).
pub const PAD_JUMPS: [f64; 8] = [-1.0, 1.0, -2.0, 2.0, -4.0, 4.0, -8.0, 8.0];

/// What a pad shows and does in a mode.
struct Pad {
    label: String,
    caption: Option<String>,
    colour: Color32,
    tinted: bool,
    tip: String,
    /// What it sends on press (and release, for a hot cue).
    control: Option<DeckControl>,
    /// The value a press sends.
    value: f64,
}

impl DeckUi {
    pub(super) fn controller_pads(&mut self, ui: &mut Ui, info: &DeckInfo, out: &mut Vec<Command>) {
        let d = self.dims;
        // Shown: as wide as the page box allows; measured: their least.
        let gap = d.px(GAP_X);
        let w = self.page_box.map_or(d.px(PAD_W), |b| {
            ((b - 3.0 * gap) / 4.0).max(d.px(PAD_W)).floor()
        });
        let mode = self.pad_mode;
        egui::Grid::new(("controller-pads", self.id.index()))
            .spacing(vec2(d.px(GAP_X), d.px(GAP_Y)))
            .show(ui, |ui| {
                for row in 0..2 {
                    for col in 0..4 {
                        let i = row * 4 + col;
                        let pad = self.pad(mode, i, info);
                        let r = ui
                            .add(
                                HwButton::new(&pad.label, pad.colour, vec2(w, d.px(BTN_H)))
                                    .font(d.px(FONT_PAD))
                                    .caption_font(d.px(FONT_CAPTION))
                                    .caption(pad.caption.as_deref())
                                    .tinted(pad.tinted),
                            )
                            .on_hover_text(format!("{} · pad {}\n{}", mode.label(), i + 1, pad.tip));
                        let Some(control) = pad.control else { continue };
                        match held(ui, &r) {
                            Some(true) => self.control(out, control, pad.value),
                            Some(false) if control == DeckControl::Hotcue(i as u8) => {
                                self.control(out, control, 0.0)
                            }
                            _ => {}
                        }
                    }
                    ui.end_row();
                }
            });
    }

    /// Pad `i` (0-based) in `mode`.
    fn pad(&self, mode: PadMode, i: usize, info: &DeckInfo) -> Pad {
        match mode {
            PadMode::HotCue => {
                let letter = hot_cue_letter(i as u8);
                let set = self.state.hotcues[i];
                let name = info.cues.hot_labels[i].clone().filter(|_| set.is_some());
                Pad {
                    label: match set {
                        Some(t) => format!("{letter} {}", util::fmt_tenths(t)),
                        None => letter.to_string(),
                    },
                    caption: name,
                    colour: THEME.hotcue[i],
                    tinted: set.is_some(),
                    tip: match set {
                        Some(_) => "Jump to the hot cue (held: preview)".into(),
                        None => "Set a hot cue here".into(),
                    },
                    control: Some(DeckControl::Hotcue(i as u8)),
                    value: 1.0,
                }
            }
            PadMode::BeatLoop => {
                let beats = PAD_LOOPS[i];
                Pad {
                    label: size_label(beats),
                    caption: None,
                    colour: THEME.loop_button,
                    tinted: false,
                    tip: format!("Loop {} beats (again: end it)", size_label(beats)),
                    control: Some(DeckControl::BeatLoop(beats)),
                    value: 1.0,
                }
            }
            PadMode::BeatJump => {
                let beats = PAD_JUMPS[i];
                let n = beats.abs();
                Pad {
                    label: if beats < 0.0 { format!("◀ {n}") } else { format!("{n} ▶") },
                    caption: None,
                    colour: THEME.text,
                    tinted: false,
                    tip: format!(
                        "Jump {n} beat{} {}",
                        if n == 1.0 { "" } else { "s" },
                        if beats < 0.0 { "back" } else { "forward" }
                    ),
                    control: Some(DeckControl::BeatJump),
                    value: beats,
                }
            }
            _ => Pad {
                label: String::new(),
                caption: None,
                colour: THEME.line,
                tinted: false,
                tip: "Not in earshot yet".into(),
                control: None,
                value: 0.0,
            },
        }
    }
}
