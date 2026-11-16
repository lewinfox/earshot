//! The player page with a controller connected: its 8 performance pads, 4 by 2, each
//! labelled with what it does in the pad mode picked on the controller (Rekordbox
//! shows the same). They fill the page box: the room to spare is theirs, for long
//! labels. Clicking one does what the controller's pad does.
//!
//! - Hot cue: the cue's letter, its time and its label.
//! - Beat loop: a loop of 1/4, 1/2, 1, 2, 4, 8, 16 or 32 beats.
//! - Beat jump: back and forward in pairs, by 1, 2, 4 and 8 beats.
//! - Keyboard: first each hot cue's letter, to pick one; then each pad's semitones
//!   on its page (`core::pads`), the track's own key (±0) tinted.
//! - Key shift: each pad's semitones, or its step on page 5; the key now tinted.
//! - Pad FX 1 and 2: each pad's effect and its length in beats, with its letter in
//!   the bank (A-P, Q-AF); the one held tinted.
//! - Sampler: each slot's sound (slots 1-8 on the left decks, 9-16 on the right),
//!   tinted while it plays. An empty slot's pad opens the sampler's window.
//! - The other modes are things earshot doesn't do yet: the pads are blank.
//!
//! While the controller's SHIFT (or the keyboard's Shift) is held, the pads show and
//! do what they do with SHIFT: clear a hot cue; in keyboard and key shift mode, turn
//! the page (pads 7 and 8) and pick another hot cue (keyboard, pads 1-6); in pad FX
//! mode, the bank's other 8 effects; stop a sampler slot.

use egui::{Color32, Ui, vec2};

use super::design::*;
use super::loops::size_label;
use super::{DeckInfo, DeckUi};
use crate::core::bus::{DeckControl, hot_cue_letter};
use crate::core::command::Command;
use crate::core::pads::{self, PadAction};
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
        let shift = self.shift_held || ui.input(|i| i.modifiers.shift);
        egui::Grid::new(("controller-pads", self.id.index()))
            .spacing(vec2(d.px(GAP_X), d.px(GAP_Y)))
            .show(ui, |ui| {
                for row in 0..2 {
                    for col in 0..4 {
                        let i = row * 4 + col;
                        let pad = if shift {
                            self.shifted_pad(mode, i)
                        } else {
                            self.pad(mode, i, info)
                        };
                        let r = ui
                            .add(
                                HwButton::new(&pad.label, pad.colour, vec2(w, d.px(BTN_H)))
                                    .font(d.px(FONT_PAD))
                                    .caption_font(d.px(FONT_CAPTION))
                                    .caption(pad.caption.as_deref())
                                    .tinted(pad.tinted),
                            )
                            .on_hover_text(format!(
                                "{} · pad {}\n{}",
                                mode.label(),
                                i + 1,
                                pad.tip
                            ));
                        if mode == PadMode::Sampler {
                            if held(ui, &r) == Some(true) {
                                out.extend(self.sampler_press(i, shift));
                            }
                            continue;
                        }
                        let Some(control) = pad.control else { continue };
                        match held(ui, &r) {
                            Some(true) => self.control(out, control, pad.value),
                            Some(false)
                                if control == DeckControl::Hotcue(i as u8)
                                    || matches!(control, DeckControl::ModePad { .. }) =>
                            {
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
                    label: if beats < 0.0 {
                        format!("◀ {n}")
                    } else {
                        format!("{n} ▶")
                    },
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
            PadMode::Keyboard | PadMode::KeyShift => self.mode_pad_face(mode, i),
            PadMode::PadFx1 | PadMode::PadFx2 => self.pad_fx_face(mode, i, false),
            PadMode::Sampler => self.sampler_pad(i, false),
        }
    }

    /// Pad `i` with SHIFT held.
    fn shifted_pad(&self, mode: PadMode, i: usize) -> Pad {
        let blank = |tip: &str| Pad {
            label: String::new(),
            caption: None,
            colour: THEME.line,
            tinted: false,
            tip: tip.into(),
            control: None,
            value: 0.0,
        };
        match mode {
            PadMode::HotCue => {
                let letter = hot_cue_letter(i as u8);
                if self.state.hotcues[i].is_none() {
                    return blank(&format!("Hot cue {letter} isn't set"));
                }
                Pad {
                    label: format!("✕ {letter}"),
                    caption: Some("clear".into()),
                    colour: THEME.hotcue[i],
                    tinted: false,
                    tip: format!("Clear hot cue {letter}"),
                    control: Some(DeckControl::HotcueClear(i as u8)),
                    value: 1.0,
                }
            }
            PadMode::Keyboard | PadMode::KeyShift => {
                let pads = &self.pads;
                let page = pads.page(mode);
                let pages = if mode == PadMode::Keyboard { 4 } else { 5 };
                let control = Some(DeckControl::ModePad {
                    mode,
                    pad: i as u8,
                    shift: true,
                });
                let pad = |label: String, caption: String, tip: String| Pad {
                    label,
                    caption: Some(caption),
                    colour: THEME.accent,
                    tinted: false,
                    tip,
                    control,
                    value: 1.0,
                };
                match i {
                    6 if page > 0 => pad(
                        "◀ PAGE".into(),
                        format!("to {} of {pages}", page),
                        "The page before (higher notes in keyboard mode)".into(),
                    ),
                    7 if page + 1 < pages => pad(
                        "PAGE ▶".into(),
                        format!("to {} of {pages}", page + 2),
                        "The page after (lower notes in keyboard mode)".into(),
                    ),
                    6 | 7 => blank(&format!("Page {} of {pages}: no page that way", page + 1)),
                    _ if mode == PadMode::Keyboard && pads.keyboard_cue(&self.state).is_some() => {
                        pad(
                            "PICK".into(),
                            "another hot cue".into(),
                            "Pick another hot cue to play on the keyboard".into(),
                        )
                    }
                    _ => blank("Nothing with SHIFT"),
                }
            }
            PadMode::PadFx1 | PadMode::PadFx2 => self.pad_fx_face(mode, i, true),
            PadMode::Sampler => self.sampler_pad(i, true),
            _ => blank("Nothing with SHIFT"),
        }
    }

    /// Pad `i` in a pad FX mode, with SHIFT or not: its effect, on while it's held.
    fn pad_fx_face(&self, mode: PadMode, i: usize, shift: bool) -> Pad {
        let (Some(fx), Some(letter)) = (
            pads::pad_fx(mode, i, shift),
            pads::pad_fx_letter(mode, i, shift),
        ) else {
            return Pad {
                label: String::new(),
                caption: None,
                colour: THEME.line,
                tinted: false,
                tip: String::new(),
                control: None,
                value: 0.0,
            };
        };
        let length = pads::beats_label(fx.beats);
        let name = fx.kind.name();
        let other = if shift {
            "Without SHIFT"
        } else {
            "Shift-click"
        };
        Pad {
            label: format!("{} {length}", name.to_uppercase()),
            caption: Some(letter.clone()),
            colour: THEME.accent,
            tinted: self.pads.fx_held == Some((mode, i as u8, shift)),
            tip: format!(
                "Effect {letter}: {name}, {length} beat{}, while held\n{other}: the bank's other 8 effects",
                if fx.beats == 1.0 { "" } else { "s" }
            ),
            control: Some(DeckControl::ModePad {
                mode,
                pad: i as u8,
                shift,
            }),
            value: 1.0,
        }
    }

    /// Pad `i` in sampler mode (with SHIFT held: `shift`): its slot's sound.
    fn sampler_pad(&self, i: usize, shift: bool) -> Pad {
        let s = &self.sample_pads[i];
        let n = s.slot + 1;
        let (label, caption, tip) = match (&s.name, shift) {
            (None, true) => (String::new(), None, format!("Slot {n} is empty")),
            (None, false) => (
                n.to_string(),
                Some("empty".to_string()),
                format!("Slot {n} is empty: click to give it a sound (Menu > Sampler…)"),
            ),
            (Some(name), true) => (
                format!("■ {n}"),
                Some("stop".to_string()),
                format!("Stop slot {n}: {name}"),
            ),
            (Some(name), false) => (
                name.clone(),
                Some(match (s.loaded, s.looping) {
                    (false, _) => format!("{n} · not loaded"),
                    (true, true) => format!("{n} · loop"),
                    (true, false) => n.to_string(),
                }),
                format!("Play slot {n} from its start: {name}\nShift-click stops it"),
            ),
        };
        Pad {
            label,
            caption,
            colour: if s.name.is_some() {
                THEME.accent
            } else {
                THEME.line
            },
            tinted: s.playing && !shift,
            tip,
            // Not a deck's control: `sampler_press`.
            control: None,
            value: 1.0,
        }
    }

    /// What clicking pad `i` in sampler mode sends: play its slot, or stop it with
    /// SHIFT; an empty slot's pad opens the sampler's window, to give it a sound.
    fn sampler_press(&self, i: usize, shift: bool) -> Vec<Command> {
        use crate::core::sampler::SamplerCmd;
        let s = &self.sample_pads[i];
        let Some(slot) = earshot_engine::SampleSlot::new(s.slot) else {
            return vec![];
        };
        match (s.name.is_some(), shift) {
            (true, true) => vec![Command::Sampler(SamplerCmd::Stop(slot))],
            (true, false) => vec![Command::Sampler(SamplerCmd::Play(slot))],
            (false, true) => vec![],
            (false, false) => vec![Command::View(crate::core::view::ViewCmd::ToggleModal(
                crate::core::view::Modal::Sampler,
            ))],
        }
    }

    /// Pad `i` in keyboard or key shift mode.
    fn mode_pad_face(&self, mode: PadMode, i: usize) -> Pad {
        let pads = &self.pads;
        let page = pads.page(mode);
        let pages = if mode == PadMode::Keyboard { 4 } else { 5 };
        let control = Some(DeckControl::ModePad {
            mode,
            pad: i as u8,
            shift: false,
        });
        let paging = format!(
            "Page {} of {pages}: shift-click pad 7 / 8 for the page before / after",
            page + 1
        );
        let semis = |n: i8| match n {
            0 => "±0".to_string(),
            n => format!("{n:+}"),
        };
        if mode == PadMode::Keyboard {
            let Some(slot) = pads.keyboard_cue(&self.state) else {
                // Picking a hot cue.
                let letter = hot_cue_letter(i as u8);
                let set = self.state.hotcues[i].is_some();
                return Pad {
                    label: letter.to_string(),
                    caption: Some("pick".into()),
                    colour: THEME.hotcue[i],
                    tinted: set,
                    tip: if set {
                        format!("Play hot cue {letter} on the keyboard")
                    } else {
                        format!("Set hot cue {letter} here and play it on the keyboard")
                    },
                    control,
                    value: 1.0,
                };
            };
            let letter = hot_cue_letter(slot);
            let note = pads.action(mode, i);
            return Pad {
                label: match note {
                    Some(PadAction::Semitones(n)) => semis(n),
                    _ => String::new(),
                },
                caption: (note == Some(PadAction::Semitones(0)))
                    .then(|| format!("{letter} · own key")),
                colour: THEME.hotcue[slot as usize],
                tinted: note == Some(PadAction::Semitones(0)),
                tip: format!(
                    "Play hot cue {letter} in this key\n{paging}\nShift-click pads 1-6: pick another hot cue"
                ),
                control,
                value: 1.0,
            };
        }
        let action = pads.action(mode, i);
        let label = match action {
            Some(PadAction::Semitones(n)) => semis(n),
            Some(PadAction::Step(n)) if n > 0 => format!("↑{n}"),
            Some(PadAction::Step(n)) => format!("↓{}", -n),
            Some(PadAction::Reset) => "RESET".into(),
            Some(PadAction::KeySync) | None => String::new(),
        };
        let tip = match action {
            Some(PadAction::Semitones(_)) => "Shift the key to this".to_string(),
            Some(PadAction::Step(_)) => "Move the key by this many semitones".into(),
            Some(PadAction::Reset) => "Back to the track's own key".into(),
            Some(PadAction::KeySync) => "Key sync: not in earshot yet".into(),
            None => String::new(),
        };
        Pad {
            label,
            caption: None,
            colour: THEME.accent,
            tinted: matches!(action, Some(PadAction::Semitones(n)) if n as f64 == self.state.key_shift.round()),
            tip: format!("{tip}\n{paging}"),
            control,
            value: 1.0,
        }
    }
}
