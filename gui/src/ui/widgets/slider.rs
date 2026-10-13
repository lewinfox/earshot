//! The app's slider for settings and tools (not Perform's faders): its name, the rail
//! filled up to the handle, and the value as editable text after it. Built on
//! `egui::Slider`; the theme colours its rail.

use std::ops::RangeInclusive;

use egui::emath::Numeric;
use egui::{Response, Ui, Widget};

/// Points of rail, so sliders line up.
const WIDTH: f32 = 180.0;

pub struct Slider<'a, N: Numeric> {
    value: &'a mut N,
    range: RangeInclusive<N>,
    label: &'a str,
}

impl<'a, N: Numeric> Slider<'a, N> {
    /// `label` goes before the rail, and names it to screen readers and tests.
    pub fn new(label: &'a str, value: &'a mut N, range: RangeInclusive<N>) -> Self {
        Slider {
            value,
            range,
            label,
        }
    }
}

impl<N: Numeric> Widget for Slider<'_, N> {
    fn ui(self, ui: &mut Ui) -> Response {
        ui.horizontal(|ui| {
            let name = ui.label(self.label);
            ui.spacing_mut().slider_width = WIDTH;
            ui.add(
                egui::Slider::new(self.value, self.range)
                    .trailing_fill(true)
                    .handle_shape(egui::style::HandleShape::Circle),
            )
            .labelled_by(name.id)
        })
        .inner
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    #[test]
    fn pressing_the_end_sets_the_most() {
        let mut h = Harness::new_ui_state(
            |ui, n: &mut usize| {
                ui.add(Slider::new("Background workers", n, 1..=8));
            },
            2,
        );
        let rail = h.get_by_role(egui::accesskit::Role::Slider).rect();
        // Press at the right end: the most.
        h.drag_at(rail.right_center() - egui::vec2(1.0, 0.0));
        h.run();
        assert_eq!(*h.state(), 8);
    }
}
