//! Commands for the library, its collections and the Flow canvas, and loading
//! tracks onto the players.

use super::*;
use crate::core::edit::EditCmd;

impl EarshotApp {
    pub(super) fn library_cmd(&mut self, c: LibraryCmd) {
        match c {
            LibraryCmd::Load(track) => self.load(None, track),
            LibraryCmd::Analyse { tracks, dynamic } => self.analyse(tracks, dynamic),
            LibraryCmd::PickRoots => {
                // The picker blocks, so on its own thread; what's picked comes back as
                // a command.
                let sink = self.sink.clone();
                std::thread::spawn(move || {
                    if let Some(dirs) = rfd::FileDialog::new()
                        .set_title("Add music folders to the library")
                        .pick_folders()
                    {
                        sink(Event::Command(LibraryCmd::AddRoots(dirs).into()));
                    }
                });
            }
            LibraryCmd::AddRoots(folders) => {
                self.lib.store.send(StoreCmd::AddRoots(folders));
                self.rescan();
            }
            LibraryCmd::RemoveRoot(path) => self.lib.store.send(StoreCmd::RemoveRoot(path)),
            LibraryCmd::Rescan => self.rescan(),
        }
    }

    /// The collection tree's and track menus' actions become Store commands; the
    /// new list comes back as an event.
    pub(super) fn collection_cmd(&mut self, c: CollectionCmd) {
        let store = self.lib.store.clone();
        let lib = &mut self.panels.library;
        let flow = &mut self.panels.flow;
        match c {
            CollectionCmd::Open(id) => match self.collections.get(id).map(|c| c.kind) {
                Some(kind) => {
                    // A Flow goes on the canvas. Beside it (the Flow tab), the track
                    // list stays as it was; elsewhere it lists the Flow's tracks.
                    if kind == CollectionKind::Flow {
                        store.send(StoreCmd::LoadFlow(id));
                    }
                    if kind != CollectionKind::Flow || !self.prefs.flow_tab() {
                        lib.browser.set_scope(Scope::Collection(id));
                    }
                }
                None => {}
            },
            CollectionCmd::Show(id) => {
                if self.collections.get(id).is_some() {
                    lib.browser.set_scope(Scope::Collection(id));
                }
            }
            CollectionCmd::Create {
                kind: CollectionKind::Folder,
                parent,
                ..
            } => {
                // In its parent's section; at the top level, Playlists'.
                let section = parent
                    .and_then(|p| self.collections.get(p))
                    .map_or(Section::Playlists, |p| p.section);
                self.collection_cmd(CollectionCmd::CreateFolder { section, parent });
            }
            CollectionCmd::CreateFolder { section, parent } => {
                let id = store.new_collection_id();
                store.send(StoreCmd::CreateFolder {
                    id,
                    parent,
                    section,
                    name: CollectionKind::Folder.untitled().to_string(),
                });
                self.pending_rename = Some(id);
            }
            CollectionCmd::Create {
                kind,
                parent,
                tracks,
                query,
            } => {
                let id = store.new_collection_id();
                let name = kind.untitled().to_string();
                store.send(StoreCmd::Create {
                    id,
                    parent,
                    kind,
                    name: name.clone(),
                    query,
                });
                if kind == CollectionKind::Flow {
                    // Onto the canvas, in the Flow editor. The canvas holds it from
                    // now on: its tracks go there (as starting cards), or its first
                    // save would drop them.
                    self.load_flow(Some((id, name, Default::default())));
                    self.panels.flow.add_tracks(&tracks);
                    self.show_flow_tab();
                } else {
                    if !tracks.is_empty() {
                        store.send(StoreCmd::AddTracks { id, tracks });
                    }
                    lib.browser.set_scope(Scope::Collection(id));
                }
                self.pending_rename = Some(id);
            }
            // As the playlist's whole new order, so it's one undo step.
            CollectionCmd::MoveEntries { playlist, from, to } => {
                if let Some(c) = self.collections.get(playlist) {
                    let tracks = crate::core::collections::move_items(&c.tracks, &from, to);
                    if tracks != c.tracks {
                        store.send(StoreCmd::Edit(EditCmd::SetTracks {
                            collection: playlist,
                            tracks,
                        }));
                    }
                }
            }
            CollectionCmd::Insert {
                playlist,
                tracks,
                at,
            } => {
                if let Some(c) = self.collections.get(playlist) {
                    let mut all = c.tracks.clone();
                    let at = at.min(all.len());
                    all.splice(at..at, tracks);
                    store.send(StoreCmd::Edit(EditCmd::SetTracks {
                        collection: playlist,
                        tracks: all,
                    }));
                }
            }
            CollectionCmd::Rename(id, name) => {
                if flow.flow == Some(id) {
                    flow.name = name.clone();
                }
                store.send(StoreCmd::Edit(EditCmd::Rename {
                    collection: id,
                    name,
                }));
            }
            CollectionCmd::Move(id, parent) => store.send(StoreCmd::Edit(EditCmd::Move {
                collection: id,
                parent,
            })),
            CollectionCmd::Duplicate(from) => {
                let to = store.new_collection_id();
                store.send(StoreCmd::Duplicate { from, to });
                self.pending_rename = Some(to);
            }
            // The open Flow may go with it: see `StoreEvent::Deleted`.
            CollectionCmd::Delete(id) => store.send(StoreCmd::Delete(id)),
            CollectionCmd::AddTracks { to, tracks } => {
                if flow.flow == Some(to) {
                    // The canvas has it: add there, or its next save would undo this.
                    flow.add_tracks(&tracks);
                } else {
                    store.send(StoreCmd::Edit(EditCmd::AddTracks { to, tracks }));
                }
            }
            CollectionCmd::RemoveTracks { from, tracks } => {
                store.send(StoreCmd::Edit(EditCmd::RemoveTracks { from, tracks }))
            }
            CollectionCmd::SetQuery(id, query) => store.send(StoreCmd::SetQuery { id, query }),
        }
    }

    pub(super) fn flow_cmd(&mut self, c: FlowCmd) {
        match c {
            FlowCmd::AddTracks(tracks) => self.flow().add_tracks(&tracks),
            FlowCmd::BakePath { flow, nodes, name } => {
                // The Store bakes from what it has: save the canvas first.
                self.save_flow();
                let playlist = self.lib.store.new_collection_id();
                self.lib.store.send(StoreCmd::ExportPath {
                    flow,
                    nodes,
                    playlist,
                    name,
                });
                self.pending_rename = Some(playlist);
            }
        }
    }

    pub(super) fn flow(&mut self) -> &mut FlowUi {
        &mut self.panels.flow
    }

    /// Prepare's Flow tab.
    pub(super) fn show_flow_tab(&mut self) {
        self.prefs.mode = Mode::Prepare;
        self.prefs.prepare.tool = Tool::Flow;
    }

    /// Ask for the collections and the Flow that was open last time.
    pub(super) fn restore_flow(&mut self) {
        self.lib.store.send(StoreCmd::ListCollections);
        if let Some(id) = self.settings.flow_open {
            self.lib.store.send(StoreCmd::LoadFlow(id));
        }
    }

    /// Put a Flow on the canvas (`None`: an empty one), looking where it was looking
    /// last. Where the canvas was looking before is saved first.
    pub(super) fn load_flow(&mut self, flow: Option<(CollectionId, String, FlowGraph)>) {
        self.save_flow_view_now();
        let camera = flow.as_ref().and_then(|(id, ..)| {
            settings::read(&self.lib.store.reader, &settings::flow_camera(*id))
        });
        self.flow().load(flow, camera);
    }

    /// Save which Flow is open, and where the canvas looks once it has held still.
    pub(super) fn save_flow_view(&mut self, ctx: &egui::Context) {
        let open = self.panels.flow.flow;
        if open != self.settings.flow_open {
            self.lib.store.send(settings::save(settings::FLOW_OPEN, &open));
            self.settings.flow_open = open;
        }
        let Some(id) = open else {
            return;
        };
        let busy = ctx.input(|i| i.pointer.any_down());
        if let Some(camera) = self.panels.flow.camera_to_save(busy) {
            self.lib
                .store
                .send(settings::save(&settings::flow_camera(id), &camera));
        } else if let Some(wait) = self.panels.flow.camera_wait() {
            // No frames come while nothing moves: wake up to save it.
            ctx.request_repaint_after(wait);
        }
    }

    /// Save where the canvas looks now, held still or not.
    fn save_flow_view_now(&mut self) {
        if let Some(id) = self.panels.flow.flow
            && let Some(camera) = self.panels.flow.unsaved_camera()
        {
            self.lib
                .store
                .send(settings::save(&settings::flow_camera(id), &camera));
        }
    }

    /// Save the canvas if it changed, making a new Flow for the first card.
    /// Each save is one undo step: the canvas only has a change once a gesture ends.
    pub(super) fn save_flow(&mut self) {
        let Some((before, graph)) = self.flow().take_change() else {
            return;
        };
        let store = self.lib.store.clone();
        let flow = &mut self.panels.flow;
        let id = match flow.flow {
            Some(id) => id,
            None if graph.is_empty() => return,
            None => {
                let id = store.new_collection_id();
                let name = CollectionKind::Flow.untitled().to_string();
                store.send(StoreCmd::Create {
                    id,
                    parent: None,
                    kind: CollectionKind::Flow,
                    name: name.clone(),
                    query: None,
                });
                flow.name = name;
                flow.flow = Some(id);
                id
            }
        };
        self.edits.record(edit::flow_edit(id, &before, &graph));
        store.send(StoreCmd::SaveFlow { id, graph });
    }

    pub(super) fn rescan(&mut self) {
        self.panels.library.browser.scanning = true;
        let (store, sink, hasher) = (
            self.lib.store.clone(),
            self.sink.clone(),
            self.hasher.clone(),
        );
        std::thread::spawn(move || {
            sink(Event::Scanned(library::scan_library(&store)));
            let _ = hasher.send(());
        });
    }

    pub(super) fn load(&mut self, deck: Option<DeckId>, track_id: i64) {
        // Double-click: Prepare's player, or the first deck that isn't playing.
        let decks = &self.panels.decks;
        let deck = deck.unwrap_or_else(|| match self.prefs.mode {
            Mode::Prepare => DeckId::PREPARE,
            Mode::Perform => DeckId::first(self.prefs.decks().len())
                .find(|d| !decks[d.index()].playing())
                .unwrap_or(DeckId::new(0).expect("deck 1")),
        });
        // Show the track's details at once; the audio follows when it has decoded.
        self.panels.decks[deck.index()]
            .start_load(PendingLoad::new(track_id, self.index.get(track_id)));
        let (player, lib, sink) = (self.player.clone(), self.lib.clone(), self.sink.clone());
        std::thread::spawn(move || {
            let result = player::load_track(&player, &lib, deck, track_id);
            sink(Event::Loaded {
                deck,
                track_id,
                result,
            });
        });
    }

    pub(super) fn analyse(&self, ids: Vec<i64>, dynamic: bool) {
        let mode = if dynamic {
            AnalysisMode::Dynamic
        } else {
            AnalysisMode::Static
        };
        for id in ids {
            self.lib.analyser.queue(id, None, true, mode);
        }
    }
}
