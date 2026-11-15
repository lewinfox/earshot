//! The sampler's bank: assigning sounds to its slots, their settings, and loading
//! the sounds in the background (`core::sampler`).

use std::path::PathBuf;

use earshot_engine::SampleSlot;

use super::*;
use crate::core::sampler::{self, SamplerCmd};

impl EarshotApp {
    /// At start: give the engine the saved bank's settings, and load its sounds.
    pub(super) fn start_sampler(&mut self) {
        sampler::set_bank(&self.player, &self.settings.sampler);
        let files: Vec<(SampleSlot, PathBuf)> = (self.settings.sampler.files())
            .map(|(slot, file)| (slot, file.to_path_buf()))
            .collect();
        for (slot, file) in files {
            self.load_sample(slot, file);
        }
    }

    /// Decode `file` in the background and put it in `slot`; how it went comes back
    /// as [`Event::Sample`].
    fn load_sample(&mut self, slot: SampleSlot, file: PathBuf) {
        self.sample_errors[slot.index()] = None;
        let ticket = sampler::request_load(&self.player, slot);
        let (player, sink) = (self.player.clone(), self.sink.clone());
        crate::core::background::spawn("earshot-sample", move || {
            let result = sampler::load(&player, slot, &file, ticket);
            sink(Event::Sample { slot, result });
        });
    }

    /// A slot's sound loaded, or couldn't be: say why (unless a newer one replaced it).
    pub(super) fn sample_loaded(&mut self, slot: SampleSlot, result: Result<(), String>) {
        let Err(e) = result else { return };
        if e == sampler::SUPERSEDED {
            return;
        }
        let name = self.settings.sampler.slot(slot).name().unwrap_or_default();
        self.notify(
            notice::Level::Warning,
            format!(
                "Sampler slot {}: couldn't load {name}: {e}",
                slot.index() + 1
            ),
        );
        self.sample_errors[slot.index()] = Some(e);
    }

    fn save_sampler(&self) {
        self.lib
            .store
            .send(settings::save(settings::SAMPLER, &self.settings.sampler));
    }

    pub(super) fn sampler_cmd(&mut self, cmd: SamplerCmd) {
        match cmd {
            // Straight to the engine (`command::dispatch`): here only without audio.
            SamplerCmd::Play(_) | SamplerCmd::Stop(_) => {}
            SamplerCmd::Volume(slot, v) => {
                let v = if v.is_finite() {
                    v.clamp(0.0, 1.0)
                } else {
                    return;
                };
                self.settings.sampler.slot_mut(slot).volume = v;
                sampler::set_volume(&self.player, slot, v);
                self.save_sampler();
            }
            SamplerCmd::Loop(slot, on) => {
                self.settings.sampler.slot_mut(slot).looping = on;
                sampler::set_loop(&self.player, slot, on);
                self.save_sampler();
            }
            SamplerCmd::PickFile(slot) => {
                // The picker blocks, so on its own thread; what's picked comes back as
                // a command.
                let sink = self.sink.clone();
                let dir = (self.settings.sampler.slot(slot).file.as_ref())
                    .and_then(|f| f.parent().map(|d| d.to_path_buf()));
                std::thread::spawn(move || {
                    let mut d = rfd::FileDialog::new()
                        .set_title(format!("A sound for sampler slot {}", slot.index() + 1))
                        .add_filter(
                            "Audio",
                            &[
                                "wav", "aif", "aiff", "flac", "mp3", "m4a", "aac", "ogg", "opus",
                            ],
                        )
                        .add_filter("All files", &["*"]);
                    if let Some(dir) = dir {
                        d = d.set_directory(dir);
                    }
                    if let Some(file) = d.pick_file() {
                        sink(Event::Command(Command::Sampler(SamplerCmd::Assign(
                            slot, file,
                        ))));
                    }
                });
            }
            SamplerCmd::Assign(slot, file) => {
                self.settings.sampler.slot_mut(slot).file = Some(file.clone());
                self.save_sampler();
                self.load_sample(slot, file);
            }
            SamplerCmd::AssignSelected(slot) => {
                let (lib, flow) = (&self.panels.library, &self.panels.flow);
                let Some(id) = lib.focused_track(flow) else {
                    self.notify(notice::Level::Info, "Select a track in the library first.");
                    return;
                };
                // Another library's track (not imported) by its file; earshot's own
                // from the Store.
                let file = match self.lib.store.outside.get(id) {
                    Some(t) => Ok(t.file),
                    None => (self.lib.store.reader.track_path(id)).map_err(|e| e.to_string()),
                };
                match file {
                    Ok(file) => self.sampler_cmd(SamplerCmd::Assign(slot, file)),
                    Err(e) => self.notify(
                        notice::Level::Warning,
                        format!("Sampler slot {}: {e}", slot.index() + 1),
                    ),
                }
            }
            SamplerCmd::Clear(slot) => {
                self.settings.sampler.slot_mut(slot).file = None;
                self.sample_errors[slot.index()] = None;
                sampler::clear(&self.player, slot);
                self.save_sampler();
            }
        }
    }

    /// What each deck's pads page shows in sampler mode: its 8 slots' names, and
    /// whether each has its sound and is playing.
    pub(super) fn sample_pads(
        &self,
        deck: DeckId,
    ) -> [crate::ui::panels::deck::SamplePad; sampler::PADS] {
        std::array::from_fn(|pad| {
            let Some(slot) = sampler::pad_slot(deck.index(), pad) else {
                return Default::default();
            };
            let snap = (self.frame.snap.as_ref()).map(|s| s.sampler[slot.index()]);
            crate::ui::panels::deck::SamplePad {
                slot: slot.index(),
                name: self.settings.sampler.slot(slot).name(),
                loaded: snap.is_some_and(|s| s.loaded),
                playing: snap.is_some_and(|s| s.playing),
                looping: self.settings.sampler.slot(slot).looping,
            }
        })
    }
}
