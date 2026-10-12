//! The links between cards: a curve from one card's "+" to the next card's in port,
//! and the one being dragged out to make a new link.

use egui::epaint::CubicBezierShape;
use egui::{Color32, Pos2, Stroke, Ui, vec2};

use super::{FlowUi, Port};
use crate::core::collections::NodeId;
use crate::ui::theme::THEME;

impl FlowUi {
    pub(super) fn edges(&self, ui: &Ui) {
        let stroke = Stroke::new(2.0_f32, THEME.card_hover);
        for &(a, b) in &self.graph.edges {
            let (Some(a), Some(b)) = (self.graph.node(a), self.graph.node(b)) else {
                continue;
            };
            edge(ui, self.port(a, Port::Out), self.port(b, Port::In), stroke);
        }
    }

    /// The link being dragged from `from`'s `port` to the pointer at `at`.
    pub(super) fn new_link(&self, ui: &Ui, from: NodeId, port: Port, at: Pos2) {
        let Some(n) = self.graph.node(from) else {
            return;
        };
        let p = self.port(n, port);
        let (a, b) = match port {
            Port::Out => (p, at),
            Port::In => (at, p),
        };
        edge(ui, a, b, Stroke::new(2.0_f32, THEME.accent));
    }
}

/// A curve from an out port at `a` to an in port at `b`.
fn edge(ui: &Ui, a: Pos2, b: Pos2, stroke: Stroke) {
    let dx = ((b.x - a.x).abs() / 2.0).max(40.0);
    ui.painter().add(CubicBezierShape::from_points_stroke(
        [a, a + vec2(dx, 0.0), b - vec2(dx, 0.0), b],
        false,
        Color32::TRANSPARENT,
        stroke,
    ));
}
