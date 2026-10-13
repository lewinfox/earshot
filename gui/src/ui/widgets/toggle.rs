//! On/off and one-of-a-few choices, outside Perform's hardware. [`Toggle`] is a small
//! switch with its label ("follow playhead"); [`segmented`] is a row of joined
//! segments with one lit ("Original | Straightened", "Drop | Hold | Build").

use egui::{
    Align2, CornerRadius, FontId, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, Widget,
    vec2,
};

use crate::ui::theme::THEME;
use crate::ui::util;

const TEXT_SIZE: f32 = 12.5;

/// A switch and its label; clicking either flips `on` and marks the response changed.
/// Read to screen readers (and tests) as a checkbox named by the label.
pub struct Toggle<'a> {
    on: &'a mut bool,
    label: &'a str,
}

impl<'a> Toggle<'a> {
    pub fn new(on: &'a mut bool, label: &'a str) -> Self {
        Toggle { on, label }
    }
}

impl Widget for Toggle<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let track = vec2(26.0, 14.0);
        let galley = ui.painter().layout_no_wrap(
            self.label.into(),
            FontId::proportional(TEXT_SIZE),
            egui::Color32::PLACEHOLDER,
        );
        let gap = 6.0;
        let size = vec2(
            track.x + gap + galley.size().x,
            track
                .y
                .max(galley.size().y)
                .max(ui.spacing().interact_size.y),
        );
        let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
        if resp.clicked() {
            *self.on = !*self.on;
            resp.mark_changed();
        }
        let on = *self.on;
        resp.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), on, self.label)
        });
        if !ui.is_rect_visible(rect) {
            return resp;
        }
        let enabled = ui.is_enabled();
        let hover = enabled && resp.hovered();
        // Eases the knob across.
        let t = ui.ctx().animate_bool_responsive(resp.id, on);
        let track = Rect::from_center_size(rect.left_center() + vec2(track.x / 2.0, 0.0), track);
        let fill = if on {
            THEME.accent
        } else if hover {
            THEME.hover_line
        } else {
            THEME.rail
        };
        let p = ui.painter();
        p.rect_filled(track, CornerRadius::same(7), fill);
        if resp.has_focus() {
            p.rect_stroke(
                track.expand(2.0),
                CornerRadius::same(9),
                Stroke::new(1.5_f32, THEME.selection),
                StrokeKind::Inside,
            );
        }
        let r = track.height() / 2.0 - 2.0;
        let x = egui::lerp(track.left() + r + 2.0..=track.right() - r - 2.0, t);
        p.circle_filled(egui::pos2(x, track.center().y), r, THEME.marker);
        let colour = if !enabled {
            THEME.muted
        } else if hover {
            THEME.marker
        } else {
            THEME.text
        };
        p.galley(
            egui::pos2(track.right() + gap, rect.center().y - galley.size().y / 2.0),
            galley,
            colour,
        );
        resp
    }
}

/// One choice in a [`segmented`] row.
pub struct Seg<'a, T> {
    value: T,
    label: &'a str,
    tip: Option<&'a str>,
    disabled_tip: Option<&'a str>,
    enabled: bool,
}

impl<'a, T> Seg<'a, T> {
    pub fn new(value: T, label: &'a str) -> Self {
        Seg {
            value,
            label,
            tip: None,
            disabled_tip: None,
            enabled: true,
        }
    }

    pub fn tip(mut self, tip: &'a str) -> Self {
        self.tip = Some(tip);
        self
    }

    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }

    /// Why it can't be chosen, shown while it's disabled.
    pub fn disabled_tip(mut self, tip: &'a str) -> Self {
        self.disabled_tip = Some(tip);
        self
    }
}

/// A row of joined segments, `current` lit. Returns the segment clicked, if it's
/// another. Each segment reads as a selectable button named by its label.
pub fn segmented<T: Copy + PartialEq>(ui: &mut Ui, current: T, segs: &[Seg<'_, T>]) -> Option<T> {
    let mut picked = None;
    let pad_x = 8.0;
    let font = FontId::proportional(TEXT_SIZE);
    // As tall as a button beside it.
    let height = ui.spacing().interact_size.y + 2.0;
    let widths: Vec<f32> = segs
        .iter()
        .map(|s| {
            let text = ui
                .painter()
                .layout_no_wrap(s.label.into(), font.clone(), THEME.text);
            text.size().x + 2.0 * pad_x
        })
        .collect();
    let size = vec2(widths.iter().sum(), height);
    // Left to right even in a right-to-left row, so the order reads the same anywhere.
    let layout = egui::Layout::left_to_right(egui::Align::Center);
    ui.allocate_ui_with_layout(size, layout, |ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        // The frame round them all, drawn once their size is known.
        let back = ui.painter().add(Shape::Noop);
        let mut whole = Rect::NOTHING;
        for (i, (s, &w)) in segs.iter().zip(&widths).enumerate() {
            let on = s.value == current;
            let r = ui
                .add_enabled_ui(s.enabled, |ui| {
                    let (rect, resp) = ui.allocate_exact_size(vec2(w, height), Sense::click());
                    resp.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::WidgetType::Button,
                            ui.is_enabled(),
                            on,
                            s.label,
                        )
                    });
                    let hover = s.enabled && resp.hovered();
                    let p = ui.painter();
                    // Corners round only at the row's ends.
                    let (first, last) = (i == 0, i + 1 == segs.len());
                    let radius = CornerRadius {
                        nw: if first { 4 } else { 0 },
                        sw: if first { 4 } else { 0 },
                        ne: if last { 4 } else { 0 },
                        se: if last { 4 } else { 0 },
                    };
                    if on {
                        p.rect_filled(rect, radius, THEME.selection);
                    } else if hover {
                        p.rect_filled(rect, radius, util::dim(THEME.panel_2, 1.4));
                    }
                    // Dividers between unlit segments.
                    if !first && !on && segs[i - 1].value != current {
                        p.vline(
                            rect.left(),
                            rect.y_range().shrink(4.0),
                            Stroke::new(1.0_f32, THEME.line),
                        );
                    }
                    if resp.has_focus() {
                        p.rect_stroke(
                            rect.shrink(1.0),
                            radius,
                            Stroke::new(1.5_f32, THEME.selection),
                            StrokeKind::Inside,
                        );
                    }
                    let colour = if !s.enabled {
                        util::dim(THEME.muted, 0.7)
                    } else if on || hover {
                        THEME.text
                    } else {
                        THEME.muted
                    };
                    p.text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        s.label,
                        font.clone(),
                        colour,
                    );
                    resp
                })
                .inner;
            whole = whole.union(r.rect);
            let r = match s.tip {
                Some(t) => r.on_hover_text(t),
                None => r,
            };
            let r = match s.disabled_tip {
                Some(t) => r.on_disabled_hover_text(t),
                None => r,
            };
            if r.clicked() && !on {
                picked = Some(s.value);
            }
        }
        ui.painter().set(
            back,
            Shape::rect_filled(whole, CornerRadius::same(4), THEME.panel_2),
        );
        ui.painter().rect_stroke(
            whole,
            CornerRadius::same(4),
            Stroke::new(1.0_f32, THEME.line),
            StrokeKind::Inside,
        );
    });
    picked
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    #[test]
    fn toggle_flips_and_reports_a_change() {
        let mut h = Harness::new_ui_state(
            |ui, (on, changes): &mut (bool, u32)| {
                if ui.add(Toggle::new(on, "follow playhead")).changed() {
                    *changes += 1;
                }
            },
            (false, 0),
        );
        h.get_by_label("follow playhead").click();
        h.run();
        assert_eq!(*h.state(), (true, 1));
        h.get_by_label("follow playhead").click();
        h.run();
        assert_eq!(*h.state(), (false, 2));
    }

    #[test]
    fn segmented_picks_only_enabled_others() {
        let mut h = Harness::new_ui_state(
            |ui, v: &mut u8| {
                let segs = [
                    Seg::new(0, "Drop"),
                    Seg::new(1, "Hold"),
                    Seg::new(2, "Build").enabled(false),
                ];
                if let Some(p) = segmented(ui, *v, &segs) {
                    *v = p;
                }
            },
            0,
        );
        h.get_by_label("Hold").click();
        h.run();
        assert_eq!(*h.state(), 1);
        h.get_by_label("Build").click();
        h.run();
        assert_eq!(*h.state(), 1);
    }
}
