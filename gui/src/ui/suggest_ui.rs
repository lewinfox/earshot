//! The Suggestions pane: what to play after the reference track, ranked, with each
//! part of the score shown as a small bar. Master / A / B / Selected picks the
//! reference; Drop / Hold / Build picks the energy direction.

use egui::{Align, Color32, CornerRadius, RichText, Sense, Ui, vec2};
use egui_extras::{Column, TableBuilder};

use super::deck::key_chip;
use super::library_ui::{LibAction, LibraryUi};
use super::util;
use crate::core::keys::{Camelot, key_name};
use crate::core::suggest::{Direction, Scored};

const BARS: [(&str, Color32); 3] = [
    ("key", Color32::from_rgb(0xc2, 0x5b, 0xff)),
    ("tempo", Color32::from_rgb(0x3f, 0xa7, 0xff)),
    ("energy", Color32::from_rgb(0xff, 0xcc, 0x4d)),
];

/// Where the reference track comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Source {
    /// Whichever deck is playing (the one started most recently, if both are).
    #[default]
    Master,
    /// What's loaded on deck A (0) or B (1), playing or not.
    Deck(usize),
    /// The track highlighted in the library.
    Selected,
}

/// What the suggestions are measured from, for the pane's header.
pub struct RefInfo {
    pub title: String,
    pub bpm: Option<f64>,
    pub key: Option<Camelot>,
    /// The deck it's on, if it's from a deck, and whether that deck is playing.
    pub deck: Option<usize>,
    pub playing: bool,
}

#[derive(Default)]
pub struct SuggestUi {
    pub source: Source,
    pub direction: Direction,
    pub list: Vec<Scored>,
    pub reference: Option<RefInfo>,
    /// Inputs the list was computed from; recomputed when they change.
    pub computed_for: String,
    /// Row with keyboard focus.
    focus: Option<usize>,
    scroll_to: Option<usize>,
}

impl SuggestUi {
    pub fn focused(&self) -> Option<i64> {
        self.focus.and_then(|f| self.list.get(f)).map(|s| s.id)
    }

    pub fn move_focus(&mut self, delta: i32) {
        if self.list.is_empty() {
            return;
        }
        let f = self.focus.map_or(0, |f| {
            (f as i32 + delta).clamp(0, self.list.len() as i32 - 1) as usize
        });
        self.focus = Some(f);
        self.scroll_to = Some(f);
    }

    /// Replace the list, keeping focus on the same track if it's still there.
    pub fn set(&mut self, list: Vec<Scored>) {
        let id = self.focused();
        self.focus = id.and_then(|id| list.iter().position(|s| s.id == id));
        self.list = list;
    }

    /// Returns true when the pane was clicked (so it takes keyboard focus).
    pub fn show(
        &mut self,
        ui: &mut Ui,
        lib: &LibraryUi,
        has_focus: bool,
        actions: &mut Vec<LibAction>,
    ) -> bool {
        let mut clicked_pane = false;
        ui.horizontal(|ui| {
            let title = RichText::new("Suggestions").strong();
            ui.label(if has_focus {
                title.color(util::ACCENT)
            } else {
                title
            })
            .on_hover_text("Tab switches the keyboard between the library and this list");
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                // Right to left, so Build first.
                for (d, name, tip) in [
                    (Direction::Build, "Build", "More energy than now"),
                    (Direction::Hold, "Hold", "About the same energy"),
                    (Direction::Drop, "Drop", "Less energy than now"),
                ] {
                    if ui
                        .selectable_label(self.direction == d, name)
                        .on_hover_text(tip)
                        .clicked()
                    {
                        self.direction = d;
                    }
                }
            });
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("After").color(util::MUTED));
            for (src, name, tip) in [
                (Source::Master, "Master", "Whichever deck is playing"),
                (Source::Deck(0), "A", "The track on deck A, playing or not"),
                (Source::Deck(1), "B", "The track on deck B, playing or not"),
                (Source::Selected, "Selected", "The track highlighted in the library"),
            ] {
                if ui
                    .selectable_label(self.source == src, name)
                    .on_hover_text(tip)
                    .clicked()
                {
                    self.source = src;
                }
            }
        });
        ui.horizontal(|ui| match &self.reference {
            None => {
                let hint = match self.source {
                    Source::Master => "No deck is playing".to_string(),
                    Source::Deck(d) => format!("Nothing on deck {}", ["A", "B"][d]),
                    Source::Selected => "Select a track in the library".to_string(),
                };
                ui.label(RichText::new(hint).color(util::MUTED));
            }
            Some(r) => {
                match r.deck {
                    Some(d) => {
                        let text = RichText::new(format!(
                            "{}deck {}",
                            if r.playing { "LIVE " } else { "" },
                            ["A", "B"][d]
                        ))
                        .color(util::DECK[d]);
                        ui.label(if r.playing { text.strong() } else { text })
                    }
                    None => ui.label(RichText::new("Selected").color(util::MUTED)),
                };
                let detail = format!(
                    "{} BPM  {}",
                    r.bpm.map_or("?".into(), |b| format!("{b:.1}")),
                    r.key.map_or("?".into(), key_name)
                );
                ui.label(RichText::new(detail).color(util::MUTED));
                ui.add(egui::Label::new(&r.title).truncate());
            }
        });
        ui.add_space(2.0);

        let mut table = TableBuilder::new(ui)
            .id_salt("suggestions")
            .striped(true)
            .sense(Sense::click_and_drag())
            .cell_layout(egui::Layout::left_to_right(Align::Center))
            .column(Column::remainder().at_least(120.0).clip(true))
            .column(Column::exact(48.0))
            .column(Column::exact(40.0))
            .column(Column::exact(28.0))
            .column(Column::exact(54.0));
        if let Some(r) = self.scroll_to.take() {
            table = table.scroll_to_row(r, Some(Align::Center));
        }
        let mut clicked: Option<usize> = None;
        table.body(|body| {
            body.rows(34.0, self.list.len(), |mut row| {
                let r = row.index();
                let s = self.list[r];
                let Some(t) = lib.track(s.id) else { return };
                row.set_selected(has_focus && self.focus == Some(r));
                let played = lib.played.contains(&s.id);
                row.col(|ui| {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let title = RichText::new(&t.lib.title);
                        ui.add(
                            egui::Label::new(if played {
                                title.color(util::MUTED)
                            } else {
                                title
                            })
                            .truncate()
                            .selectable(false),
                        );
                        ui.add(
                            egui::Label::new(
                                RichText::new(&t.lib.artist).size(11.0).color(util::MUTED),
                            )
                            .truncate()
                            .selectable(false),
                        );
                    });
                });
                row.col(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        let ch = s
                            .tempo_change
                            .map(|c| match c * 100.0 {
                                // No "-0.0%" for a change too small to show.
                                p if p.abs() < 0.05 => "0.0%".to_string(),
                                p => format!("{p:+.1}%"),
                            })
                            .unwrap_or_default();
                        ui.add(
                            egui::Label::new(RichText::new(ch).size(11.0).color(util::MUTED))
                                .selectable(false),
                        )
                        .on_hover_text("Tempo change needed (allowing half or double time)");
                    });
                });
                row.col(|ui| {
                    if let Some(k) = t.key {
                        key_chip(ui, k);
                    }
                });
                row.col(|ui| {
                    ui.label(RichText::new(format!("{:.0}", s.total * 100.0)).strong());
                });
                row.col(|ui| bars(ui, &s));
                let resp = row.response();
                if resp.clicked() || resp.secondary_clicked() {
                    clicked = Some(r);
                }
                if resp.double_clicked() {
                    actions.push(LibAction::Load {
                        deck: None,
                        track_id: s.id,
                    });
                }
                resp.dnd_set_drag_payload(s.id);
                resp.context_menu(|ui| {
                    if ui.button("Preview in headphones").clicked() {
                        actions.push(LibAction::Preview(s.id));
                        ui.close();
                    }
                    for (deck, label) in [(0, "Load to deck A"), (1, "Load to deck B")] {
                        if ui.button(label).clicked() {
                            actions.push(LibAction::Load {
                                deck: Some(deck),
                                track_id: s.id,
                            });
                            ui.close();
                        }
                    }
                });
            });
        });
        if let Some(r) = clicked {
            self.focus = Some(r);
            clicked_pane = true;
        }
        clicked_pane
    }
}

/// One small bar per part of the score, with the numbers on hover.
fn bars(ui: &mut Ui, s: &Scored) {
    let (rect, resp) = ui.allocate_exact_size(vec2(50.0, 18.0), Sense::hover());
    let values = [s.key, s.tempo, s.energy];
    let h = 4.0;
    for (i, ((_, colour), v)) in BARS.iter().zip(values).enumerate() {
        let y = rect.top() + 2.0 + i as f32 * (h + 1.5);
        let track = egui::Rect::from_min_size(egui::pos2(rect.left(), y), vec2(rect.width(), h));
        ui.painter()
            .rect_filled(track, CornerRadius::same(2), util::LINE);
        let mut fill = track;
        fill.set_width(track.width() * v.clamp(0.0, 1.0) as f32);
        ui.painter()
            .rect_filled(fill, CornerRadius::same(2), *colour);
    }
    resp.on_hover_text(
        BARS.iter()
            .zip(values)
            .map(|((name, _), v)| format!("{name} {:.0}%", v * 100.0))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}
