//! CUE above play, and the jog ring with the BPM in it.

use std::f32::consts::TAU;

use egui::{Align2, FontId, Sense, Stroke, Ui, pos2, vec2};

use super::{DeckUi, Dims};
use crate::core::bus::DeckControl;
use crate::core::command::Command;
use crate::ui::theme::THEME;
use crate::ui::widgets::{HwButton, Icon, held};

impl DeckUi {
    /// One unit, like Rekordbox's: CUE above play, and the BPM ring to their right.
    pub(super) fn transport(&mut self, ui: &mut Ui, out: &mut Vec<Command>) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            self.cue_play(ui, out);
            self.jog(ui, out);
        });
    }

    pub(super) fn cue_play(&mut self, ui: &mut Ui, out: &mut Vec<Command>) {
        ui.vertical(|ui| {
            let size = vec2(self.dims.transport(), self.dims.transport());
            // CUE lights while the deck sits at its cue point, ready to go.
            let at_cue = !self.state.playing
                && self.state.duration > 0.0
                && (self.state.position - self.state.cue_point).abs() < 0.01;
            let r = ui
                .add(
                    HwButton::new("CUE", THEME.cue_button, size)
                        .round(true)
                        .font(11.0)
                        .tinted(true)
                        .lit(at_cue && self.pending.is_none()),
                )
                .on_hover_text("Cue: set, or jump back and preview while held");
            if let Some(d) = held(ui, &r) {
                self.control(out, DeckControl::Cue, d as u8 as f64);
            }
            let playing = self.state.playing;
            let r = ui
                .add(
                    HwButton::new("Play", THEME.play_button, size)
                        .round(true)
                        .icon(Icon::PlayPause)
                        .font(12.0)
                        .tinted(true)
                        .lit(playing),
                )
                .on_hover_text("Play / pause");
            if held(ui, &r) == Some(true) {
                self.control(out, DeckControl::Play, 1.0);
            }
        });
    }

    /// A jog-wheel ring, like Rekordbox's: the BPM at the current tempo in the middle,
    /// the tempo change and fader range under it, VINYL over it (lit in vinyl mode;
    /// a click turns it on or off), and a dot on the rim that turns like a record at
    /// 33⅓ rpm (1.8 s a turn).
    pub(super) fn jog(&self, ui: &mut Ui, out: &mut Vec<Command>) {
        let s = self.state;
        let jog = self.dims.jog();
        // The text scales with the ring (it is smaller on a compact deck).
        let k = jog / Dims::FULL.jog();
        let (rect, resp) = ui.allocate_exact_size(vec2(jog, jog), Sense::click());
        let resp = resp.on_hover_text(
            "BPM at the current tempo; the tempo change and the fader's range. \
             VINYL: the jog wheel's top and the waveform scratch (click to turn off: \
             then the jog wheel only bends)",
        );
        if resp.clicked() {
            self.control(out, DeckControl::Vinyl, 1.0);
        }
        let p = ui.painter();
        let c = rect.center();
        let r = jog / 2.0 - 2.0 * k;
        p.circle(c, r, THEME.jog_face, Stroke::new(3.0_f32, self.colour()));
        if self.pending.is_none() {
            let a = ((self.shown / 1.8).fract() as f32) * TAU - TAU / 4.0;
            p.circle_filled(c + vec2(a.cos(), a.sin()) * r, 3.5, THEME.jog_dot);
        }
        let bpm = match &self.pending {
            Some(pl) => pl.bpm.map(|b| b * (1.0 + s.rate)),
            None => s.bpm,
        };
        // "124" large, ".00" small, sitting on one baseline.
        let (whole, frac) = match bpm {
            Some(b) => {
                let t = format!("{b:.2}");
                let (w, f) = t.split_at(t.len() - 3);
                (w.to_string(), f.to_string())
            }
            None => ("-".into(), String::new()),
        };
        let big = p.layout_no_wrap(whole, FontId::monospace(20.0 * k), THEME.text);
        let small = p.layout_no_wrap(frac, FontId::monospace(12.0 * k), THEME.text);
        let w = big.size().x + small.size().x;
        let top = c.y - big.size().y + 4.0 * k;
        let left = c.x - w / 2.0;
        let small_top = top + big.size().y - small.size().y - 2.0 * k;
        p.galley(pos2(left, top), big.clone(), THEME.text);
        p.galley(pos2(left + big.size().x, small_top), small, THEME.text);
        let pct = p.layout_no_wrap(
            format!("{:+.1}%", s.rate * 100.0),
            FontId::monospace(9.5 * k),
            THEME.muted,
        );
        let range = p.layout_no_wrap(
            format!(" {}", s.range.label()),
            FontId::monospace(9.5 * k),
            self.colour(),
        );
        let w = pct.size().x + range.size().x;
        let y = c.y + 7.0 * k;
        p.galley(pos2(c.x - w / 2.0, y), pct.clone(), THEME.muted);
        p.galley(pos2(c.x - w / 2.0 + pct.size().x, y), range, self.colour());
        let vinyl = if s.vinyl { self.colour() } else { THEME.line };
        p.text(
            pos2(c.x, top - 2.0 * k),
            Align2::CENTER_BOTTOM,
            "VINYL",
            FontId::proportional(8.0 * k),
            vinyl,
        );
    }
}
