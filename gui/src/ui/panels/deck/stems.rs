//! The deck's stems page, two by two: per stem, its waveform around the playhead, a
//! level knob and a button. All on (the track as it is) by default. What a button
//! does is a setting ([`StemMode`]): turn its stem off or on, or solo it.
//!
//! Without stems: one button to work them out, showing its progress while it runs.

use earshot_engine::stems::{COUNT, NAMES};
use earshot_engine::{DeckCmd as EngineCmd, Set};
use std::sync::Arc;

use egui::{Color32, Sense, Ui, vec2};

use super::design::*;
use super::wave::{self, DeckWave, Grid, Marks};
use super::{DeckInfo, DeckUi, Page};
use crate::core::bus::{Range, VOLUME};
use crate::core::command::Command;
use crate::core::stems::StemsCmd;
use crate::core::view::StemMode;
use crate::ui::theme::THEME;
use crate::ui::widgets::{HwButton, Knob};

const LABELS: [&str; COUNT] = ["DRUMS", "BASS", "VOX", "OTHER"];

/// A stem's level: the channel fader's range under its own name.
const LEVEL: Range = Range {
    name: "Level",
    ..VOLUME
};

/// Seconds of track either side of the playhead in a stem's waveform.
const HALF: f64 = 4.0;

/// Which stems play after pressing `s`, given which play now.
pub(super) fn press(mode: StemMode, on: [bool; COUNT], s: usize) -> [bool; COUNT] {
    if mode == StemMode::Toggle {
        let mut next = on;
        next[s] = !next[s];
        return next;
    }
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
        let d = self.dims;
        let w = d.px(STEM_W);
        let s = &self.state;
        if !s.stems {
            let Some(track) = s.track_id else {
                // Nothing loaded: hold the group's place.
                ui.allocate_exact_size(
                    vec2(w * 2.0 + d.px(GAP_X), d.double()),
                    egui::Sense::hover(),
                );
                return;
            };
            if crate::core::outside::is_outside(track) {
                // Library tracks only, for now.
                ui.allocate_exact_size(
                    vec2(w * 2.0 + d.px(GAP_X), d.double()),
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
                        vec2(w * 2.0 + d.px(GAP_X), d.double()),
                    )
                    .font(d.px(FONT_LOOP))
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
        let level = s.stem_level;
        let (pos, colour) = (self.shown, self.colour());
        // As wide as the page box (the widest other page), or the least that fits.
        let (gap, group) = (d.px(GAP_X), d.px(GROUP_GAP));
        let knob = d.px(STEM_KNOB_D);
        let fixed = knob + 4.0 + w + 2.0 * gap;
        let least = 2.0 * (d.px(STEM_WAVE_MIN) + fixed) + group;
        let others = [Page::Player, Page::Grid]
            .iter()
            .filter_map(|p| self.page_widths[*p as usize])
            .fold(0.0_f32, f32::max);
        let wave_w = ((others.max(least) - group) / 2.0 - fixed).floor();
        let h = d.px(BTN_H);
        if let Some(sw) = &info.stem_waves
            && self
                .stem_waves
                .as_ref()
                .is_none_or(|(had, _)| !Arc::ptr_eq(had, sw))
        {
            let drawn = (0..COUNT)
                .map(|i| {
                    let (bands, detail) = &sw.waves[i];
                    let name = format!("deck-{}-stem-{i}", self.id.index());
                    DeckWave::new(ui.ctx(), &name, bands.clone(), detail.clone())
                })
                .collect();
            self.stem_waves = Some((sw.clone(), drawn));
        }
        let waves = info
            .stem_waves
            .as_ref()
            .and(self.stem_waves.as_ref())
            .map(|(_, w)| w);
        let marks = Marks {
            grid: info.grid.as_deref().map_or(Grid::None, Grid::Beatgrid),
            ..Marks::default()
        };
        let mut pressed = None;
        let mut levels = Vec::new();
        ui.vertical(|ui| {
            for row in [[0, 1], [2, 3]] {
                ui.horizontal(|ui| {
                    for (n, i) in row.into_iter().enumerate() {
                        if n > 0 {
                            ui.add_space(group - gap);
                        }
                        let (rect, _) = ui.allocate_exact_size(vec2(wave_w, h), Sense::hover());
                        let painter = ui.painter_at(rect);
                        let view = (pos - HALF, pos + HALF);
                        wave::window(&painter, rect, waves.map(|w| &w[i]), view, pos, &marks);
                        // A stem not heard is dimmed, as much as it's turned down.
                        let gain = if on[i] { level[i] } else { 0.0 };
                        if gain < 1.0 {
                            let a = (200.0 * (1.0 - gain)) as u8;
                            painter.rect_filled(rect, 4.0, Color32::from_black_alpha(a));
                        }
                        let mut v = level[i];
                        let r = ui.add_sized(
                            vec2(knob + 4.0, h),
                            Knob::new(&LEVEL, &mut v).diameter(knob).colour(colour),
                        );
                        if r.changed() {
                            levels.push((i, v));
                        }
                        let r = ui
                            .add(
                                HwButton::new(LABELS[i], THEME.loop_button, vec2(w, h))
                                    .font(d.px(FONT_STEM))
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
        for (i, level) in levels {
            self.engine(
                out,
                EngineCmd::StemLevel {
                    stem: i as u8,
                    level,
                },
            );
        }
        if let Some(i) = pressed {
            let next = press(self.stem_mode, on, i);
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
        let vox = press(StemMode::Solo, all, 2);
        assert_eq!(vox, [false, false, true, false]);
        // The soloed one again: the whole track.
        assert_eq!(press(StemMode::Solo, vox, 2), all);
        // Another one adds to it.
        let vox_drums = press(StemMode::Solo, vox, 0);
        assert_eq!(vox_drums, [true, false, true, false]);
        assert_eq!(press(StemMode::Solo, vox_drums, 0), vox);
    }

    #[test]
    fn a_toggle_press_turns_just_that_stem_off_or_on() {
        let all = [true; COUNT];
        let no_vox = press(StemMode::Toggle, all, 2);
        assert_eq!(no_vox, [true, true, false, true]);
        assert_eq!(press(StemMode::Toggle, no_vox, 2), all);
        // All off is allowed: silence.
        let none = [false; COUNT];
        assert_eq!(
            press(StemMode::Toggle, [false, false, true, false], 2),
            none
        );
    }
}
