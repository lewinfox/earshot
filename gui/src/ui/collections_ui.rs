//! Collections in the left pane: Flows, crates, playlists and smart playlists, in
//! folders you can nest, rename and drag between. Each kind has its own icon; a
//! Flow that is one chain of cards gets a list, one that branches a fork. Drop
//! tracks on a crate, playlist or Flow to add them.

use egui::{Color32, RichText, Sense, Stroke, Ui, pos2, vec2};

use super::util;
use crate::core::collections::{
    Collection, CollectionCmd, CollectionId, CollectionKind, Collections,
};
use crate::core::track::TrackId;

/// A collection being dragged round the tree (a track's payload is a bare id).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Item(pub CollectionId);

/// Kinds the "+" menu makes, in its order.
const NEW: [CollectionKind; 5] = [
    CollectionKind::Flow,
    CollectionKind::Crate,
    CollectionKind::Playlist,
    CollectionKind::Smart,
    CollectionKind::Folder,
];

#[derive(Default)]
pub struct CollectionsUi {
    /// The item whose name is being edited, and the text so far.
    renaming: Option<(CollectionId, String)>,
    /// Focus the rename box on the next frame.
    focus: bool,
}

/// What the tree needs from the rest of the window.
pub struct TreeCtx<'a> {
    pub collections: &'a Collections,
    /// The one showing (on the canvas or as the track list), highlighted.
    pub open: Option<CollectionId>,
    /// The library's search, which a new smart playlist starts from.
    pub search: &'a str,
    /// The library's selection: dropping one of these tracks adds them all.
    pub selected: &'a [TrackId],
}

impl CollectionsUi {
    /// Edit an item's name in place (e.g. just after creating it).
    pub fn start_rename(&mut self, c: &Collection) {
        self.renaming = Some((c.id, c.name.clone()));
        self.focus = true;
    }

    pub fn show(&mut self, ui: &mut Ui, cx: &TreeCtx, out: &mut Vec<CollectionCmd>) {
        let header = ui
            .horizontal(|ui| {
                ui.label(RichText::new("COLLECTIONS").size(10.0).color(util::MUTED));
                ui.menu_button(RichText::new("+").color(util::MUTED), |ui| {
                    new_menu(ui, None, cx, out)
                })
                .response
                .on_hover_text("New Flow, crate, playlist or folder");
            })
            .response;
        // Drop on the heading: move to the top level.
        drop_target(ui, &header, None, cx, out);
        if cx.collections.all().is_empty() {
            ui.label(
                RichText::new("Flows, crates and playlists you make are kept here.")
                    .size(11.0)
                    .color(util::MUTED),
            );
        }
        self.contents(ui, None, cx, out);
    }

    /// What's directly inside `folder`.
    fn contents(
        &mut self,
        ui: &mut Ui,
        folder: Option<CollectionId>,
        cx: &TreeCtx,
        out: &mut Vec<CollectionCmd>,
    ) {
        for c in cx.collections.children(folder) {
            if c.kind == CollectionKind::Folder {
                let id = ui.make_persistent_id(("collection-folder", c.id));
                let state = egui::collapsing_header::CollapsingState::load_with_default_open(
                    ui.ctx(),
                    id,
                    true,
                );
                let (_, header, _) = state
                    .show_header(ui, |ui| self.folder_label(ui, c, cx, out))
                    .body(|ui| self.contents(ui, Some(c.id), cx, out));
                drop_target(ui, &header.response, Some(c), cx, out);
            } else {
                let r = ui.horizontal(|ui| {
                    ui.add_space(ui.spacing().indent - 14.0);
                    icon(ui, c);
                    self.label(ui, c, cx, out);
                });
                drop_target(ui, &r.response, Some(c), cx, out);
            }
        }
    }

    fn folder_label(
        &mut self,
        ui: &mut Ui,
        f: &Collection,
        cx: &TreeCtx,
        out: &mut Vec<CollectionCmd>,
    ) {
        if self.rename_box(ui, f.id, out) {
            return;
        }
        let r = ui
            .selectable_label(cx.open == Some(f.id), RichText::new(&f.name).strong())
            .interact(Sense::click_and_drag());
        r.dnd_set_drag_payload(Item(f.id));
        if r.clicked() {
            out.push(CollectionCmd::Open(f.id));
        }
        r.context_menu(|ui| {
            ui.menu_button("New here", |ui| new_menu(ui, Some(f.id), cx, out));
            ui.separator();
            if ui.button("Rename").clicked() {
                self.start_rename(f);
                ui.close();
            }
            move_menu(ui, f, cx.collections, out);
            let inside = cx.collections.count_inside(f.id);
            ui.menu_button("Delete", |ui| {
                let label = match inside {
                    0 => "Delete this folder".to_string(),
                    1 => "Delete this folder and the one thing in it".to_string(),
                    n => format!("Delete this folder and the {n} things in it"),
                };
                if ui.button(label).clicked() {
                    out.push(CollectionCmd::Delete(f.id));
                    ui.close();
                }
            });
        });
    }

    fn label(&mut self, ui: &mut Ui, c: &Collection, cx: &TreeCtx, out: &mut Vec<CollectionCmd>) {
        if self.rename_box(ui, c.id, out) {
            return;
        }
        let n = c.tracks.len();
        let tip = match c.kind {
            CollectionKind::Flow => format!(
                "Flow: {n} card{}{}",
                if n == 1 { "" } else { "s" },
                if c.linear {
                    " in one chain"
                } else {
                    ", branching"
                }
            ),
            CollectionKind::Smart => format!(
                "Smart playlist: {}",
                c.query
                    .as_deref()
                    .filter(|q| !q.is_empty())
                    .unwrap_or("every track")
            ),
            k => format!("{}: {n} track{}", k.label(), if n == 1 { "" } else { "s" }),
        };
        let r = ui
            .selectable_label(cx.open == Some(c.id), &c.name)
            .interact(Sense::click_and_drag())
            .on_hover_text(tip);
        r.dnd_set_drag_payload(Item(c.id));
        if r.clicked() {
            out.push(CollectionCmd::Open(c.id));
        }
        if r.double_clicked() {
            self.start_rename(c);
        }
        r.context_menu(|ui| {
            if ui.button("Rename").clicked() {
                self.start_rename(c);
                ui.close();
            }
            if ui.button("Duplicate").clicked() {
                out.push(CollectionCmd::Duplicate(c.id));
                ui.close();
            }
            if c.kind == CollectionKind::Smart
                && ui
                    .add_enabled(
                        !cx.search.trim().is_empty(),
                        egui::Button::new("Use the library's search"),
                    )
                    .clicked()
            {
                out.push(CollectionCmd::SetQuery(c.id, cx.search.trim().to_string()));
                ui.close();
            }
            move_menu(ui, c, cx.collections, out);
            ui.menu_button("Delete", |ui| {
                if ui.button(format!("Delete \"{}\"", c.name)).clicked() {
                    out.push(CollectionCmd::Delete(c.id));
                    ui.close();
                }
            });
        });
    }

    /// The name being edited, if it's this item's. Enter or clicking away keeps it,
    /// Escape cancels. True when it drew the box.
    fn rename_box(&mut self, ui: &mut Ui, id: CollectionId, out: &mut Vec<CollectionCmd>) -> bool {
        let Some((editing, text)) = self.renaming.as_mut() else {
            return false;
        };
        if *editing != id {
            return false;
        }
        let r = ui.add(egui::TextEdit::singleline(text).desired_width(150.0));
        if std::mem::take(&mut self.focus) {
            r.request_focus();
        }
        if r.lost_focus() {
            let name = text.trim().to_string();
            if !ui.input(|i| i.key_pressed(egui::Key::Escape)) && !name.is_empty() {
                out.push(CollectionCmd::Rename(id, name));
            }
            self.renaming = None;
        }
        true
    }
}

/// "New Flow", "New crate" … in `parent`.
fn new_menu(ui: &mut Ui, parent: Option<CollectionId>, cx: &TreeCtx, out: &mut Vec<CollectionCmd>) {
    for kind in NEW {
        let label = match kind {
            CollectionKind::Smart if !cx.search.trim().is_empty() => {
                "New smart playlist from the search".to_string()
            }
            k => format!("New {}", k.label()),
        };
        if ui.button(label).clicked() {
            out.push(CollectionCmd::Create {
                kind,
                parent,
                tracks: vec![],
                query: (kind == CollectionKind::Smart).then(|| cx.search.trim().to_string()),
            });
            ui.close();
        }
    }
}

/// "Move to" with every folder the item can go into.
fn move_menu(ui: &mut Ui, c: &Collection, cs: &Collections, out: &mut Vec<CollectionCmd>) {
    ui.menu_button("Move to", |ui| {
        if ui.button("Top level").clicked() {
            out.push(CollectionCmd::Move(c.id, None));
            ui.close();
        }
        for (f, depth) in cs.folders_in_order() {
            if c.kind == CollectionKind::Folder && cs.is_within(f.id, c.id) {
                continue;
            }
            let label = format!("{}{}", "   ".repeat(depth), f.name);
            if ui.button(label).clicked() {
                out.push(CollectionCmd::Move(c.id, Some(f.id)));
                ui.close();
            }
        }
    });
}

/// Highlight while something that fits is dragged over `r`. A collection dropped
/// on a folder (or the heading, `None`) moves there; tracks dropped on a crate,
/// playlist or Flow are added to it.
fn drop_target(
    ui: &Ui,
    r: &egui::Response,
    target: Option<&Collection>,
    cx: &TreeCtx,
    out: &mut Vec<CollectionCmd>,
) {
    use egui::DragAndDrop;
    if !ui.rect_contains_pointer(r.rect) {
        return;
    }
    let folder = target.is_none_or(|t| t.kind == CollectionKind::Folder);
    let item = folder && DragAndDrop::has_payload_of_type::<Item>(ui.ctx());
    let tracks = target.is_some_and(|t| t.kind.takes_tracks())
        && DragAndDrop::has_payload_of_type::<TrackId>(ui.ctx());
    if !item && !tracks {
        return;
    }
    ui.painter().rect_stroke(
        r.rect,
        3,
        Stroke::new(1.0_f32, util::ACCENT),
        egui::StrokeKind::Inside,
    );
    if !ui.input(|i| i.pointer.any_released()) {
        return;
    }
    if item
        && let Some(Item(id)) = DragAndDrop::take_payload::<Item>(ui.ctx())
            .as_deref()
            .copied()
        && target.is_none_or(|t| t.id != id)
    {
        out.push(CollectionCmd::Move(id, target.map(|t| t.id)));
    }
    if tracks
        && let Some(t) = target
        && let Some(id) = DragAndDrop::take_payload::<TrackId>(ui.ctx())
            .as_deref()
            .copied()
    {
        let tracks = if cx.selected.contains(&id) {
            cx.selected.to_vec()
        } else {
            vec![id]
        };
        out.push(CollectionCmd::AddTracks { to: t.id, tracks });
    }
}

/// A small painted icon per kind: a list (playlist, or a Flow in one chain), a fork
/// (a branching Flow), a box (crate) or a star-ish ring (smart playlist).
fn icon(ui: &mut Ui, c: &Collection) {
    let (rect, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
    let p = ui.painter();
    let s = Stroke::new(1.3_f32, Color32::from_gray(140));
    let (l, r, m) = (rect.left() + 1.0, rect.right() - 1.0, rect.center());
    match c.kind {
        CollectionKind::Flow if !c.linear => {
            let fork = pos2(m.x - 1.0, m.y);
            p.line_segment([pos2(l, m.y), fork], s);
            p.line_segment([fork, pos2(r, m.y - 4.0)], s);
            p.line_segment([fork, pos2(r, m.y + 4.0)], s);
        }
        CollectionKind::Flow | CollectionKind::Playlist => {
            for dy in [-3.5, 0.0, 3.5] {
                p.line_segment([pos2(l, m.y + dy), pos2(r, m.y + dy)], s);
            }
            if c.kind == CollectionKind::Playlist {
                p.circle_filled(pos2(l, m.y - 3.5), 1.3, Color32::from_gray(140));
            }
        }
        CollectionKind::Crate => {
            p.rect_stroke(rect.shrink(1.5), 1, s, egui::StrokeKind::Inside);
            p.line_segment([pos2(l + 1.0, m.y - 1.0), pos2(r - 1.0, m.y - 1.0)], s);
        }
        CollectionKind::Smart => {
            p.circle_stroke(m, 4.5, s);
            p.circle_filled(m, 1.5, Color32::from_gray(140));
        }
        CollectionKind::Folder => {}
    }
}
