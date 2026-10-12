//! The canvas's popups: the "+" suggestions beside a card (nine tiles, with Drop /
//! Hold / Build), and "Add track", a library search for a starting card.

use egui::{Align, Align2, Pos2, Rect, RichText, Sense, Stroke, Ui, UiBuilder, vec2};

use super::cards::{art, chips_row};
use super::{FlowUi, POP_W, PickStamp, Pop};
use crate::core::collections::{GraphOp, NodeId};
use crate::core::history::RefTrack;
use crate::core::query;
use crate::core::suggest::Direction;
use crate::core::track::{Track, TrackId};
use crate::ui::panels::CoreView;
use crate::ui::panels::library::tracks::Media;
use crate::ui::theme::THEME;

impl FlowUi {
    /// The open popup, if any.
    pub(super) fn popups(&mut self, ui: &Ui, canvas: Rect, v: &CoreView, media: &Media) {
        match self.pop {
            Some(Pop::Suggest(id)) => {
                let Some(n) = self.graph.node(id) else {
                    return;
                };
                // Beside the "+", or left of the card when there's no room.
                let r = self.rect(n);
                let right = self.to_screen * (r.right_center() + vec2(26.0, 0.0));
                let at = if right.x + POP_W + 20.0 <= canvas.right() {
                    (right, Align2::LEFT_CENTER)
                } else {
                    let left = self.to_screen * (r.left_center() - vec2(14.0, 0.0));
                    (left, Align2::RIGHT_CENTER)
                };
                let (track, node) = (n.track, n.id);
                self.suggest_pop(ui, canvas, at, node, track, v, media);
            }
            Some(Pop::Search(at)) => self.search_pop(ui, canvas, at, v),
            None => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn suggest_pop(
        &mut self,
        ui: &Ui,
        canvas: Rect,
        (anchor, pivot): (Pos2, Align2),
        node: NodeId,
        track: TrackId,
        v: &CoreView,
        media: &Media,
    ) {
        let Some(t) = v.lib.get(track) else { return };
        let stamp = PickStamp {
            track,
            direction: self.direction,
            lib: v.lib.rev(),
            history: v.history.rev(),
            exclude: self.graph.tracks(),
        };
        if self.picks_for.as_ref() != Some(&stamp) {
            // Tracks already in the Flow are left out; tracks played this session
            // still count as played.
            self.picks =
                v.history
                    .suggest(v.lib, &RefTrack::of(t), self.direction, &stamp.exclude, 9);
            self.picks_for = Some(stamp);
        }
        let mut picked = None;
        let mut close = false;
        egui::Area::new(ui.id().with("flow-suggest"))
            .order(egui::Order::Foreground)
            .pivot(pivot)
            .fixed_pos(anchor)
            .constrain_to(canvas)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(POP_W);
                    ui.horizontal(|ui| {
                        for (d, name) in [
                            (Direction::Drop, "Drop"),
                            (Direction::Hold, "Hold"),
                            (Direction::Build, "Build"),
                        ] {
                            if ui.selectable_label(self.direction == d, name).clicked() {
                                self.direction = d;
                            }
                        }
                        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                            if ui.button("Close").clicked() {
                                close = true;
                            }
                        });
                    });
                    ui.add_space(4.0);
                    if self.picks.is_empty() {
                        ui.label(
                            RichText::new(
                                "No suggestions: the library needs more tracks with BPM and key.",
                            )
                            .color(THEME.muted),
                        );
                    }
                    let w = (POP_W - 2.0 * 6.0) / 3.0;
                    for row in self.picks.chunks(3) {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            for s in row {
                                if let Some(c) = v.lib.get(s.id) {
                                    let art = media.art(ui.ctx(), c.id);
                                    if pick_tile(ui, w, c, art, s.total).clicked() {
                                        picked = Some(s.id);
                                    }
                                }
                            }
                        });
                    }
                });
            });
        if let Some(track) = picked {
            self.edit(GraphOp::AddChild {
                parent: node,
                track,
            });
            self.pop = None;
        }
        if close {
            self.pop = None;
        }
    }

    fn search_pop(&mut self, ui: &Ui, canvas: Rect, at: Pos2, v: &CoreView) {
        let mut picked = None;
        egui::Area::new(ui.id().with("flow-search"))
            .order(egui::Order::Foreground)
            .pivot(Align2::RIGHT_TOP)
            .fixed_pos(at)
            .constrain_to(canvas)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(440.0);
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.search)
                            .hint_text("Search the library   bpm:120-126  key:8A")
                            .desired_width(f32::INFINITY),
                    );
                    if std::mem::take(&mut self.focus_search) {
                        r.request_focus();
                    }
                    let q = match query::parse(&self.search) {
                        Ok(q) => q,
                        Err(e) => {
                            ui.label(RichText::new(e.to_string()).color(THEME.bad));
                            return;
                        }
                    };
                    egui::ScrollArea::vertical()
                        .max_height(canvas.height() * 0.6)
                        .show(ui, |ui| {
                            for id in v.lib.query(&q, v.collections, None).into_iter().take(50) {
                                let Some(t) = v.lib.get(id) else { continue };
                                if search_row(ui, t).clicked() {
                                    picked = Some(id);
                                }
                            }
                        });
                });
            });
        if let Some(id) = picked {
            self.edit(GraphOp::AddRoot(id));
            self.pop = None;
        }
    }
}

/// A search result: title and artist, outlined while hovered.
fn search_row(ui: &mut Ui, t: &Track) -> egui::Response {
    let row = ui.horizontal(|ui| {
        ui.add(
            egui::Label::new(format!("{}  {}", t.title, t.artist))
                .truncate()
                .selectable(false),
        );
    });
    let row = ui
        .interact(row.response.rect, row.response.id, Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if row.hovered() {
        ui.painter().rect_stroke(
            row.rect.expand(1.0),
            3,
            Stroke::new(1.0_f32, THEME.accent),
            egui::StrokeKind::Outside,
        );
    }
    row
}

/// A suggestion in the "+" grid: art, title, artist, chips and the match score.
fn pick_tile(
    ui: &mut Ui,
    w: f32,
    t: &Track,
    tex: Option<egui::TextureId>,
    score: f64,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 70.0), Sense::click());
    let resp = resp
        .on_hover_text(&t.path)
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    ui.painter().rect(
        rect,
        7,
        THEME.card,
        Stroke::new(
            1.0_f32,
            if resp.hovered() {
                THEME.accent
            } else {
                THEME.card_line
            },
        ),
        egui::StrokeKind::Inside,
    );
    let mut ui = ui.new_child(
        UiBuilder::new()
            .max_rect(rect.shrink(5.0))
            .layout(egui::Layout::top_down(Align::Min)),
    );
    ui.spacing_mut().item_spacing = vec2(7.0, 4.0);
    ui.horizontal(|ui| {
        art(ui, 28.0, t, tex);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.add(
                egui::Label::new(RichText::new(&t.title).strong().size(12.0))
                    .truncate()
                    .selectable(false),
            );
            ui.add(
                egui::Label::new(RichText::new(&t.artist).size(11.0).color(THEME.muted))
                    .truncate()
                    .selectable(false),
            );
        });
    });
    chips_row(&mut ui, t, |ui| {
        ui.label(
            RichText::new(format!("{:.0}", score * 100.0))
                .size(11.0)
                .strong()
                .color(THEME.muted),
        )
        .on_hover_text("Match");
    });
    resp
}
