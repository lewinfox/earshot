//! Edits and undo: edits go to the Store, which reports each one it made
//! (`StoreEvent::Edited`) for the history; the Flow canvas's are recorded when they're
//! saved. Undo and redo apply an edit exactly (`core::edit`).

use super::*;
use crate::core::edit::{Edit, EditCmd};

impl EarshotApp {
    pub(super) fn edit_cmd(&mut self, c: EditCmd) {
        match c {
            EditCmd::Editor(tracks) => self.modals.edit_tracks(tracks, &self.index),
            c => self.lib.store.send(StoreCmd::Edit(c)),
        }
    }

    pub(super) fn undo(&mut self) {
        if let Some(e) = self.edits.undo() {
            self.apply_edit(e);
        }
    }

    pub(super) fn redo(&mut self) {
        if let Some(e) = self.edits.redo() {
            self.apply_edit(e);
        }
    }

    /// Make an edit as it is (undo's inverse, or redo's edit), not recording it.
    fn apply_edit(&mut self, e: Edit) {
        let store = self.lib.store.clone();
        let flow = &mut self.panels.flow;
        match e {
            // The canvas has this Flow: change it there (else its next save would undo
            // this), and save at once so the change isn't recorded as a new edit.
            Edit::FlowGraph { flow: id, ops, .. } if flow.flow == Some(id) => {
                flow.apply_ops(&ops);
                if let Some((_, graph)) = flow.take_change() {
                    store.send(StoreCmd::SaveFlow { id, graph });
                }
            }
            // Players with the track need the grid switched back too, once it's saved.
            Edit::Grid { track, .. } => {
                let player = self.player.clone();
                std::thread::spawn(move || {
                    let done = store
                        .call(move |w| w.apply_edit(&e))
                        .map_err(|e| e.to_string())
                        .and_then(|_| player::reload_grid(&player, &store.reader, track));
                    if let Err(e) = done {
                        eprintln!("undo: {e}");
                    }
                });
            }
            e => {
                if let Edit::Name {
                    collection, new, ..
                } = &e
                    && flow.flow == Some(*collection)
                {
                    flow.name = new.clone();
                }
                store.send(StoreCmd::ApplyEdit(e));
            }
        }
    }
}
