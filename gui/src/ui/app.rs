//! The shell: starts the services, reads the engine once a frame, draws the top bar,
//! the mode's view and the windows, and carries out every command in [`dispatch`],
//! the one place app state changes. Repaints only while something moves (a deck or
//! preview playing, a scrub, analysis); otherwise it waits for an event.
//!
//! [`dispatch`]: EarshotApp::dispatch

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use earshot_engine::{
    DeckCmd as EngineCmd, DeckId, Monitor, OutputInfo, OutputOptions, TempoRange,
};
use egui::Key;

use super::modals::{ModalEnv, Modals};
use super::panels::deck::{DeckInfo, PendingLoad};
use super::panels::flow::FlowUi;
use super::panels::library::LibraryPane;
use super::panels::library::browser::Scope;
use super::panels::library::tracks::take_wanted_art;
use super::panels::status::{AnalysisSummary, MidiLog, TopBar};
use super::panels::{CoreView, Cx, EngineFrame, Panel, PreviewState};
use super::prefs::{self, UiPrefs};
use super::theme::THEME;
use super::views::{self, Panels};
use crate::core::analysis::{Analyser, Mode as AnalysisMode};
use crate::core::collections::{CollectionCmd, CollectionId, CollectionKind, Collections, FlowCmd};
use crate::core::edit;
use crate::core::command::{self, AppCmd, AudioCmd, Command, DeckCmd, PreviewCmd, ViewCmd};
use crate::core::history::{DeckNow, History};
use crate::core::index::{LibraryCmd, LibraryIndex};
use crate::core::keymap::Keymap;
use crate::core::library::{self, Library};
use crate::core::midi::{self, Midi};
use crate::core::player::{self, AudioSettings, DECKS, Player};
use crate::core::services::Services;
use crate::core::session;
use crate::core::settings::{self, Settings};
use crate::core::store::{self, EventSink, Store, StoreCmd, StoreEvent};
use crate::core::tempo_lock::TempoLock;
use crate::core::track::AnalysisState;
use crate::core::view::{Mode, Pane, Region};
use crate::core::worker_client::Role;
use crate::core::{Event, Sink};

/// After the last thing moved, keep drawing this long (meters fall, playheads settle).
const SETTLE: Duration = Duration::from_millis(500);

pub struct EarshotApp {
    ctx: egui::Context,
    // ---- services ----------------------------------------------------------------------
    player: Player,
    lib: Arc<Library>,
    sink: Sink,
    rx: Receiver<Event>,
    monitor: Option<Monitor>,
    audio: Result<OutputInfo, String>,
    /// Always the null output (tests).
    null_audio: bool,
    midi: Midi,
    midi_ports: Vec<String>,
    midi_log: VecDeque<String>,
    /// Send track ids here to load their cover art.
    art_loader: std::sync::mpsc::Sender<i64>,
    /// Nudge after a scan: works out audio hashes the files' tags didn't have.
    hasher: std::sync::mpsc::Sender<()>,
    /// The tempo lock tool (its tab is still to come).
    tempo_lock: TempoLock,

    // ---- core state the UI holds ---------------------------------------------------------
    /// The library, collections and play history: the domain state the panes read.
    index: LibraryIndex,
    collections: Collections,
    history: History,
    /// Edits that can be undone and redone.
    edits: edit::History,
    /// Hot cue labels and memory cues of the tracks on the players, by track id.
    cues: HashMap<i64, Arc<store::TrackCues>>,
    analysis: AnalysisSummary,
    /// The track in the headphones' preview player.
    preview: Option<Preview>,
    /// Deck and audio settings, as saved in the Store.
    settings: Settings,
    /// A collection just made, to rename once the Store's list has it.
    pending_rename: Option<CollectionId>,
    keymap: Keymap,
    /// Keys held down for buttons that act while held, and what letting go sends.
    held_keys: HashMap<Key, Command>,
    hud: Hud,

    // ---- the view ----------------------------------------------------------------------------
    prefs: UiPrefs,
    modals: Modals,
    panels: Panels,
    /// The engine as read at the start of this frame.
    frame: EngineFrame,
    /// Keep repainting until then (see [`SETTLE`]).
    settle_until: Option<Instant>,
}

/// Start audio on the chosen device; failing that the default one, then the null
/// output (so the app still runs without a sound card).
fn start_audio(
    player: &Player,
    settings: &AudioSettings,
    null: bool,
) -> Result<OutputInfo, String> {
    let mut opts = player::output_options(settings);
    opts.null |= null;
    player::start_engine(player, &opts)
        .or_else(|e| {
            if opts.device.is_none() || opts.null {
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

impl EarshotApp {
    pub fn new(cc: &eframe::CreationContext<'_>, services: Services) -> Result<EarshotApp, String> {
        THEME.install(&cc.egui_ctx);
        let (tx, rx) = channel();
        let ctx = cc.egui_ctx.clone();
        let tx = Mutex::new(tx);
        let sink: Sink = Arc::new(move |e| {
            let _ = tx.lock().unwrap().send(e);
            ctx.request_repaint();
        });

        let store = Store::open(&services.db, store_sink(&sink)).map_err(|e| e.to_string())?;
        let settings = Settings::load(&store.reader);
        let player = player::new_player();
        player.lock().unwrap().routing = settings.audio.routing;
        let audio = start_audio(&player, &settings.audio, services.null_audio);
        let monitor = player.lock().unwrap().monitor();

        let analyser =
            Analyser::start(sink.clone(), player.clone(), store.clone(), services.worker);
        let hasher = library::start_hasher(store.clone());
        let tempo_lock = {
            let sink = sink.clone();
            let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
            let data_dir = services.db.parent().unwrap_or(std::path::Path::new("."));
            TempoLock::with_worker(
                (services.worker)(Role::Background, cores),
                store.clone(),
                Arc::new(move |e| sink(Event::TempoLock(e))),
                data_dir,
                Some(hasher.clone()),
            )
        };
        let art_loader = library::start_art_loader(store.reader.clone(), sink.clone());
        let lib = Arc::new(Library { store, analyser });
        let prefs: UiPrefs = cc
            .storage
            .and_then(|s| eframe::get_value(s, prefs::KEY))
            .unwrap_or_default();
        cc.egui_ctx.set_zoom_factor(prefs.ui_scale);
        let flow = FlowUi::from_saved(prefs.flow.clone());
        let mut app = EarshotApp {
            ctx: cc.egui_ctx.clone(),
            player,
            lib,
            sink,
            rx,
            monitor,
            audio,
            null_audio: services.null_audio,
            midi: Midi::default(),
            midi_ports: vec![],
            midi_log: VecDeque::new(),
            art_loader,
            hasher,
            tempo_lock,
            index: LibraryIndex::default(),
            collections: Collections::default(),
            history: History::default(),
            edits: edit::History::default(),
            cues: HashMap::new(),
            analysis: AnalysisSummary::default(),
            preview: None,
            settings,
            pending_rename: None,
            keymap: Keymap::built_in()
                .spell_keys(|name| Key::from_name(name).map(|k| k.name().to_string()))?,
            held_keys: HashMap::new(),
            hud: Hud::default(),
            prefs,
            modals: Modals::default(),
            panels: Panels::new(LibraryPane::new(flow)),
            frame: EngineFrame::default(),
            settle_until: None,
        };
        app.lib.store.send(StoreCmd::ListRoots);
        let ranges = app.settings.tempo_ranges.clone();
        for (deck, range) in DeckId::first(DECKS).zip(ranges) {
            app.dispatch((deck, EngineCmd::TempoRange(range)).into());
        }
        app.restore_flow();
        app.rescan();
        if services.midi {
            app.connect_midi();
        }
        Ok(app)
    }

    /// (Re)open every MIDI input, e.g. after plugging the controller in.
    fn connect_midi(&mut self) {
        // Close the old ports first so they can be reopened.
        self.midi = Midi::default();
        let lane = self.player.lock().unwrap().midi.clone();
        (self.midi, self.midi_ports) = midi::connect_all(&self.sink, &lane).unwrap_or_else(|e| {
            eprintln!("MIDI: {e}");
            (Midi::default(), vec![])
        });
    }

    /// Each deck's tempo range, as the engine has it.
    fn tempo_ranges(&self) -> Vec<TempoRange> {
        let Some(sh) = &self.frame.snap else {
            return vec![];
        };
        sh.live().iter().map(|d| d.range).collect()
    }

    /// Save the decks' tempo ranges when they change (from the deck, keys or MIDI).
    fn save_tempo_ranges(&mut self) {
        let ranges = self.tempo_ranges();
        if !ranges.is_empty() && ranges != self.settings.tempo_ranges {
            self.lib
                .store
                .send(settings::save(settings::TEMPO_RANGES, &ranges));
            self.settings.tempo_ranges = ranges;
        }
    }

    fn save_audio_settings(&self) {
        self.lib
            .store
            .send(settings::save(settings::AUDIO, &self.settings.audio));
    }

    /// Carry out a command, from whichever input: the only place app state changes.
    /// The decks', mixer's and engine's are done in core; what's left is the UI's.
    fn dispatch(&mut self, cmd: Command) {
        if let Command::Audio(AudioCmd::Route(r)) = &cmd {
            self.settings.audio.routing = Some(*r);
            self.save_audio_settings();
        }
        match command::dispatch(&self.player, &self.lib.store, cmd) {
            Ok(Some(cmd)) => self.dispatch_ui(cmd),
            Ok(None) => {}
            Err(e) => eprintln!("command: {e}"),
        }
    }

    /// Commands about what's on screen, loads, previews and restarting audio.
    fn dispatch_ui(&mut self, cmd: Command) {
        match cmd {
            Command::View(v) => self.view_cmd(v),
            Command::App(AppCmd::ReconnectMidi) => self.connect_midi(),
            Command::App(AppCmd::DismissFailures) => {
                self.analysis.failed = 0;
                self.analysis.last_error.clear();
            }
            Command::Deck(deck, DeckCmd::Load(id)) => self.load(Some(deck), id),
            Command::Library(c) => self.library_cmd(c),
            Command::TempoLock(c) => self.tempo_lock.run(c),
            Command::Collection(c) => self.collection_cmd(c),
            Command::Flow(c) => self.flow_cmd(c),
            Command::Edit(c) => self.edit_cmd(c),
            Command::Undo => self.undo(),
            Command::Redo => self.redo(),
            Command::Preview(PreviewCmd::Toggle(id)) => self.toggle_preview(id),
            Command::Preview(PreviewCmd::Seek(id, at)) => self.seek_preview(id, at),
            Command::Preview(PreviewCmd::Stop(id)) => self.stop_preview(id),
            Command::Audio(AudioCmd::Restart(settings)) => {
                self.settings.audio = settings;
                self.save_audio_settings();
                self.preview = None;
                self.audio = start_audio(&self.player, &self.settings.audio, self.null_audio);
                self.monitor = self.player.lock().unwrap().monitor();
                self.frame.snap = self.monitor.as_ref().map(|m| m.snapshot());
            }
            other => eprintln!("command: nothing handles {other:?}"),
        }
    }

    /// What's on screen: the mode, the layout, focus, windows and zoom.
    fn view_cmd(&mut self, cmd: ViewCmd) {
        let lib = &mut self.panels.library;
        match cmd {
            ViewCmd::Browse(n) => match lib.pane {
                Pane::Library => lib.browser.move_focus(n),
                Pane::Suggest => lib.suggest.move_focus(n),
            },
            ViewCmd::TogglePane => {
                lib.pane = match lib.pane {
                    Pane::Library => Pane::Suggest,
                    Pane::Suggest => Pane::Library,
                };
            }
            ViewCmd::FocusPane(p) => lib.pane = p,
            ViewCmd::FocusSearch => lib.browser.search_focus = true,
            ViewCmd::LoadSelected(deck) => {
                let id = match lib.pane {
                    Pane::Library => lib.browser.focused(),
                    Pane::Suggest => lib.suggest.focused(),
                };
                if let Some(id) = id {
                    self.load(Some(deck), id);
                }
            }
            ViewCmd::SetMode(m) => self.prefs.mode = m,
            ViewCmd::Source(s) => self.prefs.layout_mut().source = s,
            ViewCmd::Aside(a) => self.prefs.layout_mut().aside = a,
            ViewCmd::Tool(t) => self.prefs.layout_mut().tool = t,
            ViewCmd::ShowFlow(on) => lib.flow.open = on,
            ViewCmd::ToggleModal(m) => self.modals.toggle(m),
            ViewCmd::CloseModal(m) => self.modals.close(m),
            ViewCmd::WaveZoom(z) => self.prefs.wave_zoom = z,
            ViewCmd::UiScale(z) => {
                self.prefs.ui_scale = z;
                if (self.ctx.zoom_factor() - z).abs() > 1e-3 {
                    self.ctx.set_zoom_factor(z);
                }
            }
            ViewCmd::Resize(region, size) => {
                let sizes = &mut self.prefs.layout_mut().sizes;
                match region {
                    Region::Waves => sizes.waves = size,
                    Region::Source => sizes.source = size,
                    Region::Aside => sizes.aside = size,
                }
            }
        }
    }

    /// The decks as the play history sees them: track, playing, tempo, key shift.
    fn decks_now(&self) -> Vec<DeckNow> {
        let inner = self.player.lock().unwrap();
        DeckId::first(DECKS)
            .map(|d| {
                let track = inner.session(d).track_id;
                match &self.frame.snap {
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
        let lib = &mut self.panels.library;
        let selected = match lib.flow.open {
            true => lib.flow.selected_track().or(lib.browser.focused()),
            false => lib.browser.focused(),
        };
        let view = CoreView {
            lib: &self.index,
            history: &self.history,
            collections: &self.collections,
            open_flow: lib.flow.flow,
        };
        lib.suggest.refresh(&view, &decks, selected);
    }

    /// Draw the window, collecting what it asks for.
    fn draw(&mut self, ctx: &egui::Context) -> Vec<Command> {
        let mut out: Vec<Command> = vec![];
        let mut cx = Cx {
            core: CoreView {
                lib: &self.index,
                history: &self.history,
                collections: &self.collections,
                open_flow: self.panels.library.flow.flow,
            },
            engine: &self.frame,
            theme: &THEME,
            prefs: &self.prefs,
            out: &mut out,
        };
        egui::TopBottomPanel::top("top")
            .frame(THEME.bar_frame())
            .show(ctx, |ui| {
                TopBar {
                    audio: &self.audio,
                    midi_ports: &self.midi_ports,
                    analysis: &self.analysis,
                    hud: [&self.hud.text, &self.hud.audio],
                }
                .ui(ui, &mut cx)
            });
        egui::TopBottomPanel::bottom("midi")
            .frame(THEME.bar_frame())
            .show(ctx, |ui| MidiLog(&self.midi_log).ui(ui, &mut cx));
        views::show(ctx, &mut self.panels, &mut cx);

        let routing = player::routing(&self.player, self.audio.as_ref().map_or(2, |i| i.channels));
        let env = ModalEnv {
            audio_settings: &self.settings.audio,
            audio: &self.audio,
            routing,
            keymap: &self.keymap,
            mode: self.prefs.mode,
        };
        self.modals.show(ctx, &env, cx.out);
        out
    }
}

impl eframe::App for EarshotApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.prefs.flow = self.panels.library.flow.saved();
        eframe::set_value(storage, prefs::KEY, &self.prefs);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events(ctx);
        self.read_engine(ctx);
        // Background analysis holds back while a deck plays in Perform mode. In Prepare,
        // listening while an import analyses is normal, so it runs at full width.
        let playing = self.prefs.mode == Mode::Perform
            && self
                .frame
                .snap
                .as_ref()
                .is_some_and(|sh| sh.live().iter().any(|d| d.playing));
        self.lib.analyser.set_playing(playing);
        self.keyboard(ctx);
        // Ctrl +/- (egui's own) changes the size too.
        if (ctx.zoom_factor() - self.prefs.ui_scale).abs() > 1e-3 {
            self.dispatch(ViewCmd::UiScale(ctx.zoom_factor()).into());
        }
        self.tick_hud(ctx);
        self.track_plays();
        self.refresh_suggestions();
        self.save_tempo_ranges();

        for c in self.draw(ctx) {
            self.dispatch(c);
        }
        // Save first, so opening another Flow doesn't lose the last change.
        self.save_flow();
        for id in take_wanted_art(ctx) {
            let media = &mut self.panels.library.media;
            if !media.art.contains_key(&id) {
                media.art.insert(id, None);
                let _ = self.art_loader.send(id);
            }
        }
        self.save_flow();
        self.schedule_repaint(ctx);
    }
}

/// Store events, delivered through the app's channel like everything else.
fn store_sink(sink: &Sink) -> EventSink {
    let sink = sink.clone();
    Arc::new(move |e| sink(Event::Store(e)))
}

mod edits;
mod events;
mod frame;
mod input;
mod library_ops;
mod preview;
#[cfg(test)]
mod tests;
