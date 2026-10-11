//! Lit buttons like a controller's: CUE, play, hot cue pads, SYNC, headphone cue.
//! They light in their colour when on; [`held`] gives press and release edges for
//! buttons that act while held.

use egui::{
    Align2, Color32, CornerRadius, FontId, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui,
    Widget, pos2, vec2,
};

use super::BUTTON;
use crate::ui::util;

/// Symbols drawn as shapes, for glyphs the font lacks.
#[derive(Clone, Copy)]
pub enum Icon {
    /// ▶ ❚❚, like the play button on Mixxx and most controllers.
    PlayPause,
}

pub struct HwButton<'a> {
    label: &'a str,
    icon: Option<Icon>,
    colour: Color32,
    lit: bool,
    size: egui::Vec2,
    font: f32,
    /// Show the colour on the border and label even when off (a set-but-idle hot cue).
    tinted: bool,
    /// A circle (like a CDJ's play and cue buttons) as wide as the button is tall.
    round: bool,
}

impl<'a> HwButton<'a> {
    pub fn new(label: &'a str, colour: Color32, size: egui::Vec2) -> HwButton<'a> {
        HwButton {
            label,
            icon: None,
            colour,
            lit: false,
            size,
            font: 11.0,
            tinted: false,
            round: false,
        }
    }

    pub fn lit(mut self, on: bool) -> Self {
        self.lit = on;
        self
    }

    /// Draw this instead of the label (which is still the accessible name).
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn tinted(mut self, on: bool) -> Self {
        self.tinted = on;
        self
    }

    pub fn round(mut self, on: bool) -> Self {
        self.round = on;
        self
    }

    pub fn font(mut self, size: f32) -> Self {
        self.font = size;
        self
    }
}

impl Widget for HwButton<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let (rect, resp) = ui.allocate_exact_size(self.size, Sense::click_and_drag());
        resp.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::Button, true, self.lit, self.label)
        });
        if !ui.is_rect_visible(rect) {
            return resp;
        }
        let down = resp.is_pointer_button_down_on();
        let (fill, border, text) = if self.lit {
            (
                self.colour,
                self.colour,
                Color32::from_rgb(0x10, 0x10, 0x12),
            )
        } else if self.tinted {
            (util::dim(self.colour, 0.25), self.colour, self.colour)
        } else {
            (BUTTON, util::LINE, util::TEXT)
        };
        let fill = if down {
            util::dim(fill, 0.75)
        } else if resp.hovered() && !self.lit {
            util::dim(fill, 1.35)
        } else {
            fill
        };
        let p = ui.painter();
        let stroke = Stroke::new(
            if self.round { 1.5 } else { 1.0 },
            if resp.hovered() {
                util::dim(border, 1.3)
            } else {
                border
            },
        );
        if self.round {
            let r = rect.width().min(rect.height()) / 2.0 - 1.0;
            p.circle(rect.center(), r, fill, stroke);
        } else {
            p.rect(
                rect.shrink(0.5),
                CornerRadius::same(3),
                fill,
                stroke,
                StrokeKind::Inside,
            );
        }
        match self.icon {
            Some(Icon::PlayPause) => play_pause(p, rect, self.font, text),
            None => {
                p.text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    self.label,
                    FontId::proportional(self.font),
                    text,
                );
            }
        }
        resp
    }
}

/// ▶ ❚❚, `size` points tall, centred in `rect`.
fn play_pause(p: &egui::Painter, rect: Rect, size: f32, colour: Color32) {
    let h = size * 0.8;
    let c = rect.center();
    let left = c.x - h * 0.95;
    p.add(Shape::convex_polygon(
        vec![
            pos2(left, c.y - h / 2.0),
            pos2(left + h * 0.8, c.y),
            pos2(left, c.y + h / 2.0),
        ],
        colour,
        Stroke::NONE,
    ));
    for i in 0..2 {
        let x = c.x + h * 0.2 + i as f32 * h * 0.45;
        p.rect_filled(
            Rect::from_min_size(pos2(x, c.y - h / 2.0), vec2(h * 0.25, h)),
            0.0,
            colour,
        );
    }
}

/// Press and release edges of a button that acts while held: `Some(true)` on press,
/// `Some(false)` on release.
pub fn held(ui: &Ui, r: &Response) -> Option<bool> {
    let down = r.is_pointer_button_down_on();
    let id = r.id.with("held");
    let was = ui.ctx().data(|d| d.get_temp::<bool>(id).unwrap_or(false));
    if down == was {
        return None;
    }
    ui.ctx().data_mut(|d| d.insert_temp(id, down));
    Some(down)
}
