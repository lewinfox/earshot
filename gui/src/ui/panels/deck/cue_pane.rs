//! Prepare's cue pane, beside its waveform (like Rekordbox's): the loaded track's
//! memory cues and loops.

use earshot_engine::beatgrid::Beatgrid;
use egui::{Align, FontId, Layout, Rect, RichText, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

use super::{DeckInfo, DeckUi, LabelEdit, LabelTarget};
use crate::core::command::{Command, DeckCmd};
use crate::core::store::Memory;
use crate::ui::panels::{Cx, Panel};
use crate::ui::theme::{self, THEME};
use crate::ui::util;

/// The pane's width beside the waveform, and a row's height.
pub const CUE_PANE_W: f32 = 300.0;
const CUE_ROW_H: f32 = 20.0;

impl DeckUi {
    /// Prepare's cue pane, beside its waveform: the track's memory cues and loops in
    /// order through it, as Rekordbox lists them. Each row: colour, time (and a loop's
    /// length), name. Click a row to go there (a cue becomes the cue point; a loop
    /// starts), double-click to name it, click the colour to change it, × to forget it.
    pub fn cue_pane(&mut self, ui: &mut Ui, info: &DeckInfo, out: &mut Vec<Command>) {
        let deck = self.id;
        let memories = &info.cues.memories;
        let size = ui.available_size();
        // Top to bottom, whatever the layout it's placed in.
        let ui = &mut ui.new_child(egui::UiBuilder::new().layout(Layout::top_down(Align::Min)));
        egui::Frame::new()
            .fill(THEME.panel)
            .stroke(Stroke::new(1.0_f32, THEME.line))
            .corner_radius(4)
            .inner_margin(egui::Margin::symmetric(6, 4))
            .show(ui, |ui| {
                ui.set_width(size.x - 14.0);
                ui.set_height(size.y - 10.0);
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.horizontal(|ui| {
                    ui.label(RichText::new("MEMORY").size(10.0).color(THEME.muted));
                    let rest = vec2(ui.available_width(), 18.0);
                    ui.allocate_ui_with_layout(rest, Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .small_button("+ M")
                            .on_hover_text("Save the cue point, or the loop while looping (M)")
                            .clicked()
                        {
                            out.push(Command::Deck(deck, DeckCmd::SaveMemory));
                        }
                    });
                });
                egui::ScrollArea::vertical()
                    .id_salt(("memories", deck.index()))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if memories.is_empty() {
                            ui.label(
                                RichText::new("C sets a cue, M saves it here")
                                    .size(11.0)
                                    .color(THEME.muted),
                            );
                        }
                        for m in memories {
                            self.cue_row(ui, m, info.grid.as_deref(), out);
                        }
                    });
            });
    }

    /// One memory cue or loop in the cue pane.
    pub(super) fn cue_row(
        &mut self,
        ui: &mut Ui,
        m: &Memory,
        grid: Option<&Beatgrid>,
        out: &mut Vec<Command>,
    ) {
        let deck = self.id;
        let (rect, row) =
            ui.allocate_exact_size(vec2(ui.available_width(), CUE_ROW_H), Sense::click());
        let p = ui.painter_at(rect);
        if row.hovered() {
            p.rect_filled(rect, 3.0, THEME.panel_2);
        }
        let (cy, left) = (rect.center().y, rect.left());
        let colour = THEME.memory_colour(m);

        // The colour: a square for a cue, a wider bar for a loop. Click to pick one.
        let swatch = Rect::from_center_size(
            pos2(left + 9.0, cy),
            if m.end.is_some() {
                vec2(14.0, 8.0)
            } else {
                vec2(10.0, 10.0)
            },
        );
        let pick = ui
            .interact(swatch.expand(3.0), row.id.with("colour"), Sense::click())
            .on_hover_text("Colour");
        p.rect_filled(swatch, 2.0, colour);
        if pick.hovered() {
            p.rect_stroke(
                swatch.expand(1.5),
                2.0,
                Stroke::new(1.0_f32, THEME.text),
                StrokeKind::Outside,
            );
        }
        egui::Popup::menu(&pick).show(|ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                for (rgb, name) in theme::CUE_PALETTE {
                    let (r, resp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
                    ui.painter()
                        .rect_filled(r.shrink(1.0), 3.0, theme::rgb(rgb));
                    if m.color == Some(rgb) {
                        ui.painter().rect_stroke(
                            r,
                            3.0,
                            Stroke::new(1.5_f32, THEME.text),
                            StrokeKind::Inside,
                        );
                    }
                    if resp.on_hover_text(name).clicked() {
                        out.push(Command::Deck(
                            deck,
                            DeckCmd::SetMemoryColor(m.id, Some(rgb)),
                        ));
                        ui.close();
                    }
                }
            });
            if m.color.is_some() && ui.button("Default colour").clicked() {
                out.push(Command::Deck(deck, DeckCmd::SetMemoryColor(m.id, None)));
                ui.close();
            }
        });

        // The time, and a loop's length (in beats when there's a grid).
        let font = FontId::monospace(11.0);
        p.text(
            pos2(left + 22.0, cy),
            egui::Align2::LEFT_CENTER,
            util::fmt_tenths(m.start),
            font,
            THEME.text,
        );
        if let Some(end) = m.end {
            let len = match grid {
                Some(g) => {
                    let beats = g.beat_at(end) - g.beat_at(m.start);
                    format!("{} beats", (beats * 4.0).round() / 4.0)
                }
                None => format!("{:.1} s", end - m.start),
            };
            p.text(
                pos2(left + 76.0, cy),
                egui::Align2::LEFT_CENTER,
                len,
                FontId::proportional(10.0),
                THEME.muted,
            );
        }

        // The name, or the field to type it in.
        let name = Rect::from_x_y_ranges(left + 128.0..=rect.right() - 18.0, rect.y_range());
        let editing = self
            .edit
            .as_ref()
            .is_some_and(|e| e.target == LabelTarget::Memory(m.id));
        if editing {
            let edit = self.edit.as_mut().expect("editing");
            let r = ui.put(name.shrink2(vec2(0.0, 1.0)), edit.field());
            if let Some(save) = edit.end(ui, &r) {
                let edit = self.edit.take().expect("editing");
                if save {
                    out.push(Command::Deck(deck, edit.save()));
                }
            }
        } else if let Some(label) = &m.label {
            ui.painter_at(name).text(
                pos2(name.left(), cy),
                egui::Align2::LEFT_CENTER,
                label,
                FontId::proportional(11.0),
                THEME.text,
            );
        }

        let forget = ui
            .put(
                Rect::from_center_size(pos2(rect.right() - 8.0, cy), vec2(14.0, CUE_ROW_H)),
                egui::Label::new(RichText::new("×").color(THEME.muted)).sense(Sense::click()),
            )
            .on_hover_text("Forget this one");
        if forget.clicked() {
            out.push(Command::Deck(deck, DeckCmd::DeleteMemory(m.id)));
        }
        row.widget_info(|| {
            let what = if m.end.is_some() { "loop" } else { "cue" };
            let at = util::fmt_tenths(m.start);
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                true,
                format!("Memory {what} at {at}"),
            )
        });
        let row = row.on_hover_text("Click: go here · Double-click: name it");
        if row.double_clicked() {
            self.edit = Some(LabelEdit::new(
                LabelTarget::Memory(m.id),
                m.label.as_deref(),
            ));
        } else if row.clicked() && !editing {
            out.push(Command::Deck(deck, DeckCmd::RecallMemory(m.clone())));
        }
    }
}

/// Prepare's cue pane, filling the space it's given.
pub struct CuePane<'a>(pub &'a mut DeckUi);

impl Panel for CuePane<'_> {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        crate::ui::theme::normal_text(ui);
        let info = cx.engine.deck(self.0.id);
        self.0.cue_pane(ui, info, cx.out);
    }
}
