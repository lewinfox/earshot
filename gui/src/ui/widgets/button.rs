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

/// Room left between a button's label and its edges.
const LABEL_PAD: f32 = 4.0;
/// The smallest a label is shrunk to, to fit a fixed width.
const MIN_FONT: f32 = 7.0;

/// A controller-style button. Its size is fixed by default: a label too wide for it is
/// drawn smaller, so the button never changes size with its label (a loop's "4" or
/// "1/32") and stays lined up with the buttons around it. [`HwButton::min_width`]
/// lets it grow to fit its label instead.
pub struct HwButton<'a> {
    label: &'a str,
    icon: Option<Icon>,
    colour: Color32,
    lit: bool,
    size: egui::Vec2,
    /// `size.x` is the least it is: wider when its label needs it.
    grow: bool,
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
            grow: false,
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

    /// Treat the width given as a minimum, growing to fit the label, instead of
    /// shrinking the label to fit it.
    #[cfg_attr(not(test), allow(dead_code))] // every button is fixed for now
    pub fn min_width(mut self) -> Self {
        self.grow = true;
        self
    }

    /// Its size, and the label's font size, for a label `text_w` wide at the font
    /// size asked for.
    fn fit(&self, text_w: f32) -> (egui::Vec2, f32) {
        let room = self.size.x - 2.0 * LABEL_PAD;
        if self.icon.is_some() || self.round || text_w <= room {
            (self.size, self.font)
        } else if self.grow {
            (vec2(text_w + 2.0 * LABEL_PAD, self.size.y), self.font)
        } else {
            let font = (self.font * room / text_w).max(MIN_FONT);
            (self.size, font)
        }
    }
}

impl Widget for HwButton<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let text_w = ui
            .painter()
            .layout_no_wrap(
                self.label.into(),
                FontId::proportional(self.font),
                Color32::WHITE,
            )
            .size()
            .x;
        let (size, font) = self.fit(text_w);
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
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
            Some(icon) => icon.paint(p, rect, font, text),
            None => match self.caption {
                Some(caption) => {
                    p.text(
                        rect.center() - vec2(0.0, font * 0.45),
                        Align2::CENTER_CENTER,
                        self.label,
                        FontId::proportional(font),
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
                        pos2(x, rect.center().y + font * 0.2),
                        galley,
                        text,
                    );
                }
                None => {
                    p.text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        self.label,
                        FontId::proportional(font),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fixed_button_shrinks_its_label_and_a_growing_one_widens() {
        let b = HwButton::new("⟲ 1/32", THEME.text, vec2(40.0, 28.0)).font(10.0);
        assert_eq!(b.fit(20.0), (vec2(40.0, 28.0), 10.0), "fits as it is");
        let (size, font) = b.fit(40.0);
        assert_eq!(size, vec2(40.0, 28.0), "same size whatever the label");
        assert!((font - 8.0).abs() < 1e-4, "{font}");
        assert_eq!(b.fit(500.0).1, MIN_FONT);
        let b = b.min_width();
        assert_eq!(b.fit(48.0), (vec2(56.0, 28.0), 10.0));
        assert_eq!(b.fit(20.0).0, vec2(40.0, 28.0), "never narrower than asked");
    }
}
