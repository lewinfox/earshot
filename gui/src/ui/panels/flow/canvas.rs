//! The canvas: pan, zoom, the cards and their links, tracks dropped from the library,
//! and what the cards ask for (carried out once they're all drawn).

use egui::emath::TSTransform;
use egui::{Align2, FontId, Pos2, Rect, Response, Ui};

use super::cards::{CardAction, CardState, card};
use super::{FlowUi, Moving, Pop, Port, ZOOM, camera};
use crate::core::collections::GraphOp;
use crate::core::command::{Command, PreviewCmd};
use crate::ui::panels::library::drag::{self, DragPayload, Origin};
use crate::ui::panels::library::tracks::Media;
use crate::ui::panels::{CoreView, PreviewState};
use crate::ui::theme::THEME;

impl FlowUi {
    /// The toolbar and the canvas below it. `preview`: the track playing in the
    /// headphones, and how far through it is.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        v: &CoreView,
        media: &mut Media,
        preview: Option<PreviewState>,
        out: &mut Vec<Command>,
    ) {
        // To stop the preview if its card goes (removed, or Reset).
        let had_preview = preview.is_some_and(|p| self.graph.nodes.iter().any(|n| n.track == p.id));
        self.toolbar(ui, v, out);

        let canvas = ui.available_rect_before_wrap();
        if let Some(want) = self.camera.reveal.take() {
            self.camera.set(camera::reveal(self.camera.view, want));
        }
        if std::mem::take(&mut self.camera.fit)
            && let Some(b) = self.bounds()
        {
            self.camera.fit_to(b, canvas.size());
        }
        ui.painter().rect_filled(canvas, 0.0, THEME.bg);

        // The wheel zooms (egui's Scene would pan with it): take the scroll before the
        // Scene sees it, unless a popup or menu is under the pointer.
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let over_canvas = pointer.is_some_and(|p| {
            canvas.contains(p)
                && ui
                    .ctx()
                    .layer_id_at(p)
                    .is_none_or(|l| l.order < egui::Order::Foreground)
        });
        let wheel = if over_canvas {
            ui.ctx()
                .input_mut(|i| std::mem::take(&mut i.smooth_scroll_delta))
                .y
        } else {
            0.0
        };

        let mut view = self.camera.view;
        let mut acts = vec![];
        let mut link_target = None;
        let resp = egui::Scene::new()
            .zoom_range(ZOOM)
            .show(ui, &mut view, |ui| {
                self.to_screen = ui
                    .ctx()
                    .layer_transform_to_global(ui.layer_id())
                    .unwrap_or_default();
                let at = pointer.map(|p| self.to_screen.inverse() * p);
                link_target = self.draw(ui, v, media, preview, at, &mut acts, out);
            })
            .response;
        self.camera.set(view);
        if wheel != 0.0
            && let Some(p) = pointer
        {
            self.zoom(wheel, p, canvas);
        }
        if let Some((from, port)) = self.linking
            && ui.input(|i| !i.pointer.any_down())
        {
            self.linking = None;
            if let Some(to) = link_target {
                match port {
                    Port::Out => self.edit(GraphOp::Link { from, to }),
                    Port::In => self.edit(GraphOp::Link { from: to, to: from }),
                };
            }
        }

        if self.graph.is_empty() {
            ui.painter().text(
                canvas.center(),
                Align2::CENTER_CENTER,
                "Add a track to start: Add track above, drag tracks here from the library,\n\
                 or right-click a track > Add to Flow.",
                FontId::proportional(13.0),
                THEME.muted,
            );
        }
        self.drop_tracks(ui, &resp);
        if resp.clicked() || resp.drag_started() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.pop = None;
            self.selected = None;
        }
        if let Some(id) = self.selected
            && !ui.ctx().wants_keyboard_input()
            && ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace))
        {
            acts.push(CardAction::Edit(GraphOp::Remove(id)));
        }
        self.act(ui, canvas, acts, out);

        self.minimap(ui, canvas, preview);
        self.popups(ui, canvas, v, media);

        if had_preview
            && let Some(p) = preview
            && !self.graph.nodes.iter().any(|n| n.track == p.id)
        {
            out.push(Command::Preview(PreviewCmd::Stop(p.id)));
        }
    }

    /// The links and the cards, inside the Scene. `at`: the pointer, in canvas points.
    /// Returns the card a link being dragged would land on.
    #[allow(clippy::too_many_arguments)]
    fn draw(
        &self,
        ui: &mut Ui,
        v: &CoreView,
        media: &mut Media,
        preview: Option<PreviewState>,
        at: Option<Pos2>,
        acts: &mut Vec<CardAction>,
        out: &mut Vec<Command>,
    ) -> Option<crate::core::collections::NodeId> {
        self.edges(ui);
        let mut link_target = None;
        if let (Some((from, port)), Some(at)) = (self.linking, at) {
            link_target = self
                .graph
                .nodes
                .iter()
                .rev()
                .find(|n| n.id != from && self.rect(n).expand(14.0).contains(at))
                .map(|n| n.id);
            self.new_link(ui, from, port, at);
        }
        let ctx = ui.ctx().clone();
        for n in &self.graph.nodes {
            let Some(t) = v.lib.get(n.track) else {
                continue;
            };
            let rect = self.rect(n);
            let state = CardState {
                rect,
                preview: preview.filter(|p| p.id == t.id),
                link_target: link_target == Some(n.id),
                selected: self.selected == Some(n.id),
                // Near enough to reach the corner button.
                near: at.is_some_and(|p| rect.expand(14.0).contains(p)),
                thumb: media.thumb(&ctx, t.id),
                art: media.art(&ctx, t.id),
            };
            card(ui, &self.graph, n, t, v, state, acts, out);
        }
        link_target
    }

    /// Zoom about the pointer at `p` (as the Scene does for Ctrl + wheel).
    fn zoom(&mut self, wheel: f32, p: Pos2, canvas: Rect) {
        let k = (self.to_screen.scaling * (wheel * 0.0015).exp()).clamp(*ZOOM.start(), *ZOOM.end());
        let f = k / self.to_screen.scaling;
        let t = TSTransform::from_translation(p.to_vec2())
            * TSTransform::from_scaling(f)
            * TSTransform::from_translation(-p.to_vec2())
            * self.to_screen;
        self.camera.set(t.inverse() * canvas);
    }

    /// Tracks dragged here from the library (or the suggestions) become cards where
    /// they're dropped, one under the other. A card brought back onto the canvas only
    /// moves.
    fn drop_tracks(&mut self, ui: &Ui, resp: &Response) {
        if self.moving.is_some() {
            return;
        }
        let Some((tracks, origin)) = drag::take_tracks(resp) else {
            return;
        };
        if origin != Origin::Flow
            && let Some(p) = ui.input(|i| i.pointer.interact_pos())
        {
            self.add_at(&tracks, self.to_screen.inverse() * p);
        }
    }

    /// Carry out what the cards asked for.
    fn act(&mut self, ui: &Ui, canvas: Rect, acts: Vec<CardAction>, out: &mut Vec<Command>) {
        let pointer = ui.input(|i| i.pointer.latest_pos());
        for a in acts {
            match a {
                CardAction::Select(id) => self.selected = Some(id),
                CardAction::Edit(op) => {
                    if let GraphOp::Remove(id) = op {
                        if self.selected == Some(id) {
                            self.selected = None;
                        }
                        if matches!(self.pop, Some(Pop::Suggest(p)) if p == id) {
                            self.pop = None;
                        }
                    }
                    self.edit(op);
                }
                CardAction::Plus(id) => {
                    self.pop = match self.pop {
                        Some(Pop::Suggest(p)) if p == id => None,
                        _ => Some(Pop::Suggest(id)),
                    }
                }
                CardAction::Drag(node, d) => {
                    let m = self.moving.get_or_insert(Moving {
                        node,
                        by: egui::Vec2::ZERO,
                        outside: false,
                    });
                    m.by += d;
                    m.outside = pointer.is_some_and(|p| !canvas.contains(p));
                    // Off the canvas it carries its track, and the shell draws the
                    // drag ghost; back on it, it's just a card moving.
                    let track = self.graph.node(node).map(|n| n.track);
                    match (m.outside, track) {
                        (true, Some(t)) => egui::DragAndDrop::set_payload(
                            ui.ctx(),
                            DragPayload::track(t, Origin::Flow),
                        ),
                        _ => egui::DragAndDrop::clear_payload(ui.ctx()),
                    }
                }
                CardAction::DragEnd => {
                    // Let go on the canvas, it moves there; anywhere else (a deck),
                    // it stays where it was.
                    if let Some(m) = self.moving.take()
                        && !m.outside
                        && let Some(n) = self.graph.node(m.node)
                    {
                        let to = super::pos(n) + m.by;
                        self.edit(GraphOp::Move {
                            node: m.node,
                            x: to.x,
                            y: to.y,
                        });
                    }
                }
                CardAction::StartLink(id, port) => {
                    self.linking = Some((id, port));
                    self.pop = None;
                }
                CardAction::Bake(id) => self.bake(self.graph.path_through(id), out),
            }
        }
    }
}
