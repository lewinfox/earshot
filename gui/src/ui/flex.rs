//! Flexbox layout: how the window, its rows and its panels share out space.
//!
//! Layout is [taffy](https://docs.rs/taffy) (CSS flexbox) through `egui_taffy`: a view
//! says what its parts are and how they share a row or a column, and taffy works out
//! the sizes. Nothing is sized by hand from the window's size any more.
//!
//! - [`root`] starts a layout in the space a `Ui` has.
//! - Containers ([`TuiBuilderLogic::add`] with [`row`] or [`col`]) hold parts.
//! - [`pane`] is a part whose size is all the layout's: what's drawn in it fills its
//!   box and is cut off at its edges (a waveform, a list, a canvas).
//! - [`fit`] is a part as big as what's drawn in it (a header, a bar of buttons).
//! - [`framed`] is a container drawn as a panel (fill, border, rounded corners), which
//!   hands its children the box it was given (the deck works out its scale from it).
//!
//! Styles are plain [`Style`]s, built with [`row`], [`col`] and [`leaf`] and the
//! [`Flex`] methods, which read like their CSS:
//!
//! | here | CSS |
//! |---|---|
//! | `.w(36.0)` | `width: 36px; flex-shrink: 0` |
//! | `.grow()` | `flex: 1 1 0` |
//! | `.basis_pct(0.3)` | `flex: 0 1 30%` |
//! | `.min_w(240.0)` | `min-width: 240px` |
//! | `.aspect(2.0)` | `aspect-ratio: 2` |
//! | `.w_pct(1.0)` | `width: 100%` |
//! | `.max_h_pct(0.45)` | `max-height: 45%` |
//! | `.gap(4.0)` | `gap: 4px` |
//! | `.pad(m)` | `padding: m` |
//!
//! Every part starts at `min-width: 0; min-height: 0` (CSS's flex items would be as
//! wide as their content at least), so a part only ever gets more than its share by
//! asking for it with `min_w` / `min_h`.
//!
//! egui draws in one pass, so a part is drawn where last frame's layout put it; when
//! the layout changes, that frame is thrown away and drawn again, so a change lands
//! whole.

pub use egui_taffy::taffy::{self, Style};
use egui_taffy::{TaffyContainerUi, TuiContainerResponse};
pub use egui_taffy::{Tui, TuiBuilderLogic};
use taffy::prelude::*;

/// Start a layout in all the room `ui` has. `id` must be unique among layouts.
pub fn root<R>(
    ui: &mut egui::Ui,
    id: impl Into<egui::Id>,
    style: Style,
    f: impl FnOnce(&mut Tui) -> R,
) -> R {
    egui_taffy::tui(ui, id)
        .reserve_available_space()
        .style(Style {
            size: percent(1.0_f32),
            ..style
        })
        .show(f)
}

/// A part with no size of its own: it takes what the style gives it.
pub fn leaf() -> Style {
    Style {
        min_size: Size {
            width: length(0.0_f32),
            height: length(0.0_f32),
        },
        ..Style::default()
    }
}

/// A container laying its parts out left to right, stretched to its height.
pub fn row() -> Style {
    Style {
        flex_direction: FlexDirection::Row,
        ..leaf()
    }
}

/// A container laying its parts out top to bottom, stretched to its width.
pub fn col() -> Style {
    Style {
        flex_direction: FlexDirection::Column,
        ..leaf()
    }
}

/// The CSS-like setters on a [`Style`].
pub trait Flex: Sized {
    fn style(&mut self) -> &mut Style;

    /// Always this wide (and doesn't shrink).
    fn w(mut self, w: f32) -> Self {
        let s = self.style();
        s.size.width = length(w);
        s.flex_shrink = 0.0;
        self
    }
    /// Always this tall (and doesn't shrink).
    fn h(mut self, h: f32) -> Self {
        let s = self.style();
        s.size.height = length(h);
        s.flex_shrink = 0.0;
        self
    }
    /// This share of the container's width.
    fn w_pct(mut self, p: f32) -> Self {
        self.style().size.width = percent(p);
        self
    }
    /// This share of the container's height.
    fn h_pct(mut self, p: f32) -> Self {
        self.style().size.height = percent(p);
        self
    }
    fn min_w(mut self, w: f32) -> Self {
        self.style().min_size.width = length(w);
        self
    }
    fn min_h(mut self, h: f32) -> Self {
        self.style().min_size.height = length(h);
        self
    }
    fn max_h_pct(mut self, p: f32) -> Self {
        self.style().max_size.height = percent(p);
        self
    }
    /// Never squeezed below its own size by its neighbours.
    fn no_shrink(mut self) -> Self {
        self.style().flex_shrink = 0.0;
        self
    }
    /// Takes an equal share of the room the others leave (`flex: 1 1 0`).
    fn grow(self) -> Self {
        self.grow_by(1.0)
    }
    fn grow_by(mut self, g: f32) -> Self {
        let s = self.style();
        s.flex_grow = g;
        s.flex_shrink = 1.0;
        s.flex_basis = length(0.0_f32);
        self
    }
    /// Starts at this share of the row and shrinks if there isn't room for it.
    fn basis_pct(mut self, p: f32) -> Self {
        let s = self.style();
        s.flex_basis = percent(p);
        s.flex_shrink = 1.0;
        self
    }
    /// Width over height, used for whichever of the two isn't otherwise set. (Taffy
    /// only uses it from a width it calls definite: a stretched width isn't, but
    /// `w_pct(1.0)` is.)
    fn aspect(mut self, ratio: f32) -> Self {
        self.style().aspect_ratio = Some(ratio);
        self
    }
    /// Between the parts.
    fn gap(mut self, g: f32) -> Self {
        self.style().gap = Size {
            width: length(g),
            height: length(g),
        };
        self
    }
    /// Inside the container's edges.
    fn pad(mut self, m: egui::Margin) -> Self {
        self.style().padding = Rect {
            left: length(m.left as f32),
            right: length(m.right as f32),
            top: length(m.top as f32),
            bottom: length(m.bottom as f32),
        };
        self
    }
    /// Lays its parts out the other way (right to left, bottom to top): a left deck is
    /// a right deck mirrored.
    fn reverse(mut self, yes: bool) -> Self {
        if yes {
            let s = self.style();
            s.flex_direction = match s.flex_direction {
                FlexDirection::Row => FlexDirection::RowReverse,
                FlexDirection::Column => FlexDirection::ColumnReverse,
                d => d,
            };
        }
        self
    }
}

impl Flex for Style {
    fn style(&mut self) -> &mut Style {
        self
    }
}

/// A part whose size is the layout's alone: `add` draws in its box (all of it is
/// `ui.max_rect()`), cut off at its edges. What's drawn doesn't change its size, and
/// nothing is drawn until the layout has given it a box.
pub fn pane<R: Default>(tui: &mut Tui, style: Style, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    tui.style(style).ui_manual(|ui, _| {
        let rect = ui.max_rect();
        ui.set_clip_rect(rect.intersect(ui.clip_rect()));
        let inner = if rect.width() >= 1.0 && rect.height() >= 1.0 {
            add(ui)
        } else {
            R::default()
        };
        TuiContainerResponse {
            inner,
            min_size: egui::Vec2::ZERO,
            intrinsic_size: None,
            max_size: egui::Vec2::ZERO,
            infinite: egui::Vec2b::FALSE,
        }
    })
}

/// A part as big as what `add` draws (as last drawn), unless its style says otherwise.
pub fn fit<R>(tui: &mut Tui, style: Style, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    tui.style(style).ui(add)
}

/// A container drawn as `frame` (its fill, border and corners; its margins are the
/// style's padding), and the box it was given (border included) handed to `inside`
/// with its parts.
pub fn framed<R>(
    tui: &mut Tui,
    style: Style,
    frame: egui::Frame,
    inside: impl FnOnce(&mut Tui, egui::Rect) -> R,
) -> R {
    let style = style.pad(frame.inner_margin + egui::Margin::same(frame.stroke.width as i8));
    tui.style(style)
        .add_with_background_ui(
            |ui: &mut egui::Ui, c: &TaffyContainerUi| {
                let rect = c.full_container();
                if rect.is_positive() {
                    let p = ui.painter();
                    p.rect_filled(rect, frame.corner_radius, frame.fill);
                    p.rect_stroke(
                        rect,
                        frame.corner_radius,
                        frame.stroke,
                        egui::StrokeKind::Inside,
                    );
                }
                rect
            },
            |tui, rect| inside(tui, *rect),
        )
        .main
}

/// Fill all of `ui`'s box with `frame` (its fill and border), and draw `add` inside
/// its margins. For a [`pane`] that's drawn as a panel.
pub fn boxed<R>(ui: &mut egui::Ui, frame: egui::Frame, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let rect = ui.max_rect();
    let p = ui.painter();
    p.rect_filled(rect, frame.corner_radius, frame.fill);
    p.rect_stroke(
        rect,
        frame.corner_radius,
        frame.stroke,
        egui::StrokeKind::Inside,
    );
    let inner = rect - (frame.inner_margin + egui::Margin::same(frame.stroke.width as i8));
    // (Never inside out, in a box smaller than the margins.)
    let inner = egui::Rect::from_min_max(inner.min, inner.max.max(inner.min));
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    add(&mut child)
}
