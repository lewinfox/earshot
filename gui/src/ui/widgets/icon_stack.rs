//! A vertical stack of icons that opens one pane at a time, like Rekordbox's: left
//! of the library it picks the source (what we're operating on); on the right it
//! opens side panes such as the suggestions. Clicking the open one closes it.

use egui::{Align2, FontId, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

use crate::ui::theme::THEME;

/// The stack's width; each icon is a square this size.
pub const WIDTH: f32 = 30.0;

/// One icon in a stack.
pub struct StackItem<T> {
    pub id: T,
    /// A glyph the default fonts have.
    pub icon: &'static str,
    pub tip: &'static str,
    /// Greyed out (a placeholder for what isn't built yet).
    pub enabled: bool,
}

impl<T> StackItem<T> {
    pub const fn new(id: T, icon: &'static str, tip: &'static str) -> StackItem<T> {
        StackItem {
            id,
            icon,
            tip,
            enabled: true,
        }
    }
}

/// Which side of the stack its panes open on.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Opens {
    Left,
    Right,
}

/// Draw the stack with `open` lit. Returns what was clicked: `Some(new)`, where `new`
/// is the item to open, or `None` to close the open one.
pub fn icon_stack<T: Copy + PartialEq>(
    ui: &mut Ui,
    items: &[StackItem<T>],
    open: Option<T>,
    opens: Opens,
) -> Option<Option<T>> {
    let mut picked = None;
    ui.vertical(|ui| {
        ui.set_width(WIDTH);
        ui.spacing_mut().item_spacing.y = 4.0;
        for it in items {
            let on = open == Some(it.id);
            let sense = if it.enabled {
                Sense::click()
            } else {
                Sense::hover()
            };
            let (rect, resp) = ui.allocate_exact_size(vec2(WIDTH, WIDTH), sense);
            let p = ui.painter();
            let fill = if on || (resp.hovered() && it.enabled) {
                THEME.panel_2
            } else {
                THEME.panel
            };
            p.rect(
                rect,
                4,
                fill,
                Stroke::new(1.0_f32, if on { THEME.line } else { fill }),
                StrokeKind::Inside,
            );
            if on {
                // A bar on the edge nearest the pane it opens.
                let bar = match opens {
                    Opens::Left => {
                        Rect::from_min_max(rect.min, pos2(rect.left() + 2.0, rect.bottom()))
                    }
                    Opens::Right => {
                        Rect::from_min_max(pos2(rect.right() - 2.0, rect.top()), rect.max)
                    }
                };
                p.rect_filled(bar, 1, THEME.accent);
            }
            let colour = match (it.enabled, on) {
                (false, _) => THEME.line,
                (true, true) => THEME.text,
                (true, false) => THEME.muted,
            };
            p.text(
                rect.center(),
                Align2::CENTER_CENTER,
                it.icon,
                FontId::proportional(15.0),
                colour,
            );
            let resp = resp.on_hover_text(it.tip);
            if resp.clicked() {
                picked = Some((!on).then_some(it.id));
            }
        }
    });
    picked
}
