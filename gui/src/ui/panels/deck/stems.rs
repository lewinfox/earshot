//! The deck's stems group: a button per stem, two by two. All on (the track as it
//! is) by default. With all on, a press plays just that stem; otherwise a press turns
//! that stem on or off, and turning off the last one puts them all back on.
//!
//! Without stems: one button to work them out, showing its progress while it runs.

use earshot_engine::stems::{COUNT, NAMES};
use earshot_engine::{DeckCmd as EngineCmd, Set};
use egui::{Ui, vec2};

use super::{DeckInfo, DeckUi};
use crate::core::command::Command;
use crate::core::stems::StemsCmd;
use crate::ui::theme::THEME;
use crate::ui::widgets::HwButton;

const LABELS: [&str; COUNT] = ["DRUMS", "BASS", "VOX", "OTHER"];

/// Which stems play after pressing `s`, given which play now.
pub(super) fn press(on: [bool; COUNT], s: usize) -> [bool; COUNT] {
    if on.iter().all(|&o| o) {
        return std::array::from_fn(|i| i == s);
    }
    let mut next = on;
    next[s] = !next[s];
    if next.iter().all(|&o| !o) {
        return [true; COUNT];
    }
    next
}

impl DeckUi {
    pub(super) fn stems(&mut self, ui: &mut Ui, info: &DeckInfo, out: &mut Vec<Command>) {
        let w = self.dims.btn_w + 10.0;
        let s = &self.state;
        if !s.stems {
            let Some(track) = s.track_id else {
                // Nothing loaded: hold the group's place.
                ui.allocate_exact_size(
                    vec2(w * 2.0 + 3.0, self.dims.double()),
                    egui::Sense::hover(),
                );
                return;
            };
            if crate::core::outside::is_outside(track) {
                // Library tracks only, for now.
                ui.allocate_exact_size(
                    vec2(w * 2.0 + 3.0, self.dims.double()),
                    egui::Sense::hover(),
                );
                return;
            }
            let job = info.stems_job;
            let label = match job {
                Some(j) if j.cancelling => "STEMS\nstopping".to_string(),
                Some(j) if j.queued => "STEMS\nwaiting".to_string(),
                Some(j) => format!("STEMS\n{:.0} %", j.fraction * 100.0),
                None => "STEMS".into(),
            };
            let r = ui
                .add(
                    HwButton::new(
                        &label,
                        THEME.loop_button,
                        vec2(w * 2.0 + 3.0, self.dims.double()),
                    )
                    .font(10.0)
                    .lit(job.is_some()),
                )
                .on_hover_text(match job {
                    Some(_) => "Calculating stems",
                    None => "Calculate stems",
                });
            if r.clicked() && job.is_none() {
                out.push(StemsCmd::Calculate(track).into());
            }
            return;
        }
        let on: [bool; COUNT] = std::array::from_fn(|i| !s.stem_mute[i]);
        let mut pressed = None;
        ui.vertical(|ui| {
            for row in [[0, 1], [2, 3]] {
                ui.horizontal(|ui| {
                    for i in row {
                        let r = ui
                            .add(
                                HwButton::new(
                                    LABELS[i],
                                    THEME.loop_button,
                                    vec2(w, self.dims.btn_h),
                                )
                                .font(9.0)
                                .lit(on[i]),
                            )
                            .on_hover_text(NAMES[i]);
                        if r.clicked() {
                            pressed = Some(i);
                        }
                    }
                });
            }
        });
        if let Some(i) = pressed {
            let next = press(on, i);
            for (stem, &play) in next.iter().enumerate() {
                if play != on[stem] {
                    self.engine(
                        out,
                        EngineCmd::StemMute {
                            stem: stem as u8,
                            set: Set::from_bool(!play),
                        },
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_press_solos_from_all_and_the_last_one_off_brings_all_back() {
        let all = [true; COUNT];
        let vox = press(all, 2);
        assert_eq!(vox, [false, false, true, false]);
        // The soloed one again: the whole track.
        assert_eq!(press(vox, 2), all);
        // Another one adds to it.
        let vox_drums = press(vox, 0);
        assert_eq!(vox_drums, [true, false, true, false]);
        assert_eq!(press(vox_drums, 0), vox);
    }
}
