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
use super::panels::library::{LibraryPane, LoadedFrom, drag};
use super::panels::status::{AnalysisSummary, Meter, StatusLine, TopBar};
use super::panels::{CoreView, Cx, EngineFrame, Panel, PreviewState};
use super::prefs::{self, UiPrefs};
use super::theme::{self, THEME};
use super::views::{self, Panels};
use crate::core::analysis::{self, Analyser, Mode as AnalysisMode};
use crate::core::audio_watch;
use crate::core::collections::{
    CollectionCmd, CollectionId, CollectionKind, Collections, FlowCmd, FlowGraph, Section,
};
use crate::core::command::{self, AppCmd, AudioCmd, Command, DeckCmd, PreviewCmd, ViewCmd};
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
    // ---- services ----------------------------------------------------------------------
    player: Player,
    lib: Arc<Library>,
    sink: Sink,
    rx: Receiver<Event>,
    monitor: Option<Monitor>,
    audio: Result<OutputInfo, String>,
    /// Audio stalls (`core::audio_watch`): when each reopened the device, the last
    /// minute's, and what to show about them (until when; or for good, once reopening
    /// stopped helping).
    audio_stalls: VecDeque<Instant>,
    audio_note: Option<(String, Option<Instant>)>,
    /// Messages for the user, in the bottom bar.
    notices: Notices,
    /// "Do you really want to quit?" is showing; and the user said yes.
    asking_quit: bool,
    quitting: bool,
    /// Each player's track and the list it was loaded from, by `DeckId::index()`.
    loaded_from: [Option<(i64, LoadedFrom)>; session::SLOTS],
    /// Always the null output (tests).
    null_audio: bool,
    midi: Midi,
    midi_ports: Vec<String>,
    /// Send track ids here to load their cover art.
    art_loader: std::sync::mpsc::Sender<i64>,
    /// Nudge after a scan: works out audio hashes the files' tags didn't have.
    hasher: std::sync::mpsc::Sender<()>,
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
    /// Keep repainting until then (see [`SETTLE`]).
    settle_until: Option<Instant>,
    /// The engine has the players' saved keylock and quantise (see
    /// [`EarshotApp::save_deck_options`]).
    deck_options_restored: bool,
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

        {
            let sink = sink.clone();
            audio_watch::start(player.clone(), move |quiet, decks| {
                sink(Event::Command(Command::Audio(AudioCmd::Stalled {
                    quiet,
                    decks,
                })))
            });
        }
        {
            let sink = sink.clone();
            crate::core::drives::watch(move |d| sink(Event::Drives(d)));
        }
        let analyser =
            Analyser::start(sink.clone(), player.clone(), store.clone(), services.worker);
        let hasher = library::start_hasher(store.clone());
        let data_dir = services.db.parent().unwrap_or(std::path::Path::new("."));
        let keymap_path = data_dir.join(keymap::FILE_NAME);
        let (keymap, keymap_origin) = keymap::load_user(&keymap_path, spell_key);
        let background_workers = analyser.background_workers();
        let tempo_lock = {
            let sink = sink.clone();
            let cores = analysis::cores();
            TempoLock::with_worker(
                (services.worker)(Role::Background, cores),
                store.clone(),
                Arc::new(move |e| sink(Event::TempoLock(e))),
                data_dir,
                Some(hasher.clone()),
            )
        };
        crate::core::stems::init(data_dir, (settings.stems_cache_gb.max(0.0) * 1e9) as u64);
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
        prefs.ui_scale = prefs::text_size(prefs.ui_scale);
        // View > Text size changes the text, not everything: egui's own zoom stays at 1
        // (Ctrl +/- are ours, below).
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        cc.egui_ctx.set_zoom_factor(1.0);
        theme::set_text_scale(&cc.egui_ctx, prefs.ui_scale);
        let tables = prefs.tables.clone();
        let mut app = EarshotApp {
            ctx: cc.egui_ctx.clone(),
            player,
            lib,
            sink,
            rx,
            monitor,
            audio,
            audio_stalls: VecDeque::new(),
            audio_note: None,
            loaded_from: Default::default(),
            notices: Notices::default(),
            asking_quit: false,
            quitting: false,
            null_audio: services.null_audio,
            midi: Midi::default(),
            midi_ports: vec![],
            art_loader,
            hasher,
            tempo_lock,
            listener: Default::default(),
            stems,
            index: LibraryIndex::default(),
            collections: Collections::default(),
            history: History::default(),
            edits: edit::History::default(),
            sources: vec![],
            drives: vec![],
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
        app.lib.store.send(StoreCmd::ListSources);
        app.lib.store.send(StoreCmd::StartSession);
        let ranges = app.settings.tempo_ranges.clone();
        for (deck, range) in DeckId::first(DECKS).zip(ranges) {
            app.dispatch((deck, EngineCmd::TempoRange(range)).into());
        }
        let _ = player::set_hotcue_mode(&app.player, app.settings.hotcue_mode);
        let _ = player::set_fx_decks(&app.player, app.prefs.decks().len());
        player::set_cue_first_beat(&app.player, app.settings.cue_first_beat);
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
        self.prefs.set_controller(!self.midi_ports.is_empty());
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

    /// Switch mode. Not to Prepare while a deck plays (the set would go on under
    /// Prepare's player): says so instead, and returns false. To Perform, Prepare's
    /// player stops.
    pub(super) fn set_mode(&mut self, m: Mode) -> bool {
        if m == self.prefs.mode {
            return true;
        }
        match m {
            Mode::Prepare if self.decks_playing() > 0 => {
                let text = "Stop the playing decks before switching to Prepare.";
                self.notify(notice::Level::Warning, text);
                return false;
            }
            Mode::Perform
                if self
                    .frame
                    .snap
                    .as_ref()
                    .is_some_and(|sh| sh.prepare.playing) =>
            {
                self.dispatch((DeckId::PREPARE, EngineCmd::Play(Set::Off)).into());
            }
            _ => {}
        }
        self.prefs.mode = m;
        true
    }

    /// Closing the window in Perform while a deck plays: ask first ("Quit" closes it
    /// again, for good).
    fn confirm_quit(&mut self, ctx: &egui::Context) {
        let asked = ctx.input(|i| i.viewport().close_requested());
        if asked && !self.quitting && self.prefs.mode == Mode::Perform && self.decks_playing() > 0 {
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
            Command::App(AppCmd::CueFirstBeat(on)) => {
                self.settings.cue_first_beat = on;
                player::set_cue_first_beat(&self.player, on);
                self.lib
                    .store
                    .send(settings::save(settings::CUE_FIRST_BEAT, &on));
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
                std::thread::spawn(move || {
                    let result = if queued {
                        lib.analyser.queue_stale()
                    } else {
                        lib.analyser.stale_tracks().map(|t| t.len())
                    };
                    sink(Event::Stale { queued, result });
                });
            }
            Command::App(AppCmd::CopyKeymap) => {
                // Never over the user's own file.
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
            Command::Deck(deck, DeckCmd::Load(id)) => self.load(Some(deck), id),
            Command::Library(c) => self.library_cmd(c),
            Command::External(c) => self.external_cmd(c),
            Command::Export(c) => self.export_cmd(c),
            Command::TempoLock(TempoLockCmd::Listen(c)) => self.listen(c),
            Command::TempoLock(c) => self.tempo_lock.run(c),
            Command::Stems(c) => self.stems.run(c),
            Command::Collection(c) => self.collection_cmd(c),
            Command::Flow(c) => self.flow_cmd(c),
            Command::Edit(c) => self.edit_cmd(c),
            Command::Undo => self.undo(),
            Command::Redo => self.redo(),
            Command::Preview(PreviewCmd::Toggle(id)) => self.toggle_preview(id),
            Command::Preview(PreviewCmd::Seek(id, at)) => self.seek_preview(id, at),
            Command::Preview(PreviewCmd::Stop(id)) => self.stop_preview(id),
            Command::Audio(AudioCmd::Restart(settings)) => {
                self.audio_stalls.clear();
                self.audio_note = None;
                self.settings.audio = settings;
                self.save_audio_settings();
                self.preview = None;
                self.audio = start_audio(&self.player, &self.settings.audio, self.null_audio);
                self.monitor = self.player.lock().unwrap().monitor();
                self.frame.snap = self.monitor.as_ref().map(|m| m.snapshot());
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
        self.audio = start_audio(&self.player, &self.settings.audio, self.null_audio);
        self.monitor = self.player.lock().unwrap().monitor();
        self.frame.snap = self.monitor.as_ref().map(|m| m.snapshot());
        self.audio_note = Some((
            format!("audio stopped for {ms} ms; restarted it"),
            Some(now + Duration::from_secs(30)),
        ));
    }

    /// What's on screen: the mode, the layout, focus, windows and zoom.
    fn view_cmd(&mut self, cmd: ViewCmd) {
        let (lib, flow) = (&mut self.panels.library, &self.panels.flow);
        match cmd {
            ViewCmd::Browse(n) => lib.browse(n),
            ViewCmd::TogglePane => {
                lib.focused_pane = match lib.focused_pane {
                    Pane::Library | Pane::Flow => Pane::Suggest,
                    Pane::Suggest => Pane::Library,
                };
            }
            ViewCmd::FocusPane(p) => lib.focused_pane = p,
            ViewCmd::FocusSearch => lib.browser.search_focus = true,
            ViewCmd::LoadSelected(deck) => {
                if let Some(id) = lib.focused_track(flow) {
                    self.load(Some(deck), id);
                }
            }
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
            ViewCmd::FourDecks(on) => {
                self.prefs.four_decks = on;
                if let Err(e) = player::set_fx_decks(&self.player, self.prefs.decks().len()) {
                    eprintln!("effect decks: {e}");
                }
            }
            ViewCmd::SetMode(m) => {
                self.set_mode(m);
            }
            ViewCmd::Source(s) => self.prefs.layout_mut().source = s,
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
            ViewCmd::ShowFx(show) => self.prefs.show_fx = show,
            ViewCmd::AutoHideMixer(on) => self.prefs.auto_hide_mixer = on,
            ViewCmd::StemMode(m) => self.prefs.stem_mode = m,
            ViewCmd::UiScale(z) => {
                self.prefs.ui_scale = z;
                theme::set_text_scale(&self.ctx, z);
            }
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
        let prepare = (self.prefs.mode == Mode::Prepare).then(|| self.deck_now(DeckId::PREPARE));
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
        lib.suggest
            .refresh(&view, &decks, prepare.as_ref(), selected);
    }

    /// Draw the window, collecting what it asks for.
    fn draw(&mut self, ctx: &egui::Context) -> Vec<Command> {
        let jobs = self.jobs.list();
        if let Some(next) = self.notices.expire(Instant::now()) {
            ctx.request_repaint_after(next.saturating_duration_since(Instant::now()));
        }
        let mut out: Vec<Command> = vec![];
        // What's on the decks shown, and on Prepare's player in Prepare.
        let mut players = self.prefs.decks();
        if self.prefs.mode == Mode::Prepare {
            players = vec![DeckId::PREPARE];
        }
        let loaded: Vec<(i64, DeckId)> = {
            let p = self.player.lock().unwrap();
            players
                .into_iter()
                .filter_map(|d| p.session(d).track_id.map(|t| (t, d)))
                .collect()
        };
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
                    audio_note: self.audio_note.as_ref().map(|(n, _)| n.as_str()),
                    midi_ports: &self.midi_ports,
                    analysis: &self.analysis,
                    jobs: &jobs,
                    hud: Meter {
                        load: self.hud.load,
                        detail: [&self.hud.text, &self.hud.audio],
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
        views::show(ctx, &mut self.panels, &mut cx);
        drag::paint_ghost(ctx, cx.core.lib);

        let routing = player::routing(&self.player, self.audio.as_ref().map_or(2, |i| i.channels));
        let env = ModalEnv {
            audio_settings: &self.settings.audio,
            audio: &self.audio,
            routing,
            hotcue_mode: self.settings.hotcue_mode,
            cue_first_beat: self.settings.cue_first_beat,
            auto_hide_mixer: self.prefs.auto_hide_mixer,
            stem_mode: self.prefs.stem_mode,
            keymap: &self.keymap,
            mode: self.prefs.mode,
            analysis: AnalysisEnv {
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
        };
        self.modals.show(ctx, &env, cx.out);
        out
    }
}

impl eframe::App for EarshotApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.prefs.tables = self.panels.library.tables.clone();
        eframe::set_value(storage, prefs::KEY, &self.prefs);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events(ctx);
        self.read_engine(ctx);
        self.sync_tempo_lock();
        // Background analysis holds back while a deck plays in Perform mode. In Prepare,
        // listening while an import analyses is normal, so it runs at full width.
        let playing = self.prefs.mode == Mode::Perform
            && self
                .frame
                .snap
                .as_ref()
                .is_some_and(|sh| sh.live().iter().any(|d| d.playing));
        self.lib.analyser.set_playing(playing);
        self.confirm_quit(ctx);
        self.keyboard(ctx);
        input::full_screen_key(ctx);
        // Ctrl +/- and Ctrl 0 change the text size too (egui's shortcuts, our steps).
        if let Some(z) = self.text_size_keys(ctx) {
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
                let _ = self.art_loader.send(id);
            }
        }
        self.save_flow();
        self.save_flow_view(ctx);
        self.schedule_repaint(ctx);
    }
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
mod tempo_lock;
#[cfg(test)]
mod tests;
