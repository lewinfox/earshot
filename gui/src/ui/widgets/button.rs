//! Lit buttons like a controller's: CUE, play, hot cue pads, SYNC, headphone cue.
//! They light in their colour when on; [`held`] gives press and release edges for
//! buttons that act while held.

use egui::{
    Align2, Color32, CornerRadius, FontId, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui,
    Widget, pos2, vec2,
};

use crate::ui::theme::THEME;
use crate::ui::util;

/// Symbols drawn as shapes, for glyphs the font lacks. [`HwButton`] and
/// [`RoundButton`](super::RoundButton) draw them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    /// ▶ ❚❚, like the play button on Mixxx and most controllers.
    PlayPause,
    /// ▶
    Play,
    /// ❚❚
    Pause,
    /// ■
    Stop,
    /// A bar line, red over white, as Rekordbox marks "put the 1 here".
    Downbeat,
}

impl Icon {
    /// Draw the icon about `size` points tall, centred in `rect`, in `colour` (the
    /// downbeat keeps its own colours and fills the rect's height).
    pub fn paint(self, p: &egui::Painter, rect: Rect, size: f32, colour: Color32) {
        let c = rect.center();
        match self {
            Icon::PlayPause => play_pause(p, rect, size, colour),
            Icon::Play => {
                // Right of centre: a triangle's weight is towards its flat side.
                let (h, w) = (size, size * 0.85);
                let left = c.x - w * 0.4;
                p.add(Shape::convex_polygon(
                    vec![
                        pos2(left, c.y - h / 2.0),
                        pos2(left + w, c.y),
                        pos2(left, c.y + h / 2.0),
                    ],
                    colour,
                    Stroke::NONE,
                ));
            }
            Icon::Pause => {
                for dx in [-0.27, 0.27] {
                    p.rect_filled(
                        Rect::from_center_size(
                            c + vec2(dx * size, 0.0),
                            vec2(size * 0.3, size * 0.9),
                        ),
                        1,
                        colour,
                    );
                }
            }
            Icon::Stop => {
                p.rect_filled(
                    Rect::from_center_size(c, vec2(size, size) * 0.75),
                    1,
                    colour,
                );
            }
            Icon::Downbeat => {
                let (half, w) = (rect.height() * 0.32, 3.0);
                p.rect_filled(
                    Rect::from_min_max(pos2(c.x - w / 2.0, c.y - half), pos2(c.x + w / 2.0, c.y)),
                    0,
                    THEME.downbeat,
                );
                p.rect_filled(
                    Rect::from_min_max(pos2(c.x - w / 2.0, c.y), pos2(c.x + w / 2.0, c.y + half)),
                    0,
                    THEME.marker,
                );
            }
        }
    }
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
    /// Small text under the label (a hot cue's name), cut off at the button's edges.
    caption: Option<&'a str>,
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
            caption: None,
        }
    }

    pub fn caption(mut self, caption: Option<&'a str>) -> Self {
        self.caption = caption;
        self
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
            (self.colour, self.colour, THEME.ink)
        } else if self.tinted {
            (util::dim(self.colour, 0.25), self.colour, self.colour)
        } else {
            (THEME.button, THEME.line, THEME.text)
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
            if self.round { 1.5 } else { 1.0_f32 },
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
            Some(icon) => icon.paint(p, rect, self.font, text),
            None => match self.caption {
                Some(caption) => {
                    p.text(
                        rect.center() - vec2(0.0, self.font * 0.45),
                        Align2::CENTER_CENTER,
                        self.label,
                        FontId::proportional(self.font),
                        text,
                    );
                    // Left-aligned once it's too long to centre, and cut off.
                    let galley = p.layout_no_wrap(caption.into(), FontId::proportional(8.5), text);
                    let inner = rect.shrink(3.0);
                    let x = if galley.size().x > inner.width() {
                        inner.left()
                    } else {
                        rect.center().x - galley.size().x / 2.0
                    };
                    p.with_clip_rect(inner).galley(
                        pos2(x, rect.center().y + self.font * 0.2),
                        galley,
                        text,
                    );
                }
                None => {
                    p.text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        self.label,
                        FontId::proportional(self.font),
                        text,
                    );
                }
            },
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
/// `Some(false)` on release. Only the primary button presses (a right-click opens a
/// menu, where there is one).
pub fn held(ui: &Ui, r: &Response) -> Option<bool> {
    let down = r.is_pointer_button_down_on() && ui.input(|i| i.pointer.primary_down());
    let id = r.id.with("held");
    let was = ui.ctx().data(|d| d.get_temp::<bool>(id).unwrap_or(false));
    if down == was {
        return None;
    }
    ui.ctx().data_mut(|d| d.insert_temp(id, down));
    Some(down)
}
