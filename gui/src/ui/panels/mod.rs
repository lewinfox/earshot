//! The pieces a view is built from. Each panel draws from a [`Cx`]: core state to
//! read, the engine as it is this frame, the theme, and a list to push [`Command`]s
//! onto. Panels keep their own view state (selection, scroll, text being typed);
//! anything else they want changed goes out as a command, which the shell's
//! `dispatch` carries out after the frame.

pub mod deck;
pub mod flow;
pub mod library;
pub mod mixer;
pub mod status;
pub mod tempo_lock;

use earshot_engine::{DeckId, EngineSnapshot};
use egui::Ui;

use self::deck::DeckInfo;
use super::prefs::UiPrefs;
use super::theme::Theme;
use crate::core::collections::{CollectionId, Collections};
use crate::core::command::{Command, ViewCmd};
use crate::core::history::History;
use crate::core::index::LibraryIndex;
use crate::core::session::SLOTS;
use crate::core::track::TrackId;
use crate::core::view::Region;

/// Something drawn into a region of a view.
pub trait Panel {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx);
}

/// What a panel draws from, and where its commands go.
pub struct Cx<'a> {
    pub core: CoreView<'a>,
    pub engine: &'a EngineFrame,
    pub theme: &'a Theme,
    /// The UI's preferences (mode, layout, zoom), to read; change them by command.
    pub prefs: &'a UiPrefs,
    pub out: &'a mut Vec<Command>,
}

impl Cx<'_> {
    pub fn send(&mut self, cmd: impl Into<Command>) {
        self.out.push(cmd.into());
    }
}

/// Note a resizable panel's size (`now`, in points) once it has been dragged to a
/// new one; not while it slides in or out.
pub fn resized(ctx: &egui::Context, now: f32, region: Region, saved: f32, cx: &mut Cx) {
    if (now - saved).abs() >= 1.0 && ctx.input(|i| i.pointer.primary_down()) {
        cx.send(ViewCmd::Resize(region, now));
    }
}

/// The domain state the panes read, all from core.
#[derive(Clone, Copy)]
pub struct CoreView<'a> {
    pub lib: &'a LibraryIndex,
    pub history: &'a History,
    pub collections: &'a Collections,
    /// The Flow on the canvas, if it has been saved.
    pub open_flow: Option<CollectionId>,
}

/// The engine as it is this frame, read once by the shell before anything is drawn.
#[derive(Default)]
pub struct EngineFrame {
    /// The engine's snapshot (`None` while audio isn't running).
    pub snap: Option<EngineSnapshot>,
    /// Each player's track details, by `DeckId::index()`.
    pub decks: [Option<DeckInfo>; SLOTS],
    /// The track in the headphones' preview player.
    pub preview: Option<PreviewState>,
}

impl EngineFrame {
    /// A player's details this frame. Every player has them once the frame is read.
    pub fn deck(&self, id: DeckId) -> &DeckInfo {
        self.decks[id.index()].as_ref().expect("a player's info")
    }
}

/// The track in the headphones' preview player.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PreviewState {
    pub id: TrackId,
    /// How far through, 0..1.
    pub at: f32,
    /// Playing (or about to, while it decodes) rather than paused or finished.
    pub playing: bool,
}
