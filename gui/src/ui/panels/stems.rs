//! Prepare's Stems tab: the player's track split into drums, bass, vocals and "other",
//! each with its waveform, a level and a mute, all playing in sync on the player.
//! Without stems: a button to work them out, and its progress.

use std::sync::Arc;

use earshot_engine::stems::{COUNT, NAMES};
use earshot_engine::{DeckCmd as EngineCmd, DeckId, Set};
use egui::{Color32, CornerRadius, Sense, Ui, vec2};

use super::deck::wave::{self, DeckWave, Grid, Marks};
use super::{Cx, Panel};
use crate::core::stems::{Stage, Status, StemWaves, StemsCmd, StemsEvent};
use crate::core::track::TrackId;
use crate::ui::widgets::Button;

const PLAYER: DeckId = DeckId::PREPARE;
const ROW_H: f32 = 64.0;
const CONTROLS_W: f32 = 230.0;
/// Seconds either side of the playhead: the closest and widest zoom.
const ZOOM: (f64, f64) = (0.5, 60.0);

#[derive(Default)]
pub struct StemsUi {
    /// The last calculation's failure, by track.
    error: Option<(TrackId, String)>,
    /// The waveforms drawn, for the stems they were made from.
    waves: Option<(Arc<StemWaves>, Vec<DeckWave>)>,
    /// Seconds of track either side of the playhead.
    half: f64,
}

impl StemsUi {
    pub fn on_event(&mut self, e: StemsEvent) {
        match e {
            StemsEvent::Done { track } | StemsEvent::Cancelled { track } => {
                if self.error.as_ref().is_some_and(|(t, _)| *t == track) {
                    self.error = None;
                }
            }
            StemsEvent::Failed { track, error } => self.error = Some((track, error)),
        }
    }
}

pub struct StemsPanel<'a> {
    pub tab: &'a mut StemsUi,
}

impl Panel for StemsPanel<'_> {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let tab = &mut *self.tab;
        if tab.half <= 0.0 {
            tab.half = 6.0;
        }
        let snap = cx.engine.snap.as_ref().map(|s| *s.deck(PLAYER));
        let Some(track) = snap.and_then(|s| s.track_id) else {
            ui.weak("No track loaded.");
            return;
        };
        let snap = snap.unwrap();
        let info = cx.engine.deck(PLAYER);
        let (stem_waves, grid) = (info.stem_waves.clone(), info.grid.clone());

        match (&stem_waves, snap.stems) {
            (Some(w), true) => {
                if tab
                    .waves
                    .as_ref()
                    .is_none_or(|(had, _)| !Arc::ptr_eq(had, w))
                {
                    let drawn = (0..COUNT)
                        .map(|s| {
                            let (bands, detail) = &w.waves[s];
                            DeckWave::new(
                                ui.ctx(),
                                &format!("stem-{s}"),
                                bands.clone(),
                                detail.clone(),
                            )
                        })
                        .collect();
                    tab.waves = Some((w.clone(), drawn));
                }
            }
            _ => {
                tab.waves = None;
                calculate(ui, tab, track, info.stems_job, cx);
                return;
            }
        }

        ui.horizontal(|ui| {
            ui.label("Zoom");
            ui.add(
                egui::Slider::new(&mut tab.half, ZOOM.0..=ZOOM.1)
                    .logarithmic(true)
                    .suffix(" s"),
            );
            if ui.add(Button::subtle("All on").small()).clicked() {
                for s in 0..COUNT {
                    cx.send((
                        PLAYER,
                        EngineCmd::StemMute {
                            stem: s as u8,
                            set: Set::Off,
                        },
                    ));
                    cx.send((
                        PLAYER,
                        EngineCmd::StemLevel {
                            stem: s as u8,
                            level: 1.0,
                        },
                    ));
                }
            }
        });
        ui.add_space(4.0);
        let Some((_, waves)) = &tab.waves else { return };
        let pos = snap.position;
        let view = (pos - tab.half, pos + tab.half);
        for s in 0..COUNT {
            ui.horizontal(|ui| {
                ui.allocate_ui(vec2(CONTROLS_W, ROW_H), |ui| {
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.add_sized(
                                vec2(60.0, 18.0),
                                egui::Label::new(egui::RichText::new(NAMES[s]).strong()),
                            );
                            let muted = snap.stem_mute[s];
                            if ui.selectable_label(muted, "Mute").clicked() {
                                cx.send((
                                    PLAYER,
                                    EngineCmd::StemMute {
                                        stem: s as u8,
                                        set: Set::Toggle,
                                    },
                                ));
                            }
                            if ui.button("Solo").clicked() {
                                for o in 0..COUNT {
                                    let set = Set::from_bool(o != s);
                                    cx.send((PLAYER, EngineCmd::StemMute { stem: o as u8, set }));
                                }
                            }
                        });
                        let mut level = snap.stem_level[s];
                        ui.spacing_mut().slider_width = CONTROLS_W - 50.0;
                        if ui
                            .add(
                                egui::Slider::new(&mut level, 0.0..=1.0)
                                    .show_value(false)
                                    .trailing_fill(true),
                            )
                            .changed()
                        {
                            cx.send((
                                PLAYER,
                                EngineCmd::StemLevel {
                                    stem: s as u8,
                                    level,
                                },
                            ));
                        }
                    });
                });
                let (rect, r) =
                    ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                let marks = Marks {
                    grid: grid.as_deref().map_or(Grid::None, Grid::Beatgrid),
                    ..Marks::default()
                };
                let painter = ui.painter_at(rect);
                wave::window(&painter, rect, Some(&waves[s]), view, pos, &marks);
                // Silent stems are dimmed.
                let gain = if snap.stem_mute[s] {
                    0.0
                } else {
                    snap.stem_level[s]
                };
                if gain < 1.0 {
                    let a = (200.0 * (1.0 - gain)) as u8;
                    painter.rect_filled(rect, CornerRadius::same(4), Color32::from_black_alpha(a));
                }
                // The wheel zooms every stem's waveform together (and not the page).
                if r.hovered() {
                    let dy = ui.input(|i| i.smooth_scroll_delta.y);
                    if dy != 0.0 {
                        ui.ctx()
                            .input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
                        tab.half = (tab.half * (-dy as f64 * 0.004).exp()).clamp(ZOOM.0, ZOOM.1);
                    }
                }
                if r.clicked()
                    && let Some(p) = r.interact_pointer_pos()
                {
                    let t = view.0
                        + (p.x - rect.left()) as f64 / rect.width() as f64 * (view.1 - view.0);
                    cx.send((PLAYER, EngineCmd::Seek(t.max(0.0))));
                }
            });
            ui.add_space(2.0);
        }
    }
}

/// No stems on the player yet: work them out, or show how far that's got.
fn calculate(ui: &mut Ui, tab: &mut StemsUi, track: TrackId, job: Option<Status>, cx: &mut Cx) {
    if crate::core::outside::is_outside(track) {
        ui.weak("Library tracks only.");
        return;
    }
    match job {
        Some(j) => {
            ui.label(match (j.cancelling, j.queued, j.stage) {
                (true, _, _) => "Cancelling…",
                (_, true, _) => "Waiting…",
                (_, _, Stage::Download) => "Downloading model…",
                (_, _, Stage::Separate) => "Separating…",
            });
            ui.horizontal(|ui| {
                ui.add(
                    egui::ProgressBar::new(j.fraction as f32)
                        .show_percentage()
                        .desired_width(320.0),
                );
                if ui
                    .add_enabled(!j.cancelling, Button::new("Cancel"))
                    .clicked()
                {
                    cx.send(StemsCmd::Cancel(track));
                }
            });
        }
        None => {
            ui.label("No stems yet.");
            if ui.add(Button::primary("Calculate stems")).clicked() {
                cx.send(StemsCmd::Calculate(track));
            }
            if let Some((t, e)) = &tab.error
                && *t == track
            {
                ui.colored_label(cx.theme.warn, format!("Failed: {e}"));
            }
        }
    }
}
