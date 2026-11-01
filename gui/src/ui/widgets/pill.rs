//! A small rounded status label with a border in its [`Intent`]'s colour; clickable.
//! The top bar's audio, MIDI, analysis and jobs pills. `.busy()` puts a spinner
//! before the text.

use egui::{CornerRadius, Margin, Response, RichText, Stroke, Ui, Widget};

use super::Intent;
use crate::ui::theme::{THEME, fs};

pub struct Pill<'a> {
    text: &'a str,
    intent: Intent,
    busy: bool,
}

impl<'a> Pill<'a> {
    pub fn new(text: &'a str, intent: Intent) -> Self {
        Pill {
            text,
            intent,
            busy: false,
        }
    }

    /// A spinner before the text: something is running.
    pub fn busy(mut self) -> Self {
        self.busy = true;
        self
    }
}

impl Widget for Pill<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        egui::Frame::new()
            .fill(THEME.panel_2)
            .stroke(Stroke::new(1.0_f32, self.intent.colour()))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::symmetric(8, 2))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.horizontal(|ui| {
                    if self.busy {
                        ui.add(egui::Spinner::new().size(fs(11.0)));
                    }
                    ui.label(RichText::new(self.text).size(fs(11.0)));
                });
            })
            .response
            .interact(egui::Sense::click())
    }
}
