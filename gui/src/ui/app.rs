//! The window: top bar, the stacked waveforms, the decks and mixer, and the library.
//! Repaints every frame (vsync), reading the engine's state directly.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use earshot_engine::{
    DeckCmd as EngineCmd, DeckId, EngineSnapshot, Monitor, OutputInfo, OutputOptions, TempoRange,
};
use egui::{Key, RichText, Stroke, Ui, vec2};

use super::audio_ui::AudioUi;
use super::collections_ui::{CollectionsUi, TreeCtx};
use super::deck::{DeckInfo, DeckUi, PendingLoad, Side};
use super::flow::FlowUi;
use super::library_ui::{LibraryUi, PreviewState, Scope};
use super::mixer;
use super::suggest_ui::SuggestUi;
use super::tracks::{Media, View, take_wanted_art};
use super::util;
use crate::core::analysis::{Analyser, Mode};
use crate::core::collections::{CollectionCmd, CollectionId, CollectionKind, Collections, FlowCmd};
use crate::core::command::{self, AudioCmd, Command, DeckCmd, PreviewCmd, ViewCmd};
use crate::core::history::{DeckNow, History};
use crate::core::index::{LibraryCmd, LibraryIndex};
use crate::core::keymap::{self, Keymap};
use crate::core::library::{self, Library};
use crate::core::midi::{self, Midi};
use crate::core::player::{self, AudioSettings, DECKS, Player};
use crate::core::session::{self, SLOTS};
use crate::core::store::{self, EventSink, Store, StoreCmd, StoreEvent};
use crate::core::track::AnalysisState;
use crate::core::{Event, Sink};

pub struct EarshotApp {
    player: Player,
    lib: Arc<Library>,
    sink: Sink,
    rx: Receiver<Event>,
    shared: Option<Monitor>,
    /// The engine's state for this frame.
    snap: Option<EngineSnapshot>,
    audio: Result<OutputInfo, String>,
    _midi: Midi,
    midi_ports: Vec<String>,
    midi_log: VecDeque<String>,
    /// Each player's view, indexed by `DeckId::index()`.
    decks: [DeckUi; SLOTS],
    /// Prepare (one player, grid and cue editing) or Perform (the decks and a mixer).
    mode: UiMode,
    /// Memory cues and loops of the track in Prepare's player.
    memories: Vec<store::Memory>,
    /// Real seconds of waveform either side of the playhead, shared by both decks.
    half_window: f64,
    /// The library, collections and play history: the domain state the panes read.
    index: LibraryIndex,
    collections: Collections,
    history: History,
    /// Waveform thumbnails and cover art for every list.
    media: Media,
    library: LibraryUi,
    suggest: SuggestUi,
    flow: FlowUi,
    colls: CollectionsUi,
    /// Send track ids here to load their cover art.
    art_loader: std::sync::mpsc::Sender<i64>,
    /// Nudge after a scan: works out audio hashes the files' tags didn't have.
    hasher: std::sync::mpsc::Sender<()>,
    /// A collection just made, to rename once the Store's list has it.
    pending_rename: Option<CollectionId>,
    /// Which list the keyboard and the controller's browse knob move through.
    pane: Pane,
    analysis: AnalysisSummary,
    hud: Hud,
    show_keys: bool,
    audio_settings: AudioSettings,
    audio_ui: AudioUi,
    /// The track in the headphones' preview player.
    preview: Option<Preview>,
    keymap: Keymap,
    /// Keys held down for buttons that act while held, and what letting go sends.
    held_keys: HashMap<Key, Command>,
    /// Folders picked in the import dialog (on its own thread).
    picked: Arc<Mutex<Option<Vec<PathBuf>>>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    Library,
    Suggest,
}

/// Where the Flow, audio settings and waveform zoom are kept in eframe's storage.
const FLOW_KEY: &str = "earshot.flow";
const AUDIO_KEY: &str = "earshot.audio";
/// Waveform zoom: seconds either side of the playhead.
const WAVE_ZOOM_KEY: &str = "earshot.wave_zoom";
/// Each deck's tempo fader range, deck 1 first.
const TEMPO_RANGE_KEY: &str = "earshot.tempo_ranges";
const MODE_KEY: &str = "earshot.mode";

/// The two halves of the app, like Rekordbox's.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
enum UiMode {
    /// One player across the top for setting cues, fixing grids and building playlists.
    Prepare,
    /// The decks and the mixer, for playing a set.
    #[default]
    Perform,
}

impl UiMode {
    fn keys(self) -> keymap::Mode {
        match self {
            UiMode::Prepare => keymap::Mode::Prepare,
            UiMode::Perform => keymap::Mode::Perform,
        }
    }
}

/// Start audio on the chosen device; failing that the default one, then the null
/// output (so the app still runs without a sound card).
fn start_audio(player: &Player, settings: &AudioSettings) -> Result<OutputInfo, String> {
    let opts = player::output_options(settings);
    player::start_engine(player, &opts)
        .or_else(|e| {
            if opts.device.is_none() {
                return Err(e);
            }
            eprintln!("audio: {e}; trying the default device");
            player::start_engine(
                player,
                &OutputOptions {
                    device: None,
                    ..opts.clone()
                },
            )
        })
        .or_else(|e| {
            eprintln!("audio: {e}; falling back to the null output");
            player::start_engine(player, &OutputOptions { null: true, ..opts })
        })
}

#[derive(Default)]
struct AnalysisSummary {
    busy: HashSet<i64>,
    running: HashMap<i64, f64>,
    failed: u32,
    last_error: String,
}

/// The track asked to play in the headphones.
struct Preview {
    id: i64,
    /// Still decoding: the engine doesn't have it yet.
    decoding: Arc<AtomicBool>,
    /// Where it was asked to start (0..1), shown while it decodes.
    start: f32,
}

#[derive(Default)]
struct Hud {
    frames: VecDeque<(Instant, f32)>,
    text: String,
    audio: String,
    last: Option<Instant>,
}

/// Where earshot keeps its data (the name is the app id it was first released under).
/// `EARSHOT_DATA_DIR` overrides it (e.g. to try things on a copy).
fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("EARSHOT_DATA_DIR") {
        return PathBuf::from(d);
    }
    directories::BaseDirs::new()
        .map(|d| d.data_dir().join("io.github.lewinfox.earshot"))
        .unwrap_or_else(|| PathBuf::from("."))
}

impl EarshotApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Result<EarshotApp, String> {
        super::theme(&cc.egui_ctx);
        let (tx, rx) = channel();
        let ctx = cc.egui_ctx.clone();
        let tx = Mutex::new(tx);
        let sink: Sink = Arc::new(move |e| {
            let _ = tx.lock().unwrap().send(e);
            ctx.request_repaint();
        });

        let player = player::new_player();
        let audio_settings: AudioSettings = cc
            .storage
            .and_then(|s| eframe::get_value(s, AUDIO_KEY))
            .unwrap_or_default();
        player.lock().unwrap().routing = audio_settings.routing;
        let audio = start_audio(&player, &audio_settings);
        let shared = player.lock().unwrap().monitor();

        let store = Store::open(&data_dir().join(store::FILE_NAME), store_sink(&sink))
            .map_err(|e| e.to_string())?;
        let analyser = Analyser::start(sink.clone(), player.clone(), store.clone());
        let hasher = library::start_hasher(store.clone());
        let art_loader = library::start_art_loader(store.reader.clone(), sink.clone());
        let lib = Arc::new(Library { store, analyser });
        let mut app = EarshotApp {
            player,
            lib,
            sink,
            rx,
            shared,
            snap: None,
            audio,
            _midi: Midi::default(),
            midi_ports: vec![],
            midi_log: VecDeque::new(),
            decks: std::array::from_fn(|i| DeckUi::new(session::slot(i))),
            mode: cc
                .storage
                .and_then(|s| eframe::get_value(s, MODE_KEY))
                .unwrap_or_default(),
            memories: vec![],
            half_window: cc
                .storage
                .and_then(|s| eframe::get_value(s, WAVE_ZOOM_KEY))
                .unwrap_or(4.0),
            index: LibraryIndex::default(),
            collections: Collections::default(),
            history: History::default(),
            media: Media::default(),
            library: LibraryUi::default(),
            suggest: SuggestUi::default(),
            flow: FlowUi::from_saved(
                cc.storage
                    .and_then(|s| eframe::get_value(s, FLOW_KEY))
                    .unwrap_or_default(),
            ),
            colls: CollectionsUi::default(),
            art_loader,
            hasher,
            pending_rename: None,
            pane: Pane::Library,
            analysis: AnalysisSummary::default(),
            hud: Hud::default(),
            show_keys: false,
            audio_settings,
            audio_ui: AudioUi::default(),
            preview: None,
            keymap: Keymap::built_in()
                .spell_keys(|name| Key::from_name(name).map(|k| k.name().to_string()))?,
            held_keys: HashMap::new(),
            picked: Arc::new(Mutex::new(None)),
        };
        app.lib.store.send(StoreCmd::ListRoots);
        let ranges: Vec<TempoRange> = cc
            .storage
            .and_then(|s| eframe::get_value(s, TEMPO_RANGE_KEY))
            .unwrap_or_default();
        for (deck, range) in DeckId::first(DECKS).zip(ranges) {
            app.dispatch((deck, EngineCmd::TempoRange(range)).into());
        }
        app.restore_flow();
        app.rescan();
        app.connect_midi();
        Ok(app)
    }

    /// (Re)open every MIDI input, e.g. after plugging the controller in.
    fn connect_midi(&mut self) {
        // Close the old ports first so they can be reopened.
        self._midi = Midi::default();
        let lane = self.player.lock().unwrap().midi.clone();
        (self._midi, self.midi_ports) = midi::connect_all(&self.sink, &lane).unwrap_or_else(|e| {
            eprintln!("MIDI: {e}");
            (Midi::default(), vec![])
        });
    }

    /// Each deck's tempo range, as the engine has it.
    fn tempo_ranges(&self) -> Vec<TempoRange> {
        let Some(sh) = &self.snap else {
            return vec![];
        };
        sh.live().iter().map(|d| d.range).collect()
    }

    // ---- Prepare mode -----------------------------------------------------------------

    /// Ask for the memory cues of the track in Prepare's player (they arrive as
    /// `StoreEvent::Memories`).
    fn refresh_memories(&mut self) {
        match self
            .player
            .lock()
            .unwrap()
            .session(DeckId::PREPARE)
            .track_id
        {
            Some(id) => self.lib.store.request_memories(id, store_sink(&self.sink)),
            None => self.memories.clear(),
        }
    }

    // ---- collections ------------------------------------------------------------------

    /// Ask for the collections and the Flow that was open last time.
    fn restore_flow(&mut self) {
        self.lib.store.send(StoreCmd::ListCollections);
        if let Some(id) = self.flow.flow {
            self.lib.store.send(StoreCmd::LoadFlow(id));
        }
    }

    /// Save the canvas if it changed, making a new Flow for the first card.
    fn save_flow(&mut self) {
        let Some(graph) = self.flow.changed_graph() else {
            return;
        };
        let store = &self.lib.store;
        let id = match self.flow.flow {
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
                self.flow.name = name;
                self.flow.flow = Some(id);
                id
            }
        };
        store.send(StoreCmd::SaveFlow { id, graph });
    }

    /// The collection tree's and track menus' actions become Store commands; the
    /// new list comes back as an event.
    fn collection_cmd(&mut self, c: CollectionCmd) {
        let store = &self.lib.store;
        match c {
            CollectionCmd::Open(id) => match self.collections.get(id).map(|c| c.kind) {
                Some(CollectionKind::Flow) => {
                    store.send(StoreCmd::LoadFlow(id));
                    self.flow.open = true;
                }
                Some(_) => {
                    self.library.set_scope(Scope::Collection(id));
                    self.flow.open = false;
                }
                None => {}
            },
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
                if !tracks.is_empty() {
                    store.send(StoreCmd::AddTracks { id, tracks });
                }
                match kind {
                    CollectionKind::Flow => {
                        self.flow.load(Some((id, name, Default::default())));
                        self.flow.open = true;
                    }
                    CollectionKind::Folder => {}
                    _ => {
                        self.library.set_scope(Scope::Collection(id));
                        self.flow.open = false;
                    }
                }
                self.pending_rename = Some(id);
            }
            CollectionCmd::Rename(id, name) => {
                if self.flow.flow == Some(id) {
                    self.flow.name = name.clone();
                }
                store.send(StoreCmd::Rename { id, name });
            }
            CollectionCmd::Move(id, parent) => store.send(StoreCmd::Move { id, parent }),
            CollectionCmd::Duplicate(from) => {
                let to = store.new_collection_id();
                store.send(StoreCmd::Duplicate { from, to });
                self.pending_rename = Some(to);
            }
            // The open Flow may go with it: see `StoreEvent::Deleted`.
            CollectionCmd::Delete(id) => store.send(StoreCmd::Delete(id)),
            CollectionCmd::AddTracks { to, tracks } => {
                if self.flow.flow == Some(to) {
                    // The canvas has it: add there, or its next save would undo this.
                    self.flow.add_tracks(&tracks);
                } else {
                    store.send(StoreCmd::AddTracks { id: to, tracks });
                }
            }
            CollectionCmd::RemoveTracks { from, tracks } => {
                store.send(StoreCmd::RemoveTracks { id: from, tracks })
            }
            CollectionCmd::SetQuery(id, query) => store.send(StoreCmd::SetQuery { id, query }),
        }
    }

    fn flow_cmd(&mut self, c: FlowCmd) {
        match c {
            FlowCmd::AddTracks(tracks) => {
                self.flow.open = true;
                self.flow.add_tracks(&tracks);
            }
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

    fn library_cmd(&mut self, c: LibraryCmd) {
        match c {
            LibraryCmd::Load(track) => self.load(None, track),
            LibraryCmd::Analyse { tracks, dynamic } => self.analyse(tracks, dynamic),
            LibraryCmd::RemoveRoot(path) => self.lib.store.send(StoreCmd::RemoveRoot(path)),
        }
    }

    fn rescan(&mut self) {
        self.library.scanning = true;
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

    fn load(&mut self, deck: Option<DeckId>, track_id: i64) {
        // Double-click: Prepare's player, or the first deck that isn't playing.
        let deck = deck.unwrap_or_else(|| match self.mode {
            UiMode::Prepare => DeckId::PREPARE,
            UiMode::Perform => DeckId::first(DECKS)
                .find(|d| !self.decks[d.index()].playing())
                .unwrap_or(DeckId::new(0).expect("deck 1")),
        });
        // Show the track's details at once; the audio follows when it has decoded.
        self.decks[deck.index()].start_load(PendingLoad::new(track_id, self.index.get(track_id)));
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

    /// Carry out a command, from whichever input. The decks, mixer and engine's are
    /// done in core; what's left is the UI's.
    fn dispatch(&mut self, cmd: Command) {
        match command::dispatch(&self.player, &self.lib.store, cmd) {
            Ok(Some(cmd)) => self.dispatch_ui(cmd),
            Ok(None) => {}
            Err(e) => eprintln!("command: {e}"),
        }
    }

    /// Commands about what's on screen, loads, previews and restarting audio.
    fn dispatch_ui(&mut self, cmd: Command) {
        match cmd {
            Command::View(ViewCmd::Browse(n)) => match self.pane {
                Pane::Library => self.library.move_focus(n),
                Pane::Suggest => self.suggest.move_focus(n),
            },
            Command::View(ViewCmd::TogglePane) => {
                self.pane = match self.pane {
                    Pane::Library => Pane::Suggest,
                    Pane::Suggest => Pane::Library,
                };
            }
            Command::View(ViewCmd::FocusSearch) => self.library.search_focus = true,
            Command::View(ViewCmd::LoadSelected(deck)) => {
                let id = match self.pane {
                    Pane::Library => self.library.focused(),
                    Pane::Suggest => self.suggest.focused(),
                };
                if let Some(id) = id {
                    self.load(Some(deck), id);
                }
            }
            Command::Deck(deck, DeckCmd::Load(id)) => self.load(Some(deck), id),
            Command::Library(c) => self.library_cmd(c),
            Command::Collection(c) => self.collection_cmd(c),
            Command::Flow(c) => self.flow_cmd(c),
            Command::Preview(PreviewCmd::Toggle(id)) => self.toggle_preview(id),
            Command::Preview(PreviewCmd::Seek(id, at)) => self.seek_preview(id, at),
            Command::Preview(PreviewCmd::Stop(id)) => self.stop_preview(id),
            Command::Audio(AudioCmd::Restart(settings)) => {
                self.audio_settings = settings;
                self.preview = None;
                self.audio = start_audio(&self.player, &self.audio_settings);
                self.shared = self.player.lock().unwrap().monitor();
                self.snap = self.shared.as_ref().map(|m| m.snapshot());
            }
            other => eprintln!("command: nothing handles {other:?}"),
        }
    }

    /// The decks as the play history sees them: track, playing, tempo, key shift.
    fn decks_now(&self) -> Vec<DeckNow> {
        let inner = self.player.lock().unwrap();
        DeckId::first(DECKS)
            .map(|d| {
                let track = inner.session(d).track_id;
                match &self.snap {
                    Some(sh) => {
                        let sh = sh.deck(d);
                        DeckNow {
                            track,
                            playing: sh.playing,
                            bpm: sh.bpm,
                            // How far the key has moved: not at all with keylock.
                            key_speed: if sh.keylock { 1.0 } else { sh.tempo },
                        }
                    }
                    None => DeckNow {
                        track,
                        key_speed: 1.0,
                        ..Default::default()
                    },
                }
            })
            .collect()
    }

    /// Note which deck started playing last, and mark tracks played after 30 s.
    fn track_plays(&mut self) {
        let decks = self.decks_now();
        self.history.tick(Instant::now(), &decks);
    }

    /// Suggestions after the source's reference: a deck's track at its actual tempo
    /// and key, or the selected track (the Flow's selected card, while it's open).
    fn refresh_suggestions(&mut self) {
        let decks = self.decks_now();
        let selected = match self.flow.open {
            true => self.flow.selected_track().or(self.library.focused()),
            false => self.library.focused(),
        };
        let view = View {
            lib: &self.index,
            history: &self.history,
            collections: &self.collections,
            open_flow: self.flow.flow,
        };
        self.suggest.refresh(&view, &decks, selected);
    }

    /// Play a track in the headphones from `start` (0..1), decoding it first. A track
    /// with no beatgrid yet is queued for analysis too.
    fn start_preview(&mut self, id: i64, start: f32) {
        if self.index.get(id).is_some_and(|t| !t.analysed()) && self.index.analysis(id).is_none() {
            self.lib.analyser.queue(id, None, false, Mode::Static);
        }
        let decoding = Arc::new(AtomicBool::new(true));
        self.preview = Some(Preview {
            id,
            decoding: decoding.clone(),
            start,
        });
        let (player, store) = (self.player.clone(), self.lib.store.clone());
        std::thread::spawn(move || {
            if let Err(e) = player::preview(&player, &store, id, start as f64) {
                eprintln!("preview: {e}");
            }
            decoding.store(false, Relaxed);
        });
    }

    /// Play a track in the headphones, or pause / resume it if it's the one loaded.
    fn toggle_preview(&mut self, id: i64) {
        match self.preview_state() {
            Some(p) if p.id == id => {
                if self
                    .preview
                    .as_ref()
                    .is_some_and(|p| p.decoding.load(Relaxed))
                {
                    return;
                }
                if let Err(e) = player::play_preview(&self.player, !p.playing) {
                    eprintln!("preview: {e}");
                }
            }
            _ => self.start_preview(id, 0.0),
        }
    }

    /// Move the preview of `id` to `at` (0..1), starting it there if it isn't loaded.
    fn seek_preview(&mut self, id: i64, at: f32) {
        let loaded = self
            .preview
            .as_ref()
            .is_some_and(|p| p.id == id && !p.decoding.load(Relaxed));
        if !loaded {
            return self.start_preview(id, at);
        }
        if let Err(e) = player::seek_preview(&self.player, at as f64) {
            eprintln!("preview: {e}");
        }
    }

    fn stop_preview(&mut self, id: i64) {
        if self.preview.as_ref().is_some_and(|p| p.id == id) {
            self.preview = None;
            if let Err(e) = player::stop_preview(&self.player) {
                eprintln!("preview: {e}");
            }
        }
    }

    /// The track in the preview player, how far through it is, and whether it's playing.
    fn preview_state(&self) -> Option<PreviewState> {
        let p = self.preview.as_ref()?;
        if p.decoding.load(Relaxed) {
            return Some(PreviewState {
                id: p.id,
                at: p.start,
                playing: true,
            });
        }
        let sh = &self.snap.as_ref()?.preview;
        let at = if sh.duration > 0.0 {
            sh.position / sh.duration
        } else {
            0.0
        };
        Some(PreviewState {
            id: p.id,
            at: at as f32,
            playing: sh.playing,
        })
    }

    fn analyse(&self, ids: Vec<i64>, dynamic: bool) {
        for id in ids {
            self.lib.analyser.queue(
                id,
                None,
                true,
                if dynamic { Mode::Dynamic } else { Mode::Static },
            );
        }
    }

    /// Load waveform thumbnails (on a reader thread; they arrive as an event).
    fn refresh_previews(&mut self, ids: Option<Vec<i64>>) {
        self.lib.store.request_previews(ids, store_sink(&self.sink));
    }

    fn store_event(&mut self, e: StoreEvent) {
        match e {
            StoreEvent::Roots(roots) => self.library.folders = roots,
            StoreEvent::Library(tracks) => {
                self.index.replace(tracks);
                self.refresh_previews(None);
            }
            StoreEvent::TracksChanged(tracks) => self.index.patch(tracks),
            StoreEvent::Previews(p) => {
                for (id, bytes) in p {
                    self.media.set_preview(id, bytes);
                }
            }
            StoreEvent::Collections(list) => {
                self.collections.set(list);
                let collections = &self.collections;
                if let Some(id) = self
                    .pending_rename
                    .take_if(|id| collections.get(*id).is_some())
                {
                    self.colls.start_rename(collections.get(id).unwrap());
                }
            }
            StoreEvent::Memories { track, memories } => {
                // Only the track still in Prepare's player.
                if self
                    .player
                    .lock()
                    .unwrap()
                    .session(DeckId::PREPARE)
                    .track_id
                    == Some(track)
                {
                    self.memories = memories;
                }
            }
            StoreEvent::FlowLoaded(flow) => self.flow.load(flow),
            StoreEvent::Deleted(ids) => {
                if self.flow.flow.is_some_and(|id| ids.contains(&id)) {
                    self.flow.load(None);
                }
                if matches!(self.library.scope(), Scope::Collection(id) if ids.contains(id)) {
                    self.library.set_scope(Scope::All);
                }
            }
            StoreEvent::Error(e) => eprintln!("library: {e}"),
        }
    }

    // ---- events from other threads ------------------------------------------------------

    fn drain_events(&mut self, ctx: &egui::Context) {
        while let Ok(e) = self.rx.try_recv() {
            match e {
                Event::Art(id, rgba) => {
                    let s = library::ART_SIZE as usize;
                    let tex = rgba.map(|px| {
                        let img = egui::ColorImage::from_rgba_unmultiplied([s, s], &px);
                        ctx.load_texture(format!("art-{id}"), img, egui::TextureOptions::LINEAR)
                    });
                    self.media.art.insert(id, tex);
                }
                Event::Scanned(Ok(())) => self.library.scanning = false,
                Event::Store(e) => self.store_event(e),
                Event::Scanned(Err(e)) => {
                    self.library.scanning = false;
                    eprintln!("scan: {e}");
                }
                Event::Loaded {
                    deck,
                    track_id,
                    result,
                } => {
                    // Stop showing the pending track (on failure the deck shows what
                    // it had). A stale result, for an older request, changes nothing.
                    self.decks[deck.index()].finish_load(track_id);
                    match result {
                        Ok(()) if deck == DeckId::PREPARE => self.refresh_memories(),
                        Ok(()) => {}
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
            }
        }
        let picked = self.picked.lock().unwrap().take();
        if let Some(folders) = picked {
            self.lib.store.send(StoreCmd::AddRoots(folders));
            self.rescan();
        }
    }

    // ---- keyboard ---------------------------------------------------------------------------

    /// Keys do what the keymap says. Deck keys work unless a text field has focus;
    /// Esc leaves the field.
    fn keyboard(&mut self, ctx: &egui::Context) {
        let typing = ctx.wants_keyboard_input();
        let events: Vec<_> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match *e {
                    egui::Event::Key {
                        key,
                        physical_key,
                        pressed,
                        repeat,
                        modifiers,
                    } => Some((physical_key.unwrap_or(key), pressed, repeat, modifiers)),
                    _ => None,
                })
                .collect()
        });
        for (key, pressed, repeat, modifiers) in events {
            if key == Key::Escape {
                ctx.memory_mut(|m| m.stop_text_input());
                continue;
            }
            if !pressed {
                // Let go of a held button, whatever the modifiers are now.
                if let Some(cmd) = self.held_keys.remove(&key) {
                    self.dispatch(cmd);
                }
                continue;
            }
            if typing || modifiers.command || modifiers.alt {
                continue;
            }
            let Some(b) = self
                .keymap
                .find(self.mode.keys(), key.name(), modifiers.shift)
            else {
                continue;
            };
            if repeat && !b.repeats() {
                continue;
            }
            let (press, release) = (b.press(), b.release());
            if let Some(release) = release {
                self.held_keys.insert(key, release);
            }
            if let Some(cmd) = press {
                self.dispatch(cmd);
            }
        }
    }

    // ---- panels -------------------------------------------------------------------------------

    fn top_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("earshot").strong().color(util::ACCENT));
            egui::ComboBox::from_id_salt("mode")
                .selected_text(match self.mode {
                    UiMode::Prepare => "Prepare",
                    UiMode::Perform => "Perform",
                })
                .width(90.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.mode, UiMode::Prepare, "Prepare")
                        .on_hover_text("One player: set cues, fix beat grids, build playlists");
                    ui.selectable_value(&mut self.mode, UiMode::Perform, "Perform")
                        .on_hover_text("Two decks and the mixer");
                });
            ui.menu_button("Menu", |ui| {
                ui.menu_button("Import", |ui| {
                    if ui.button("Folder…").clicked() {
                        let picked = self.picked.clone();
                        let ctx = ui.ctx().clone();
                        std::thread::spawn(move || {
                            if let Some(dirs) = rfd::FileDialog::new()
                                .set_title("Add music folders to the library")
                                .pick_folders()
                            {
                                *picked.lock().unwrap() = Some(dirs);
                                ctx.request_repaint();
                            }
                        });
                        ui.close();
                    }
                });
                if ui.button("Audio…").clicked() {
                    self.audio_ui.toggle();
                    ui.close();
                }
                if ui.button("Rescan library").clicked() {
                    self.rescan();
                    ui.close();
                }
                ui.menu_button("View", |ui| {
                    ui.menu_button("Size", |ui| {
                        let now = ui.ctx().zoom_factor();
                        for z in [0.8, 0.9, 1.0, 1.1, 1.25] {
                            if ui
                                .radio((now - z).abs() < 0.01, format!("{:.0}%", z * 100.0))
                                .clicked()
                            {
                                ui.ctx().set_zoom_factor(z);
                                ui.close();
                            }
                        }
                    });
                });
            });
            let (audio, ok) = match &self.audio {
                Ok(i) => (
                    format!(
                        "{}{} · {} kHz · {} frames · {} ch",
                        if i.null { "⚠ " } else { "" },
                        i.device,
                        i.sample_rate / 1000,
                        i.buffer_frames.map_or("default".into(), |b| b.to_string()),
                        i.channels
                    ),
                    !i.null,
                ),
                Err(e) => (format!("no audio: {e}"), false),
            };
            if pill(ui, &audio, if ok { util::OK } else { util::WARN })
                .on_hover_text("Audio settings")
                .clicked()
            {
                self.audio_ui.toggle();
            }
            let midi = if self.midi_ports.is_empty() {
                "MIDI: none".to_string()
            } else {
                format!("MIDI: {}", self.midi_ports.join(", "))
            };
            if pill(
                ui,
                &midi,
                if self.midi_ports.is_empty() {
                    util::LINE
                } else {
                    util::OK
                },
            )
            .on_hover_text("Click to reconnect MIDI")
            .clicked()
            {
                self.connect_midi();
            }
            self.analysis_pill(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Keys").clicked() {
                    self.show_keys = !self.show_keys;
                }
                ui.label(RichText::new(&self.hud.audio).monospace().size(11.0));
                ui.label(RichText::new(&self.hud.text).monospace().size(11.0));
            });
        });
    }

    fn analysis_pill(&mut self, ui: &mut Ui) {
        let a = &self.analysis;
        let fail = if a.failed > 0 {
            format!(" · {} failed", a.failed)
        } else {
            String::new()
        };
        let n = a.busy.len();
        let text = if n > 0 {
            format!("analysing {n} track{}{fail}", if n == 1 { "" } else { "s" })
        } else if a.failed > 0 {
            format!("analysis: {} failed", a.failed)
        } else {
            return;
        };
        let r = pill(
            ui,
            &text,
            // Only failures, nothing running: red. Still working: amber.
            if a.failed > 0 && n == 0 {
                util::BAD
            } else {
                util::WARN
            },
        );
        let running: Vec<String> = a
            .running
            .keys()
            .filter_map(|id| self.index.get(*id))
            .map(|t| format!("Now: {}", t.title))
            .collect();
        let failures = if a.last_error.is_empty() {
            "Click to dismiss failures".to_string()
        } else {
            format!("Last error: {}\nClick to dismiss.", a.last_error)
        };
        let r = r.on_hover_text([running.join("\n"), failures].join("\n").trim().to_string());
        if r.clicked() {
            self.analysis.failed = 0;
            self.analysis.last_error.clear();
        }
    }

    fn deck_info(&self, deck: DeckId) -> DeckInfo {
        let inner = self.player.lock().unwrap();
        let s = inner.session(deck);
        let t = s.track_id.and_then(|id| self.index.get(id));
        DeckInfo {
            title: s.title.clone(),
            artist: s.artist.clone(),
            grid: s.grid.clone(),
            grid_source: s.grid_source.clone(),
            generation: s.generation,
            waveform: s.waveform.clone(),
            detail: s.detail.clone(),
            tag_bpm: t.and_then(|t| t.tag_bpm),
            key: t.and_then(|t| t.key),
            analysing: s.track_id.and_then(|id| match self.index.analysis(id)? {
                AnalysisState::Running { progress, stage } => Some((*progress, stage.to_string())),
                AnalysisState::Queued => Some((0.0, "waiting to analyse".into())),
                AnalysisState::Failed(_) => None,
            }),
            jump_size: s.jump_size,
            loop_size: s.loop_size,
            leader: inner.sync_leader == Some(deck),
        }
    }

    fn tick_hud(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        let dt = ctx.input(|i| i.unstable_dt);
        self.hud.frames.push_back((now, dt));
        while self
            .hud
            .frames
            .front()
            .is_some_and(|(t, _)| now.duration_since(*t).as_secs_f32() > 2.0)
        {
            self.hud.frames.pop_front();
        }
        if self
            .hud
            .last
            .is_some_and(|l| now.duration_since(l).as_secs_f32() < 0.5)
        {
            return;
        }
        self.hud.last = Some(now);
        let mut dts: Vec<f32> = self.hud.frames.iter().map(|f| f.1).collect();
        if dts.is_empty() {
            return;
        }
        dts.sort_by(|a, b| b.total_cmp(a));
        let avg = dts.len() as f32 / dts.iter().sum::<f32>();
        let low = 1.0 / dts[(dts.len() / 100).min(dts.len() - 1)];
        self.hud.text = format!(
            "{avg:.0} fps · 1% {low:.0} · worst {:.0} ms",
            dts[0] * 1000.0
        );
        if let Some(m) = &self.shared {
            let (snap, s) = (m.snapshot(), m.stats());
            let block = snap.last_block_frames as f64;
            let rate = snap.sample_rate as f64;
            let budget = if rate > 0.0 {
                block / rate * 1000.0
            } else {
                0.0
            };
            let worst = s.take_max_callback_ns() as f64 / 1e6;
            self.hud.audio = format!(
                "cb {worst:.2}/{budget:.1} ms · slow {} · xrun {}",
                s.slow_callbacks.load(Relaxed),
                s.xruns.load(Relaxed)
            );
        }
    }
}

impl eframe::App for EarshotApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, AUDIO_KEY, &self.audio_settings);
        eframe::set_value(storage, FLOW_KEY, &self.flow.saved());
        eframe::set_value(storage, WAVE_ZOOM_KEY, &self.half_window);
        eframe::set_value(storage, TEMPO_RANGE_KEY, &self.tempo_ranges());
        eframe::set_value(storage, MODE_KEY, &self.mode);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events(ctx);
        // Catch the decks' sessions up with the engine (and save changed hot cues).
        self.snap = player::poll(&self.player, &self.lib.store);
        // Background analysis holds back while a deck plays in Perform mode. In Prepare,
        // listening while an import analyses is normal, so it runs at full width.
        let playing = self.mode == UiMode::Perform
            && self
                .snap
                .as_ref()
                .is_some_and(|sh| sh.live().iter().any(|d| d.playing));
        self.lib.analyser.set_playing(playing);
        self.keyboard(ctx);
        self.tick_hud(ctx);
        self.track_plays();
        self.refresh_suggestions();
        let players: Vec<DeckId> = session::players(DECKS).collect();
        let decks: Vec<DeckId> = DeckId::first(DECKS).collect();
        let mut infos: [Option<DeckInfo>; SLOTS] = Default::default();
        for &id in &players {
            let info = self.deck_info(id);
            let sh = self.snap.as_ref().map(|s| s.deck(id));
            self.decks[id.index()].tick(ctx, sh, &info);
            infos[id.index()] = Some(info);
        }
        let info = |id: DeckId| infos[id.index()].as_ref().expect("a player's info");
        let prep = DeckId::PREPARE.index();
        let mut out: Vec<Command> = vec![];
        let mut coll_cmds: Vec<CollectionCmd> = vec![];
        let preview = self.preview_state();
        // What the collection tree needs from the library pane, read before it's
        // borrowed for drawing.
        let (search, selected) = (self.library.search.clone(), self.library.selected());
        let open = if self.flow.open {
            self.flow.flow
        } else {
            match self.library.scope() {
                Scope::Collection(id) => Some(*id),
                _ => None,
            }
        };

        egui::TopBottomPanel::top("top")
            .frame(bar_frame())
            .show(ctx, |ui| self.top_bar(ui));
        egui::TopBottomPanel::bottom("midi")
            .frame(bar_frame())
            .show(ctx, |ui| {
                let log: Vec<&str> = self.midi_log.iter().map(|s| s.as_str()).collect();
                ui.label(
                    RichText::new(format!("MIDI  {}", log.join("   |   ")))
                        .monospace()
                        .size(11.0)
                        .color(util::MUTED),
                );
            });
        // Full-screen Flow hides the decks and side panes while planning.
        let planning = self.flow.open && self.flow.full;
        egui::TopBottomPanel::top("waves")
            .resizable(true)
            .default_height(150.0)
            .height_range(60.0..=520.0)
            .frame(egui::Frame::new().fill(util::BG).inner_margin(6))
            .show_animated(ctx, !planning, |ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                if self.mode == UiMode::Prepare {
                    let h = ui.available_height();
                    let deck = &mut self.decks[prep];
                    deck.show_wave(
                        ui,
                        h,
                        info(DeckId::PREPARE),
                        &mut self.half_window,
                        &mut out,
                    );
                    return;
                }
                let n = decks.len() as f32;
                let h = (ui.available_height() - 3.0 * (n - 1.0)) / n;
                for &d in &decks {
                    let deck = &mut self.decks[d.index()];
                    deck.show_wave(ui, h, info(d), &mut self.half_window, &mut out);
                }
            });
        // A fixed height (the mixer's), so the decks can't be stretched by a height
        // egui remembered from an older layout.
        let decks_h = mixer::HEIGHT + 12.0;
        egui::TopBottomPanel::top("decks")
            .exact_height(decks_h + 8.0)
            .frame(
                egui::Frame::new()
                    .fill(util::BG)
                    .inner_margin(egui::Margin::symmetric(6, 4)),
            )
            .show_animated(ctx, !planning, |ui| {
                let height = decks_h;
                if self.mode == UiMode::Prepare {
                    let w = ui.available_width();
                    let (memories, info) = (&self.memories, info(DeckId::PREPARE));
                    self.decks[prep].show_prepare(ui, w, height, info, memories, &mut out);
                    return;
                }
                // Decks on the crossfader's left, the mixer, then the rest.
                let side = |d: &DeckId| {
                    if mixer::on_left(*d) {
                        Side::Left
                    } else {
                        Side::Right
                    }
                };
                let mixer_w = mixer::width(&decks) + 16.0;
                let n = decks.len() as f32;
                let w = (ui.available_width() - mixer_w - 6.0 * n) / n;
                ui.horizontal_top(|ui| {
                    for d in decks.iter().filter(|d| side(d) == Side::Left) {
                        self.decks[d.index()].show(ui, w, height, Side::Left, info(*d), &mut out);
                    }
                    ui.allocate_ui(vec2(mixer_w, 0.0), |ui| {
                        egui::Frame::new()
                            .fill(util::PANEL)
                            .stroke(Stroke::new(1.0_f32, util::LINE))
                            .corner_radius(8)
                            .inner_margin(egui::Margin::symmetric(8, 6))
                            .show(ui, |ui| {
                                ui.set_width(mixer_w - 16.0);
                                mixer::show(ui, self.snap.as_ref(), &decks, &mut out);
                            });
                    });
                    for d in decks.iter().filter(|d| side(d) == Side::Right) {
                        self.decks[d.index()].show(ui, w, height, Side::Right, info(*d), &mut out);
                    }
                });
            });
        egui::SidePanel::left("tree")
            .resizable(true)
            .default_width(250.0)
            .width_range(140.0..=600.0)
            .show_animated(ctx, !planning, |ui| {
                let view = View {
                    lib: &self.index,
                    history: &self.history,
                    collections: &self.collections,
                    open_flow: self.flow.flow,
                };
                let tree = TreeCtx {
                    collections: &self.collections,
                    open,
                    search: &search,
                    selected: &selected,
                };
                let colls = &mut self.colls;
                self.library.tree(ui, &view, &mut out, |ui| {
                    colls.show(ui, &tree, &mut coll_cmds)
                });
            });
        egui::SidePanel::right("suggest")
            .resizable(true)
            .default_width(380.0)
            .width_range(240.0..=700.0)
            .show_animated(ctx, !planning, |ui| {
                let focus = self.pane == Pane::Suggest;
                let view = View {
                    lib: &self.index,
                    history: &self.history,
                    collections: &self.collections,
                    open_flow: self.flow.flow,
                };
                if self.suggest.show(ui, &view, focus, &mut out) {
                    self.pane = Pane::Suggest;
                }
            });
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.selectable_label(!self.flow.open, "Tracks").clicked() {
                    self.flow.open = false;
                }
                if ui
                    .selectable_label(self.flow.open, "Flow")
                    .on_hover_text("Plan a set: add tracks, then grow it from suggestions")
                    .clicked()
                {
                    self.flow.open = true;
                }
            });
            ui.separator();
            let view = View {
                lib: &self.index,
                history: &self.history,
                collections: &self.collections,
                open_flow: self.flow.flow,
            };
            if self.flow.open {
                self.flow
                    .show(ui, &view, &mut self.media, preview, &mut out);
            } else if self.library.list(ui, &view, &mut self.media, &mut out) {
                self.pane = Pane::Library;
            }
        });

        let routing = player::routing(&self.player, self.audio.as_ref().map_or(2, |i| i.channels));
        if let Some(c) = self
            .audio_ui
            .show(ctx, &self.audio_settings, &self.audio, routing)
        {
            if let Command::Audio(AudioCmd::Route(r)) = c {
                self.audio_settings.routing = Some(r);
            }
            out.push(c);
        }

        if self.show_keys {
            let (keymap, mode) = (&self.keymap, self.mode.keys());
            egui::Window::new("Keyboard")
                .open(&mut self.show_keys)
                .resizable(false)
                .show(ctx, |ui| keys_help(ui, keymap, mode));
        }

        out.extend(coll_cmds.into_iter().map(Command::Collection));
        for c in out {
            self.dispatch(c);
        }
        // Save first, so opening another Flow doesn't lose the last change.
        self.save_flow();
        for id in take_wanted_art(ctx) {
            if !self.media.art.contains_key(&id) {
                self.media.art.insert(id, None);
                let _ = self.art_loader.send(id);
            }
        }
        self.save_flow();
        // Waveforms move every frame.
        ctx.request_repaint();
    }
}

/// Store events, delivered through the app's channel like everything else.
fn store_sink(sink: &Sink) -> EventSink {
    let sink = sink.clone();
    Arc::new(move |e| sink(Event::Store(e)))
}

fn bar_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(util::PANEL)
        .inner_margin(egui::Margin::symmetric(10, 5))
}

fn pill(ui: &mut Ui, text: &str, border: egui::Color32) -> egui::Response {
    egui::Frame::new()
        .fill(util::PANEL_2)
        .stroke(Stroke::new(1.0_f32, border))
        .corner_radius(10)
        .inner_margin(egui::Margin::symmetric(8, 2))
        .show(ui, |ui| ui.label(RichText::new(text).size(11.0)))
        .response
        .interact(egui::Sense::click())
}

/// The key help, made from the keymap: a column per deck (or player), then the library.
fn keys_help(ui: &mut Ui, keymap: &Keymap, mode: keymap::Mode) {
    ui.label(
        RichText::new(
            "Deck keys work unless a text field has focus. Esc leaves the field. \
             Shift + a hot cue key clears it.",
        )
        .color(util::MUTED),
    );
    ui.add_space(6.0);
    let mut sections = keymap.help(mode);
    // Built into egui and the lists rather than the keymap.
    if let Some(lib) = sections.iter_mut().find(|s| s.title == "Library") {
        lib.rows.extend([
            ("Ctrl A".into(), "select all".into()),
            ("Ctrl - / = / 0".into(), "UI size".into()),
        ]);
    }
    ui.columns(sections.len().max(1), |c| {
        for (ui, s) in c.iter_mut().zip(&sections) {
            let colour = s.deck.map_or(util::TEXT, util::deck_colour);
            ui.label(RichText::new(&s.title).strong().color(colour));
            egui::Grid::new(&s.title).show(ui, |ui| {
                for (k, what) in &s.rows {
                    ui.label(RichText::new(k).monospace());
                    ui.label(what);
                    ui.end_row();
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Renders the whole window off-screen to a PNG, with two tracks loaded and one
    /// playing. Run on a copy of the library:
    /// `EARSHOT_DATA_DIR=/tmp/copy EARSHOT_NULL_AUDIO=1 EARSHOT_SHOT=/tmp/shot.png
    ///  cargo test --release -- --ignored screenshot`
    #[test]
    #[ignore]
    fn screenshot() {
        let path = std::env::var("EARSHOT_SHOT").expect("set EARSHOT_SHOT");
        let mut h = egui_kittest::Harness::builder()
            .with_size(vec2(1700.0, 1000.0))
            .wgpu()
            .build_eframe(|cc| EarshotApp::new(cc).expect("app"));
        let settle = |h: &mut egui_kittest::Harness<EarshotApp>, frames: usize| {
            for _ in 0..frames {
                h.step();
                std::thread::sleep(Duration::from_millis(16));
            }
        };
        for _ in 0..500 {
            settle(&mut h, 1);
            if !h.state().library.scanning {
                break;
            }
        }
        let ids: Vec<i64> = h
            .state()
            .index
            .tracks()
            .iter()
            .filter(|t| t.analysed())
            .take(2)
            .map(|t| t.id)
            .collect();
        let decks: Vec<DeckId> = DeckId::first(DECKS).collect();
        for (deck, id) in decks.iter().zip(&ids) {
            h.state_mut().load(Some(*deck), *id);
        }
        settle(&mut h, 120);
        let play = |d| Command::from((d, EngineCmd::Play(earshot_engine::Set::On)));
        h.state_mut()
            .dispatch(Command::Deck(decks[0], DeckCmd::SeekFraction(0.3)));
        h.state_mut().dispatch(play(decks[0]));
        h.state_mut().library.move_focus(3);
        settle(&mut h, 90);
        // How long the app takes to build a frame (layout + waveforms), both decks playing.
        h.state_mut().dispatch(play(decks[1]));
        let t = Instant::now();
        let n = 300;
        let mut worst = 0f64;
        for _ in 0..n {
            let f = Instant::now();
            h.step();
            worst = worst.max(f.elapsed().as_secs_f64());
        }
        eprintln!(
            "frame build: {:.2} ms average, {:.2} ms worst over {n} frames",
            t.elapsed().as_secs_f64() * 1000.0 / n as f64,
            worst * 1000.0
        );
        h.render().expect("render").save(&path).expect("save");
    }

    /// Prepare mode with a track on its player; writes to `EARSHOT_SHOT`.
    #[test]
    #[ignore]
    fn prepare_screenshot() {
        let path = std::env::var("EARSHOT_SHOT").expect("set EARSHOT_SHOT");
        let mut h = egui_kittest::Harness::builder()
            .with_size(vec2(1700.0, 1000.0))
            .wgpu()
            .build_eframe(|cc| EarshotApp::new(cc).expect("app"));
        for _ in 0..500 {
            h.step();
            std::thread::sleep(Duration::from_millis(16));
            if !h.state().library.scanning {
                break;
            }
        }
        let id = h
            .state()
            .index
            .tracks()
            .iter()
            .find(|t| t.analysed())
            .map(|t| t.id)
            .expect("an analysed track");
        h.state_mut().mode = UiMode::Prepare;
        h.state_mut().load(Some(DeckId::PREPARE), id);
        for _ in 0..300 {
            h.step();
            std::thread::sleep(Duration::from_millis(16));
            if h.state().decks[DeckId::PREPARE.index()].wave.is_some() {
                break;
            }
        }
        h.run_steps(5);
        h.render().expect("render").save(&path).expect("save");
    }

    /// The Flow tab with two cards and the "+" suggestions open. Same setup as
    /// `screenshot`; writes to `EARSHOT_SHOT`.
    #[test]
    #[ignore]
    fn flow_screenshot() {
        let path = std::env::var("EARSHOT_SHOT").expect("set EARSHOT_SHOT");
        let mut h = egui_kittest::Harness::builder()
            .with_size(vec2(1700.0, 1000.0))
            .wgpu()
            .build_eframe(|cc| EarshotApp::new(cc).expect("app"));
        for _ in 0..500 {
            h.step();
            std::thread::sleep(Duration::from_millis(16));
            if !h.state().library.scanning {
                break;
            }
        }
        let ids: Vec<i64> = h
            .state()
            .index
            .tracks()
            .iter()
            .filter(|t| t.analysed() && t.key.is_some())
            .take(2)
            .map(|t| t.id)
            .collect();
        let app = h.state_mut();
        app.flow = FlowUi::from_saved(Default::default());
        app.flow.open = true;
        app.flow.add_tracks(&ids);
        app.flow.fit();
        // Give the cover art a moment to load.
        for _ in 0..30 {
            h.step();
            std::thread::sleep(Duration::from_millis(10));
        }
        h.state_mut().flow.open_first_suggestions();
        h.run_steps(5);
        h.render().expect("render").save(&path).expect("save");
    }
}
