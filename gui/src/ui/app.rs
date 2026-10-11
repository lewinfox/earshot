//! The window: top bar, the stacked waveforms, the decks and mixer, and the library.
//! Repaints every frame (vsync), reading the engine's state directly.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use earshot_engine::{OutputInfo, OutputOptions, Shared};
use egui::{Key, RichText, Stroke, Ui, vec2};

use super::audio_ui::{AudioAction, AudioUi};
use super::deck::{Action, DeckInfo, DeckUi, PendingLoad};
use super::flow::FlowUi;
use super::library_ui::{LibAction, LibraryUi};
use super::mixer;
use super::suggest_ui::{RefInfo, SuggestUi};
use super::util;
use crate::core::analysis::{Analyser, Mode};
use crate::core::library::{self, Library};
use crate::core::midi::{self, Midi};
use crate::core::player::{self, AudioSettings, Player};
use crate::core::suggest::{self, Reference};
use crate::core::{Event, Sink, controls, db};

pub struct EarshotApp {
    player: Player,
    lib: Arc<Library>,
    sink: Sink,
    rx: Receiver<Event>,
    shared: Option<Arc<Shared>>,
    audio: Result<OutputInfo, String>,
    _midi: Midi,
    midi_ports: Vec<String>,
    midi_log: VecDeque<String>,
    decks: [DeckUi; 2],
    /// Real seconds of waveform either side of the playhead, shared by both decks.
    half_window: f64,
    library: LibraryUi,
    suggest: SuggestUi,
    flow: FlowUi,
    /// Which list the keyboard and the controller's browse knob move through.
    pane: Pane,
    /// The deck that most recently started playing (the live reference).
    last_started: Option<usize>,
    /// When each deck started playing, to count a track as played after 30 s.
    play_started: [Option<Instant>; 2],
    analysis: AnalysisSummary,
    hud: Hud,
    show_keys: bool,
    audio_settings: AudioSettings,
    audio_ui: AudioUi,
    /// The track asked to play in the headphones, and whether it's still decoding.
    preview: Option<(i64, Arc<AtomicBool>)>,
    /// Keys held down for deck buttons, so release sends 0.
    held_keys: HashSet<Key>,
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

/// A track counts as played once it has been playing this long.
const PLAYED_AFTER_SECS: f32 = 30.0;

#[derive(Default)]
struct AnalysisSummary {
    busy: HashSet<i64>,
    running: HashMap<i64, f64>,
    failed: u32,
    last_error: String,
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
        let shared = player.lock().unwrap().shared();

        let db = db::open(&data_dir().join("earshot.db"))?;
        let analyser = Analyser::start(sink.clone(), player.clone(), db.clone());
        let lib = Arc::new(Library { db, analyser });
        let mut app = EarshotApp {
            player,
            lib,
            sink,
            rx,
            shared,
            audio,
            _midi: Midi::default(),
            midi_ports: vec![],
            midi_log: VecDeque::new(),
            decks: [DeckUi::new(0), DeckUi::new(1)],
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
            pane: Pane::Library,
            last_started: None,
            play_started: [None; 2],
            analysis: AnalysisSummary::default(),
            hud: Hud::default(),
            show_keys: false,
            audio_settings,
            audio_ui: AudioUi::default(),
            preview: None,
            held_keys: HashSet::new(),
            picked: Arc::new(Mutex::new(None)),
        };
        app.library.folders = db::folders(&app.lib.db.lock().unwrap())?;
        app.rescan();
        app.connect_midi();
        Ok(app)
    }

    /// (Re)open every MIDI input, e.g. after plugging the controller in.
    fn connect_midi(&mut self) {
        // Close the old ports first so they can be reopened.
        self._midi = Midi::default();
        (self._midi, self.midi_ports) =
            midi::connect_all(&self.sink, &self.player).unwrap_or_else(|e| {
                eprintln!("MIDI: {e}");
                (Midi::default(), vec![])
            });
    }

    fn rescan(&mut self) {
        self.library.scanning = true;
        let (db, sink) = (self.lib.db.clone(), self.sink.clone());
        std::thread::spawn(move || sink(Event::Scanned(library::scan_library(&db))));
    }

    fn load(&mut self, deck: Option<usize>, track_id: i64) {
        // Double-click: a deck that isn't playing, preferring A.
        let deck = deck.unwrap_or(if self.decks[0].playing() && !self.decks[1].playing() {
            1
        } else {
            0
        });
        // Show the track's details at once; the audio follows when it has decoded.
        self.decks[deck].start_load(PendingLoad::new(track_id, self.library.track(track_id)));
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

    fn control(&mut self, group: &str, key: &str, value: f64) {
        let mut inner = self.player.lock().unwrap();
        match controls::apply(&mut inner, group, key, value) {
            Ok(controls::Outcome::Ui) => {
                drop(inner);
                self.ui_control(group, key, value);
            }
            Ok(_) => {}
            Err(e) => eprintln!("control {group} {key}: {e}"),
        }
    }

    /// Controls the UI owns: library navigation and "load selected".
    fn ui_control(&mut self, group: &str, key: &str, value: f64) {
        match key {
            "MoveVertical" => match self.pane {
                Pane::Library => self.library.move_focus(value.round() as i32),
                Pane::Suggest => self.suggest.move_focus(value.round() as i32),
            },
            "TogglePane" if value > 0.0 => {
                self.pane = match self.pane {
                    Pane::Library => Pane::Suggest,
                    Pane::Suggest => Pane::Library,
                };
            }
            "LoadSelectedTrack" if value > 0.0 => {
                let id = match self.pane {
                    Pane::Library => self.library.focused(),
                    Pane::Suggest => self.suggest.focused(),
                };
                if let Some(id) = id {
                    self.load(Some(if group == "[Channel2]" { 1 } else { 0 }), id);
                }
            }
            _ => {}
        }
    }

    /// Note which deck started playing last, and mark tracks played after 30 s.
    fn track_plays(&mut self) {
        let Some(sh) = &self.shared else { return };
        let now = Instant::now();
        for d in 0..2 {
            let playing = sh.decks[d].playing.load(Relaxed);
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

    /// The live deck (most recently started, still playing) at its actual tempo and
    /// pitch; otherwise the track selected in the library.
    fn refresh_suggestions(&mut self) {
        let live = self.shared.as_ref().and_then(|sh| {
            let inner = self.player.lock().unwrap();
            [self.last_started, Some(0), Some(1)]
                .into_iter()
                .flatten()
                .find(|&d| sh.decks[d].playing.load(Relaxed) && inner.decks[d].track_id.is_some())
                .map(|d| {
                    let bpm = sh.decks[d].bpm.get();
                    let speed = sh.decks[d].tempo.get();
                    (
                        d,
                        inner.decks[d].track_id.unwrap(),
                        bpm.is_finite().then_some(bpm),
                        speed,
                    )
                })
        });
        let (id, deck, bpm, speed) = match live {
            Some((d, id, bpm, speed)) => (id, Some(d), bpm, speed),
            None => match self.library.focused() {
                Some(id) => (id, None, None, 1.0),
                None => {
                    self.suggest.reference = None;
                    self.suggest.set(vec![]);
                    self.suggest.computed_for.clear();
                    return;
                }
            },
        };
        let Some(t) = self.library.track(id) else {
            return;
        };
        let bpm = bpm.or(t.bpm());
        let key = suggest::effective_key(t.key, speed);
        let stamp = format!(
            "{id}|{deck:?}|{:.1}|{key:?}|{:?}|{}|{}",
            bpm.unwrap_or(0.0),
            self.suggest.direction,
            self.library.played.len(),
            self.library.version,
        );
        if stamp == self.suggest.computed_for {
            return;
        }
        let candidates = self.library.candidates();
        let reference = Reference {
            id,
            bpm,
            key,
            energy: t.energy,
            direction: self.suggest.direction,
            played: &self.library.played,
        };
        let list = suggest::suggest(&reference, &candidates, 80);
        self.suggest.reference = Some(RefInfo {
            title: t.lib.title.clone(),
            bpm,
            key,
            deck,
        });
        self.suggest.set(list);
        self.suggest.computed_for = stamp;
    }

    /// Play a track in the headphones, or stop it if it's the one already playing.
    fn toggle_preview(&mut self, id: i64) {
        if self.preview_state().is_some_and(|(p, _)| p == id) {
            self.preview = None;
            if let Err(e) = player::stop_preview(&self.player) {
                eprintln!("preview: {e}");
            }
            return;
        }
        let decoding = Arc::new(AtomicBool::new(true));
        self.preview = Some((id, decoding.clone()));
        let (player, db) = (self.player.clone(), self.lib.db.clone());
        std::thread::spawn(move || {
            if let Err(e) = player::preview(&player, &db, id) {
                eprintln!("preview: {e}");
            }
            decoding.store(false, Relaxed);
        });
    }

    /// The track previewing (or about to) and how far through it is, 0..1.
    fn preview_state(&self) -> Option<(i64, f32)> {
        let (id, decoding) = self.preview.as_ref()?;
        if decoding.load(Relaxed) {
            return Some((*id, 0.0));
        }
        let sh = self.shared.as_ref()?;
        if !sh.preview_playing.load(Relaxed) {
            return None;
        }
        let d = sh.preview_duration.get();
        let f = if d > 0.0 {
            sh.preview_position.get() / d
        } else {
            0.0
        };
        Some((*id, f as f32))
    }

    fn apply_audio(&mut self, a: AudioAction) {
        match a {
            AudioAction::Route(r) => {
                self.audio_settings.routing = Some(r);
                if let Err(e) = player::set_routing(&self.player, r) {
                    eprintln!("routing: {e}");
                }
            }
            AudioAction::Restart(settings) => {
                self.audio_settings = settings;
                self.preview = None;
                self.audio = start_audio(&self.player, &self.audio_settings);
                self.shared = self.player.lock().unwrap().shared();
            }
        }
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

    fn refresh_previews(&mut self, ids: Option<&[i64]>) {
        if let Ok(p) = db::previews(&self.lib.db.lock().unwrap(), ids) {
            for (id, bytes) in p {
                self.library.invalidate_thumb(id);
                self.library.previews.insert(id, bytes);
            }
        }
    }

    // ---- events from other threads ------------------------------------------------------

    fn drain_events(&mut self) {
        while let Ok(e) = self.rx.try_recv() {
            match e {
                Event::Scanned(Ok(tracks)) => {
                    self.library.scanning = false;
                    self.library.set_tracks(tracks);
                    self.refresh_previews(None);
                }
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
                    self.decks[deck].finish_load(track_id);
                    match result {
                        Ok(()) => {
                            if !self.library.previews.contains_key(&track_id) {
                                self.refresh_previews(Some(&[track_id]));
                            }
                        }
                        Err(e) => eprintln!("load to deck {}: {e}", deck + 1),
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
                        t.error = ev.error.clone();
                        if ev.state == "done" {
                            t.analysed = true;
                            t.lib.bpm = ev.bpm.or(t.lib.bpm);
                            t.drifts = ev.drifts.unwrap_or(false);
                            t.dynamic = ev.grid_source.as_deref()
                                == Some(crate::core::analysis::DYNAMIC_SOURCE);
                        }
                    }
                    if ev.state == "done" {
                        self.library.version += 1;
                        self.refresh_previews(Some(&[ev.track_id]));
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
                Event::UiControl { group, key, value } => self.ui_control(&group, &key, value),
            }
        }
        let picked = self.picked.lock().unwrap().take();
        if let Some(folders) = picked {
            for f in folders {
                match library::add_folder(&self.lib.db, &f.to_string_lossy()) {
                    Ok(list) => self.library.folders = list,
                    Err(e) => eprintln!("import: {e}"),
                }
            }
            self.rescan();
        }
    }

    // ---- keyboard ---------------------------------------------------------------------------

    fn keyboard(&mut self, ctx: &egui::Context) {
        let typing = ctx.wants_keyboard_input();
        let events = ctx.input(|i| i.events.clone());
        for e in events {
            let egui::Event::Key {
                key,
                physical_key,
                pressed,
                repeat,
                modifiers,
            } = e
            else {
                continue;
            };
            let key = physical_key.unwrap_or(key);
            if key == Key::Escape {
                ctx.memory_mut(|m| m.stop_text_input());
                continue;
            }
            if typing || modifiers.command || modifiers.alt {
                continue;
            }
            if pressed && key == Key::Slash {
                self.library.search_focus = true;
                continue;
            }
            match (key, modifiers.shift) {
                (Key::Tab, _) if pressed => self.control("[Library]", "TogglePane", 1.0),
                (Key::ArrowUp, _) if pressed => self.control("[Library]", "MoveVertical", -1.0),
                (Key::ArrowDown, _) if pressed => self.control("[Library]", "MoveVertical", 1.0),
                (Key::ArrowLeft, true) if pressed => {
                    self.control("[Channel1]", "LoadSelectedTrack", 1.0)
                }
                (Key::ArrowRight, true) if pressed => {
                    self.control("[Channel2]", "LoadSelectedTrack", 1.0)
                }
                _ => {}
            }
            let Some((group, control, toggle)) = deck_key(key) else {
                continue;
            };
            if pressed && repeat {
                continue;
            }
            if !pressed && !self.held_keys.remove(&key) {
                continue;
            }
            if toggle {
                if pressed {
                    let on = controls::current(&self.player.lock().unwrap(), group, control)
                        .unwrap_or(false);
                    self.control(group, control, if on { 0.0 } else { 1.0 });
                }
                continue;
            }
            if pressed {
                self.held_keys.insert(key);
            }
            // Shift + a hot cue key clears it.
            if modifiers.shift && control.starts_with("hotcue_") {
                if pressed {
                    self.control(group, &control.replace("_activate", "_clear"), 1.0);
                }
                continue;
            }
            self.control(group, control, pressed as u8 as f64);
        }
    }

    // ---- panels -------------------------------------------------------------------------------

    fn top_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("earshot").strong().color(util::ACCENT));
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
        let waiting = a.busy.len() - a.running.len();
        let fail = if a.failed > 0 {
            format!(" · {} failed", a.failed)
        } else {
            String::new()
        };
        let text = if !a.running.is_empty() {
            let avg = a.running.values().sum::<f64>() / a.running.len() as f64;
            let what = if a.running.len() == 1 {
                a.running
                    .keys()
                    .next()
                    .and_then(|id| self.library.track(*id))
                    .map(|t| t.lib.title.clone())
                    .unwrap_or_default()
            } else {
                format!("{} tracks", a.running.len())
            };
            format!(
                "analysing {what} · {:.0}%{}{fail}",
                avg * 100.0,
                if waiting > 0 {
                    format!(" · {waiting} waiting")
                } else {
                    String::new()
                }
            )
        } else if waiting > 0 {
            format!("analysis · {waiting} waiting{fail}")
        } else if a.failed > 0 {
            format!("analysis: {} failed", a.failed)
        } else {
            return;
        };
        let r = pill(
            ui,
            &text,
            if a.failed > 0 && a.running.is_empty() {
                util::WARN
            } else {
                util::WARN
            },
        );
        let r = r.on_hover_text(if a.last_error.is_empty() {
            "Click to dismiss failures".to_string()
        } else {
            format!("Last error: {}\nClick to dismiss.", a.last_error)
        });
        if r.clicked() {
            self.analysis.failed = 0;
            self.analysis.last_error.clear();
        }
    }

    fn deck_info(&self, d: usize) -> DeckInfo {
        let inner = self.player.lock().unwrap();
        let s = &inner.decks[d];
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
        if let Some(s) = &self.shared {
            let block = s.last_block_frames.load(Relaxed) as f64;
            let rate = s.sample_rate.load(Relaxed) as f64;
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
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();
        self.keyboard(ctx);
        self.tick_hud(ctx);
        self.track_plays();
        self.refresh_suggestions();
        let infos = [self.deck_info(0), self.deck_info(1)];
        for (d, info) in infos.iter().enumerate() {
            let sh = self.shared.as_ref().map(|s| &s.decks[d]);
            self.decks[d].tick(ctx, sh, info);
        }
        let mut actions: Vec<Action> = vec![];
        let mut lib_actions: Vec<LibAction> = vec![];

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
                let h = (ui.available_height() - 3.0) / 2.0;
                ui.spacing_mut().item_spacing.y = 3.0;
                for d in 0..2 {
                    self.decks[d].show_wave(ui, h, &infos[d], &mut self.half_window, &mut actions);
                }
            });
        egui::TopBottomPanel::top("decks")
            .frame(
                egui::Frame::new()
                    .fill(util::BG)
                    .inner_margin(egui::Margin::symmetric(6, 4)),
            )
            .show_animated(ctx, !planning, |ui| {
                let mixer_w = mixer::WIDTH + 16.0;
                let height = mixer::HEIGHT + 12.0;
                let w = (ui.available_width() - mixer_w - 12.0) / 2.0;
                ui.horizontal_top(|ui| {
                    self.decks[0].show(ui, w, height, &infos[0], &mut actions);
                    ui.allocate_ui(vec2(mixer_w, 0.0), |ui| {
                        egui::Frame::new()
                            .fill(util::PANEL)
                            .stroke(Stroke::new(1.0, util::LINE))
                            .corner_radius(8)
                            .inner_margin(8)
                            .show(ui, |ui| {
                                ui.set_width(mixer_w - 16.0);
                                mixer::show(ui, self.shared.as_deref(), &mut actions);
                            });
                    });
                    self.decks[1].show(ui, w, height, &infos[1], &mut actions);
                });
            });
        egui::SidePanel::left("tree")
            .resizable(true)
            .default_width(250.0)
            .width_range(140.0..=600.0)
            .show_animated(ctx, !planning, |ui| {
                self.library.tree(ui, &mut lib_actions);
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
        if let Some(a) = self
            .audio_ui
            .show(ctx, &self.audio_settings, &self.audio, routing)
        {
            self.apply_audio(a);
        }

        if self.show_keys {
            egui::Window::new("Keyboard")
                .open(&mut self.show_keys)
                .resizable(false)
                .show(ctx, keys_help);
        }

        for a in actions {
            match a {
                Action::Control { group, key, value } => self.control(group, &key, value),
                Action::Grid { deck, op } => {
                    if let Err(e) = player::edit_grid(&self.player, &self.lib.db, deck, op) {
                        eprintln!("grid edit: {e}");
                    }
                }
                Action::Load { deck, track_id } => self.load(Some(deck), track_id),
            }
        }
        for a in lib_actions {
            match a {
                LibAction::Load { deck, track_id } => self.load(deck, track_id),
                LibAction::Analyse { ids, dynamic } => self.analyse(ids, dynamic),
                LibAction::Preview(id) => self.toggle_preview(id),
                LibAction::AddToFlow(ids) => {
                    self.flow.open = true;
                    for id in ids {
                        self.flow.add_root(id);
                    }
                }
                LibAction::RemoveFolder(path) => {
                    if let Ok(list) = library::remove_folder(&self.lib.db, &path) {
                        self.library.folders = list;
                        self.rescan();
                    }
                }
            }
        }
        // Waveforms move every frame.
        ctx.request_repaint();
    }
}

fn bar_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(util::PANEL)
        .inner_margin(egui::Margin::symmetric(10, 5))
}

fn pill(ui: &mut Ui, text: &str, border: egui::Color32) -> egui::Response {
    egui::Frame::new()
        .fill(util::PANEL_2)
        .stroke(Stroke::new(1.0, border))
        .corner_radius(10)
        .inner_margin(egui::Margin::symmetric(8, 2))
        .show(ui, |ui| ui.label(RichText::new(text).size(11.0)))
        .response
        .interact(egui::Sense::click())
}

/// Physical keys → deck controls: left hand plays deck A, right hand deck B.
/// (group, control, toggles on press)
fn deck_key(key: Key) -> Option<(&'static str, &'static str, bool)> {
    const A: &str = "[Channel1]";
    const B: &str = "[Channel2]";
    Some(match key {
        Key::A => (A, "cue_default", false),
        Key::S => (A, "play", true),
        Key::D => (A, "sync_enabled", true),
        Key::F => (A, "pfl", true),
        Key::Q => (A, "beatjump_backward", false),
        Key::W => (A, "beatjump_forward", false),
        Key::E => (A, "beatloop_activate", false),
        Key::R => (A, "reloop_toggle", false),
        Key::Z => (A, "rate_temp_down", false),
        Key::X => (A, "rate_temp_up", false),
        Key::Num1 => (A, "hotcue_1_activate", false),
        Key::Num2 => (A, "hotcue_2_activate", false),
        Key::Num3 => (A, "hotcue_3_activate", false),
        Key::Num4 => (A, "hotcue_4_activate", false),
        Key::J => (B, "cue_default", false),
        Key::K => (B, "play", true),
        Key::L => (B, "sync_enabled", true),
        Key::Semicolon => (B, "pfl", true),
        Key::U => (B, "beatjump_backward", false),
        Key::I => (B, "beatjump_forward", false),
        Key::O => (B, "beatloop_activate", false),
        Key::P => (B, "reloop_toggle", false),
        Key::M => (B, "rate_temp_down", false),
        Key::Comma => (B, "rate_temp_up", false),
        Key::Num7 => (B, "hotcue_1_activate", false),
        Key::Num8 => (B, "hotcue_2_activate", false),
        Key::Num9 => (B, "hotcue_3_activate", false),
        Key::Num0 => (B, "hotcue_4_activate", false),
        _ => return None,
    })
}

fn keys_help(ui: &mut Ui) {
    ui.label(RichText::new("Deck keys work unless a text field has focus. Esc leaves the field, / jumps to search. Shift + a hot cue key clears it.").color(util::MUTED));
    ui.add_space(6.0);
    ui.columns(3, |c| {
        let rows = |ui: &mut Ui, title: &str, colour: egui::Color32, keys: &[(&str, &str)]| {
            ui.label(RichText::new(title).strong().color(colour));
            egui::Grid::new(title).show(ui, |ui| {
                for (k, what) in keys {
                    ui.label(RichText::new(*k).monospace());
                    ui.label(*what);
                    ui.end_row();
                }
            });
        };
        let deck = |k: [&'static str; 14]| -> Vec<(&'static str, &'static str)> {
            let what = [
                "cue",
                "play / pause",
                "sync",
                "headphone cue",
                "beat jump back",
                "beat jump forward",
                "loop on/off",
                "reloop / exit",
                "nudge slower",
                "nudge faster",
                "hot cue 1",
                "hot cue 2",
                "hot cue 3",
                "hot cue 4",
            ];
            k.into_iter().zip(what).collect()
        };
        rows(
            &mut c[0],
            "Deck A",
            util::DECK[0],
            &deck([
                "A", "S", "D", "F", "Q", "W", "E", "R", "Z", "X", "1", "2", "3", "4",
            ]),
        );
        rows(
            &mut c[1],
            "Deck B",
            util::DECK[1],
            &deck([
                "J", "K", "L", ";", "U", "I", "O", "P", "M", ",", "7", "8", "9", "0",
            ]),
        );
        rows(
            &mut c[2],
            "Library",
            util::TEXT,
            &[
                ("Up / Down", "previous / next track"),
                ("Tab", "library / suggestions"),
                ("Shift Left", "load to deck A"),
                ("Shift Right", "load to deck B"),
                ("Ctrl A", "select all"),
                ("Ctrl - / = / 0", "UI size"),
            ],
        );
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
        for (deck, id) in ids.iter().enumerate() {
            h.state_mut().load(Some(deck), *id);
        }
        settle(&mut h, 120);
        h.state_mut().control("[Channel1]", "playposition", 0.3);
        h.state_mut().control("[Channel1]", "play", 1.0);
        h.state_mut().library.move_focus(3);
        settle(&mut h, 90);
        // How long the app takes to build a frame (layout + waveforms), both decks playing.
        h.state_mut().control("[Channel2]", "play", 1.0);
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
        h.run_steps(5);
        h.state_mut().flow.open_first_suggestions();
        h.run_steps(5);
        h.render().expect("render").save(&path).expect("save");
    }
}
