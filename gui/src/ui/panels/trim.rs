//! Prepare's Trim tab: pick the start and end of the part of the player's track to
//! keep (from the playhead, snapped to beats if asked, or typed), hear the cuts, and
//! save that part as a new file. The trim points show over the player's waveform
//! ([`TrimPoints`], set by the shell while this tab is open).

use std::path::PathBuf;

use earshot_engine::{DeckCmd as EngineCmd, DeckId, Set};
use egui::{Color32, Rect, RichText, Stroke, Ui, pos2};

use super::deck::wave::{Layer, Timeline};
use super::{Cx, Panel};
use crate::core::track::TrackId;
use crate::core::trim::{TrimCmd, TrimEvent};
use crate::ui::theme::THEME;
use crate::ui::widgets::Button;

const PLAYER: DeckId = DeckId::PREPARE;
/// How much before the end "Hear the end" starts.
const LEAD_IN: f64 = 5.0;

pub struct TrimUi {
    /// The track the points are for (the player's, when they were set).
    pub track: Option<TrackId>,
    /// The part kept, in seconds.
    pub start: f64,
    pub end: f64,
    /// The track's length when the points were reset.
    duration: f64,
    /// Points set from the playhead go to the nearest beat.
    pub snap: bool,
    /// Saving now.
    pub busy: bool,
    /// The last save: where it went, or why it didn't.
    pub note: Option<Result<PathBuf, String>>,
}

impl Default for TrimUi {
    fn default() -> Self {
        TrimUi {
            track: None,
            start: 0.0,
            end: 0.0,
            duration: 0.0,
            snap: true,
            busy: false,
            note: None,
        }
    }
}

impl TrimUi {
    pub fn on_event(&mut self, e: TrimEvent) {
        self.busy = false;
        let (track, note) = match e {
            TrimEvent::Saved { track, path, .. } => (track, Ok(path)),
            TrimEvent::Failed { track, error } => (track, Err(error)),
        };
        // Only for the track the tab is on now.
        if self.track == Some(track) {
            self.note = Some(note);
        }
    }

    /// Start again on `track`, `duration` s long: all of it kept.
    fn reset(&mut self, track: TrackId, duration: f64) {
        self.track = Some(track);
        self.duration = duration;
        self.start = 0.0;
        self.end = duration;
        self.note = None;
    }
}

pub struct TrimPanel<'a> {
    pub tab: &'a mut TrimUi,
}

impl Panel for TrimPanel<'_> {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let tab = &mut *self.tab;
        let snap = cx.engine.snap.as_ref().map(|s| *s.deck(PLAYER));
        let Some((track, s)) = snap.and_then(|s| Some((s.track_id?, s))) else {
            ui.weak("Load a track into the player to trim it.");
            return;
        };
        if s.duration <= 0.0 {
            ui.weak("Loading…");
            return;
        }
        // A new track, or the same one now fully loaded: keep all of it.
        if tab.track != Some(track) || tab.duration != s.duration {
            tab.reset(track, s.duration);
        }
        let grid = cx.engine.deck(PLAYER).grid.clone();
        let snapped = |t: f64, on: bool| match (&grid, on) {
            (Some(g), true) => g.quantize(t).clamp(0.0, s.duration),
            _ => t,
        };

        ui.spacing_mut().item_spacing.y = 8.0;
        ui.label(RichText::new(&cx.engine.deck(PLAYER).title).strong());

        egui::Grid::new("trim-points")
            .num_columns(4)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                for (name, is_start) in [("Start", true), ("End", false)] {
                    ui.label(name);
                    let value = if is_start { tab.start } else { tab.end };
                    let mut v = value;
                    ui.add(
                        egui::DragValue::new(&mut v)
                            .range(0.0..=s.duration)
                            .speed(0.01)
                            .custom_formatter(|v, _| clock(v))
                            .custom_parser(parse_clock),
                    );
                    if ui
                        .add(Button::new("Set to playhead"))
                        .on_hover_text(if grid.is_some() && tab.snap {
                            "Where the player is, to the nearest beat"
                        } else {
                            "Where the player is"
                        })
                        .clicked()
                    {
                        v = snapped(s.position, tab.snap);
                    }
                    if ui
                        .add(Button::subtle("Go"))
                        .on_hover_text("Move the player here")
                        .clicked()
                    {
                        cx.send((PLAYER, EngineCmd::Seek(value)));
                    }
                    ui.end_row();
                    // Start stays before end.
                    if v != value {
                        if is_start {
                            tab.start = v.min(tab.end - 0.1).max(0.0);
                        } else {
                            tab.end = v.max(tab.start + 0.1).min(s.duration);
                        }
                    }
                }
            });

        ui.add_enabled(
            grid.is_some(),
            egui::Checkbox::new(&mut tab.snap, "Snap to beats"),
        )
        .on_disabled_hover_text("The track has no beatgrid yet");

        ui.label(format!(
            "Keeping {} of {}",
            clock(tab.end - tab.start),
            clock(s.duration)
        ));

        ui.horizontal(|ui| {
            if ui.add(Button::new("Play from start")).clicked() {
                cx.send((PLAYER, EngineCmd::Seek(tab.start)));
                cx.send((PLAYER, EngineCmd::Play(Set::On)));
            }
            if ui
                .add(Button::new("Hear the end"))
                .on_hover_text("Play the last few seconds before the end")
                .clicked()
            {
                cx.send((PLAYER, EngineCmd::Seek((tab.end - LEAD_IN).max(tab.start))));
                cx.send((PLAYER, EngineCmd::Play(Set::On)));
            }
            if ui
                .add(Button::subtle("Keep all"))
                .on_hover_text("Put the start and end back at the track's ends")
                .clicked()
            {
                tab.reset(track, s.duration);
            }
        });

        ui.horizontal(|ui| {
            let can = !tab.busy && tab.end - tab.start >= 0.1;
            if ui
                .add_enabled(can, Button::primary("Save as…"))
                .on_hover_text(
                    "Save the part kept as a new file. Its name's extension sets the \
                     format (.mp3, .flac, .wav, .m4a…). Saved in a library folder, it's \
                     added to the library.",
                )
                .clicked()
            {
                cx.send(TrimCmd::SaveAs {
                    track,
                    start: tab.start,
                    end: tab.end,
                });
            }
            if tab.busy {
                ui.spinner();
                ui.label("Saving…");
            }
        });
        match &tab.note {
            Some(Ok(path)) => {
                ui.colored_label(cx.theme.ok, format!("Saved {}", path.display()));
            }
            Some(Err(e)) => {
                ui.colored_label(cx.theme.warn, format!("Couldn't save: {e}"));
            }
            None => {}
        }
    }
}

/// The trim points over the player's waveform: the parts cut off are dimmed, with a
/// line at each cut.
pub struct TrimPoints {
    pub start: f64,
    pub end: f64,
}

impl Layer for TrimPoints {
    fn draw(&self, tl: &Timeline) {
        let r = tl.rect;
        let dim = Color32::from_black_alpha(150);
        let (a, b) = (tl.x_of(self.start), tl.x_of(self.end));
        if a > r.left() {
            let x = a.min(r.right());
            tl.painter
                .rect_filled(Rect::from_x_y_ranges(r.left()..=x, r.y_range()), 0.0, dim);
        }
        if b < r.right() {
            let x = b.max(r.left());
            tl.painter
                .rect_filled(Rect::from_x_y_ranges(x..=r.right(), r.y_range()), 0.0, dim);
        }
        let line = Stroke::new(2.0_f32, THEME.ok);
        for x in [a, b] {
            if (r.left()..=r.right()).contains(&x) {
                tl.painter
                    .line_segment([pos2(x, r.top()), pos2(x, r.bottom())], line);
            }
        }
    }
}

/// "m:ss.mmm".
fn clock(s: f64) -> String {
    let ms = (s.max(0.0) * 1000.0).round() as u64;
    format!("{}:{:02}.{:03}", ms / 60_000, (ms / 1000) % 60, ms % 1000)
}

/// "m:ss.mmm", "m:ss" or plain seconds.
fn parse_clock(text: &str) -> Option<f64> {
    let text = text.trim();
    match text.split_once(':') {
        Some((m, s)) => Some(m.trim().parse::<f64>().ok()? * 60.0 + s.trim().parse::<f64>().ok()?),
        None => text.parse().ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_round_trips() {
        assert_eq!(clock(83.25), "1:23.250");
        assert_eq!(parse_clock("1:23.250"), Some(83.25));
        assert_eq!(parse_clock("12.5"), Some(12.5));
        assert_eq!(parse_clock("x"), None);
    }
}
