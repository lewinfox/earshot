//! The window: top bar, the stacked waveforms, the decks and mixer, and the library.
//! Repaints every frame (vsync), reading the engine's state directly.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use earshot_engine::{
    DeckCmd as EngineCmd, DeckId, EngineSnapshot, MAX_DECKS, Monitor, OutputInfo, OutputOptions,
    TempoRange,
};
use egui::{Key, RichText, Stroke, Ui, vec2};

use super::audio_ui::AudioUi;
use super::deck::{DeckInfo, DeckUi, PendingLoad, Side};
use super::flow::FlowUi;
use super::flows_ui::{FlowsAction, FlowsUi, Item};
use super::library_ui::{LibAction, LibraryUi, PreviewState, take_wanted_art};
use super::mixer;
use super::suggest_ui::{RefInfo, Source, SuggestUi};
use super::util;
use crate::core::analysis::{Analyser, Mode};
use crate::core::command::{self, AudioCmd, Command, DeckCmd, PreviewCmd, ViewCmd};
use crate::core::keymap::{self, Keymap};
use crate::core::library::{self, Library};
use crate::core::midi::{self, Midi};
use crate::core::player::{self, AudioSettings, DECKS, Player};
use crate::core::session::{self, SLOTS};
use crate::core::store::{self, EventSink, Store, StoreCmd, StoreEvent};
use crate::core::suggest::{self, Reference};
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
    library: LibraryUi,
    suggest: SuggestUi,
    flow: FlowUi,
    flows: FlowsUi,
    /// Send track ids here to load their cover art.
    art_loader: std::sync::mpsc::Sender<i64>,
    /// Nudge after a scan: works out audio hashes the files' tags didn't have.
    hasher: std::sync::mpsc::Sender<()>,
    /// A Flow or folder just made, to rename once the Store's list has it.
    pending_rename: Option<Item>,
    /// Which list the keyboard and the controller's browse knob move through.
    pane: Pane,
    /// The deck that most recently started playing (the live reference).
    last_started: Option<usize>,
    /// When each deck started playing, to count a track as played after 30 s.
    play_started: [Option<Instant>; MAX_DECKS],
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
/// The name a new Flow gets until it's renamed.
const UNTITLED: &str = "Untitled Flow";

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

/// Suggestions follow a tempo change once the tempo has held still this long.
const SUGGEST_SETTLE: Duration = Duration::from_secs(1);

/// A track counts as played once it has been playing this long.
const PLAYED_AFTER_SECS: f32 = 30.0;

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
            library: LibraryUi::default(),
            suggest: SuggestUi::default(),
            flow: FlowUi::from_saved(
                cc.storage
                    .and_then(|s| eframe::get_value(s, FLOW_KEY))
                    .unwrap_or_default(),
            ),
            flows: FlowsUi::default(),
            art_loader,
            hasher,
            pending_rename: None,
            pane: Pane::Library,
            last_started: None,
            play_started: [None; MAX_DECKS],
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

    // ---- saved Flows ------------------------------------------------------------------

    /// Ask for the Flow list and the Flow that was open last time. Cards kept from
    /// before Flows were saved in the database are still on the canvas with no Flow,
    /// so the first save makes them an "Untitled Flow".
    fn restore_flow(&mut self) {
        self.lib.store.send(StoreCmd::ListFlows);
        if let Some(id) = self.flow.flow {
            self.lib.store.send(StoreCmd::LoadFlow(id));
        }
    }

    /// Save the canvas if its cards changed, making a new Flow for the first card.
    fn save_flow(&mut self) {
        let Some(cards) = self.flow.changed_cards() else {
            return;
        };
        let store = &self.lib.store;
        let id = match self.flow.flow {
            Some(id) => id,
            None if cards.is_empty() => return,
            None => {
                let id = store.new_collection_id();
                store.send(StoreCmd::CreateFlow {
                    id,
                    folder: None,
                    name: UNTITLED.into(),
                });
                self.flow.name = UNTITLED.to_string();
                self.flow.flow = Some(id);
                id
            }
        };
        store.send(StoreCmd::SaveFlow { id, cards });
    }

    /// The tree's actions become Store commands; the new list comes back as an event.
    fn flows_action(&mut self, a: FlowsAction) {
        let store = &self.lib.store;
        match a {
            FlowsAction::Open(id) => {
                store.send(StoreCmd::LoadFlow(id));
                self.flow.open = true;
            }
            FlowsAction::New(folder) => {
                let id = store.new_collection_id();
                store.send(StoreCmd::CreateFlow {
                    id,
                    folder,
                    name: UNTITLED.into(),
                });
                self.flow.load(Some((id, UNTITLED.to_string(), vec![])));
                self.flow.open = true;
                self.pending_rename = Some(Item::Flow(id));
            }
            FlowsAction::NewFolder(parent) => {
                let id = store.new_collection_id();
                store.send(StoreCmd::CreateFolder {
                    id,
                    parent,
                    name: "New folder".into(),
                });
                self.pending_rename = Some(Item::Folder(id));
            }
            FlowsAction::Rename(item, name) => {
                let id = match item {
                    Item::Flow(id) => {
                        if self.flow.flow == Some(id) {
                            self.flow.name = name.clone();
                        }
                        id
                    }
                    Item::Folder(id) => id,
                };
                store.send(StoreCmd::Rename { id, name });
            }
            FlowsAction::Move(Item::Flow(id) | Item::Folder(id), parent) => {
                store.send(StoreCmd::Move { id, parent })
            }
            FlowsAction::Duplicate(from) => {
                let to = store.new_collection_id();
                store.send(StoreCmd::DuplicateFlow { from, to });
                self.pending_rename = Some(Item::Flow(to));
            }
            // The open Flow may go with it: see `StoreEvent::Deleted`.
            FlowsAction::Delete(Item::Flow(id) | Item::Folder(id)) => {
                store.send(StoreCmd::Delete(id))
            }
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
        self.decks[deck.index()]
            .start_load(PendingLoad::new(track_id, self.library.track(track_id)));
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

    /// Note which deck started playing last, and mark tracks played after 30 s.
    fn track_plays(&mut self) {
        let Some(sh) = &self.snap else { return };
        let now = Instant::now();
        for (d, deck) in sh.live().iter().enumerate() {
            let playing = deck.playing;
            match (playing, self.play_started[d]) {
                (true, None) => {
                    self.play_started[d] = Some(now);
                    self.last_started = Some(d);
                }
                (false, Some(_)) => self.play_started[d] = None,
                (true, Some(t)) if now.duration_since(t).as_secs_f32() > PLAYED_AFTER_SECS => {
                    if let Some(id) = self.player.lock().unwrap().decks[d].track_id {
                        self.library.played.insert(id);
                    }
                }
                _ => {}
            }
        }
    }

    /// The reference track the Suggestions pane's source asks for: a deck's track at
    /// its actual tempo and key, or the track selected in the library.
    fn refresh_suggestions(&mut self) {
        // (deck, track, BPM as playing, how far the key has moved, playing)
        let on_deck = |d: usize| {
            let sh = &self.snap.as_ref()?.decks[d];
            let id = self.player.lock().unwrap().decks[d].track_id?;
            // How far the key has moved: not at all with keylock.
            let speed = if sh.keylock { 1.0 } else { sh.tempo };
            Some((d, id, sh.bpm, speed, sh.playing))
        };
        let source = match self.suggest.source {
            Source::Master => self
                .last_started
                .into_iter()
                .chain(0..DECKS)
                .filter_map(on_deck)
                .find(|r| r.4),
            Source::Deck(d) => on_deck(d),
            Source::Selected => self.library.focused().map(|id| (0, id, None, 1.0, false)),
        };
        let deck = match self.suggest.source {
            Source::Selected => None,
            _ => source.map(|r| r.0),
        };
        let Some((_, id, bpm, speed, playing)) = source else {
            self.suggest.reference = None;
            self.suggest.set(vec![]);
            self.suggest.computed_for.clear();
            return;
        };
        let Some(t) = self.library.track(id) else {
            return;
        };
        let bpm = bpm.or(t.bpm());
        let key = suggest::effective_key(t.key, speed);
        let what = format!(
            "{id}|{deck:?}|{playing}|{:?}|{}|{}|",
            self.suggest.direction,
            self.library.played.len(),
            self.library.version,
        );
        let tempo = format!("{:.1}|{key:?}", bpm.unwrap_or(0.0));
        let stamp = format!("{what}{tempo}");
        if stamp == self.suggest.computed_for {
            self.suggest.settling = None;
            return;
        }
        // Only the tempo moved (someone's on the tempo fader): wait until it has held
        // still for a moment rather than re-ranking on every step of the sweep.
        if self.suggest.computed_for.starts_with(&what) {
            match &self.suggest.settling {
                Some((t, since)) if *t == tempo => {
                    if since.elapsed() < SUGGEST_SETTLE {
                        return;
                    }
                }
                _ => {
                    self.suggest.settling = Some((tempo, Instant::now()));
                    return;
                }
            }
        }
        self.suggest.settling = None;
        let candidates = self.library.candidates();
        let reference = Reference {
            id,
            bpm,
            key,
            energy: t.energy,
            direction: self.suggest.direction,
            played: &self.library.played,
            exclude: &HashSet::new(),
        };
        let list = suggest::suggest(&reference, &candidates, 80);
        self.suggest.reference = Some(RefInfo {
            title: t.lib.title.clone(),
            bpm,
            key,
            deck,
            playing,
        });
        self.suggest.set(list);
        self.suggest.computed_for = stamp;
    }

    /// Play a track in the headphones from `start` (0..1), decoding it first. A track
    /// with no beatgrid yet is queued for analysis too.
    fn start_preview(&mut self, id: i64, start: f32) {
        if self
            .library
            .track(id)
            .is_some_and(|t| !t.analysed && !t.queued && t.running.is_none() && t.error.is_none())
        {
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
                self.library.set_tracks(tracks);
                self.refresh_previews(None);
            }
            StoreEvent::TracksChanged(tracks) => self.library.update_tracks(tracks),
            StoreEvent::Previews(p) => {
                for (id, bytes) in p {
                    self.library.invalidate_thumb(id);
                    self.library.previews.insert(id, bytes);
                }
            }
            StoreEvent::Flows(folders, flows) => {
                self.flows.set((folders, flows));
                let listed = |item: &Item| match *item {
                    Item::Flow(id) => self.flows.flows.iter().any(|f| f.id == id),
                    Item::Folder(id) => self.flows.folders.iter().any(|f| f.id == id),
                };
                if let Some(item) = self.pending_rename.take_if(|i| listed(i)) {
                    self.flows.start_rename(item);
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
                    self.library.art.insert(id, tex);
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
                    match ev.state {
                        "queued" | "running" => {
                            a.busy.insert(ev.track_id);
                        }
                        _ => {
                            a.busy.remove(&ev.track_id);
                        }
                    }
                    if ev.state == "running" {
                        a.running.insert(ev.track_id, ev.progress.unwrap_or(0.0));
                    } else {
                        a.running.remove(&ev.track_id);
                    }
                    let title = self
                        .library
                        .track(ev.track_id)
                        .map(|t| t.lib.title.clone())
                        .unwrap_or_default();
                    if let Some(e) = &ev.error {
                        a.failed += 1;
                        a.last_error = format!("{title}: {e}");
                    }
                    if let Some(t) = self.library.track_mut(ev.track_id) {
                        t.queued = ev.state == "queued";
                        t.running = (ev.state == "running").then(|| {
                            (
                                ev.progress.unwrap_or(0.0),
                                ev.stage.unwrap_or("analysing").to_string(),
                            )
                        });
                        // The results themselves (BPM, grid, thumbnail) come from the
                        // Store as `TracksChanged` and `Previews`.
                        t.error = ev.error.clone();
                    }
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
            .filter_map(|id| self.library.track(*id))
            .map(|t| format!("Now: {}", t.lib.title))
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
        let t = s.track_id.and_then(|id| self.library.track(id));
        DeckInfo {
            title: s.title.clone(),
            artist: s.artist.clone(),
            grid: s.grid.clone(),
            grid_source: s.grid_source.clone(),
            generation: s.generation,
            waveform: s.waveform.clone(),
            detail: s.detail.clone(),
            tag_bpm: t.and_then(|t| t.lib.tag_bpm),
            key: t.and_then(|t| t.key),
            analysing: t.and_then(|t| t.running.clone()).or_else(|| {
                t.filter(|t| t.queued)
                    .map(|_| (0.0, "waiting to analyse".into()))
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
        // Background analysis holds back while anything plays.
        let playing = self
            .snap
            .as_ref()
            .is_some_and(|sh| sh.prepare.playing || sh.live().iter().any(|d| d.playing));
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
        let mut lib_actions: Vec<LibAction> = vec![];
        let mut flows_actions: Vec<FlowsAction> = vec![];

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
                let (flows, open) = (&mut self.flows, self.flow.flow);
                self.library.tree(ui, &mut lib_actions, |ui| {
                    flows.show(ui, open, &mut flows_actions)
                });
            });
        egui::SidePanel::right("suggest")
            .resizable(true)
            .default_width(380.0)
            .width_range(240.0..=700.0)
            .show_animated(ctx, !planning, |ui| {
                let focus = self.pane == Pane::Suggest;
                if self
                    .suggest
                    .show(ui, &self.library, focus, &mut lib_actions)
                {
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
            if self.flow.open {
                let preview = self.preview_state();
                self.flow.show(ui, &self.library, preview, &mut lib_actions);
            } else if self.library.list(ui, &mut lib_actions) {
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

        for c in out {
            self.dispatch(c);
        }
        for a in lib_actions {
            match a {
                LibAction::Load { deck, track_id } => {
                    self.load(deck.and_then(DeckId::new), track_id)
                }
                LibAction::Analyse { ids, dynamic } => self.analyse(ids, dynamic),
                LibAction::Preview(id) => self.dispatch(Command::Preview(PreviewCmd::Toggle(id))),
                LibAction::PreviewSeek(id, at) => {
                    self.dispatch(Command::Preview(PreviewCmd::Seek(id, at)))
                }
                LibAction::PreviewStop(id) => self.dispatch(Command::Preview(PreviewCmd::Stop(id))),
                LibAction::AddToFlow(ids) => {
                    self.flow.open = true;
                    for id in ids {
                        self.flow.add_root(id);
                    }
                }
                LibAction::RemoveFolder(path) => self.lib.store.send(StoreCmd::RemoveRoot(path)),
            }
        }
        // Save first, so opening another Flow doesn't lose the last change.
        self.save_flow();
        for id in take_wanted_art(ctx) {
            if !self.library.art.contains_key(&id) {
                self.library.art.insert(id, None);
                let _ = self.art_loader.send(id);
            }
        }
        for a in flows_actions {
            self.flows_action(a);
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
            .library
            .tracks
            .iter()
            .filter(|t| t.analysed)
            .take(2)
            .map(|t| t.lib.id)
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
            .library
            .tracks
            .iter()
            .find(|t| t.analysed)
            .map(|t| t.lib.id)
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
            .library
            .tracks
            .iter()
            .filter(|t| t.analysed && t.key.is_some())
            .take(2)
            .map(|t| t.lib.id)
            .collect();
        let app = h.state_mut();
        app.flow = FlowUi::from_saved(Default::default());
        app.flow.open = true;
        for id in ids {
            app.flow.add_root(id);
        }
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
