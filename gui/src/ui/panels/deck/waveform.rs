//! A player's waveforms: the scrolling one (drawn in the row above the decks, or
//! across the top in Prepare) and the whole-track overview under its title.

use std::time::Instant;

use earshot_engine::{DeckCmd as EngineCmd, LEAD_IN, Set};
use egui::{FontId, Rect, Sense, Ui, pos2, vec2};

use super::wave;
use super::{DeckInfo, DeckUi, Scrub};
use crate::core::bus::DeckControl;
use crate::core::command::{Command, DeckCmd, ViewCmd};
use crate::ui::panels::library::drag;
use crate::ui::panels::{Cx, Panel};

impl DeckUi {
    // ---- the scrolling waveform (drawn in the stacked row above the decks) -----------

    /// `half_window`: real seconds visible either side of the playhead, shared by both
    /// decks. Each shows `half_window × tempo` of its track, so both scroll at the same
    /// speed and beat markers are spaced by the tempo you hear. (Tempo, not speed: sync's
    /// phase corrections and nudges would make the zoom wobble.)
    pub fn show_wave(
        &mut self,
        ui: &mut Ui,
        height: f32,
        info: &DeckInfo,
        half_window: f64,
        out: &mut Vec<Command>,
    ) {
        let (rect, resp) =
            ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click_and_drag());
        if self.pending.is_some() {
            self.loading(ui, rect);
            if let Some(id) = drag::take_track(&resp) {
                self.send(out, DeckCmd::Load(id));
            }
            return;
        }
        let half = half_window
            * if self.state.tempo > 0.0 {
                self.state.tempo
            } else {
                1.0
            };
        let hotcues = self.state.hotcues;
        let marks = self.marks(info, &hotcues);
        let painter = ui.painter_at(rect);
        wave::scrolling(&painter, rect, self.wave.as_ref(), self.shown, half, &marks);
        painter.rect_filled(
            Rect::from_min_size(rect.min, vec2(3.0, rect.height())),
            0.0,
            self.colour(),
        );
        if let Some(label) = self.bar_beat(info) {
            painter.text(
                pos2(rect.center().x - 6.0, rect.top() + 3.0),
                egui::Align2::RIGHT_TOP,
                label,
                FontId::proportional(12.0),
                self.colour(),
            );
        }

        // Drag to scratch, in vinyl mode: a hand on the record. The track follows the
        // pointer, forwards or back, and you hear what passes; let go and a playing deck
        // carries on. Out of vinyl mode the drag moves the playhead silently, the deck
        // held meanwhile.
        let duration = self.state.duration;
        if resp.drag_started() && duration > 0.0 {
            let x = resp.interact_pointer_pos().map_or(0.0, |p| p.x);
            let (hand, playing) = (self.state.vinyl, self.state.playing);
            self.scrub = Some(Scrub {
                x0: x,
                t0: self.shown,
                last: x,
                hand,
                resume: !hand && playing,
            });
            if hand {
                self.engine(out, EngineCmd::Hand(true));
            } else if playing {
                self.engine(out, EngineCmd::Play(Set::Off));
            }
        }
        if let (Some(sc), Some(p)) = (self.scrub, resp.interact_pointer_pos())
            && resp.dragged()
        {
            // Seconds of track per pixel; dragging right pulls the record back.
            let per_px = 2.0 * half / rect.width() as f64;
            let t = (sc.t0 - (p.x - sc.x0) as f64 * per_px).clamp(-LEAD_IN, duration);
            self.shown = t;
            if p.x != sc.last {
                if sc.hand {
                    let by = -((p.x - sc.last) as f64) * per_px;
                    self.engine(
                        out,
                        EngineCmd::HandMove {
                            by,
                            at: Instant::now(),
                        },
                    );
                } else {
                    self.control(out, DeckControl::Position, t / duration);
                }
                self.scrub = Some(Scrub { last: p.x, ..sc });
            }
        }
        if resp.drag_stopped()
            && let Some(sc) = self.scrub.take()
        {
            if sc.hand {
                self.engine(out, EngineCmd::Hand(false));
            } else if sc.resume {
                self.engine(out, EngineCmd::Play(Set::On));
            }
        }
        if resp.hovered() {
            let scroll = ui.input(|i| i.raw_scroll_delta.y);
            if scroll != 0.0 {
                // About 7% per wheel notch.
                let zoom = (half_window * (-(scroll as f64) * 0.0014).exp()).clamp(0.5, 30.0);
                out.push(ViewCmd::WaveZoom(zoom).into());
            }
        }
        if let Some(id) = drag::take_track(&resp) {
            self.send(out, DeckCmd::Load(id));
        }
    }

    pub(super) fn bar_beat(&self, info: &DeckInfo) -> Option<String> {
        // Counts on before the first 1 and before 0 s (the lead-in): 0, -1 ….
        let (bar, beat) = info.grid.as_ref()?.bar_beat(self.shown)?;
        Some(format!("{bar}.{beat} Bars"))
    }

    pub(super) fn overview(
        &mut self,
        ui: &mut Ui,
        info: &DeckInfo,
        out: &mut Vec<Command>,
        height: f32,
    ) {
        let (rect, resp) =
            ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click_and_drag());
        if self.pending.is_some() {
            self.loading(ui, rect);
            return;
        }
        let hotcues = self.state.hotcues;
        let marks = self.marks(info, &hotcues);
        wave::overview(
            &ui.painter_at(rect),
            rect,
            self.wave.as_ref(),
            self.shown,
            self.state.duration,
            &marks,
            info.analysing.as_ref().map(|a| a.0),
        );
        if (resp.clicked() || resp.dragged())
            && let Some(p) = resp.interact_pointer_pos()
        {
            let at = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64;
            self.control(out, DeckControl::Position, at);
        }
    }
}

/// A player's scrolling waveform, `height` tall, at the shared zoom.
pub struct WavePanel<'a> {
    pub deck: &'a mut DeckUi,
    pub height: f32,
}

impl Panel for WavePanel<'_> {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        crate::ui::theme::normal_text(ui);
        let info = cx.engine.deck(self.deck.id);
        self.deck
            .show_wave(ui, self.height, info, cx.prefs.wave_zoom, cx.out);
    }
}
