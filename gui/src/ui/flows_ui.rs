//! Saved Flows in the left pane: folders you can nest, rename and drag between.
//! A Flow that is one chain of cards (a playlist) gets a list icon; one that
//! branches, a fork.

use egui::{Color32, RichText, Sense, Stroke, Ui, pos2, vec2};

use super::util;
use crate::core::flows::{FlowEntry, Folder};

/// Something in the tree; also the payload when it's dragged.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Item {
    Flow(i64),
    Folder(i64),
}

/// What the tree asks the app to do.
pub enum FlowsAction {
    Open(i64),
    /// A new Flow (or folder) in this folder (`None`: top level).
    New(Option<i64>),
    NewFolder(Option<i64>),
    Rename(Item, String),
    Move(Item, Option<i64>),
    Duplicate(i64),
    Delete(Item),
}

#[derive(Default)]
pub struct FlowsUi {
    pub folders: Vec<Folder>,
    pub flows: Vec<FlowEntry>,
    /// The item whose name is being edited, and the text so far.
    renaming: Option<(Item, String)>,
    /// Focus the rename box on the next frame.
    focus: bool,
}

impl FlowsUi {
    pub fn set(&mut self, (folders, flows): (Vec<Folder>, Vec<FlowEntry>)) {
        self.folders = folders;
        self.flows = flows;
    }

    /// Edit an item's name in place (e.g. just after creating it).
    pub fn start_rename(&mut self, item: Item) {
        let name = match item {
            Item::Flow(id) => self.flows.iter().find(|f| f.id == id).map(|f| &f.name),
            Item::Folder(id) => self.folders.iter().find(|f| f.id == id).map(|f| &f.name),
        };
        if let Some(name) = name {
            self.renaming = Some((item, name.clone()));
            self.focus = true;
        }
    }

    pub fn show(&mut self, ui: &mut Ui, open: Option<i64>, actions: &mut Vec<FlowsAction>) {
        let header = ui
            .horizontal(|ui| {
                ui.label(RichText::new("FLOWS").size(10.0).color(util::MUTED));
                ui.menu_button(RichText::new("+").color(util::MUTED), |ui| {
                    if ui.button("New Flow").clicked() {
                        actions.push(FlowsAction::New(None));
                        ui.close();
                    }
                    if ui.button("New folder").clicked() {
                        actions.push(FlowsAction::NewFolder(None));
                        ui.close();
                    }
                })
                .response
                .on_hover_text("New Flow or folder");
            })
            .response;
        // Drop on the heading: move to the top level.
        drop_target(ui, &header, None, actions);
        if self.folders.is_empty() && self.flows.is_empty() {
            ui.label(
                RichText::new("Flows you build are kept here.")
                    .size(11.0)
                    .color(util::MUTED),
            );
        }
        self.contents(ui, None, open, actions);
    }

    /// The folders and Flows directly inside `folder`.
    fn contents(
        &mut self,
        ui: &mut Ui,
        folder: Option<i64>,
        open: Option<i64>,
        actions: &mut Vec<FlowsAction>,
    ) {
        let subfolders: Vec<Folder> = self
            .folders
            .iter()
            .filter(|f| f.parent == folder)
            .cloned()
            .collect();
        for f in subfolders {
            let id = ui.make_persistent_id(("flow-folder", f.id));
            let state = egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                id,
                true,
            );
            let (_, header, _) = state
                .show_header(ui, |ui| self.folder_label(ui, &f, actions))
                .body(|ui| self.contents(ui, Some(f.id), open, actions));
            drop_target(ui, &header.response, Some(f.id), actions);
        }
        let flows: Vec<FlowEntry> = self
            .flows
            .iter()
            .filter(|f| f.folder == folder)
            .cloned()
            .collect();
        for f in flows {
            ui.horizontal(|ui| {
                ui.add_space(ui.spacing().indent - 14.0);
                flow_icon(ui, f.linear);
                self.flow_label(ui, &f, open == Some(f.id), actions);
            });
        }
    }

    fn folder_label(
        &mut self,
        ui: &mut Ui,
        f: &Folder,
        actions: &mut Vec<FlowsAction>,
    ) -> egui::Response {
        let item = Item::Folder(f.id);
        if let Some(r) = self.rename_box(ui, item, actions) {
            return r;
        }
        let r = ui
            .selectable_label(false, RichText::new(&f.name).strong())
            .interact(Sense::drag());
        r.dnd_set_drag_payload(item);
        r.context_menu(|ui| {
            if ui.button("New Flow here").clicked() {
                actions.push(FlowsAction::New(Some(f.id)));
                ui.close();
            }
            if ui.button("New folder here").clicked() {
                actions.push(FlowsAction::NewFolder(Some(f.id)));
                ui.close();
            }
            ui.separator();
            if ui.button("Rename").clicked() {
                self.start_rename(item);
                ui.close();
            }
            self.move_menu(ui, item, actions);
            let inside = self.count_inside(f.id);
            ui.menu_button("Delete", |ui| {
                let label = match inside {
                    0 => "Delete this folder".to_string(),
                    1 => "Delete this folder and its Flow".to_string(),
                    n => format!("Delete this folder and its {n} Flows"),
                };
                if ui.button(label).clicked() {
                    actions.push(FlowsAction::Delete(item));
                    ui.close();
                }
            });
        });
        r
    }

    fn flow_label(
        &mut self,
        ui: &mut Ui,
        f: &FlowEntry,
        open: bool,
        actions: &mut Vec<FlowsAction>,
    ) {
        let item = Item::Flow(f.id);
        if self.rename_box(ui, item, actions).is_some() {
            return;
        }
        let r = ui
            .selectable_label(open, &f.name)
            .interact(Sense::drag())
            .on_hover_text(format!(
                "{} card{}{}",
                f.cards,
                if f.cards == 1 { "" } else { "s" },
                if f.linear {
                    " in one chain (a playlist)"
                } else {
                    ", branching"
                }
            ));
        r.dnd_set_drag_payload(item);
        if r.clicked() {
            actions.push(FlowsAction::Open(f.id));
        }
        if r.double_clicked() {
            self.start_rename(item);
        }
        r.context_menu(|ui| {
            if ui.button("Rename").clicked() {
                self.start_rename(item);
                ui.close();
            }
            if ui.button("Duplicate").clicked() {
                actions.push(FlowsAction::Duplicate(f.id));
                ui.close();
            }
            self.move_menu(ui, item, actions);
            ui.menu_button("Delete", |ui| {
                if ui.button(format!("Delete \"{}\"", f.name)).clicked() {
                    actions.push(FlowsAction::Delete(item));
                    ui.close();
                }
            });
        });
    }

    /// The name being edited, if it's this item's. Enter or clicking away keeps it,
    /// Escape cancels.
    fn rename_box(
        &mut self,
        ui: &mut Ui,
        item: Item,
        actions: &mut Vec<FlowsAction>,
    ) -> Option<egui::Response> {
        let (editing, text) = self.renaming.as_mut()?;
        if *editing != item {
            return None;
        }
        let r = ui.add(egui::TextEdit::singleline(text).desired_width(150.0));
        if std::mem::take(&mut self.focus) {
            r.request_focus();
        }
        if r.lost_focus() {
            let name = text.trim().to_string();
            if !ui.input(|i| i.key_pressed(egui::Key::Escape)) && !name.is_empty() {
                actions.push(FlowsAction::Rename(item, name));
            }
            self.renaming = None;
        }
        Some(r)
    }

    /// "Move to" with every folder the item can go into.
    fn move_menu(&self, ui: &mut Ui, item: Item, actions: &mut Vec<FlowsAction>) {
        ui.menu_button("Move to", |ui| {
            if ui.button("Top level").clicked() {
                actions.push(FlowsAction::Move(item, None));
                ui.close();
            }
            for (f, depth) in self.folders_in_order(None, 0) {
                if matches!(item, Item::Folder(id) if self.is_within(f.id, id)) {
                    continue;
                }
                let label = format!("{}{}", "   ".repeat(depth), f.name);
                if ui.button(label).clicked() {
                    actions.push(FlowsAction::Move(item, Some(f.id)));
                    ui.close();
                }
            }
        });
    }

    /// Folders as the tree shows them, with their depth.
    fn folders_in_order(&self, parent: Option<i64>, depth: usize) -> Vec<(&Folder, usize)> {
        let mut out = vec![];
        for f in self.folders.iter().filter(|f| f.parent == parent) {
            out.push((f, depth));
            out.extend(self.folders_in_order(Some(f.id), depth + 1));
        }
        out
    }

    /// `folder` is `ancestor` or somewhere inside it.
    fn is_within(&self, folder: i64, ancestor: i64) -> bool {
        let mut at = Some(folder);
        while let Some(id) = at {
            if id == ancestor {
                return true;
            }
            at = self
                .folders
                .iter()
                .find(|f| f.id == id)
                .and_then(|f| f.parent);
        }
        false
    }

    /// Flows in a folder and its subfolders.
    fn count_inside(&self, folder: i64) -> usize {
        self.flows
            .iter()
            .filter(|f| f.folder.is_some_and(|id| self.is_within(id, folder)))
            .count()
    }
}

/// Highlight while a Flow or folder is dragged over `r`; moving it into `folder`
/// when it's dropped.
fn drop_target(ui: &Ui, r: &egui::Response, folder: Option<i64>, actions: &mut Vec<FlowsAction>) {
    use egui::DragAndDrop;
    if !ui.rect_contains_pointer(r.rect) || !DragAndDrop::has_payload_of_type::<Item>(ui.ctx()) {
        return;
    }
    ui.painter().rect_stroke(
        r.rect,
        3,
        Stroke::new(1.0, util::ACCENT),
        egui::StrokeKind::Inside,
    );
    if ui.input(|i| i.pointer.any_released())
        && let Some(item) = DragAndDrop::take_payload::<Item>(ui.ctx())
        && folder.is_none_or(|f| *item != Item::Folder(f))
    {
        actions.push(FlowsAction::Move(*item, folder));
    }
}

/// A list (one chain: a playlist) or a fork (branching).
fn flow_icon(ui: &mut Ui, linear: bool) {
    let (rect, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
    let p = ui.painter();
    let s = Stroke::new(1.3, Color32::from_gray(140));
    let (l, r, c) = (rect.left() + 1.0, rect.right() - 1.0, rect.center());
    if linear {
        for dy in [-3.5, 0.0, 3.5] {
            p.line_segment([pos2(l, c.y + dy), pos2(r, c.y + dy)], s);
        }
    } else {
        let fork = pos2(c.x - 1.0, c.y);
        p.line_segment([pos2(l, c.y), fork], s);
        p.line_segment([fork, pos2(r, c.y - 4.0)], s);
        p.line_segment([fork, pos2(r, c.y + 4.0)], s);
    }
}
