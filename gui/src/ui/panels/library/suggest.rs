//! The Suggestions pane: what to play after the reference track, ranked, with each
//! part of the score shown as a small bar. Master / A / B / Selected picks the
//! reference (`core::history` works it out); Drop / Hold / Build picks the energy
//! direction.

use std::time::{Duration, Instant};

use egui::{Align, Color32, CornerRadius, RichText, Sense, Ui, vec2};
use egui_extras::{Column, TableBuilder};

use super::tracks::{MenuAt, title_cell, track_menu};
use crate::core::command::Command;
use crate::core::history::{DeckNow, RefSource, RefTrack};
use crate::core::index::LibraryCmd;
use crate::core::keys::{Camelot, key_name};
use crate::core::selection::{Mods, Selection};
use crate::core::suggest::{Direction, Scored};
use crate::core::track::TrackId;
use crate::ui::panels::CoreView;
use crate::ui::theme::THEME;
use crate::ui::widgets::key_chip;

const BARS: [(&str, Color32); 3] = [
    ("key", THEME.score[0]),
    ("tempo", THEME.score[1]),
    ("energy", THEME.score[2]),
];

/// Suggestions follow a tempo change once the tempo has held still this long.
const SETTLE: Duration = Duration::from_secs(1);

/// What the list was worked out for, apart from the tempo and key.
#[derive(Clone, Copy, PartialEq)]
struct Stamp {
    track: TrackId,
    deck: Option<usize>,
    playing: bool,
    direction: Direction,
    played: u64,
    lib: u64,
}

/// The tempo (to 0.1 BPM) and key the list was worked out for.
type Tempo = (i64, Option<Camelot>);

#[derive(Default)]
pub struct SuggestUi {
    pub source: RefSource,
    pub direction: Direction,
    pub list: Vec<Scored>,
    /// What the suggestions are measured from.
    pub reference: Option<RefTrack>,
    computed_for: Option<(Stamp, Tempo)>,
    /// A new tempo waiting to hold still before the list follows it, and since when.
    settling: Option<(Tempo, Instant)>,
    sel: Selection,
    scroll_to: Option<usize>,
}

impl SuggestUi {
    pub fn focused(&self) -> Option<TrackId> {
        self.sel.focused()
    }

    fn ids(&self) -> Vec<TrackId> {
        self.list.iter().map(|s| s.id).collect()
    }

    pub fn move_focus(&mut self, delta: i32) {
        self.scroll_to = self.sel.move_focus(delta, &self.ids());
    }

    /// Work the list out again if the reference (as `source` picks it from the decks,
    /// or `selected`) or the library changed. A tempo change alone waits until the
    /// tempo has held still for a moment, rather than re-ranking on every step of a
    /// fader sweep.
    pub fn refresh(&mut self, v: &CoreView, decks: &[DeckNow], selected: Option<TrackId>) {
        let Some(r) = v.history.reference(v.lib, self.source, decks, selected) else {
            self.reference = None;
            self.list.clear();
            self.computed_for = None;
            return;
        };
        let stamp = Stamp {
            track: r.id,
            deck: r.deck,
            playing: r.playing,
            direction: self.direction,
            played: v.history.rev(),
            lib: v.lib.rev(),
        };
        let tempo: Tempo = ((r.bpm.unwrap_or(0.0) * 10.0).round() as i64, r.key);
        match self.computed_for {
            Some(c) if c == (stamp, tempo) => {
                self.settling = None;
                return;
            }
            Some((s, _)) if s == stamp => match self.settling {
                Some((t, since)) if t == tempo => {
                    if since.elapsed() < SETTLE {
                        return;
                    }
                }
                _ => {
                    self.settling = Some((tempo, Instant::now()));
                    return;
                }
            },
            _ => {}
        }
        self.settling = None;
        self.list = v
            .history
            .suggest(v.lib, &r, self.direction, &Default::default(), 80);
        self.sel.retain(&self.ids());
        self.reference = Some(r);
        self.computed_for = Some((stamp, tempo));
    }

    /// Returns true when the pane was clicked (so it takes keyboard focus).
    pub fn show(
        &mut self,
        ui: &mut Ui,
        v: &CoreView,
        has_focus: bool,
        out: &mut Vec<Command>,
    ) -> bool {
        let mut clicked_pane = false;
        ui.horizontal(|ui| {
            let title = RichText::new("Suggestions").strong();
            ui.label(if has_focus {
                title.color(THEME.accent)
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
            ui.label(RichText::new("After").color(THEME.muted));
            for (src, name, tip) in [
                (RefSource::Master, "Master", "Whichever deck is playing"),
                (
                    RefSource::Deck(0),
                    "A",
                    "The track on deck A, playing or not",
                ),
                (
                    RefSource::Deck(1),
                    "B",
                    "The track on deck B, playing or not",
                ),
                (
                    RefSource::Selected,
                    "Selected",
                    "The track highlighted in the library, or the selected Flow card",
                ),
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
                    RefSource::Master => "No deck is playing".to_string(),
                    RefSource::Deck(d) => format!("Nothing on deck {}", ["A", "B"][d.min(1)]),
                    RefSource::Selected => "Select a track in the library".to_string(),
                };
                ui.label(RichText::new(hint).color(THEME.muted));
            }
            Some(r) => {
                match r.deck {
                    Some(d) => {
                        let text = RichText::new(format!(
                            "{}deck {}",
                            if r.playing { "LIVE " } else { "" },
                            ["A", "B"][d.min(1)]
                        ))
                        .color(THEME.deck[d.min(1)]);
                        ui.label(if r.playing { text.strong() } else { text })
                    }
                    None => ui.label(RichText::new("Selected").color(THEME.muted)),
                };
                let detail = format!(
                    "{} BPM  {}",
                    r.bpm.map_or("?".into(), |b| format!("{b:.1}")),
                    r.key.map_or("?".into(), key_name)
                );
                ui.label(RichText::new(detail).color(THEME.muted));
                if let Some(t) = v.lib.get(r.id) {
                    ui.add(egui::Label::new(&t.title).truncate());
                }
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
        let ids = self.ids();
        let mut clicked: Option<(TrackId, egui::Modifiers)> = None;
        let mut right_clicked: Option<TrackId> = None;
        table.body(|body| {
            body.rows(34.0, self.list.len(), |mut row| {
                let s = &self.list[row.index()];
                let Some(t) = v.lib.get(s.id) else { return };
                row.set_selected(has_focus && self.sel.contains(s.id));
                row.col(|ui| title_cell(ui, t, v.history.was_played(s.id), true));
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
                            egui::Label::new(RichText::new(ch).size(11.0).color(THEME.muted))
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
                row.col(|ui| bars(ui, s));
                let resp = row.response();
                if resp.clicked() {
                    clicked = Some((s.id, resp.ctx.input(|i| i.modifiers)));
                }
                if resp.secondary_clicked() {
                    right_clicked = Some(s.id);
                }
                if resp.double_clicked() {
                    out.push(LibraryCmd::Load(s.id).into());
                }
                resp.dnd_set_drag_payload(s.id);
                resp.context_menu(|ui| {
                    let sel = if self.sel.contains(s.id) {
                        self.sel.in_order(&ids)
                    } else {
                        vec![s.id]
                    };
                    track_menu(ui, &sel, v, MenuAt::default(), out);
                });
            });
        });
        if let Some(id) = right_clicked {
            self.sel.right_click(id);
            clicked_pane = true;
        }
        if let Some((id, m)) = clicked {
            let mods = Mods {
                shift: m.shift,
                command: m.command,
            };
            self.sel.click(id, &ids, mods);
            clicked_pane = true;
        }
        clicked_pane
    }
}

/// One small bar per part of the score, with the numbers on hover.
fn bars(ui: &mut Ui, s: &Scored) {
    let (rect, resp) = ui.allocate_exact_size(vec2(50.0, 18.0), Sense::hover());
    // A part that couldn't be scored (no key, say) shows as an empty bar.
    let values = BARS.map(|(name, _)| s.part(name));
    let h = 4.0;
    for (i, ((_, colour), v)) in BARS.iter().zip(values).enumerate() {
        let y = rect.top() + 2.0 + i as f32 * (h + 1.5);
        let track = egui::Rect::from_min_size(egui::pos2(rect.left(), y), vec2(rect.width(), h));
        ui.painter()
            .rect_filled(track, CornerRadius::same(2), THEME.line);
        let mut fill = track;
        fill.set_width(track.width() * v.unwrap_or(0.0).clamp(0.0, 1.0) as f32);
        ui.painter()
            .rect_filled(fill, CornerRadius::same(2), *colour);
    }
    resp.on_hover_text(
        BARS.iter()
            .zip(values)
            .map(|((name, _), v)| match v {
                Some(v) => format!("{name} {:.0}%", v * 100.0),
                None => format!("{name} unknown"),
            })
            .collect::<Vec<_>>()
            .join("\n"),
    );
}
