//! A vertical stack of icons that opens one pane at a time, like Rekordbox's: left
//! of the library it picks the source (what we're operating on); on the right it
//! opens side panes such as the suggestions. Clicking the open one closes it.
//! Something dragged and held over an icon for [`SPRING`] opens its pane, so it can
//! be dropped there.

use std::borrow::Cow;

use egui::{Align2, Color32, FontId, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

use crate::ui::theme::THEME;

/// The stack's width; each icon is a square this size.
pub const WIDTH: f32 = 30.0;

/// How long a drag rests on an icon before its pane opens, s.
const SPRING: f64 = 0.5;

/// One icon in a stack.
pub struct StackItem<T> {
    pub id: T,
    /// A glyph the default fonts have, or a few letters ("RB").
    pub icon: Cow<'static, str>,
    pub tip: Cow<'static, str>,
    /// Greyed out (a placeholder for what isn't built yet, or not set up).
    pub enabled: bool,
    /// A small light in its corner (a USB stick's: safe to remove or not).
    pub light: Option<Color32>,
    /// A gap above it, starting a group.
    pub gap: bool,
}

impl<T> StackItem<T> {
    pub const fn new(id: T, icon: &'static str, tip: &'static str) -> StackItem<T> {
        StackItem {
            id,
            icon: Cow::Borrowed(icon),
            tip: Cow::Borrowed(tip),
            enabled: true,
            light: None,
            gap: false,
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
            if it.gap {
                let (r, _) = ui.allocate_exact_size(vec2(WIDTH, 9.0), Sense::hover());
                ui.painter().hline(
                    r.x_range().shrink(6.0),
                    r.center().y,
                    Stroke::new(1.0_f32, THEME.line),
                );
            }
            let on = open == Some(it.id);
            let sense = if it.enabled {
                Sense::click()
            } else {
                Sense::hover()
            };
            let (rect, resp) = ui.allocate_exact_size(vec2(WIDTH, WIDTH), sense);
            // Named by its tip for screen readers (and tests).
            resp.widget_info(|| {
                egui::WidgetInfo::selected(egui::WidgetType::Button, it.enabled, on, &*it.tip)
            });
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
            // Letters are smaller than a glyph.
            let size = if it.icon.chars().count() > 1 {
                10.0
            } else {
                15.0
            };
            p.text(
                rect.center(),
                Align2::CENTER_CENTER,
                &*it.icon,
                FontId::proportional(size),
                colour,
            );
            if let Some(light) = it.light {
                p.circle_filled(rect.right_top() + vec2(-6.0, 6.0), 3.5, light);
            }
            let resp = resp.on_hover_text(&*it.tip);
            if resp.clicked() {
                picked = Some((!on).then_some(it.id));
            }
            // Spring-loaded while something is dragged over it.
            let since = resp.id.with("drag-over");
            let dragging = egui::DragAndDrop::has_any_payload(ui.ctx());
            if dragging && it.enabled && !on && resp.contains_pointer() {
                let now = ui.input(|i| i.time);
                let start = ui.data_mut(|d| *d.get_temp_mut_or(since, now));
                if now - start >= SPRING {
                    picked = Some(Some(it.id));
                    ui.data_mut(|d| d.remove::<f64>(since));
                } else {
                    ui.ctx().request_repaint();
                }
            } else {
                ui.data_mut(|d| d.remove::<f64>(since));
            }
        }
    });
    picked
}
