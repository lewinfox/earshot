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
    DeckCmd as EngineCmd, DeckId, Monitor, OutputInfo, OutputOptions, Set, TempoRange,
};
use egui::Key;

use super::modals::settings::{AnalysisEnv, KeymapEnv, Stale};
use super::modals::{ModalEnv, Modals};
use super::panels::deck::{DeckInfo, PendingLoad};
use super::panels::flow::FlowUi;
use super::panels::library::browser::Scope;
use super::panels::library::tracks::take_wanted_art;
use super::panels::library::{Level, LibraryPane, LoadedFrom, Nav, drag};
use super::panels::status::{AnalysisSummary, Meter, StatusLine, TopBar};
use super::panels::{CoreView, Cx, EngineFrame, Panel, PreviewState};
use super::prefs::{self, UiPrefs};
use super::theme::THEME;
use super::views::{self, Panels};
use crate::core::analysis::{self, Analyser, Mode as AnalysisMode};
use crate::core::audio_watch;
use crate::core::collections::{
    CollectionCmd, CollectionId, CollectionKind, Collections, FlowCmd, FlowGraph, Section,
};
use crate::core::command::{self, AppCmd, AudioCmd, Command, DeckCmd, PreviewCmd, ViewCmd};
use crate::core::controller_audio;
use crate::core::edit;
use crate::core::history::{DeckNow, History, PlayEvent};
use crate::core::index::{LibraryCmd, LibraryIndex};
use crate::core::keymap::{self, Keymap};
use crate::core::library::{self, Library};
use crate::core::midi::{self, Midi};
use crate::core::notice::{self, Notices};
use crate::core::player::{self, AudioSettings, DECKS, Player};
use crate::core::services::Services;
use crate::core::session;
use crate::core::settings::{self, Settings};
use crate::core::store::{self, EventSink, Store, StoreCmd, StoreEvent};
use crate::core::tempo_lock::{TempoLock, TempoLockCmd};
use crate::core::track::AnalysisState;
use crate::core::view::{Modal, Mode, Pane, Region, Tool};
use crate::core::worker_client::Role;
use crate::core::{Event, Sink};

/// After the last thing moved, keep drawing this long (meters fall, playheads settle).
const SETTLE: Duration = Duration::from_millis(500);

pub struct EarshotApp {
    ctx: egui::Context,
    /// The window kept to the screen, and the UI size applied.
    window: super::window::WindowFit,
    /// How the window was divided last frame (`layout::shell`).
    shell: super::layout::Shell,
    /// The part of the window that was divided: between the bars.
    shell_rect: egui::Rect,
    // ---- services ----------------------------------------------------------------------
    player: Player,
    lib: Arc<Library>,
    sink: Sink,
    rx: Receiver<Event>,
    monitor: Option<Monitor>,
    /// The same monitor, for the controller's lights thread.
    lights_monitor: crate::core::lights::MonitorSlot,
    /// Where a controller's messages go: the deck each hand plays, held buttons, soft
    /// takeover (`midi::Route`). Its layers are the lights' too.
    midi_route: midi::Route,
    /// The mapping's version when the engine was last told whether the controller
    /// blends the headphones (see [`EarshotApp::sync_phones_cue_only`]); `None` sends
    /// it again (a new engine).
    phones_cue_only_at: Option<u64>,
    /// A connected controller's own sound card, which audio plays to when there is one.
    controller: controller_audio::Hold,
    audio: Result<OutputInfo, String>,
    /// Audio stalls (`core::audio_watch`): when each reopened the device, the last
    /// minute's, and what to show about them (until when; or for good, once reopening
    /// stopped helping).
    audio_stalls: VecDeque<Instant>,
    audio_note: Option<(String, Option<Instant>)>,
    /// What the output audio opened on is called (`core::outputs::name_for`), for the
    /// top bar.
    audio_name: Option<String>,
    /// Messages for the user, in the bottom bar.
    notices: Notices,
    /// "Do you really want to quit?" is showing; and the user said yes.
    asking_quit: bool,
    quitting: bool,
    /// Each player's track and the list it was loaded from, by `DeckId::index()`.
    loaded_from: [Option<(i64, LoadedFrom)>; session::SLOTS],
    /// When each deck's load button was last pressed, for a double tap (instant
    /// doubles); cleared by the double tap.
    load_taps: [Option<Instant>; session::SLOTS],
    /// Always the null output (tests).
    null_audio: bool,
    midi: Midi,
    midi_ports: Vec<String>,
    /// The open MIDI inputs, each with its device's USB id and the mapping it got.
    midi_devices: Vec<midi::Port>,
    /// Every mapping there is (its key and name), for the MIDI pill's list.
    mappings: Vec<(String, String)>,
    /// Send track ids here to load their cover art.
    art_loader: crate::core::background::Mailbox<i64>,
    /// Nudge after a scan: works out audio hashes the files' tags didn't have.
    hasher: library::Hasher,
    /// Gives new tracks their quick grid, if the setting says to.
    quick: library::QuickAnalyser,
    /// The tempo lock tool, and its tab's A/B player.
    tempo_lock: TempoLock,
    listener: tempo_lock::Listener,
    /// The stems tool (its own background worker).
    stems: crate::core::stems::Stems,

    // ---- core state the UI holds ---------------------------------------------------------
    /// The library, collections and play history: the domain state the panes read.
    index: LibraryIndex,
    collections: Collections,
    history: History,
    /// Edits that can be undone and redone.
    edits: edit::History,
    /// Where tracks come from (music folders, other libraries), as the Store has them.
    sources: Vec<crate::core::sources::Source>,
    /// The USB sticks plugged in (`core::drives`), and which earshot is using.
    drives: Vec<crate::core::drives::Drive>,
    /// Each effect unit's TAP presses.
    fx_taps: [crate::core::tap::Taps; earshot_engine::FX_UNITS],
    /// Why each sampler slot's sound couldn't be loaded, if it couldn't.
    sample_errors: [Option<String>; earshot_engine::SAMPLER_SLOTS],
    /// Ends the drive watcher's thread when the app goes.
    _drives_stop: crate::core::drives::StopOnDrop,
    uses: crate::core::drives::Uses,
    /// Stick libraries asked to become sources, so they're asked for once.
    sticks_added: std::collections::HashSet<(crate::core::sources::SourceKind, std::path::PathBuf)>,
    /// Hot cue labels and memory cues of the tracks on the players, by track id.
    cues: HashMap<i64, Arc<store::TrackCues>>,
    analysis: AnalysisSummary,
    /// What's running in the background (`core::jobs`), for the status line and
    /// the jobs pill; analysis's, by track, and the tracks waiting for it.
    jobs: crate::core::jobs::Jobs,
    analysis_jobs: HashMap<i64, crate::core::jobs::Job>,
    analysis_queue: Option<crate::core::jobs::Job>,
    /// The track in the headphones' preview player.
    preview: Option<Preview>,
    /// Deck and audio settings, as saved in the Store.
    settings: Settings,
    /// A collection just made, to rename once the Store's list has it.
    pending_rename: Option<CollectionId>,
    keymap: Keymap,
    /// The user's keymap file, and whether the keymap in use came from it.
    keymap_path: std::path::PathBuf,
    keymap_origin: keymap::Origin,
    /// Background analysis workers allowed at once (as the analyser has it).
    background_workers: usize,
    /// Tracks with stale analysis, for the settings' analysis tab.
    stale: Stale,
    /// Keys held down for buttons that act while held, and what letting go sends.
    held_keys: HashMap<Key, Command>,
    hud: Hud,

    // ---- the view ----------------------------------------------------------------------------
    prefs: UiPrefs,
    modals: Modals,
    panels: Panels,
    /// The engine as read at the start of this frame.
    frame: EngineFrame,
    /// Keep repainting until then, in frame time (see [`SETTLE`]).
    settle_until: Option<f64>,
    /// The engine has the players' saved keylock and quantise (see
    /// [`EarshotApp::save_deck_options`]).
    deck_options_restored: bool,
}

/// Start audio on the chosen device; failing that the default one, then the null
/// output (so the app still runs without a sound card).
/// Start audio: on a connected controller's own sound card, directly, whenever there
/// is one (no other device, and no PipeWire volume, in between); else on the device
/// in the settings.
fn start_audio(
    player: &Player,
    settings: &AudioSettings,
    null: bool,
    controller: &mut controller_audio::Hold,
) -> Result<OutputInfo, String> {
    let mut opts = player::output_options(settings);
    // A thread that steps its background work (a test's) steps its audio too.
    opts.stepped = crate::core::background::stepped();
    opts.null |= null || opts.stepped;
    // Another output chosen over the controller's card: PipeWire may have the card.
    if !settings.use_controller {
        controller.release();
    }
    if !opts.null
        && settings.use_controller
        && let Some(device) = controller.prepare()
    {
        opts.device = Some(device);
        opts.phones_device = None;
        // The controller's own layout (master on 1-2, headphones on 3-4).
        player.lock().unwrap().routing = None;
        // PipeWire may take a moment to close the card after letting it go.
        let mut why = String::new();
        let card = controller.found.as_ref().map(|c| c.card);
        let t0 = Instant::now();
        for attempt in 1..=10 {
            eprintln!(
                "[init] audio: attempt {attempt} at +{:.0} ms; the card's playback now: {}",
                t0.elapsed().as_secs_f64() * 1000.0,
                card.map(controller_audio::card_state).unwrap_or_default()
            );
            match player::start_engine(player, &opts) {
                Ok(info) => {
                    eprintln!(
                        "[init] audio: open on attempt {attempt} after {:.0} ms; the card's playback now: {}",
                        t0.elapsed().as_secs_f64() * 1000.0,
                        card.map(controller_audio::card_state).unwrap_or_default()
                    );
                    controller.error = None;
                    return Ok(info);
                }
                Err(e) => {
                    eprintln!("[init] audio: attempt {attempt} failed: {e}");
                    why = e
                }
            }
            if !why.to_lowercase().contains("busy") {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        eprintln!("audio: the controller's sound card: {why}");
        controller.error = Some(why);
        return player::start_engine(player, &OutputOptions { null: true, ..opts });
    }
    player.lock().unwrap().routing = settings.routing;
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
    /// Frame timing and the audio callback's, for the CPU meter's tooltip.
    text: String,
    audio: String,
    /// The audio engine's load: its slowest callback lately, as a share of the time
    /// it had. `None` without audio.
    load: Option<f32>,
    xruns: u64,
    /// The headphones' own device: times they ran dry and were realigned, as last
    /// logged.
    phones_short: u64,
    phones_realigns: u64,
    /// The machine can't keep up (`EarshotApp::watch_strain`): the audio is behind
    /// (and how often it has run up against the real-time limit), and the system is
    /// holding the CPU back; as last seen (and logged), and the top bar's pills for
    /// them.
    overruns: u64,
    behind: bool,
    power: Option<crate::core::power::Warning>,
    /// Too little memory is left for the analysis worker (`power::memory_short`).
    memory_short: bool,
    /// Any of those: analysis and stems wait, and tracks get quick grids meanwhile.
    low_resource: bool,
    alerts: Vec<crate::ui::panels::status::Alert>,
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

        let store = Store::open(&services.db(), store_sink(&sink)).map_err(|e| e.to_string())?;
        let settings = Settings::load(&store.reader);
        let player = player::new_player();
        player.lock().unwrap().routing = settings.audio.routing;
        // A controller with a sound card is played to directly, from the start.
        let mut controller = controller_audio::Hold::default();
        if services.midi {
            controller.set(controller_audio::find(&midi::port_names()));
        }
        // Audio starts once the app is built, after the controller (`EarshotApp::
        // start_up`).
        let audio = Err("audio hasn't started yet".to_string());
        let monitor = None;

        let drives_stop = crate::core::drives::StopOnDrop::default();
        // The two that watch the world by the clock (the sound card, the USB sticks)
        // aren't started where nothing is to run on its own: a test says what they
        // would have.
        let watching = !crate::core::background::stepped();
        if watching {
            let sink = sink.clone();
            audio_watch::start(&player, move |quiet, decks| {
                sink(Event::Command(Command::Audio(AudioCmd::Stalled {
                    quiet,
                    decks,
                })))
            });
        }
        if watching {
            let sink = sink.clone();
            crate::core::drives::watch(drives_stop.0.clone(), move |d| sink(Event::Drives(d)));
        }
        let analyser =
            Analyser::start(sink.clone(), player.clone(), store.clone(), services.worker);
        let hasher = library::start_hasher(store.clone());
        let quick = library::start_quick_analyser(store.clone());
        let folders = &services.folders;
        let keymap_path = folders.config.join(keymap::FILE_NAME);
        let (keymap, keymap_origin) = keymap::load_user(&keymap_path, spell_key);
        let background_workers = analyser.background_workers();
        let tempo_lock = {
            let sink = sink.clone();
            let cores = analysis::cores();
            TempoLock::with_worker(
                (services.worker)(Role::Background, cores),
                store.clone(),
                Arc::new(move |e| sink(Event::TempoLock(e))),
                &folders.cache,
                Some(hasher.clone()),
            )
        };
        crate::core::stems::init(
            &folders.cache,
            (settings.stems_cache_gb.max(0.0) * 1e9) as u64,
        );
        let jobs = {
            let ctx = cc.egui_ctx.clone();
            crate::core::jobs::Jobs::new(move || ctx.request_repaint())
        };
        let stems = {
            let sink = sink.clone();
            crate::core::stems::Stems::with_worker(
                (services.worker)(Role::Background, analysis::cores()),
                store.clone(),
                player.clone(),
                jobs.clone(),
                Arc::new(move |e| sink(Event::Stems(e))),
            )
        };
        let art_loader = library::start_art_loader(store.reader.clone(), sink.clone());
        let lib = Arc::new(Library { store, analyser });
        let mut prefs: UiPrefs = cc
            .storage
            .and_then(|s| eframe::get_value(s, prefs::KEY))
            .unwrap_or_default();
        prefs.migrate();
        // The UI size is egui's zoom, set each frame from the preference and what the
        // screen has room for (`window::fit`); Ctrl +/- are ours (our steps and range).
        cc.egui_ctx.options_mut(|o| {
            o.zoom_with_keyboard = false;
            // A frame whose layout hasn't settled (a deck's controls after its scale
            // changes) is drawn again rather than shown, up to this many times.
            o.max_passes = std::num::NonZeroUsize::new(10).expect("not zero");
        });
        let tables = prefs.tables.clone();
        let mut app = EarshotApp {
            ctx: cc.egui_ctx.clone(),
            window: Default::default(),
            shell: Default::default(),
            shell_rect: egui::Rect::ZERO,
            player,
            lib,
            sink,
            rx,
            midi_route: midi::Route::default(),
            lights_monitor: Arc::new(std::sync::Mutex::new(crate::core::lights::LightsState {
                monitor: monitor.clone(),
                ..Default::default()
            })),
            phones_cue_only_at: None,
            monitor,
            audio,
            controller,
            audio_stalls: VecDeque::new(),
            audio_note: None,
            audio_name: None,
            loaded_from: Default::default(),
            load_taps: Default::default(),
            notices: Notices::default(),
            asking_quit: false,
            quitting: false,
            null_audio: services.null_audio,
            midi: Midi::default(),
            midi_ports: vec![],
            midi_devices: vec![],
            mappings: vec![],
            art_loader,
            hasher,
            quick,
            tempo_lock,
            listener: Default::default(),
            stems,
            index: LibraryIndex::default(),
            collections: Collections::default(),
            history: History::default(),
            edits: edit::History::default(),
            sources: vec![],
            drives: vec![],
            _drives_stop: drives_stop,
            fx_taps: Default::default(),
            sample_errors: Default::default(),
            uses: Default::default(),
            sticks_added: Default::default(),
            cues: HashMap::new(),
            analysis: AnalysisSummary::default(),
            jobs,
            analysis_jobs: HashMap::new(),
            analysis_queue: None,
            preview: None,
            settings,
            pending_rename: None,
            keymap,
            keymap_path,
            keymap_origin,
            background_workers,
            stale: Stale::Unknown,
            held_keys: HashMap::new(),
            hud: Hud::default(),
            prefs,
            modals: Modals::default(),
            panels: Panels::new(LibraryPane::new(tables)),
            frame: EngineFrame::default(),
            settle_until: None,
            deck_options_restored: false,
        };
        if cfg!(debug_assertions) {
            let rec = crate::core::debug::Recorder::start();
            app.lights_monitor.lock().unwrap().recorder = Some(rec.clone());
            app.panels.debug.recorder = Some(rec);
            app.panels.debug.mapping = Some(app.panels.controller.learned.clone());
            app.panels.debug.lights = Some(app.lights_monitor.clone());
        }
        app.panels.controller.lights = Some(app.lights_monitor.clone());
        // The user's own controller mappings (`core::controllers`).
        app.panels.controller.mine = Some(services.folders.config.join("controllers"));
        app.start_up(services.midi);
        app.lib.store.send(StoreCmd::ListSources);
        app.lib.store.send(StoreCmd::StartSession);
        let ranges = app.settings.tempo_ranges.clone();
        for (deck, range) in DeckId::first(DECKS).zip(ranges) {
            app.dispatch((deck, EngineCmd::TempoRange(range)).into());
        }
        let _ = player::set_hotcue_mode(&app.player, app.settings.hotcue_mode);
        let _ = player::set_xf_curve(&app.player, app.settings.xf_curve);
        let _ = player::set_fx_decks(&app.player, app.prefs.decks().len());
        player::set_cue_first_beat(&app.player, app.settings.cue_first_beat);
        app.start_sampler();
        let options = app.settings.deck_options.clone();
        for (i, o) in options.into_iter().enumerate().take(session::SLOTS) {
            let deck = session::slot(i);
            app.dispatch(
                (
                    deck,
                    EngineCmd::Keylock(earshot_engine::Set::from_bool(o.keylock)),
                )
                    .into(),
            );
            app.dispatch(
                (
                    deck,
                    EngineCmd::Quantize(earshot_engine::Set::from_bool(o.quantize)),
                )
                    .into(),
            );
            app.dispatch(
                (
                    deck,
                    EngineCmd::Vinyl(earshot_engine::Set::from_bool(o.vinyl)),
                )
                    .into(),
            );
        }
        app.restore_flow();
        app.rescan();
        // Sources' path rules whose folders have gone are dropped: the next read
        // looks for those files again.
        for mut s in app.lib.store.reader.sources().unwrap_or_default() {
            if s.config.revalidate() {
                app.lib.store.send(StoreCmd::UpdateSource(s));
            }
        }
        Ok(app)
    }

    /// Start-up, in Mixxx's order, which a DDJ-400 is happy with: the controller first
    /// (MIDI open, its lights set, asked where its controls are: `core::lights`), then
    /// audio. Nothing goes to the controller while its sound card starts.
    fn start_up(&mut self, midi: bool) {
        if midi {
            self.connect_midi(); // opens audio itself if the controller has a sound card
        }
        if self.player.lock().unwrap().engine.is_none() {
            self.reopen_audio();
        }
    }

    /// Quitting, in Mixxx's order: audio closed and the controller's sound card let go
    /// first, then the controller's lights put out and MIDI closed.
    fn shut_down(&mut self) {
        eprintln!("[exit] audio: closing");
        self.preview = None;
        self.monitor = None;
        self.lights_monitor.lock().unwrap().monitor = None;
        {
            let mut p = self.player.lock().unwrap();
            p.midi.set(None);
            p.engine = None;
        }
        self.controller.release();
        eprintln!("[exit] MIDI: closing");
        self.midi = Midi::default();
    }

    /// Start audio again (on the controller's sound card, if one's connected).
    #[track_caller]
    fn reopen_audio(&mut self) {
        eprintln!(
            "[init] audio: (re)opening (asked from {})",
            std::panic::Location::caller()
        );
        self.preview = None;
        self.audio = start_audio(
            &self.player,
            &self.settings.audio,
            self.null_audio,
            &mut self.controller,
        );
        self.audio_name = match &self.audio {
            Ok(i) if !i.null => Some(crate::core::outputs::name_for(
                &i.device,
                self.controller
                    .found
                    .as_ref()
                    .filter(|_| self.settings.audio.use_controller),
            )),
            _ => None,
        };
        self.reread_monitor();
    }

    /// Read the engine's monitor again, after the sound card was reopened.
    fn reread_monitor(&mut self) {
        self.monitor = self.player.lock().unwrap().monitor();
        self.lights_monitor.lock().unwrap().monitor = self.monitor.clone();
        self.frame.snap = self.monitor.as_ref().map(|m| m.snapshot());
        self.phones_cue_only_at = None;
        self.feed_recorder();
    }

    /// Debug builds: give the recorder the (new) engine's output and state.
    fn feed_recorder(&mut self) {
        let Some(rec) = &self.panels.debug.recorder else {
            return;
        };
        let tap = self
            .player
            .lock()
            .unwrap()
            .engine
            .as_mut()
            .and_then(|e| e.take_output_tap());
        let mut feed = rec.feed.lock().unwrap();
        feed.tap = tap;
        feed.monitor = self.monitor.clone();
    }

    /// A mapping whose headphone mix is display only (a DDJ-400's: the controller
    /// blends the master into the headphones itself) has the engine send only the
    /// cued decks to the headphones; otherwise earshot blends them. Sent when the
    /// mapping changes, and to a new engine.
    fn sync_phones_cue_only(&mut self) {
        let learned = &self.panels.controller.learned;
        let v = learned.version();
        if self.phones_cue_only_at == Some(v) {
            return;
        }
        self.phones_cue_only_at = Some(v);
        let on = learned.all().into_iter().any(|(_, l)| {
            l.display_only
                && l.control
                    == crate::core::bus::Control::Master(earshot_engine::MasterParam::HeadMix)
        });
        self.dispatch(Command::Mixer(
            crate::core::command::MixerCmd::PhonesCueOnly(on),
        ));
    }

    /// (Re)open every MIDI input, e.g. after plugging the controller in.
    #[track_caller]
    fn connect_midi(&mut self) {
        eprintln!(
            "[init] MIDI: connecting (asked from {})",
            std::panic::Location::caller()
        );
        // Close the old ports first so they can be reopened.
        self.midi = Midi::default();
        let lane = self.player.lock().unwrap().midi.clone();
        let learned = self.panels.controller.learned.clone();
        // Each controller's mapping: the one chosen for it before (the MIDI pill), else
        // the best match (`core::controllers`).
        let store = crate::core::controllers::Store::new(self.panels.controller.mine.clone());
        let chosen = self.prefs.controller_mappings.clone();
        let choose = |port: &str, usb: Option<&str>| {
            let key = crate::core::controllers::device_key(port, usb);
            store.find(port, usb, chosen.get(&key).map(String::as_str))
        };
        (self.midi, self.midi_devices) = midi::connect_all(
            &self.sink,
            &lane,
            &learned,
            &self.lights_monitor,
            &self.midi_route,
            &choose,
        )
        .unwrap_or_else(|e| {
            eprintln!("MIDI: {e}");
            (Midi::default(), vec![])
        });
        self.midi_ports = self.midi_devices.iter().map(|p| p.name.clone()).collect();
        self.mappings = store
            .models()
            .iter()
            .map(|p| (p.id.key(), p.label()))
            .collect();
        self.prefs.set_controller(!self.midi_ports.is_empty());
        self.panels.controller.ports = self.midi_ports.clone();
        self.panels.controller.devices = self.midi_devices.clone();
        self.panels
            .controller
            .set_active(self.midi_devices.iter().find_map(|p| p.mapping.clone()));
        // A controller with a sound card came or went: play to it, or stop.
        if self
            .controller
            .set(controller_audio::find(&self.midi_ports))
        {
            self.reopen_audio();
        }
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

    /// Save the players' keylock and quantise when they change (from a button, a key
    /// or MIDI).
    fn save_deck_options(&mut self) {
        let Some(sh) = &self.frame.snap else {
            return;
        };
        let options: Vec<settings::DeckOptions> = (0..session::SLOTS)
            .map(|i| {
                let d = sh.deck(session::slot(i));
                settings::DeckOptions {
                    keylock: d.keylock,
                    quantize: d.quantize,
                    vinyl: d.vinyl,
                }
            })
            .collect();
        // Not until the engine has the saved ones: before that it reports its own
        // defaults, which would overwrite them.
        if !self.deck_options_restored {
            let saved = &self.settings.deck_options;
            self.deck_options_restored = saved.is_empty() || options.starts_with(saved);
            return;
        }
        if options != self.settings.deck_options {
            self.lib
                .store
                .send(settings::save(settings::DECK_OPTIONS, &options));
            self.settings.deck_options = options;
        }
    }

    fn save_audio_settings(&self) {
        self.lib
            .store
            .send(settings::save(settings::AUDIO, &self.settings.audio));
    }

    /// How many of Perform's decks are playing.
    fn decks_playing(&self) -> usize {
        self.frame
            .snap
            .as_ref()
            .map_or(0, |sh| sh.live().iter().filter(|d| d.playing).count())
    }

    /// The memory cues and loops of the track on `deck`, earliest first.
    fn memories(&self, deck: DeckId) -> Vec<store::Memory> {
        let track = self.player.lock().unwrap().session(deck).track_id;
        let mut m = track
            .and_then(|t| self.cues.get(&t))
            .map_or(vec![], |c| c.memories.clone());
        m.sort_by(|a, b| a.start.total_cmp(&b.start));
        m
    }

    /// The memory cue just before (or after) the playhead on `deck`. One right at the
    /// playhead doesn't count, so pressing again moves on.
    fn memory_beside(&self, deck: DeckId, forward: bool) -> Option<store::Memory> {
        const NEAR: f64 = 0.05;
        let at = self.frame.snap.as_ref()?.deck(deck).position;
        let m = self.memories(deck);
        if forward {
            m.into_iter().find(|m| m.start > at + NEAR)
        } else {
            m.into_iter().rev().find(|m| m.start < at - NEAR)
        }
    }

    /// The memory cue or loop last called on `deck`: a cue at the cue point (calling
    /// one puts the cue point there), or the loop playing.
    fn called_memory(&self, deck: DeckId) -> Option<i64> {
        const NEAR: f64 = 0.01;
        let d = self.frame.snap.as_ref()?.deck(deck);
        let near = |a: f64, b: f64| (a - b).abs() < NEAR;
        self.memories(deck).into_iter().find_map(|m| {
            let hit = match (m.end, d.loop_in, d.loop_out) {
                (Some(end), Some(i), Some(o)) => d.loop_active && near(m.start, i) && near(end, o),
                (Some(_), ..) => false,
                (None, ..) => near(m.start, d.cue_point),
            };
            hit.then_some(m.id)
        })
    }

    /// The memory loop SHIFT + RELOOP/EXIT acts on: the one playing (just called),
    /// else the one nearest the playhead (none: inside it).
    fn loop_to_activate(&self, deck: DeckId) -> Option<store::Memory> {
        let called = self.called_memory(deck);
        let at = self.frame.snap.as_ref()?.deck(deck).position;
        let loops = self.memories(deck).into_iter().filter(|m| m.end.is_some());
        let away = |m: &store::Memory| {
            let end = m.end.unwrap_or(m.start);
            if at < m.start {
                m.start - at
            } else {
                (at - end).max(0.0)
            }
        };
        let loops: Vec<store::Memory> = loops.collect();
        loops
            .iter()
            .find(|m| Some(m.id) == called)
            .or_else(|| loops.iter().min_by(|a, b| away(a).total_cmp(&away(b))))
            .cloned()
    }

    /// Give each deck its track's active loop, when the engine has another (a load
    /// clears it there, and the loop may have been changed or saved since).
    fn send_active_loops(&mut self) {
        let Some(snap) = self.frame.snap.as_ref() else {
            return;
        };
        let mut out = vec![];
        for (i, d) in snap.live().iter().enumerate() {
            let Some(deck) = DeckId::new(i) else { continue };
            let want = d
                .track_id
                .and_then(|t| self.cues.get(&t))
                .and_then(|c| c.memories.iter().find(|m| m.active))
                .and_then(|m| Some((m.start, m.end?)));
            if want != d.active_loop {
                out.push(Command::Deck(
                    deck,
                    DeckCmd::Engine(earshot_engine::DeckCmd::ActiveLoop(want)),
                ));
            }
        }
        for c in out {
            self.dispatch(c);
        }
    }

    /// Switch mode: only the tabs under the decks change. Refused (with a note in the
    /// bottom bar) when leaving Perform while a deck plays; any other switch pauses the
    /// playing decks, leaving their tracks loaded where they were. True if it switched.
    pub(super) fn set_mode(&mut self, m: Mode) -> bool {
        if m == self.prefs.mode {
            return true;
        }
        if self.prefs.mode == Mode::Perform && self.decks_playing() > 0 {
            let text = format!("Stop the playing decks before switching to {}.", m.name());
            self.notify(notice::Level::Warning, text);
            return false;
        }
        let playing: Vec<DeckId> = self.frame.snap.as_ref().map_or(vec![], |sh| {
            (sh.live().iter().enumerate())
                .filter(|(_, d)| d.playing)
                .filter_map(|(i, _)| DeckId::new(i))
                .collect()
        });
        for d in playing {
            self.dispatch((d, EngineCmd::Play(Set::Off)).into());
        }
        self.prefs.mode = m;
        true
    }

    /// Closing the window while a deck plays: ask first ("Quit" closes it
    /// again, for good).
    fn confirm_quit(&mut self, ctx: &egui::Context) {
        let asked = ctx.input(|i| i.viewport().close_requested());
        if asked && !self.quitting && self.decks_playing() > 0 {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.asking_quit = true;
        }
        if !self.asking_quit {
            return;
        }
        let n = self.decks_playing();
        let r = egui::Modal::new(egui::Id::new("confirm-quit")).show(ctx, |ui| {
            ui.heading("Do you really want to quit?");
            if n > 0 {
                let decks = if n == 1 { "A deck is" } else { "Decks are" };
                ui.label(format!("{decks} still playing."));
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Quit").clicked() {
                    self.quitting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if ui.button("Cancel").clicked() {
                    self.asking_quit = false;
                }
            })
        });
        if r.should_close() {
            self.asking_quit = false;
        }
    }

    /// Show the user a message in the bottom bar.
    pub(super) fn notify(&mut self, level: notice::Level, text: impl Into<String>) {
        self.notices.push(level, text, Instant::now());
        self.ctx.request_repaint();
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
            Command::App(AppCmd::UseMapping { device, mapping }) => {
                eprintln!("MIDI: {device} uses mapping {mapping} from now on");
                self.prefs.controller_mappings.insert(device, mapping);
                self.connect_midi();
            }
            Command::App(AppCmd::TakeControllerAudio) => match self.controller.force() {
                Ok(()) => self.reopen_audio(),
                Err(e) => self.controller.error = Some(e),
            },
            Command::App(AppCmd::ControllerWaits { control, at }) => {
                crate::ui::panels::mixer::set_waiting(&self.ctx, control, at);
            }
            Command::App(AppCmd::ControllerShows { control, value }) => {
                // A button toggles what's shown on each press (the controller keeps
                // its own state; it starts off, as the controller does).
                let shown = if control.range().is_none() {
                    if value <= 0.5 {
                        return;
                    }
                    let was = crate::ui::panels::mixer::shown(&self.ctx, control).unwrap_or(0.0);
                    1.0 - was
                } else {
                    value
                };
                crate::ui::panels::mixer::set_shown(&self.ctx, control, shown);
                // Earshot's own stays where it leaves the sound to the controller, so
                // the two don't both act.
                if let Some(hold) = crate::core::midi::display_only_value(control) {
                    let now = self.frame.snap.as_ref().and_then(|s| control.get(s));
                    if now.is_some_and(|v| (v - hold).abs() > 1e-9) {
                        if let Some(c) = crate::core::midi::set_command(control, hold) {
                            self.dispatch(c);
                        }
                    }
                }
            }
            Command::App(AppCmd::FxTap { unit, at }) => {
                if let Some(bpm) = self.fx_taps.get_mut(unit).and_then(|t| t.tap(at)) {
                    self.dispatch(Command::Fx(unit, earshot_engine::FxCmd::TapBpm(bpm)));
                }
            }
            Command::App(AppCmd::CancelJob(id)) => self.jobs.cancel(id),
            Command::App(AppCmd::StopAnalysis) => {
                self.lib.analyser.stop_background();
            }
            Command::App(AppCmd::DismissFailures) => {
                self.analysis.failed = 0;
                self.analysis.last_error.clear();
            }
            Command::App(AppCmd::StemsCacheGb(gb)) => {
                self.settings.stems_cache_gb = gb;
                self.lib
                    .store
                    .send(settings::save(settings::STEMS_CACHE_GB, &gb));
            }
            Command::App(AppCmd::BackgroundWorkers(n)) => {
                self.background_workers = self.lib.analyser.set_background_workers(n);
                self.lib.store.send(settings::save(
                    analysis::SETTING_BACKGROUND_WORKERS,
                    &self.background_workers,
                ));
            }
            Command::App(AppCmd::HotcueMode(m)) => {
                self.settings.hotcue_mode = m;
                if let Err(e) = player::set_hotcue_mode(&self.player, m) {
                    eprintln!("hot cue mode: {e}");
                }
                self.lib
                    .store
                    .send(settings::save(settings::HOTCUE_MODE, &m));
            }
            Command::App(AppCmd::XfCurve(c)) => {
                self.settings.xf_curve = c;
                if let Err(e) = player::set_xf_curve(&self.player, c) {
                    eprintln!("crossfader curve: {e}");
                }
                self.lib.store.send(settings::save(settings::XF_CURVE, &c));
            }
            Command::App(AppCmd::CueFirstBeat(on)) => {
                self.settings.cue_first_beat = on;
                player::set_cue_first_beat(&self.player, on);
                self.lib
                    .store
                    .send(settings::save(settings::CUE_FIRST_BEAT, &on));
            }
            Command::App(AppCmd::LoadWhilePlaying(on)) => {
                self.settings.load_while_playing = on;
                self.lib
                    .store
                    .send(settings::save(settings::LOAD_WHILE_PLAYING, &on));
            }
            Command::App(AppCmd::QuickAnalyseNew(on)) => {
                self.settings.quick_analyse_new = on;
                self.lib
                    .store
                    .send(settings::save(settings::QUICK_ANALYSE_NEW, &on));
            }
            Command::App(AppCmd::ReloadKeymap) => self.reload_keymap(),
            Command::App(AppCmd::DismissNotice(id)) => self.notices.dismiss(id),
            Command::App(c @ (AppCmd::CountStale | AppCmd::QueueStale)) => {
                if self.stale == Stale::Counting {
                    return;
                }
                self.stale = Stale::Counting;
                let (lib, sink) = (self.lib.clone(), self.sink.clone());
                let queued = matches!(c, AppCmd::QueueStale);
                crate::core::background::spawn("earshot-stale", move || {
                    let result = if queued {
                        lib.analyser.queue_stale()
                    } else {
                        lib.analyser.stale_tracks().map(|t| t.len())
                    };
                    sink(Event::Stale { queued, result });
                });
            }
            Command::App(AppCmd::CopyKeymap) => {
                // Never over the user's own file. Its folder may not be there yet.
                if let Some(dir) = self.keymap_path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let made = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&self.keymap_path)
                    .and_then(|mut f| {
                        std::io::Write::write_all(&mut f, keymap::BUILT_IN.as_bytes())
                    });
                if let Err(e) = made {
                    eprintln!("{}: {e}", self.keymap_path.display());
                }
                self.reload_keymap();
            }
            Command::Deck(deck, DeckCmd::Load(id)) => {
                self.load(Some(deck), id);
            }
            Command::Deck(deck, DeckCmd::CallMemory { forward }) => {
                if let Some(m) = self.memory_beside(deck, forward) {
                    self.dispatch(Command::Deck(deck, DeckCmd::RecallMemory(m)));
                }
            }
            Command::Deck(deck, DeckCmd::DeleteCalledMemory) => {
                if let Some(id) = self.called_memory(deck) {
                    self.dispatch(Command::Deck(deck, DeckCmd::DeleteMemory(id)));
                }
            }
            Command::Deck(deck, DeckCmd::ToggleActiveLoop) => {
                if let Some(m) = self.loop_to_activate(deck) {
                    self.dispatch(Command::Deck(deck, DeckCmd::SetLoopActive(m.id, !m.active)));
                }
            }
            Command::Library(c) => self.library_cmd(c),
            Command::External(c) => self.external_cmd(c),
            Command::Export(c) => self.export_cmd(c),
            Command::TempoLock(TempoLockCmd::Listen { version, at }) => self.listen(version, at),
            Command::TempoLock(c) => self.tempo_lock.run(c),
            // Stems are the heaviest thing there is to work out: not while the machine
            // is short of resources (`watch_strain`).
            Command::Stems(crate::core::stems::StemsCmd::Calculate(_)) if self.hud.low_resource => {
                self.notify(
                    notice::Level::Warning,
                    "Stems will have to wait: the computer is short of resources, and the \
                     audio comes first",
                );
            }
            Command::Stems(c) => self.stems.run(c),
            Command::Trim(c) => self.trim_cmd(c),
            Command::Collection(c) => self.collection_cmd(c),
            Command::Flow(c) => self.flow_cmd(c),
            Command::Edit(c) => self.edit_cmd(c),
            Command::Undo => self.undo(),
            Command::Redo => self.redo(),
            Command::Preview(PreviewCmd::Toggle(id)) => self.toggle_preview(id),
            Command::Preview(PreviewCmd::Seek(id, at)) => self.seek_preview(id, at),
            Command::Preview(PreviewCmd::Stop(id)) => self.stop_preview(id),
            Command::Sampler(c) => self.sampler_cmd(c),
            Command::Audio(AudioCmd::Restart(settings)) => {
                self.audio_stalls.clear();
                self.audio_note = None;
                self.settings.audio = settings;
                self.save_audio_settings();
                self.reopen_audio();
            }
            Command::Audio(AudioCmd::Stalled { quiet, decks }) => self.audio_stalled(quiet, &decks),
            other => eprintln!("command: nothing handles {other:?}"),
        }
    }

    /// The sound card stopped asking for audio: reopen it, as changing the device by
    /// hand does, unless that has already been tried three times in the last minute.
    fn audio_stalled(&mut self, quiet: Duration, decks: &str) {
        const TRIES: usize = 3;
        let now = Instant::now();
        while self
            .audio_stalls
            .front()
            .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60))
        {
            self.audio_stalls.pop_front();
        }
        let ms = quiet.as_millis();
        if self.audio_stalls.len() >= TRIES {
            crate::core::log::note(&format!(
                "audio stalled ({ms} ms); not reopening it again ({TRIES} times in a minute): {decks}"
            ));
            self.audio_note = Some((
                "audio keeps stopping: pick the device again in Audio settings".into(),
                None,
            ));
            return;
        }
        self.audio_stalls.push_back(now);
        crate::core::log::note(&format!("audio stalled ({ms} ms); reopening it: {decks}"));
        self.reopen_audio();
        self.audio_note = Some((
            format!("audio stopped for {ms} ms; restarted it"),
            Some(now + Duration::from_secs(30)),
        ));
    }

    /// What's on screen: the mode, the layout, focus, windows and zoom.
    fn view_cmd(&mut self, cmd: ViewCmd) {
        let (lib, flow) = (&mut self.panels.library, &self.panels.flow);
        match cmd {
            // On a list, straight away; elsewhere when the library is next drawn (it
            // knows the sources and trees then).
            ViewCmd::Browse(n) if lib.level == Level::List => lib.browse(n),
            ViewCmd::Browse(n) => lib.nav.push(Nav::Turn(n)),
            ViewCmd::PaneStep(by) => lib.nav.push(if by > 0 { Nav::Next } else { Nav::Prev }),
            ViewCmd::FocusPane(p) => lib.focus(p),
            ViewCmd::RelatedTracks => {
                use crate::core::view::Aside;
                let lib = &mut self.panels.library;
                let layout = self.prefs.layout_mut();
                if layout.aside == Some(Aside::Suggestions) {
                    // Open: close it, and the knob goes back to the track list.
                    layout.aside = None;
                    if lib.focused_pane == Pane::Suggest {
                        lib.focus(Pane::Library);
                    }
                } else {
                    layout.aside = Some(Aside::Suggestions);
                    lib.focus(Pane::Suggest);
                    lib.suggest.focus_top();
                }
            }
            ViewCmd::ToggleLibraryMaximized => {
                self.prefs.library_maximized = !self.prefs.library_maximized;
            }
            ViewCmd::FocusSearch => lib.browser.search_focus = true,
            ViewCmd::LoadSelected(deck) => {
                // Pressed twice quickly (as rekordbox's LOAD): instant doubles.
                if double_tap(&mut self.load_taps[deck.index()], Instant::now()) {
                    self.instant_double(deck);
                } else if let Some(id) = lib.focused_track(flow) {
                    self.load(Some(deck), id);
                }
            }
            ViewCmd::InstantDouble(deck) => self.instant_double(deck),
            ViewCmd::ShowInLibrary(deck) => {
                let Some(track) = self.player.lock().unwrap().session(deck).track_id else {
                    return;
                };
                // Not if the deck's track changed since, or the crate, playlist or
                // Flow doesn't have it any more: then All tracks.
                let gone = |f: &LoadedFrom| match f {
                    LoadedFrom::List {
                        scope: Scope::Collection(id),
                        ..
                    } => self
                        .collections
                        .get(*id)
                        .is_none_or(|c| c.kind.takes_tracks() && !c.tracks.contains(&track)),
                    _ => false,
                };
                let from = self.loaded_from[deck.index()]
                    .clone()
                    .filter(|(t, f)| *t == track && !gone(f))
                    .map(|(_, f)| f);
                let mut source = self.prefs.layout().source;
                let mut out = vec![];
                let note = self
                    .panels
                    .library
                    .show_track(track, from, &mut source, &mut out);
                self.prefs.layout_mut().source = source;
                if let Some(text) = note {
                    self.notify(notice::Level::Info, text);
                }
                for c in out {
                    self.dispatch(c);
                }
            }
            ViewCmd::LoadAdjacent(deck, n) => {
                let from = lib.focused_track(flow);
                lib.browse(n);
                let to = lib.focused_track(flow);
                // Not again at either end of the list.
                if let Some(id) = to.filter(|_| to != from) {
                    self.load(Some(deck), id);
                }
            }
            ViewCmd::SwitchLayer(side) => self.switch_layer(side),
            ViewCmd::PadMode(d, m) => {
                let ui = &mut self.panels.decks[d.index()];
                let out = ui.set_pad_mode(m);
                self.lights_monitor.lock().unwrap().pads[d.index()] = ui.pads;
                for c in out {
                    self.dispatch(c);
                }
            }
            ViewCmd::Shift(d, held) => self.panels.decks[d.index()].shift_held = held,
            ViewCmd::ModePad {
                deck,
                mode,
                pad,
                shift,
                down,
            } => {
                let ui = &mut self.panels.decks[deck.index()];
                let out = ui.mode_pad(mode, pad as usize, shift, down);
                self.lights_monitor.lock().unwrap().pads[deck.index()] = ui.pads;
                for c in out {
                    self.dispatch(c);
                }
            }
            ViewCmd::Target(d) => {
                // The tools' marks leave the old deck; they're drawn on the new one
                // from the next frame.
                for deck in &mut self.panels.decks {
                    deck.set_overlay("tempo-lock", None);
                    deck.set_overlay("trim", None);
                }
                self.prefs.target = d.index();
            }
            ViewCmd::DeckCount(n) => {
                self.prefs.deck_count = crate::ui::prefs::deck_count(n);
                if let Err(e) = player::set_fx_decks(&self.player, self.prefs.decks().len()) {
                    eprintln!("effect decks: {e}");
                }
            }
            ViewCmd::SetMode(m) => {
                self.set_mode(m);
            }
            ViewCmd::NormaliseTempo(id) => {
                if self.set_mode(Mode::Prepare) {
                    self.prefs.prepare.tool = Tool::TempoLock;
                    self.tempo_lock.run(TempoLockCmd::Analyse(id));
                }
            }
            ViewCmd::Source(s) => {
                self.prefs.layout_mut().source = s;
                self.panels.library.level = Level::Sources;
            }
            ViewCmd::Aside(a) => self.prefs.layout_mut().aside = a,
            ViewCmd::Tool(t) => self.prefs.layout_mut().tool = t,
            ViewCmd::ToggleModal(m) => {
                // Counted again each time the settings open.
                if m == Modal::Settings {
                    self.stale = Stale::Unknown;
                }
                self.modals.toggle(m);
            }
            ViewCmd::Settings(tab) => {
                if !self.modals.is_open(Modal::Settings) {
                    self.stale = Stale::Unknown;
                }
                self.modals.settings(tab);
            }
            ViewCmd::CloseModal(m) => self.modals.close(m),
            ViewCmd::WaveZoom(z) => self.prefs.wave_zoom = z,
            ViewCmd::ShowMixer(show) => self.prefs.set_mixer(show),
            ViewCmd::AutoHideMixer(on) => self.prefs.auto_hide_mixer = on,
            ViewCmd::StemMode(m) => self.prefs.stem_mode = m,
            ViewCmd::PretendController(on) => self.prefs.set_pretend_controller(on),
            ViewCmd::UiScale(z) => self.prefs.ui_scale = prefs::ui_scale(z),
            ViewCmd::Resize(region, size) => {
                let sizes = &mut self.prefs.layout_mut().sizes;
                match region {
                    Region::Waves => sizes.waves = size,
                    Region::Source => sizes.source = size,
                    Region::Aside => sizes.aside = size,
                    Region::FlowLibrary => sizes.flow_library = size,
                }
            }
        }
    }

    /// Read the user's keymap file again; the built-in keymap if it's not there or
    /// doesn't load. Keys held down keep what letting go sends.
    fn reload_keymap(&mut self) {
        (self.keymap, self.keymap_origin) = keymap::load_user(&self.keymap_path, spell_key);
    }

    /// The decks as the play history sees them: track, playing, tempo, key shift.
    fn decks_now(&self) -> Vec<DeckNow> {
        DeckId::first(DECKS).map(|d| self.deck_now(d)).collect()
    }

    /// A player as the play history sees it.
    fn deck_now(&self, d: DeckId) -> DeckNow {
        let track = self.player.lock().unwrap().session(d).track_id;
        match &self.frame.snap {
            Some(sh) => {
                let sh = sh.deck(d);
                DeckNow {
                    track,
                    playing: sh.playing,
                    bpm: sh.bpm,
                    // How far the key has moved: by the tempo unless keylocked, and
                    // by the key shift.
                    key_speed: if sh.keylock { 1.0 } else { sh.tempo }
                        * earshot_engine::keylock::semitones(sh.key_shift),
                }
            }
            None => DeckNow {
                track,
                key_speed: 1.0,
                ..Default::default()
            },
        }
    }

    /// Note which deck started playing last, and mark tracks played after 30 s. Each
    /// play is saved (the `plays` table), with when it ended.
    fn track_plays(&mut self) {
        let decks = self.decks_now();
        let now = Instant::now();
        for e in self.history.tick(now, &decks) {
            let cmd = match e {
                PlayEvent::Counted { track, deck, since } => StoreCmd::SavePlay {
                    track,
                    deck,
                    started_at: unix_seconds_at(now, since),
                },
                PlayEvent::Ended { deck } => StoreCmd::EndPlay { deck },
            };
            self.lib.store.send(cmd);
            // A new play: the History lists it (after it's saved).
            if matches!(e, PlayEvent::Counted { .. }) && self.panels.library.browser.in_history() {
                self.lib.store.send(StoreCmd::ListSessions);
            }
        }
    }

    /// Suggestions after the source's reference: a deck's track at its actual tempo
    /// and key (in Prepare, the player's while it plays), or the focused list's
    /// selected track.
    fn refresh_suggestions(&mut self) {
        let decks = self.decks_now();
        let (lib, flow) = (&mut self.panels.library, &self.panels.flow);
        // The suggestions' own selection isn't a reference for themselves.
        let selected = match lib.focused_pane {
            Pane::Suggest => lib.browser.focused(),
            _ => lib.focused_track(flow),
        };
        let view = CoreView {
            lib: &self.index,
            history: &self.history,
            collections: &self.collections,
            open_flow: flow.flow,
            decks: self.prefs.decks().len(),
            drives: &self.drives,
            uses: &self.uses,
            loaded: &[],
        };
        lib.suggest.refresh(&view, &decks, selected);
    }

    /// Draw the window, collecting what it asks for.
    fn draw(&mut self, ctx: &egui::Context) -> Vec<Command> {
        let jobs = self.jobs.list();
        if let Some(next) = self.notices.expire(Instant::now()) {
            ctx.request_repaint_after(next.saturating_duration_since(Instant::now()));
        }
        let mut out: Vec<Command> = vec![];
        // What's on the decks shown.
        let players = self.prefs.decks();
        let loaded: Vec<(i64, DeckId)> = {
            let p = self.player.lock().unwrap();
            players
                .into_iter()
                .filter_map(|d| p.session(d).track_id.map(|t| (t, d)))
                .collect()
        };
        if self.prefs.mode == Mode::Debug {
            // What an export says about this run, as it is now.
            self.panels.debug.info = format!(
                "earshot {} (debug build)\naudio: {:#?}\nsettings: {:#?}\ndecks shown: {}\n",
                env!("CARGO_PKG_VERSION"),
                self.audio,
                self.settings,
                self.prefs.decks().len(),
            );
        }
        let mut cx = Cx {
            core: CoreView {
                lib: &self.index,
                history: &self.history,
                collections: &self.collections,
                open_flow: self.panels.flow.flow,
                decks: self.prefs.decks().len(),
                drives: &self.drives,
                uses: &self.uses,
                loaded: &loaded,
            },
            engine: &self.frame,
            theme: &THEME,
            prefs: &self.prefs,
            shell: self.shell,
            out: &mut out,
        };
        egui::TopBottomPanel::top("top")
            .frame(THEME.bar_frame())
            .show(ctx, |ui| {
                if self
                    .audio_note
                    .as_ref()
                    .is_some_and(|(_, until)| until.is_some_and(|u| Instant::now() > u))
                {
                    self.audio_note = None;
                }
                TopBar {
                    audio: &self.audio,
                    audio_name: self.audio_name.as_deref(),
                    alerts: &self.hud.alerts,
                    audio_note: self.audio_note.as_ref().map(|(n, _)| n.as_str()),
                    controller_busy: self
                        .controller
                        .found
                        .as_ref()
                        .and_then(|c| Some((c.id.as_str(), self.controller.error.as_deref()?))),
                    midi_devices: &self.midi_devices,
                    mappings: &self.mappings,
                    analysis: &self.analysis,
                    jobs: &jobs,
                    hud: Meter {
                        load: self.hud.load,
                        detail: [&self.hud.text, &self.hud.audio],
                        memory: crate::core::log::memory(),
                    },
                    undo: self.edits.next_undo().map(|e| e.label()),
                    redo: self.edits.next_redo().map(|e| e.label()),
                }
                .ui(ui, &mut cx)
            });
        egui::TopBottomPanel::bottom("status")
            .frame(THEME.bar_frame())
            .show(ctx, |ui| {
                StatusLine {
                    jobs: &jobs,
                    notices: self.notices.shown(),
                }
                .ui(ui, &mut cx)
            });
        // The bars have taken their room: divide the rest, then draw it as divided.
        let shell_rect = ctx.available_rect().intersect(ctx.content_rect());
        cx.shell = views::shell(ctx, &self.panels, &cx);
        let shell = cx.shell;
        views::show(ctx, &mut self.panels, &mut cx);
        drag::paint_ghost(ctx, cx.core.lib);

        let routing = player::routing(&self.player, self.audio.as_ref().map_or(2, |i| i.channels));
        let env = ModalEnv {
            audio_settings: &self.settings.audio,
            audio: &self.audio,
            controller: self.controller.found.as_ref(),
            routing,
            hotcue_mode: self.settings.hotcue_mode,
            xf_curve: self.settings.xf_curve,
            cue_first_beat: self.settings.cue_first_beat,
            load_while_playing: self.settings.load_while_playing,
            auto_hide_mixer: self.prefs.auto_hide_mixer,
            ui_scale: self.prefs.ui_scale,
            stem_mode: self.prefs.stem_mode,
            keymap: &self.keymap,
            mode: self.prefs.mode,
            analysis: AnalysisEnv {
                quick_new: self.settings.quick_analyse_new,
                workers: self.background_workers,
                cores: analysis::cores(),
                stale: &self.stale,
                stems_cache_gb: self.settings.stems_cache_gb,
                stems_used: crate::core::stems::usage(),
            },
            keymap_file: KeymapEnv {
                path: &self.keymap_path,
                origin: &self.keymap_origin,
            },
            sampler: crate::ui::modals::sampler::SamplerEnv {
                bank: &self.settings.sampler,
                slots: self.frame.snap.as_ref().map(|s| &s.sampler),
                errors: &self.sample_errors,
                selected: (self.panels.library.focused_track(&self.panels.flow)).is_some(),
            },
        };
        self.modals.show(ctx, &env, cx.out);
        self.shell = shell;
        self.shell_rect = shell_rect;
        out
    }
}

impl EarshotApp {
    /// Tell the controller and its lights which deck each hand plays (they read it on
    /// their own threads).
    fn sync_layers(&self) {
        self.midi_route
            .layers
            .set(self.prefs.layers, self.prefs.four_decks());
    }

    /// Switch the left (`side` 0) or right (1) hand to its other deck (1 and 3, 2 and
    /// 4), with four decks showing. A controller's deck follows: its pads keep the
    /// mode they're in, and its knobs and faders wait to be moved to the new deck's
    /// values (marked on screen where they are now).
    fn switch_layer(&mut self, side: usize) {
        if side > 1 || !self.prefs.four_decks() {
            return;
        }
        let from = self.prefs.active_deck(side);
        self.prefs.layers[side] = !self.prefs.layers[side];
        self.sync_layers();
        let to = self.prefs.active_deck(side);
        if !self.prefs.controller() {
            return;
        }
        // The controller's pads are in the mode they were: the new deck's follow.
        let mode = self.panels.decks[from.index()].pad_mode;
        if self.panels.decks[to.index()].pad_mode != mode {
            self.view_cmd(ViewCmd::PadMode(to, mode));
        }
        use crate::ui::panels::mixer;
        for control in mixer::takeover_controls(from).chain(mixer::takeover_controls(to)) {
            mixer::set_waiting(&self.ctx, control, None);
        }
        let snap = self.frame.snap.as_ref();
        for (control, at) in self.midi_route.waiting(|c| snap.and_then(|s| c.get(s))) {
            if matches!(control, crate::core::bus::Control::Deck(d, _) if d == to) {
                mixer::set_waiting(&self.ctx, control, Some(at));
            }
        }
    }
}

#[cfg(test)]
impl EarshotApp {
    /// Where the views end and the status bar begins, down the window, in points.
    pub(super) fn shell_bottom(&self) -> f32 {
        self.shell_rect.bottom()
    }
}

impl eframe::App for EarshotApp {
    /// Audio closed, then the controller (`shut_down`).
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.shut_down();
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.prefs.tables = self.panels.library.tables.clone();
        eframe::set_value(storage, prefs::KEY, &self.prefs);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events(ctx);
        self.read_engine(ctx);
        self.sync_tempo_lock();
        self.sync_trim();
        self.sync_phones_cue_only();
        self.send_active_loops();
        // Background analysis holds back while a deck plays, in every mode.
        let playing = self
            .frame
            .snap
            .as_ref()
            .is_some_and(|sh| sh.live().iter().any(|d| d.playing));
        self.lib.analyser.set_playing(playing);
        self.confirm_quit(ctx);
        self.keyboard(ctx);
        input::full_screen_key(ctx);
        self.window.fit(ctx, self.prefs.ui_scale);
        self.sync_layers();
        // Ctrl +/- and Ctrl 0 change the UI size (egui's shortcuts, our steps).
        if let Some(z) = self.ui_size_keys(ctx) {
            self.dispatch(ViewCmd::UiScale(z).into());
        }
        self.tick_hud(ctx);
        self.track_plays();
        self.refresh_suggestions();
        self.save_tempo_ranges();
        self.save_deck_options();

        for c in self.draw(ctx) {
            self.dispatch(c);
        }
        // Save first, so opening another Flow doesn't lose the last change.
        self.save_flow();
        for id in take_wanted_art(ctx) {
            let media = &mut self.panels.library.media;
            if !media.art.contains_key(&id) {
                media.art.insert(id, None);
                self.art_loader.send(id);
            }
        }
        self.save_flow();
        self.save_flow_view(ctx);
        self.schedule_repaint(ctx);
    }
}

/// Two presses of a load button this close together are a double tap.
const DOUBLE_TAP: Duration = Duration::from_millis(400);

/// A load button pressed at `now`, last pressed at `last`: true if that makes a double
/// tap. A double tap clears `last`, so a third press starts again.
fn double_tap(last: &mut Option<Instant>, now: Instant) -> bool {
    let double = last.is_some_and(|t| now.saturating_duration_since(t) < DOUBLE_TAP);
    *last = (!double).then_some(now);
    double
}

/// `then` (before `now`) in unix seconds.
fn unix_seconds_at(now: Instant, then: Instant) -> i64 {
    let at = std::time::SystemTime::now() - now.saturating_duration_since(then);
    at.duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// The UI's name for a key the keymap file names (`;` → `Semicolon`).
fn spell_key(name: &str) -> Option<String> {
    Key::from_name(name).map(|k| k.name().to_string())
}

/// Store events, delivered through the app's channel like everything else.
fn store_sink(sink: &Sink) -> EventSink {
    let sink = sink.clone();
    Arc::new(move |e| sink(Event::Store(e)))
}

mod edits;
mod events;
mod export_ops;
mod external_ops;
#[cfg(test)]
mod flow_tests;
mod frame;
mod input;
mod library_ops;
mod preview;
mod sampler;
mod tempo_lock;
#[cfg(test)]
mod tests;
mod trim;
