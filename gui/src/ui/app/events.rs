//! Events from other threads: loads, analysis, the Store, MIDI, cover art.

use super::*;

impl EarshotApp {
    pub(super) fn drain_events(&mut self, ctx: &egui::Context) {
        while let Ok(e) = self.rx.try_recv() {
            match e {
                Event::Art(id, rgba) => {
                    let s = library::ART_SIZE as usize;
                    let tex = rgba.map(|px| {
                        let img = egui::ColorImage::from_rgba_unmultiplied([s, s], &px);
                        ctx.load_texture(format!("art-{id}"), img, egui::TextureOptions::LINEAR)
                    });
                    self.panels.library.media.art.insert(id, tex);
                }
                Event::Scanned(Ok(())) => self.panels.library.browser.scanning = false,
                Event::Store(e) => self.store_event(e),
                Event::Scanned(Err(e)) => {
                    self.panels.library.browser.scanning = false;
                    eprintln!("scan: {e}");
                }
                Event::Loaded {
                    deck,
                    track_id,
                    result,
                } => {
                    // Stop showing the pending track (on failure the deck shows what
                    // it had). A stale result, for an older request, changes nothing.
                    self.panels.decks[deck.index()].finish_load(track_id);
                    match result {
                        Ok(()) => self.refresh_cues(deck),
                        Err(e) => eprintln!("load to deck {}: {e}", deck.index() + 1),
                    }
                }
                Event::Analysis(ev) => {
                    let a = &mut self.analysis;
                    match &ev.state {
                        Some(AnalysisState::Queued | AnalysisState::Running { .. }) => {
                            a.busy.insert(ev.track_id);
                        }
                        _ => {
                            a.busy.remove(&ev.track_id);
                        }
                    }
                    match &ev.state {
                        Some(AnalysisState::Running { progress, .. }) => {
                            a.running.insert(ev.track_id, *progress);
                        }
                        _ => {
                            a.running.remove(&ev.track_id);
                        }
                    }
                    let title = self
                        .index
                        .get(ev.track_id)
                        .map(|t| t.title.clone())
                        .unwrap_or_default();
                    if let Some(AnalysisState::Failed(e)) = &ev.state {
                        a.failed += 1;
                        a.last_error = format!("{title}: {e}");
                    }
                    // The results themselves (BPM, grid, thumbnail) come from the
                    // Store as `TracksChanged` and `Previews`.
                    self.index.set_analysis(ev.track_id, ev.state);
                }
                Event::Midi {
                    port,
                    bytes,
                    mapped,
                } => {
                    let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02X}")).collect();
                    self.midi_log.push_front(format!(
                        "{} = {}",
                        hex.join(" "),
                        mapped.unwrap_or_else(|| "unmapped".into())
                    ));
                    self.midi_log.truncate(6);
                    let _ = port;
                }
                Event::Command(cmd) => self.dispatch(cmd),
                Event::TempoLock(e) => self.tempo_lock_event(e),
            }
        }
    }

    pub(super) fn store_event(&mut self, e: StoreEvent) {
        match e {
            StoreEvent::Roots(roots) => self.panels.library.browser.folders = roots,
            StoreEvent::Library(tracks) => {
                self.index.replace(tracks);
                self.refresh_previews(None);
            }
            StoreEvent::TracksChanged(tracks) => self.index.patch(tracks),
            StoreEvent::Previews(p) => {
                for (id, bytes) in p {
                    self.panels.library.media.set_preview(id, bytes);
                }
            }
            StoreEvent::Collections(list) => {
                self.collections.set(list);
                let collections = &self.collections;
                if let Some(id) = self
                    .pending_rename
                    .take_if(|id| collections.get(*id).is_some())
                {
                    self.panels
                        .library
                        .collections
                        .start_rename(collections.get(id).unwrap());
                }
            }
            StoreEvent::Cues { track, cues } => {
                // Only for a track still on a player.
                if self.loaded_tracks().contains(&track) {
                    self.cues.insert(track, Arc::new(cues));
                }
            }
            StoreEvent::FlowLoaded(flow) => self.load_flow(flow),
            StoreEvent::Deleted(ids) => {
                if self.flow().flow.is_some_and(|id| ids.contains(&id)) {
                    self.flow().load(None, None);
                }
                let lib = &mut self.panels.library;
                if matches!(lib.browser.scope(), Scope::Collection(id) if ids.contains(id)) {
                    lib.browser.set_scope(Scope::All);
                }
            }
            StoreEvent::Edited(e) => self.edits.record(e),
            StoreEvent::Error(e) => eprintln!("library: {e}"),
        }
    }

    /// A player has a new track: ask for its hot cue labels and memory cues (they
    /// arrive as `StoreEvent::Cues`), and forget those of tracks no longer loaded.
    pub(super) fn refresh_cues(&mut self, deck: DeckId) {
        let loaded = self.loaded_tracks();
        self.cues.retain(|id, _| loaded.contains(id));
        let track = self.player.lock().unwrap().session(deck).track_id;
        if let Some(id) = track {
            self.lib.store.request_cues(id, store_sink(&self.sink));
        }
    }

    /// The tracks on the players now.
    pub(super) fn loaded_tracks(&self) -> Vec<i64> {
        let inner = self.player.lock().unwrap();
        inner.decks.iter().filter_map(|s| s.track_id).collect()
    }

    /// Load waveform thumbnails (on a reader thread; they arrive as an event).
    pub(super) fn refresh_previews(&mut self, ids: Option<Vec<i64>>) {
        self.lib.store.request_previews(ids, store_sink(&self.sink));
    }
}
