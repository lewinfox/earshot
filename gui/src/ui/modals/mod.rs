//! Windows over the app, in one stack: the last one opened is on top. They're opened
//! and closed by `ViewCmd::ToggleModal` / `CloseModal`; what they change goes out as
//! commands like everything else.
//!
//! To add one: a variant of `core::view::Modal`, a module here with its drawing,
//! and an arm in [`Modals::show`] (and in [`Modals::open`] if it keeps state that
//! should start afresh each time it opens).

pub mod audio;
pub mod keys;
pub mod metadata;

use earshot_engine::{OutputInfo, Routing};

use self::audio::AudioUi;
use self::metadata::MetaEditor;
use crate::core::command::{Command, ViewCmd};
use crate::core::index::LibraryIndex;
use crate::core::keymap::Keymap;
use crate::core::player::AudioSettings;
use crate::core::track::TrackId;
use crate::core::view::{Modal, Mode};

/// What the windows show, gathered by the shell.
pub struct ModalEnv<'a> {
    pub audio_settings: &'a AudioSettings,
    pub audio: &'a Result<OutputInfo, String>,
    pub routing: Routing,
    pub keymap: &'a Keymap,
    pub mode: Mode,
}

#[derive(Default)]
pub struct Modals {
    /// Open windows, bottom first.
    stack: Vec<Modal>,
    /// Opened since last drawn: brought to the top when next drawn.
    raise: Option<Modal>,
    audio: AudioUi,
    meta: MetaEditor,
}

impl Modals {
    pub fn is_open(&self, m: Modal) -> bool {
        self.stack.contains(&m)
    }

    pub fn open(&mut self, m: Modal) {
        if self.is_open(m) {
            return;
        }
        if m == Modal::Audio {
            self.audio.reset();
        }
        self.stack.push(m);
        self.raise = Some(m);
    }

    /// Open the metadata editor on these tracks (afresh, if it was open).
    pub fn edit_tracks(&mut self, tracks: Vec<TrackId>, lib: &LibraryIndex) {
        self.meta.open(tracks, lib);
        self.open(Modal::Metadata);
        self.raise = Some(Modal::Metadata);
    }

    pub fn close(&mut self, m: Modal) {
        self.stack.retain(|&o| o != m);
    }

    pub fn toggle(&mut self, m: Modal) {
        if self.is_open(m) {
            self.close(m);
        } else {
            self.open(m);
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, env: &ModalEnv, out: &mut Vec<Command>) {
        if let Some(m) = self.raise.take() {
            let title = match m {
                Modal::Audio => audio::TITLE,
                Modal::Keys => keys::TITLE,
                Modal::Metadata => metadata::TITLE,
            };
            ctx.move_to_top(egui::LayerId::new(
                egui::Order::Middle,
                egui::Id::new(title),
            ));
        }
        for m in self.stack.clone() {
            let mut open = true;
            match m {
                Modal::Audio => {
                    let c =
                        self.audio
                            .show(ctx, env.audio_settings, env.audio, env.routing, &mut open);
                    out.extend(c);
                }
                Modal::Keys => {
                    egui::Window::new(keys::TITLE)
                        .open(&mut open)
                        .resizable(false)
                        .show(ctx, |ui| keys::keys_help(ui, env.keymap, env.mode));
                }
                Modal::Metadata => {
                    let mut done = false;
                    egui::Window::new(metadata::TITLE)
                        .open(&mut open)
                        .resizable(false)
                        .collapsible(false)
                        .show(ctx, |ui| done = self.meta.ui(ui, out));
                    open &= !done;
                }
            }
            if !open {
                out.push(Command::View(ViewCmd::CloseModal(m)));
            }
        }
    }
}
