//! CUE above play, and the jog ring with the BPM in it.

use std::f32::consts::TAU;

use egui::{Align2, FontId, Pos2, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};

use super::DeckUi;
use super::design::*;
use crate::core::bus::DeckControl;
use crate::core::command::Command;
use crate::ui::theme::THEME;
use crate::ui::widgets::{HwButton, Icon, held};

impl DeckUi {
    /// One unit, like Rekordbox's: CUE above play, and the BPM ring to their right.
    pub(super) fn transport(&mut self, ui: &mut Ui, out: &mut Vec<Command>) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = self.dims.px(TRANSPORT_GAP);
            self.cue_play(ui, out);
            self.jog(ui, out);
        });
    }

    pub(super) fn cue_play(&mut self, ui: &mut Ui, out: &mut Vec<Command>) {
        ui.vertical(|ui| {
            let d = self.dims;
            let size = vec2(d.px(CUE_D), d.px(CUE_D));
            // CUE lights while the deck sits at its cue point, ready to go.
            let at_cue = !self.state.playing
                && self.state.duration > 0.0
                && (self.state.position - self.state.cue_point).abs() < 0.01;
            let r = ui
                .add(
                    HwButton::new("CUE", THEME.cue_button, size)
                        .round(true)
                        .font(d.px(FONT_CUE))
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
                        .font(d.px(FONT_PLAY))
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
    ///
    /// Under the mouse, as in Rekordbox: − and + under the tempo (nudge slower or
    /// faster while held) and two arrows outside the rim at the lower left (turn the
    /// jog wheel back or forward while held: a bend while playing, a slow move while
    /// stopped). The ring leaves room round itself for the arrows, and a little more
    /// on the left so they keep clear of CUE and play.
    pub(super) fn jog(&self, ui: &mut Ui, out: &mut Vec<Command>) {
        let s = self.state;
        let d = self.dims;
        let k = d.k;
        let jog = d.jog();
        let (rect, resp) = ui.allocate_exact_size(vec2(jog + d.px(JOG_LEFT), jog), Sense::hover());
        let c = pos2(rect.right() - jog / 2.0, rect.center().y);
        let r = jog / 2.0 - d.px(JOG_ROOM);
        let p = ui.painter();
        p.circle(
            c,
            r,
            THEME.jog_face,
            Stroke::new(3.0 * k.max(0.8), self.colour()),
        );
        if self.pending.is_none() {
            let a = ((self.shown / 1.8).fract() as f32) * TAU - TAU / 4.0;
            p.circle_filled(
                c + vec2(a.cos(), a.sin()) * r,
                3.5 * k.max(0.8),
                THEME.jog_dot,
            );
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
        let big = p.layout_no_wrap(whole, FontId::monospace(d.px(FONT_BPM)), THEME.text);
        let small = p.layout_no_wrap(frac, FontId::monospace(d.px(FONT_BPM_FRAC)), THEME.text);
        let w = big.size().x + small.size().x;
        let top = c.y - big.size().y + d.px(BPM_DROP);
        let left = c.x - w / 2.0;
        // Their baselines level: the smaller font's descent is that much less.
        let descent = d.px((FONT_BPM - FONT_BPM_FRAC) * 0.25);
        let small_top = top + big.size().y - small.size().y - descent;
        p.galley(pos2(left, top), big.clone(), THEME.text);
        p.galley(pos2(left + big.size().x, small_top), small, THEME.text);
        // Each part of the ring's text has its own tooltip; the ring itself has none.
        let bpm_rect = Rect::from_min_size(pos2(left, top), vec2(w, big.size().y));
        let tip = |rect: Rect, key: &str, text: &str| {
            ui.interact(rect, resp.id.with(key), Sense::hover())
                .on_hover_text(text);
        };
        tip(bpm_rect, "bpm", "BPM at the current tempo");
        let pct = p.layout_no_wrap(
            format!("{:+.1}%", s.rate * 100.0),
            FontId::monospace(d.px(FONT_TEMPO)),
            THEME.muted,
        );
        let range = p.layout_no_wrap(
            format!(" {}", s.range.label()),
            FontId::monospace(d.px(FONT_TEMPO)),
            self.colour(),
        );
        let w = pct.size().x + range.size().x;
        let y = c.y + d.px(TEMPO_DROP);
        p.galley(pos2(c.x - w / 2.0, y), pct.clone(), THEME.muted);
        let range_size = range.size();
        p.galley(pos2(c.x - w / 2.0 + pct.size().x, y), range, self.colour());
        tip(
            Rect::from_min_size(pos2(c.x - w / 2.0, y), pct.size()),
            "pct",
            "Tempo change: how far the pitch fader is from the track's own BPM",
        );
        tip(
            Rect::from_min_size(pos2(c.x - w / 2.0 + pct.size().x, y), range_size),
            "range",
            "The pitch fader's range",
        );
        // VINYL: the only part that turns vinyl mode on or off, so a stray click on
        // the ring can't.
        let label = pos2(c.x, top - d.px(VINYL_GAP));
        let text = p.layout_no_wrap(
            "VINYL".into(),
            FontId::proportional(d.px(FONT_VINYL)),
            THEME.text,
        );
        let vinyl_rect = Align2::CENTER_BOTTOM
            .anchor_size(label, text.size())
            .expand(d.px(2.0));
        let v = ui
            .interact(vinyl_rect, resp.id.with("vinyl"), Sense::click())
            .on_hover_text(
                "Vinyl mode: the jog wheel's top and the waveform scratch (click to turn \
                 off: then the jog wheel only bends)",
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        v.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::Button, true, s.vinyl, "VINYL")
        });
        if held(ui, &v) == Some(true) {
            self.control(out, DeckControl::Vinyl, 1.0);
        }
        let ink = match (s.vinyl, v.hovered()) {
            (true, _) => self.colour(),
            (false, true) => THEME.muted,
            (false, false) => THEME.line,
        };
        ui.painter()
            .galley(vinyl_rect.shrink(d.px(2.0)).min, text, ink);
        self.jog_nudges(ui, out, resp.id, rect, c, r, k);
    }

    /// The jog ring's hover controls: see [`DeckUi::jog`]. They are always live (so a
    /// press carried off the ring still lets go), but only drawn under the mouse.
    #[allow(clippy::too_many_arguments)]
    fn jog_nudges(
        &self,
        ui: &mut Ui,
        out: &mut Vec<Command>,
        base: egui::Id,
        rect: Rect,
        c: Pos2,
        r: f32,
        k: f32,
    ) {
        let mut shown = ui.rect_contains_pointer(rect);
        // − and + side by side under the tempo change: filled squares, apart.
        let (size, gap) = (10.0 * k, 6.0 * k);
        let top = c.y + 0.5 * r;
        let buttons = [
            (-1.0, DeckControl::NudgeDown, "Nudge slower (hold)"),
            (1.0, DeckControl::NudgeUp, "Nudge faster (hold)"),
        ];
        let mut drawn = Vec::new();
        for (side, control, tip) in buttons {
            let left = if side < 0.0 {
                c.x - gap / 2.0 - size
            } else {
                c.x + gap / 2.0
            };
            let b = Rect::from_min_size(pos2(left, top), vec2(size, size));
            let id = base.with(("jog_nudge", side as i32));
            let resp = ui.interact(b, id, Sense::click()).on_hover_text(tip);
            if let Some(d) = held(ui, &resp) {
                self.control(out, control, d as u8 as f64);
            }
            shown |= resp.is_pointer_button_down_on();
            drawn.push((b, side, resp.hovered() || resp.is_pointer_button_down_on()));
        }
        // Two half-arrows hugging the rim at the lower left, pointing apart with a dot
        // between: forward turns clockwise (up the left side), back anticlockwise
        // (along the bottom). Each answers to the box round it, kept in the ring's own
        // space so it can't reach CUE or play.
        let r0 = r + ARROW_GAP * k;
        let mut arrows = Vec::new();
        for (dir, tip) in [
            (1.0_f32, "Turn the jog wheel forward (hold)"),
            (-1.0, "Turn the jog wheel back (hold)"),
        ] {
            let outline = half_arrow(c, r0, k, dir);
            let hit = Rect::from_points(&outline).expand(3.0 * k).intersect(rect);
            let id = base.with(("jog_turn", dir as i32));
            let resp = ui.interact(hit, id, Sense::click()).on_hover_text(tip);
            let down = resp.is_pointer_button_down_on() && ui.input(|i| i.pointer.primary_down());
            if down {
                let dt = ui.input(|i| i.stable_dt).min(0.1) as f64;
                self.control(out, DeckControl::JogBend, dir as f64 * JOG_ARROW * dt);
                ui.ctx().request_repaint();
            }
            shown |= down;
            arrows.push((outline, resp.hovered() || down));
        }
        if !shown {
            return;
        }
        let p = ui.painter();
        for (b, side, hot) in drawn {
            let fill = if hot { THEME.muted } else { THEME.line };
            p.rect_filled(b, 1.5 * k, fill);
            let ink = Stroke::new(1.5 * k.max(0.8), THEME.text);
            let (m, h) = (b.center(), b.width() * 0.32);
            p.line_segment([m - vec2(h, 0.0), m + vec2(h, 0.0)], ink);
            if side > 0.0 {
                p.line_segment([m - vec2(0.0, h), m + vec2(0.0, h)], ink);
            }
        }
        for (outline, hot) in arrows {
            let ink = if hot {
                THEME.text
            } else {
                THEME.text.gamma_multiply(0.85)
            };
            fill_strip(p, &outline, ink);
        }
        let a = ARROW_AT.to_radians();
        p.circle_filled(
            c + vec2(a.cos(), a.sin()) * (r0 + ARROW_SHAFT * k / 2.0),
            1.2 * k.max(0.8),
            THEME.text.gamma_multiply(0.6),
        );
    }
}

/// The jog arrows, at full size: the dot between them (degrees clockwise from 3
/// o'clock), the gap from the dot to each tail and how far each reaches (degrees),
/// the gap between the rim and their flat side, the shaft's thickness, the barb's
/// height and the head's share of the length.
const ARROW_AT: f32 = 135.0;
const ARROW_FROM: f32 = 4.0;
const ARROW_SPAN: f32 = 20.0;
const ARROW_GAP: f32 = 5.0;
const ARROW_SHAFT: f32 = 2.2;
const ARROW_BARB: f32 = 6.5;
const ARROW_HEAD: f32 = 0.45;

/// One jog arrow: the outer half of `=>`, bent round the rim. Its flat side runs along
/// radius `r0`; the shaft and then the barb stand out from it, and the barb tapers to
/// the tip on the flat side. `dir` 1 runs clockwise from the dot, -1 anticlockwise.
///
/// Returned as pairs along its length, (flat side, outer edge), for [`fill_strip`].
fn half_arrow(c: Pos2, r0: f32, k: f32, dir: f32) -> Vec<Pos2> {
    let at = |deg: f32, rad: f32| {
        let a = deg.to_radians();
        c + vec2(a.cos(), a.sin()) * rad
    };
    let (shaft, barb) = (ARROW_SHAFT * k, ARROW_BARB * k);
    let tail = ARROW_AT + dir * ARROW_FROM;
    let mut pairs = Vec::new();
    let mut push = |u: f32, width: f32| {
        let deg = tail + dir * ARROW_SPAN * u;
        pairs.push(at(deg, r0));
        pairs.push(at(deg, r0 + width));
    };
    let neck = 1.0 - ARROW_HEAD;
    const N: usize = 8;
    for i in 0..=N {
        push(neck * i as f32 / N as f32, shaft);
    }
    for i in 0..=N {
        let s = i as f32 / N as f32;
        push(neck + ARROW_HEAD * s, barb * (1.0 - s));
    }
    pairs
}

/// Fill a strip given as (inner, outer) pairs, with an outline for smooth edges (a
/// mesh isn't antialiased). Unlike a convex polygon it can bend and step.
fn fill_strip(p: &egui::Painter, pairs: &[Pos2], ink: egui::Color32) {
    let mut mesh = egui::Mesh::default();
    for &v in pairs {
        mesh.colored_vertex(v, ink);
    }
    for i in 0..(pairs.len() / 2 - 1) as u32 {
        let (a, b, c, d) = (2 * i, 2 * i + 1, 2 * i + 2, 2 * i + 3);
        mesh.add_triangle(a, b, c);
        mesh.add_triangle(b, d, c);
    }
    p.add(Shape::mesh(mesh));
    // Round the edge: out along the flat side, back along the outer one.
    let mut edge: Vec<Pos2> = pairs.iter().step_by(2).copied().collect();
    edge.extend(pairs.iter().skip(1).step_by(2).rev());
    p.add(Shape::closed_line(edge, Stroke::new(0.6_f32, ink)));
}

/// How fast a held jog arrow turns the wheel, in seconds of track a second: a bend
/// of about 8 % while playing, or 80 ms a second through a stopped track.
const JOG_ARROW: f64 = 0.4;
