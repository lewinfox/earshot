//! Every card in miniature in the canvas's bottom-right corner, with the part in view
//! outlined. Click or drag on it to look somewhere else.

use egui::{Pos2, Rect, Sense, Stroke, Ui, vec2};

use super::{FlowUi, NODE_W, Port};
use crate::ui::panels::PreviewState;
use crate::ui::theme::THEME;

impl FlowUi {
    pub(super) fn minimap(&mut self, ui: &mut Ui, canvas: Rect, preview: Option<PreviewState>) {
        let Some(bounds) = self.bounds() else {
            return;
        };
        // From the cards alone, so the map holds still while you drag the view around.
        let world = bounds.expand(NODE_W / 2.0);
        let max = vec2(220.0, 140.0);
        let scale = (max.x / world.width()).min(max.y / world.height());
        let size = world.size() * scale;
        let map = Rect::from_min_size(canvas.right_bottom() - size - vec2(12.0, 12.0), size);
        let to_map = |p: Pos2| map.min + (p - world.min) * scale;

        egui::Area::new(ui.id().with("flow-minimap"))
            .fixed_pos(map.min)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                let (_, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
                let p = ui.painter();
                p.rect(
                    map,
                    4,
                    THEME.minimap_bg,
                    Stroke::new(1.0_f32, THEME.card_line),
                    egui::StrokeKind::Outside,
                );
                for &(a, b) in &self.graph.edges {
                    if let (Some(a), Some(b)) = (self.graph.node(a), self.graph.node(b)) {
                        let a = to_map(self.port(a, Port::Out));
                        let b = to_map(self.port(b, Port::In));
                        p.line_segment([a, b], Stroke::new(1.0_f32, THEME.muted));
                    }
                }
                for n in &self.graph.nodes {
                    let lit =
                        self.selected == Some(n.id) || preview.is_some_and(|pv| pv.id == n.track);
                    let r = self.rect(n);
                    let r = Rect::from_min_max(to_map(r.min), to_map(r.max));
                    let fill = if lit {
                        THEME.accent
                    } else {
                        THEME.minimap_card
                    };
                    p.rect_filled(r, 1, fill);
                }
                let view = self.camera.view;
                let view = Rect::from_min_max(to_map(view.min), to_map(view.max)).intersect(map);
                p.rect_stroke(
                    view,
                    2,
                    Stroke::new(1.5_f32, THEME.marker),
                    egui::StrokeKind::Inside,
                );
                if (resp.clicked() || resp.dragged())
                    && let Some(at) = resp.interact_pointer_pos()
                {
                    let centre = world.min + (at - map.min) / scale;
                    let view = Rect::from_center_size(centre, self.camera.view.size());
                    self.camera.set(view);
                }
            });
    }
}
