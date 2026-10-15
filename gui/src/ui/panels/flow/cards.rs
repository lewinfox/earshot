//! A card: the track's art, title and artist, a preview button, BPM, key and energy
//! chips, and its waveform as a seek bar. Ports on its edges for linking, a remove
//! button on its corner, and the track menu on right-click.

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Pos2, Rect, RichText, Sense, Stroke, Ui,
    UiBuilder, pos2, vec2,
};

use super::{Port, SEEK_H};
use crate::core::collections::{FlowGraph, FlowNode, GraphOp, NodeId};
use crate::core::command::{Command, PreviewCmd};
use crate::core::index::LibraryCmd;
use crate::core::track::Track;
use crate::ui::panels::library::tracks::{MenuAt, track_menu};
use crate::ui::panels::{CoreView, PreviewState};
use crate::ui::theme::{THEME, fs};
use crate::ui::util;
use crate::ui::widgets::{RoundButton, key_chip};

/// What a card asks for, carried out after every card is drawn.
pub enum CardAction {
    Edit(GraphOp),
    /// Open (or close) the "+" suggestions.
    Plus(NodeId),
    /// The card was dragged this far (screen points) this frame.
    Drag(NodeId, egui::Vec2),
    DragEnd,
    StartLink(NodeId, Port),
    Select(NodeId),
    Bake(NodeId),
}

/// How a card is drawn this frame.
pub struct CardState {
    /// Where it's drawn (canvas points).
    pub rect: Rect,
    /// This track is in the headphones' preview player.
    pub preview: Option<PreviewState>,
    /// A link being dragged would land here.
    pub link_target: bool,
    /// Clicked on: Delete removes it.
    pub selected: bool,
    /// The pointer is over the card (or just outside it).
    pub near: bool,
    /// The track's waveform thumbnail, once it has one.
    pub thumb: Option<egui::TextureId>,
    pub art: Option<egui::TextureId>,
}

#[allow(clippy::too_many_arguments)]
pub fn card(
    ui: &mut Ui,
    g: &FlowGraph,
    n: &FlowNode,
    t: &Track,
    v: &CoreView,
    state: CardState,
    acts: &mut Vec<CardAction>,
    out: &mut Vec<Command>,
) {
    let rect = state.rect;
    let resp = ui
        .interact(
            rect,
            ui.id().with(("flow-card", n.id)),
            Sense::click_and_drag(),
        )
        .on_hover_text(&t.path);
    if resp.clicked() || resp.drag_started() {
        acts.push(CardAction::Select(n.id));
    }
    if resp.dragged() {
        acts.push(CardAction::Drag(n.id, resp.drag_delta()));
    }
    if resp.drag_stopped() {
        acts.push(CardAction::DragEnd);
    }
    // Off the canvas it carries its track (so a deck can take it): see `canvas`.
    if resp.double_clicked() {
        out.push(LibraryCmd::Load(t.id).into());
    }
    resp.context_menu(|ui| menu(ui, g, n, t, v, acts, out));
    let border = if resp.dragged() || state.link_target || state.selected {
        THEME.accent
    } else if resp.hovered() {
        THEME.card_hover
    } else {
        THEME.card_line
    };
    let p = ui.painter();
    p.rect_filled(rect.translate(vec2(0.0, 4.0)), 8, THEME.card_shadow);
    p.rect(
        rect,
        8,
        THEME.card,
        Stroke::new(if state.selected { 2.0 } else { 1.0_f32 }, border),
        egui::StrokeKind::Inside,
    );
    seek_bar(ui, n, t, &state, out);

    let inner = rect.shrink2(vec2(8.0, 7.0));
    let mut ui = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(egui::Layout::top_down(Align::Min)),
    );
    ui.spacing_mut().item_spacing = vec2(8.0, 7.0);
    ui.horizontal(|ui| {
        art(ui, 34.0, t, state.art);
        let id_w = ui.available_width() - 28.0 - 8.0;
        ui.allocate_ui(vec2(id_w, 34.0), |ui| {
            ui.vertical(|ui| {
                ui.set_width(id_w);
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.add(
                    egui::Label::new(RichText::new(&t.title).strong())
                        .truncate()
                        .selectable(false),
                );
                ui.add(
                    egui::Label::new(RichText::new(&t.artist).size(fs(11.0)).color(THEME.muted))
                        .truncate()
                        .selectable(false),
                );
            });
        });
        let playing = state.preview.is_some_and(|p| p.playing);
        if ui
            .add(RoundButton::play_pause(playing))
            .on_hover_text(if playing {
                "Pause the preview"
            } else {
                "Preview in the headphones"
            })
            .clicked()
        {
            out.push(Command::Preview(PreviewCmd::Toggle(t.id)));
        }
    });
    chips_row(&mut ui, t, |ui| {
        ui.label(
            RichText::new(t.duration.map(util::mmss).unwrap_or_default())
                .size(fs(11.0))
                .color(THEME.muted),
        );
    });

    if state.selected || state.near {
        remove_button(&ui, rect.right_top(), n.id, acts);
    }
    in_port(&ui, rect.left_center(), n.id, acts);
    plus(&ui, rect.right_center(), n.id, acts);
}

/// Right-click: the track menu, then the Flow's own items.
fn menu(
    ui: &mut Ui,
    g: &FlowGraph,
    n: &FlowNode,
    t: &Track,
    v: &CoreView,
    acts: &mut Vec<CardAction>,
    out: &mut Vec<Command>,
) {
    track_menu(
        ui,
        &[t.id],
        v,
        MenuAt {
            viewing: v.open_flow,
        },
        out,
    );
    ui.separator();
    if ui
        .add_enabled(
            v.open_flow.is_some(),
            egui::Button::new("Save this path as a playlist"),
        )
        .on_hover_text("Back to a start and on to an end through this card, taking the top branch")
        .clicked()
    {
        acts.push(CardAction::Bake(n.id));
        ui.close();
    }
    // A card can follow several: unlink from each.
    for p in g.parents(n.id) {
        let title = g
            .node(p)
            .and_then(|p| v.lib.get(p.track))
            .map_or("?", |t| t.title.as_str());
        if ui.button(format!("Unlink from \"{title}\"")).clicked() {
            acts.push(CardAction::Edit(GraphOp::Unlink { from: p, to: n.id }));
            ui.close();
        }
    }
    if ui.button("Remove from the Flow").clicked() {
        acts.push(CardAction::Edit(GraphOp::Remove(n.id)));
        ui.close();
    }
}

/// A remove button on the card's top-right corner `c`.
fn remove_button(ui: &Ui, c: Pos2, node: NodeId, acts: &mut Vec<CardAction>) {
    let x = ui
        .interact(
            Rect::from_center_size(c, vec2(20.0, 20.0)),
            ui.id().with(("flow-remove", node)),
            Sense::click(),
        )
        .on_hover_text("Remove from the Flow (Delete)");
    let col = if x.hovered() { THEME.warn } else { THEME.muted };
    ui.painter()
        .circle(c, 9.0, THEME.bg, Stroke::new(1.5_f32, col));
    for d in [vec2(3.5, 3.5), vec2(3.5, -3.5)] {
        ui.painter()
            .line_segment([c - d, c + d], Stroke::new(1.5_f32, col));
    }
    if x.clicked() {
        acts.push(CardAction::Edit(GraphOp::Remove(node)));
    }
}

/// The in port on the left edge, at `c`: drag from it to another card to follow that
/// card too.
fn in_port(ui: &Ui, c: Pos2, node: NodeId, acts: &mut Vec<CardAction>) {
    let port = ui
        .interact(
            Rect::from_center_size(c, vec2(18.0, 18.0)),
            ui.id().with(("flow-in", node)),
            Sense::drag(),
        )
        .on_hover_text("Drag to a card that comes before this one");
    if port.drag_started() {
        acts.push(CardAction::StartLink(node, Port::In));
    }
    let col = if port.hovered() || port.dragged() {
        THEME.accent
    } else {
        THEME.muted
    };
    ui.painter()
        .circle(c, 5.0, THEME.bg, Stroke::new(2.0_f32, col));
}

/// "+" on the right edge, at `c`: click for suggestions, drag to link to the next card.
fn plus(ui: &Ui, c: Pos2, node: NodeId, acts: &mut Vec<CardAction>) {
    let plus = ui
        .interact(
            Rect::from_center_size(c, vec2(26.0, 26.0)),
            ui.id().with(("flow-plus", node)),
            Sense::click_and_drag(),
        )
        .on_hover_text("Click: suggest what to play next\nDrag: link to the next card");
    if plus.drag_started() {
        acts.push(CardAction::StartLink(node, Port::Out));
    }
    let col = if plus.hovered() {
        THEME.accent
    } else {
        THEME.muted
    };
    ui.painter()
        .circle(c, 13.0, THEME.bg, Stroke::new(2.0_f32, col));
    ui.painter().text(
        c,
        Align2::CENTER_CENTER,
        "+",
        FontId::proportional(fs(17.0)),
        if plus.hovered() {
            THEME.accent
        } else {
            THEME.marker
        },
    );
    if plus.clicked() {
        acts.push(CardAction::Plus(node));
    }
}

/// The track's waveform along the card's bottom edge: click or drag to preview from
/// there. Shows how far the preview has got.
fn seek_bar(ui: &mut Ui, n: &FlowNode, t: &Track, state: &CardState, out: &mut Vec<Command>) {
    let card = state.rect;
    let bar = Rect::from_min_max(
        pos2(card.left() + 8.0, card.bottom() - 6.0 - SEEK_H),
        pos2(card.right() - 8.0, card.bottom() - 6.0),
    );
    let resp = ui
        .interact(
            bar,
            ui.id().with(("flow-seek", n.id)),
            Sense::click_and_drag(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let frac = |x: f32| ((x - bar.left()) / bar.width()).clamp(0.0, 1.0);
    if (resp.clicked() || resp.dragged())
        && let Some(p) = resp.interact_pointer_pos()
    {
        out.push(Command::Preview(PreviewCmd::Seek(t.id, frac(p.x))));
    }
    if let (Some(p), Some(d)) = (resp.hover_pos(), t.duration) {
        resp.on_hover_text_at_pointer(util::mmss(d * frac(p.x) as f64));
    }

    let p = ui.painter();
    p.rect_filled(bar, 3, THEME.chips_bg);
    let tint = if state.preview.is_some() {
        THEME.marker
    } else {
        THEME.thumb_idle
    };
    if let Some(tex) = state.thumb {
        p.image(
            tex,
            bar,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            tint,
        );
    }
    if let Some(s) = state.preview {
        // Dim what's been played; a line at the playhead.
        let x = bar.left() + bar.width() * s.at.clamp(0.0, 1.0);
        p.rect_filled(
            Rect::from_min_max(bar.min, pos2(x, bar.bottom())),
            3,
            THEME.thumb_played,
        );
        p.line_segment(
            [pos2(x, bar.top() - 2.0), pos2(x, bar.bottom() + 2.0)],
            Stroke::new(2.0_f32, THEME.accent),
        );
    }
}

/// BPM, key and energy chips on a dark strip, with `right` added at its right end.
pub fn chips_row(ui: &mut Ui, t: &Track, right: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(THEME.chips_bg)
        .corner_radius(5)
        .inner_margin(egui::Margin::symmetric(5, 3))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                chip(
                    ui,
                    &t.bpm.map_or("-".into(), |b| format!("{:.1}", b)),
                    THEME.panel_2,
                    THEME.text,
                );
                match t.key {
                    Some(k) => key_chip(ui, k),
                    None => {
                        chip(ui, "?", THEME.line, THEME.text);
                    }
                }
                let e = t
                    .energy_level()
                    .map_or("-".into(), |e| format!("{:.0}", e.round()));
                chip(ui, &e, THEME.energy_chip, THEME.ink).on_hover_text(
                    "Energy (stand-in: loudness and brightness, ranked across your library)",
                );
                ui.with_layout(egui::Layout::right_to_left(Align::Center), right);
            });
        });
}

fn chip(ui: &mut Ui, text: &str, fill: Color32, colour: Color32) -> egui::Response {
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_string(), FontId::proportional(fs(11.0)), colour);
    let (rect, resp) = ui.allocate_exact_size(galley.size() + vec2(10.0, 2.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(4), fill);
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, colour);
    resp
}

/// The track's cover art, or a gradient with the artist's initials until it has one.
pub fn art(ui: &mut Ui, size: f32, t: &Track, tex: Option<egui::TextureId>) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    if let Some(tex) = tex {
        egui::Image::new((tex, rect.size()))
            .corner_radius(3)
            .paint_at(ui, rect);
        return;
    }
    let h = format!("{}{}", t.artist, t.title)
        .chars()
        .fold(0u32, |h, c| (h * 31 + c as u32) % 360) as f32;
    let initials: String = if t.artist.is_empty() {
        &t.title
    } else {
        &t.artist
    }
    .split_whitespace()
    .take(2)
    .filter_map(|w| w.chars().next())
    .collect::<String>()
    .to_uppercase();
    // A diagonal two-colour gradient.
    let (a, b) = (util::hsl(h, 0.55, 0.42), util::hsl(h + 60.0, 0.6, 0.28));
    let mut mesh = egui::Mesh::default();
    let mid = util::hsl(h + 30.0, 0.57, 0.35);
    mesh.colored_vertex(rect.left_top(), a);
    mesh.colored_vertex(rect.right_top(), mid);
    mesh.colored_vertex(rect.right_bottom(), b);
    mesh.colored_vertex(rect.left_bottom(), mid);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    ui.painter().add(mesh);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        initials,
        FontId::proportional(fs(size * 0.35)),
        THEME.marker,
    );
}
