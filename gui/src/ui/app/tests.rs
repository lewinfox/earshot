//! The whole app, started on the null audio output, a database in a temporary
//! folder, a stand-in analysis worker and a few generated tracks. Each test drives
//! it through commands; with `EARSHOT_SHOT=/tmp/shot.png` set, each also renders the
//! window off-screen and writes it next to that path (`/tmp/shot-perform.png`, …).

use std::path::{Path, PathBuf};
use std::time::Duration;

use egui::vec2;

use super::*;
use crate::core::worker_client::WorkerConfig;
use crate::core::worker_client::testing::{stand_in, stand_in_old_grid};

pub(super) type H = egui_kittest::Harness<'static, EarshotApp>;

/// A folder of its own for one test: the database and the music.
pub(super) fn scratch(name: &str) -> PathBuf {
    let dir = scratch_dir(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("music")).unwrap();
    dir
}

/// Where [`scratch`] puts one test's folder.
pub(super) fn scratch_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("earshot-app-{name}-{}", std::process::id()))
}

/// Write `seconds` of a four-to-the-floor beat at `bpm`: a decaying low thump on
/// every beat and a short tick off the beat. 16-bit stereo, 44.1 kHz.
fn write_track(path: &Path, bpm: f64, seconds: f64) {
    let rate = 44_100u32;
    let n = (seconds * rate as f64) as usize;
    let beat = 60.0 / bpm;
    let mut pcm = Vec::with_capacity(n * 4);
    for i in 0..n {
        let t = i as f64 / rate as f64;
        let since = t % beat;
        let kick = (-since * 18.0).exp() * (std::f64::consts::TAU * 55.0 * since).sin();
        let off = (t + beat / 2.0) % beat;
        let tick = if off < 0.01 {
            (-off * 400.0).exp() * if i % 2 == 0 { 0.3 } else { -0.3 }
        } else {
            0.0
        };
        let s = ((kick * 0.7 + tick) * i16::MAX as f64) as i16;
        for _ in 0..2 {
            pcm.extend_from_slice(&s.to_le_bytes());
        }
    }
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&2u16.to_le_bytes()); // stereo
    wav.extend_from_slice(&rate.to_le_bytes());
    wav.extend_from_slice(&(rate * 4).to_le_bytes());
    wav.extend_from_slice(&4u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(&pcm);
    std::fs::write(path, wav).unwrap();
}

/// The fixture library: a few short tracks at different tempos.
const TRACKS: [(&str, f64); 4] = [
    ("Basement Hum", 122.0),
    ("Night Bus", 124.0),
    ("Glass Steps", 126.0),
    ("Low Orbit", 128.0),
];

/// Where `EARSHOT_SHOT` asks for this test's picture, if it does.
fn shot_path(test: &str) -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var_os("EARSHOT_SHOT")?);
    let stem = p.file_stem()?.to_string_lossy().into_owned();
    Some(p.with_file_name(format!("{stem}-{test}.png")))
}

/// Start the app on the fixture library, scan it and analyse every track (with the
/// stand-in worker). Renders with wgpu only when a picture is wanted.
pub(super) fn start(test: &str) -> H {
    let mut h = start_with(test, stand_in);
    let ids: Vec<i64> = h.state().index.tracks().iter().map(|t| t.id).collect();
    h.state_mut().dispatch(
        LibraryCmd::Analyse {
            tracks: ids,
            dynamic: false,
        }
        .into(),
    );
    settle_until(&mut h, |a| a.index.tracks().iter().all(|t| t.analysed()));
    h
}

/// Start the app on the fixture library with this analysis worker, and scan it.
pub(super) fn start_with(test: &str, worker: fn(Role, usize) -> WorkerConfig) -> H {
    let dir = scratch(test);
    for (title, bpm) in TRACKS {
        write_track(&dir.join("music").join(format!("{title}.wav")), bpm, 24.0);
    }
    let services = Services {
        db: dir.join(store::FILE_NAME),
        null_audio: true,
        worker,
        midi: false,
    };
    let mut b = egui_kittest::Harness::builder().with_size(vec2(1700.0, 1000.0));
    if shot_path(test).is_some() {
        b = b.wgpu();
    }
    let mut h = b.build_eframe(move |cc| EarshotApp::new(cc, services).expect("app"));
    h.state_mut()
        .dispatch(LibraryCmd::AddRoots(vec![dir.join("music")]).into());
    settle_until(&mut h, |a| a.index.tracks().len() == TRACKS.len());
    h
}

/// Step frames (in real time: the work happens on other threads) until `ok` holds.
#[track_caller]
pub(super) fn settle_until(h: &mut H, ok: impl Fn(&EarshotApp) -> bool) {
    // A minute: the tests run side by side, each with its own analysis workers.
    let end = Instant::now() + Duration::from_secs(60);
    while Instant::now() < end {
        h.step();
        if ok(h.state()) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out");
}

pub(super) fn steps(h: &mut H, n: usize) {
    for _ in 0..n {
        h.step();
        std::thread::sleep(Duration::from_millis(16));
    }
}

pub(super) fn ids(h: &H) -> Vec<i64> {
    h.state().index.tracks().iter().map(|t| t.id).collect()
}

pub(super) fn render(h: &mut H, test: &str) {
    if let Some(path) = shot_path(test) {
        h.render().expect("render").save(&path).expect("save");
        eprintln!("wrote {}", path.display());
    }
}

/// Perform: two tracks loaded and both playing. Also times building a frame (layout
/// and waveforms).
#[test]
fn perform() {
    let mut h = start("perform");
    let ids = ids(&h);
    let decks = h.state().prefs.decks();
    for (deck, id) in decks.iter().zip(&ids) {
        h.state_mut().load(Some(*deck), *id);
    }
    settle_until(&mut h, |a| {
        decks
            .iter()
            .all(|d| a.panels.decks[d.index()].wave.is_some())
    });
    let play = |d| Command::from((d, EngineCmd::Play(earshot_engine::Set::On)));
    h.state_mut()
        .dispatch(Command::Deck(decks[0], DeckCmd::SeekFraction(0.3)));
    h.state_mut().dispatch(play(decks[0]));
    h.state_mut().dispatch(ViewCmd::Browse(2).into());
    steps(&mut h, 30);
    h.state_mut().dispatch(play(decks[1]));
    // Playing decks keep the frames coming.
    assert!(h.state().moving());
    let t = Instant::now();
    let n = 100;
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
    render(&mut h, "perform");
    // With the mixer hidden (as with a controller): no tempo faders.
    h.state_mut().dispatch(ViewCmd::ShowMixer(false).into());
    steps(&mut h, 5);
    render(&mut h, "perform-no-mixer");
}

/// Large text grows the lists and bars but leaves the decks' text (and egui's zoom)
/// alone; Ctrl - steps back down.
#[test]
fn text_size() {
    let mut h = start("text-size");
    h.state_mut().dispatch(ViewCmd::UiScale(1.2).into());
    steps(&mut h, 3);
    let ctx = h.ctx.clone();
    assert_eq!(ctx.zoom_factor(), 1.0);
    let body = |ctx: &egui::Context| egui::TextStyle::Body.resolve(&ctx.style()).size;
    let normal = egui::TextStyle::Body.resolve(&egui::Style::default()).size;
    assert!((body(&ctx) - normal * 1.2).abs() < 0.01, "{}", body(&ctx));
    render(&mut h, "text-size");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Minus);
    steps(&mut h, 3);
    assert_eq!(h.state().prefs.ui_scale, 1.0);
    assert!((body(&ctx) - normal).abs() < 0.01);
}

/// The Duplicates view lists tracks with the same audio; removing one from the
/// library (never the file) leaves none, and undo puts it back.
#[test]
fn duplicates() {
    let mut h = start("duplicates");
    let music = scratch_dir("duplicates").join("music");
    let copy = music.join("Night Bus (copy).wav");
    std::fs::copy(music.join("Night Bus.wav"), &copy).unwrap();
    h.state_mut().dispatch(LibraryCmd::Rescan.into());
    settle_until(&mut h, |a| a.index.tracks().len() == TRACKS.len() + 1);
    h.state_mut()
        .dispatch(ViewCmd::Source(Some(crate::core::view::Source::AllTracks)).into());
    steps(&mut h, 2);
    h.get_by_label("Duplicates").click();
    steps(&mut h, 2);
    assert_eq!(*h.state().panels.library.browser.scope(), Scope::Duplicates);
    // The copy's audio hash is worked out in the background, then it's found.
    let groups = |a: &EarshotApp| a.panels.library.browser.duplicates().map(<[_]>::len);
    for _ in 0..600 {
        if groups(h.state()) == Some(1) {
            break;
        }
        if h.state().panels.library.browser.duplicates().is_some() {
            h.state_mut().dispatch(LibraryCmd::FindDuplicates.into());
        }
        steps(&mut h, 5);
    }
    assert_eq!(groups(h.state()), Some(1));
    let group = h.state().panels.library.browser.duplicates().unwrap()[0].clone();
    steps(&mut h, 3);
    render(&mut h, "duplicates");

    let gone = group[1];
    h.state_mut()
        .dispatch(crate::core::edit::EditCmd::RemoveFromLibrary(vec![gone]).into());
    settle_until(&mut h, |a| {
        a.index.get(gone).is_none() && groups(a) == Some(0)
    });
    assert!(copy.exists() && music.join("Night Bus.wav").exists());
    h.state_mut().dispatch(Command::Undo);
    settle_until(&mut h, |a| {
        a.index.get(gone).is_some() && groups(a) == Some(1)
    });
}

/// A track played for 30 s is saved in the play history, in the session the app
/// opened, and stopping the deck ends the play.
#[test]
fn plays_are_saved() {
    let mut h = start("plays");
    let id = ids(&h)[0];
    let deck = DeckId::new(0).unwrap();
    h.state_mut().load(Some(deck), id);
    settle_until(&mut h, |a| a.panels.decks[0].wave.is_some());
    h.state_mut()
        .dispatch((deck, EngineCmd::Play(earshot_engine::Set::On)).into());
    settle_until(&mut h, |a| a.deck_now(deck).playing);
    // As if it started playing 31 s ago.
    let decks = h.state().decks_now();
    let ago = Instant::now() - crate::core::history::PLAYED_AFTER - Duration::from_secs(1);
    // (The frames so far saw it start just now: start it again, back then.)
    let stopped: Vec<DeckNow> = decks
        .iter()
        .map(|d| DeckNow {
            playing: false,
            ..*d
        })
        .collect();
    h.state_mut().history.tick(ago, &stopped);
    h.state_mut().history.tick(ago, &decks);
    h.state_mut().track_plays();
    assert!(h.state().history.was_played(id));
    let plays = |a: &EarshotApp| a.lib.store.reader.plays().unwrap();
    settle_until(&mut h, |a| plays(a).len() == 1);
    let p = &plays(h.state())[0];
    assert_eq!((p.track, p.deck, p.ended_at), (id, 0, None));
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        - p.started_at;
    assert!((30..=40).contains(&since), "{since}");
    h.state_mut()
        .dispatch((deck, EngineCmd::Play(earshot_engine::Set::Off)).into());
    settle_until(&mut h, |a| plays(a)[0].ended_at.is_some());
}

/// Prepare mode with a track on its player, a memory cue saved and the keys help
/// open.
#[test]
fn prepare() {
    let mut h = start("prepare");
    let id = ids(&h)[1];
    h.state_mut()
        .dispatch(ViewCmd::SetMode(Mode::Prepare).into());
    h.state_mut().dispatch(LibraryCmd::Load(id).into());
    let prep = DeckId::PREPARE.index();
    settle_until(&mut h, |a| a.panels.decks[prep].wave.is_some());
    let deck = DeckId::PREPARE;
    h.state_mut()
        .dispatch(Command::Deck(deck, DeckCmd::SeekFraction(0.25)));
    steps(&mut h, 5);
    h.state_mut()
        .dispatch(Command::Deck(deck, DeckCmd::SaveMemory));
    settle_until(&mut h, |a| {
        a.cues.get(&id).is_some_and(|c| !c.memories.is_empty())
    });
    h.state_mut()
        .dispatch(ViewCmd::ToggleModal(crate::core::view::Modal::Keys).into());
    steps(&mut h, 5);
    render(&mut h, "prepare");
    // Nothing moving: no more frames asked for once it settles.
    steps(&mut h, (SETTLE.as_millis() / 16) as usize + 10);
    assert!(!h.state().moving());
    assert!(h.state().settle_until.is_none());
}

/// Prepare's Flow tab with two cards and the "+" suggestions open.
#[test]
fn flow() {
    let mut h = start("flow");
    let ids: Vec<i64> = ids(&h).into_iter().take(2).collect();
    h.state_mut()
        .dispatch(ViewCmd::SetMode(Mode::Prepare).into());
    h.state_mut()
        .dispatch(ViewCmd::Tool(crate::core::view::Tool::Flow).into());
    h.state_mut().dispatch(FlowCmd::AddTracks(ids).into());
    h.state_mut().flow().fit();
    steps(&mut h, 10);
    h.state_mut().flow().open_first_suggestions();
    steps(&mut h, 5);
    render(&mut h, "flow");
    assert!(h.state().prefs.flow_tab());
}

/// Prepare mode with track `n` of the fixture on its player, its waveform drawn.
fn start_prepare(test: &str, n: usize) -> H {
    let mut h = start(test);
    let id = ids(&h)[n];
    h.state_mut()
        .dispatch(ViewCmd::SetMode(Mode::Prepare).into());
    h.state_mut().dispatch(LibraryCmd::Load(id).into());
    let prep = DeckId::PREPARE.index();
    settle_until(&mut h, |a| {
        a.panels.decks[prep].wave.is_some() && a.frame.deck(DeckId::PREPARE).grid.is_some()
    });
    h
}

/// A key as the window sends it: pressed and let go, with `modifiers` held, the
/// physical key as well as the logical one (as winit gives them).
fn press(h: &mut H, modifiers: egui::Modifiers, key: Key) {
    for pressed in [true, false] {
        h.event_modifiers(
            egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed,
                repeat: false,
                modifiers,
            },
            modifiers,
        );
    }
    steps(h, 3);
}

/// Prepare's player's first grid line.
fn first_beat(a: &EarshotApp) -> f64 {
    let inner = a.player.lock().unwrap();
    inner
        .session(DeckId::PREPARE)
        .grid
        .as_ref()
        .expect("a grid")
        .times()[0]
}

/// The track on Prepare's player.
fn prepared(a: &EarshotApp) -> Option<i64> {
    a.player.lock().unwrap().session(DeckId::PREPARE).track_id
}

/// Ctrl+← / → nudge Prepare's grid too (not a beat jump).
#[test]
fn ctrl_arrows_nudge_the_grid() {
    let mut h = start_prepare("nudge-ctrl", 1);
    let pos = |a: &EarshotApp| a.frame.snap.as_ref().unwrap().prepare.position;
    steps(&mut h, 20);
    let (before, at) = (first_beat(h.state()), pos(h.state()));
    press(&mut h, egui::Modifiers::COMMAND, Key::ArrowRight);
    settle_until(&mut h, |a| (first_beat(a) - (before + 0.010)).abs() < 1e-6);
    press(&mut h, egui::Modifiers::COMMAND, Key::ArrowLeft);
    settle_until(&mut h, |a| (first_beat(a) - before).abs() < 1e-6);
    assert!((pos(h.state()) - at).abs() < 0.1);
}

/// Shift+← / → nudge Prepare's grid 10 ms, through the real key events.
#[test]
fn shift_arrows_nudge_the_grid() {
    let mut h = start_prepare("nudge", 1);
    let id = prepared(h.state()).unwrap();
    let pos = |a: &EarshotApp| a.frame.snap.as_ref().unwrap().prepare.position;
    // The player settles on its first cue once the track is in.
    steps(&mut h, 20);
    let (before, at) = (first_beat(h.state()), pos(h.state()));
    press(&mut h, egui::Modifiers::SHIFT, Key::ArrowLeft);
    settle_until(&mut h, |a| (first_beat(a) - (before - 0.010)).abs() < 1e-6);
    press(&mut h, egui::Modifiers::SHIFT, Key::ArrowRight);
    press(&mut h, egui::Modifiers::SHIFT, Key::ArrowRight);
    settle_until(&mut h, |a| (first_beat(a) - (before + 0.010)).abs() < 1e-6);
    // Shift doesn't also beat jump (4 beats would be about 2 s).
    assert!(
        (pos(h.state()) - at).abs() < 0.1,
        "{} vs {at}",
        pos(h.state())
    );
    // What's drawn has it, it stays, and it's saved: loading the track again keeps it.
    steps(&mut h, 60);
    let drawn = |a: &EarshotApp| a.frame.deck(DeckId::PREPARE).grid.as_ref().unwrap().times()[0];
    assert!((drawn(h.state()) - (before + 0.010)).abs() < 1e-6);
    let other = ids(&h)[0];
    h.state_mut()
        .dispatch(Command::Deck(DeckId::PREPARE, DeckCmd::Load(other)));
    settle_until(&mut h, |a| prepared(a) == Some(other));
    h.state_mut()
        .dispatch(Command::Deck(DeckId::PREPARE, DeckCmd::Load(id)));
    settle_until(&mut h, |a| prepared(a) == Some(id));
    steps(&mut h, 30);
    assert!(
        (first_beat(h.state()) - (before + 0.010)).abs() < 1e-6,
        "{} vs {before}",
        first_beat(h.state())
    );
}

/// The rest of Prepare's keys, through real key events: Space plays, → / ← beat jump,
/// 6-9 loop (another size resizes the loop, the same key ends it), M saves to memory.
#[test]
fn prepare_keys() {
    let mut h = start_prepare("prepare-keys", 1);
    let id = prepared(h.state()).unwrap();
    let snap = |a: &EarshotApp| a.frame.snap.as_ref().unwrap().prepare;
    let grid = h.state().frame.deck(DeckId::PREPARE).grid.clone().unwrap();
    let beats = |a: &EarshotApp| grid.beat_at(snap(a).position);
    // A jump lands on a beat 4 on (from just before the first, the 1 after it).
    let start = beats(h.state());
    press(&mut h, egui::Modifiers::NONE, Key::ArrowRight);
    settle_until(&mut h, |a| beats(a) - start > 3.9);
    let there = beats(h.state());
    press(&mut h, egui::Modifiers::NONE, Key::ArrowLeft);
    settle_until(&mut h, |a| (there - beats(a) - 4.0).abs() < 0.01);

    press(&mut h, egui::Modifiers::NONE, Key::Space);
    settle_until(&mut h, |a| snap(a).playing);
    press(&mut h, egui::Modifiers::NONE, Key::Num6);
    let len = |a: &EarshotApp| {
        let s = snap(a);
        s.loop_active
            .then(|| grid.beat_at(s.loop_out.unwrap()) - grid.beat_at(s.loop_in.unwrap()))
    };
    settle_until(&mut h, |a| len(a).is_some_and(|l| (l - 4.0).abs() < 0.01));
    press(&mut h, egui::Modifiers::NONE, Key::Num7);
    settle_until(&mut h, |a| len(a).is_some_and(|l| (l - 8.0).abs() < 0.01));
    // M while looping saves the loop.
    press(&mut h, egui::Modifiers::NONE, Key::M);
    settle_until(&mut h, |a| {
        a.cues
            .get(&id)
            .is_some_and(|c| c.memories.iter().any(|m| m.end.is_some()))
    });
    press(&mut h, egui::Modifiers::NONE, Key::Num7);
    settle_until(&mut h, |a| !snap(a).loop_active);
    press(&mut h, egui::Modifiers::NONE, Key::Space);
    settle_until(&mut h, |a| !snap(a).playing);
}

/// Hot cues set on Prepare's player come back when the track is loaded again.
#[test]
fn hot_cues_are_kept_per_track() {
    let mut h = start_prepare("hot-cues", 1);
    let (id, other) = (prepared(h.state()).unwrap(), ids(&h)[0]);
    let deck = DeckId::PREPARE;
    let snap = |a: &EarshotApp| a.frame.snap.as_ref().unwrap().prepare;
    h.state_mut().dispatch((deck, EngineCmd::Seek(3.0)).into());
    settle_until(&mut h, |a| (snap(a).position - 3.0).abs() < 0.01);
    for press in [earshot_engine::Press::Down, earshot_engine::Press::Up] {
        h.state_mut()
            .dispatch((deck, EngineCmd::Hotcue { slot: 2, press }).into());
    }
    settle_until(&mut h, |a| snap(a).hotcues[2].is_some());
    let at = snap(h.state()).hotcues[2].unwrap();
    steps(&mut h, 10);
    for track in [other, id] {
        h.state_mut()
            .dispatch(Command::Deck(deck, DeckCmd::Load(track)));
        settle_until(&mut h, |a| prepared(a) == Some(track));
    }
    settle_until(&mut h, |a| snap(a).hotcues[2] == Some(at));
}

/// The main cue moved with the Cue button comes back when the track is loaded again,
/// with the playhead on it.
#[test]
fn the_main_cue_is_kept_per_track() {
    let mut h = start_prepare("main-cue", 1);
    let (id, other) = (prepared(h.state()).unwrap(), ids(&h)[0]);
    let deck = DeckId::PREPARE;
    let snap = |a: &EarshotApp| a.frame.snap.as_ref().unwrap().prepare;
    h.state_mut().dispatch((deck, EngineCmd::Seek(7.0)).into());
    settle_until(&mut h, |a| (snap(a).position - 7.0).abs() < 0.01);
    for press in [earshot_engine::Press::Down, earshot_engine::Press::Up] {
        h.state_mut().dispatch((deck, EngineCmd::Cue(press)).into());
    }
    settle_until(&mut h, |a| snap(a).cue_by_hand);
    let at = snap(h.state()).cue_point;
    assert!((at - 7.0).abs() < 1.0, "cued near the playhead: {at}");
    steps(&mut h, 10);
    for track in [other, id] {
        h.state_mut()
            .dispatch(Command::Deck(deck, DeckCmd::Load(track)));
        settle_until(&mut h, |a| prepared(a) == Some(track));
    }
    settle_until(&mut h, |a| {
        let s = snap(a);
        s.cue_point == at && (s.position - at).abs() < 1e-6
    });
}

/// Ctrl+↓ / ↑ load the next and previous track in the library into Prepare's player.
#[test]
fn ctrl_arrows_load_the_next_track() {
    let mut h = start_prepare("ctrl-load", 0);
    // The list as the library shows it, with the focus on the loaded track.
    h.state_mut().dispatch(ViewCmd::Browse(-100).into());
    let list: Vec<i64> = {
        let lib = &mut h.state_mut().panels.library.browser;
        let mut ids = vec![];
        for _ in 0..TRACKS.len() {
            ids.extend(lib.focused());
            lib.move_focus(1);
        }
        lib.move_focus(-100);
        ids
    };
    assert_eq!(list.len(), TRACKS.len());
    h.state_mut()
        .dispatch(ViewCmd::LoadSelected(DeckId::PREPARE).into());
    // Every load in, so an older one can't land after the next.
    let prep = DeckId::PREPARE.index();
    settle_until(&mut h, |a| {
        prepared(a) == Some(list[0]) && a.panels.decks[prep].pending().is_none()
    });

    press(&mut h, egui::Modifiers::COMMAND, Key::ArrowDown);
    settle_until(&mut h, |a| prepared(a) == Some(list[1]));
    press(&mut h, egui::Modifiers::COMMAND, Key::ArrowDown);
    settle_until(&mut h, |a| prepared(a) == Some(list[2]));
    press(&mut h, egui::Modifiers::COMMAND, Key::ArrowUp);
    settle_until(&mut h, |a| prepared(a) == Some(list[1]));
    // Plain ↓ only browses.
    press(&mut h, egui::Modifiers::NONE, Key::ArrowDown);
    steps(&mut h, 10);
    assert_eq!(prepared(h.state()), Some(list[1]));
    assert_eq!(h.state().panels.library.browser.focused(), Some(list[2]));
}

/// Two loads onto one player in quick succession: only the newer one lands, so the
/// player and the screen agree, whichever finishes decoding first.
#[test]
fn only_the_newest_load_lands() {
    let mut h = start_prepare("newest-load", 0);
    let prep = DeckId::PREPARE;
    let ids = ids(&h);
    let shown = |a: &EarshotApp| a.panels.decks[prep.index()].pending().map(|p| p.track_id);

    // Two quick Ctrl+↓: the second track wins, not the first.
    h.state_mut().dispatch(ViewCmd::Browse(-100).into());
    press(&mut h, egui::Modifiers::COMMAND, Key::ArrowDown);
    press(&mut h, egui::Modifiers::COMMAND, Key::ArrowDown);
    let want = h.state().panels.library.browser.focused();
    settle_until(&mut h, |a| shown(a).is_none() && prepared(a) == want);
    steps(&mut h, 30);
    assert_eq!(prepared(h.state()), want);

    // An older request finishing last, forced: it doesn't replace the newer track.
    let (old, new) = (ids[2], ids[3]);
    let player = h.state().player.clone();
    let lib = h.state().lib.clone();
    let first = player::request_load(&player, prep);
    let second = player::request_load(&player, prep);
    player::load_track(&player, &lib, prep, new, second).unwrap();
    assert_eq!(
        player::load_track(&player, &lib, prep, old, first),
        Err(player::SUPERSEDED.into())
    );
    assert_eq!(prepared(h.state()), Some(new));
}

/// C sets the cue on the beat (quantising) and the playhead goes there with it.
#[test]
fn c_puts_the_playhead_on_the_cue() {
    let mut h = start_prepare("set-cue", 1);
    let deck = DeckId::PREPARE;
    let snap = |a: &EarshotApp| a.frame.snap.as_ref().unwrap().prepare;
    if !snap(h.state()).quantize {
        h.state_mut()
            .dispatch((deck, EngineCmd::Quantize(earshot_engine::Set::On)).into());
    }
    // Somewhere between two beats.
    let grid = h.state().frame.deck(deck).grid.clone().unwrap();
    let off_beat = grid.time_at(16.4);
    h.state_mut()
        .dispatch((deck, EngineCmd::Seek(off_beat)).into());
    settle_until(&mut h, |a| (snap(a).position - off_beat).abs() < 1e-6);
    press(&mut h, egui::Modifiers::NONE, Key::C);
    let beat = grid.time_at(16.0);
    settle_until(&mut h, |a| {
        let s = snap(a);
        (s.cue_point - beat).abs() < 1e-6 && (s.position - beat).abs() < 1e-6
    });
}

/// The 2 / 4 toggle shows four decks (and their mixer channels, and lets effects go
/// on them), saved in the preferences.
#[test]
fn four_decks() {
    use egui_kittest::kittest::Queryable;
    let mut h = start("four-decks");
    let fx_decks = |h: &H| h.state().player.lock().unwrap().fx_decks;
    assert!(!h.state().prefs.four_decks);
    assert_eq!(fx_decks(&h), 2);
    h.get_by_label("4 decks").click();
    steps(&mut h, 3);
    assert!(h.state().prefs.four_decks);
    assert_eq!(h.state().prefs.decks().len(), 4);
    // Effects can go on all four.
    assert_eq!(fx_decks(&h), 4);
    // The engine runs four: deck 3 takes a track and plays.
    let ids = ids(&h);
    let d3 = DeckId::new(2).unwrap();
    for (i, d) in DeckId::first(4).enumerate() {
        h.state_mut().load(Some(d), ids[i]);
    }
    settle_until(&mut h, |a| {
        DeckId::first(4).all(|d| a.panels.decks[d.index()].wave.is_some())
    });
    h.state_mut()
        .dispatch((d3, EngineCmd::Play(earshot_engine::Set::On)).into());
    settle_until(&mut h, |a| a.frame.snap.as_ref().unwrap().deck(d3).playing);
    steps(&mut h, 3);
    // Not back to two while deck 3 plays out of sight.
    h.get_by_label("2 decks").click();
    steps(&mut h, 3);
    assert!(h.state().prefs.four_decks);
    // Times a frame with four decks shown and playing.
    for d in DeckId::first(4) {
        h.state_mut()
            .dispatch((d, EngineCmd::Play(earshot_engine::Set::On)).into());
    }
    steps(&mut h, 10);
    let t = Instant::now();
    let n = 100;
    let mut worst = 0f64;
    for _ in 0..n {
        let f = Instant::now();
        h.step();
        worst = worst.max(f.elapsed().as_secs_f64());
    }
    eprintln!(
        "four decks, frame build: {:.2} ms average, {:.2} ms worst over {n} frames",
        t.elapsed().as_secs_f64() * 1000.0 / n as f64,
        worst * 1000.0
    );
    render(&mut h, "perform4");
    // Suggestions can follow any of the four decks.
    let tip = |h: &H, t: &str| h.query_all_by_label(t).count() > 0;
    assert!(tip(&h, "C") && tip(&h, "D"), "decks C and D as sources");
    for d in DeckId::first(4) {
        h.state_mut()
            .dispatch((d, EngineCmd::Play(earshot_engine::Set::Off)).into());
    }
    settle_until(&mut h, |a| {
        a.frame
            .snap
            .as_ref()
            .unwrap()
            .live()
            .iter()
            .all(|d| !d.playing)
    });
    h.get_by_label("2 decks").click();
    steps(&mut h, 3);
    assert!(!h.state().prefs.four_decks);
}

/// Layout commands land in the preferences, per mode.
#[test]
fn layout_is_kept_per_mode() {
    let mut h = start("layout");
    let app = h.state_mut();
    app.dispatch(ViewCmd::Aside(None).into());
    app.dispatch(ViewCmd::SetMode(Mode::Prepare).into());
    assert_eq!(
        app.prefs.layout().aside,
        Some(crate::core::view::Aside::Suggestions)
    );
    app.dispatch(ViewCmd::Source(None).into());
    app.dispatch(ViewCmd::SetMode(Mode::Perform).into());
    assert_eq!(app.prefs.layout().aside, None);
    assert!(app.prefs.layout().source.is_some());
    app.dispatch(ViewCmd::ToggleModal(crate::core::view::Modal::Settings).into());
    assert!(app.modals.is_open(crate::core::view::Modal::Settings));
    app.dispatch(ViewCmd::CloseModal(crate::core::view::Modal::Settings).into());
    assert!(!app.modals.is_open(crate::core::view::Modal::Settings));
    steps(&mut h, 3);
}

/// The effect unit is in Perform's top bar, mixer or not: picking an effect from its
/// dropdown puts that effect on.
#[test]
fn the_effect_unit_is_in_the_top_bar() {
    let mut h = start("fx");
    h.state_mut()
        .dispatch(ViewCmd::SetMode(Mode::Perform).into());
    steps(&mut h, 3);
    let fx = |h: &H| h.query_all_by_label("Effect level").count() > 0;
    assert!(fx(&h));
    h.state_mut().dispatch(ViewCmd::ShowMixer(false).into());
    steps(&mut h, 3);
    assert!(fx(&h), "not with the mixer");
    let kind = |a: &EarshotApp| a.frame.snap.as_ref().unwrap().mixer.fx[0].kind;
    let next = earshot_engine::FxKind::ALL
        .into_iter()
        .find(|k| *k != kind(h.state()))
        .unwrap();
    h.get_by_value(kind(h.state()).name()).click();
    steps(&mut h, 3);
    h.get_by_label(next.name()).click();
    settle_until(&mut h, |a| kind(a) == next);
}

/// However the mixer and effects are switched, both decks stay inside the window:
/// when full-size controls don't fit, a deck uses compact ones.
#[test]
fn decks_stay_inside_the_window() {
    use egui_kittest::kittest::Queryable;
    let mut h = start("deck-fit");
    h.state_mut()
        .dispatch(ViewCmd::SetMode(Mode::Perform).into());
    steps(&mut h, 3);
    let inside = |h: &H| {
        let right = h.ctx.content_rect().right();
        let syncs: Vec<_> = h.query_all_by_label("SYNC").map(|n| n.rect()).collect();
        assert_eq!(syncs.len(), 2);
        syncs.iter().all(|r| r.right() <= right + 0.5)
    };
    assert!(inside(&h));
    for switch in ["Mixer", "Mixer"] {
        h.get_by_label(switch).click();
        steps(&mut h, 5);
        assert!(inside(&h), "after {switch}");
    }
}

/// Perform's mixer hides from the top bar's Mixer switch, and while a controller is connected
/// (unless Settings says not to); it starts hidden each time one connects.
#[test]
fn the_mixer_hides_by_hand_and_for_a_controller() {
    let mut h = start("mixer");
    h.state_mut()
        .dispatch(ViewCmd::SetMode(Mode::Perform).into());
    steps(&mut h, 3);
    let mixer = |h: &H| h.query_all_by_label("Gain").count() > 0;
    assert!(mixer(&h));
    h.state_mut().dispatch(ViewCmd::ShowMixer(false).into());
    steps(&mut h, 3);
    assert!(!mixer(&h));
    h.state_mut().dispatch(ViewCmd::ShowMixer(true).into());

    // A controller connects: hidden, until shown by hand.
    h.state_mut().prefs.set_controller(true);
    steps(&mut h, 3);
    assert!(!mixer(&h));
    h.state_mut().dispatch(ViewCmd::ShowMixer(true).into());
    steps(&mut h, 3);
    assert!(mixer(&h));
    // Disconnected and connected again: hidden again; gone: the choice without one.
    h.state_mut().prefs.set_controller(false);
    h.state_mut().prefs.set_controller(true);
    assert!(!h.state().prefs.mixer_shown());
    h.state_mut().prefs.set_controller(false);
    assert!(h.state().prefs.mixer_shown());
    // Auto-hide off: a controller changes nothing.
    h.state_mut().dispatch(ViewCmd::AutoHideMixer(false).into());
    h.state_mut().prefs.set_controller(true);
    assert!(h.state().prefs.mixer_shown());
}

/// The settings window: the analysis tab changes the background worker cap at once
/// (and saves it), the keymap tab loads the user's keymap file or says why it didn't.
#[test]
fn settings() {
    use crate::core::view::{Modal, SettingsTab};
    let mut h = start("settings");
    h.state_mut()
        .dispatch(ViewCmd::Settings(SettingsTab::Analysis).into());
    assert!(h.state().modals.is_open(Modal::Settings));
    assert_eq!(h.state().modals.settings_tab(), SettingsTab::Analysis);
    steps(&mut h, 3);
    render(&mut h, "settings-analysis");
    h.state_mut().dispatch(AppCmd::BackgroundWorkers(1).into());
    assert_eq!(h.state().lib.analyser.background_workers(), 1);
    assert_eq!(h.state().background_workers, 1);
    settle_until(&mut h, |a| {
        a.lib
            .store
            .reader
            .setting(analysis::SETTING_BACKGROUND_WORKERS)
            .ok()
            .flatten()
            .as_deref()
            == Some("1")
    });
    // Kept to the cores.
    h.state_mut()
        .dispatch(AppCmd::BackgroundWorkers(10_000).into());
    assert_eq!(h.state().background_workers, analysis::cores());

    // The keymap: built in, then a copy to edit, then a broken file.
    let path = h.state().keymap_path.clone();
    assert_eq!(path.file_name().unwrap(), keymap::FILE_NAME);
    assert_eq!(h.state().keymap_origin, keymap::Origin::BuiltIn);
    h.state_mut().dispatch(AppCmd::CopyKeymap.into());
    assert_eq!(h.state().keymap_origin, keymap::Origin::File);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), keymap::BUILT_IN);
    std::fs::write(&path, "perform  P  deck1  play\nperform  P  deck2  play\n").unwrap();
    // Not over the user's file.
    h.state_mut().dispatch(AppCmd::CopyKeymap.into());
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .starts_with("perform  P")
    );
    assert!(matches!(h.state().keymap_origin, keymap::Origin::Broken(_)));
    assert!(
        h.state().keymap.find(Mode::Perform, "S", false).is_some(),
        "built in"
    );
    std::fs::write(&path, "perform  P  deck1  play\n").unwrap();
    h.state_mut().dispatch(AppCmd::ReloadKeymap.into());
    assert_eq!(h.state().keymap_origin, keymap::Origin::File);
    assert!(h.state().keymap.find(Mode::Perform, "S", false).is_none());
    std::fs::write(&path, "perform  P  deck1  play\nperform  P  deck2  play\n").unwrap();
    h.state_mut().dispatch(AppCmd::ReloadKeymap.into());
    h.state_mut()
        .dispatch(ViewCmd::Settings(SettingsTab::Keymap).into());
    steps(&mut h, 3);
    render(&mut h, "settings-keymap");
    h.state_mut()
        .dispatch(ViewCmd::Settings(SettingsTab::Audio).into());
    steps(&mut h, 3);
    render(&mut h, "settings-audio");
}

/// A worker whose grid analyser isn't the app's version is refused: the tracks fail
/// with why, and the top bar says the worker is out of date.
#[test]
fn a_worker_with_another_grid_version_is_refused() {
    let mut h = start_with("old-worker", stand_in_old_grid);
    let id = ids(&h)[0];
    h.state_mut().dispatch(
        LibraryCmd::Analyse {
            tracks: vec![id],
            dynamic: false,
        }
        .into(),
    );
    settle_until(&mut h, |a| a.analysis.worker_problem.is_some());
    let why = h.state().analysis.worker_problem.clone().unwrap();
    assert!(
        why.contains("grid analyser is version 2, this app's is 3"),
        "{why}"
    );
    assert!(!h.state().index.get(id).unwrap().analysed());
    steps(&mut h, 3);
    h.get_by_label("analysis worker out of date");
    render(&mut h, "old-worker");
}

/// The settings' analysis tab counts the tracks with stale analysis, and re-analyses
/// them.
#[test]
fn stale_tracks_are_counted_and_analysed_again() {
    use crate::ui::modals::settings::Stale;
    let mut h = start("stale");
    h.state_mut()
        .dispatch(ViewCmd::Settings(crate::core::view::SettingsTab::Analysis).into());
    // The tab asks for the count when it's drawn.
    settle_until(&mut h, |a| a.stale == Stale::Count(0));
    // A grid from an older analyser.
    let id = ids(&h)[2];
    let db = rusqlite::Connection::open(scratch_dir("stale").join(store::FILE_NAME)).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db.execute(
        "UPDATE analysis_results SET version = 0 WHERE track_id = ?1 AND analyser = 'grid'",
        [id],
    )
    .unwrap();
    h.state_mut().dispatch(AppCmd::CountStale.into());
    settle_until(&mut h, |a| a.stale == Stale::Count(1));
    steps(&mut h, 2);
    render(&mut h, "settings-stale");
    h.state_mut().dispatch(AppCmd::QueueStale.into());
    settle_until(&mut h, |a| a.stale == Stale::Queued(1));
    // Up to date once its new result is saved.
    for _ in 0..600 {
        if h.state().stale == Stale::Count(0) {
            return;
        }
        if h.state().stale != Stale::Counting {
            h.state_mut().dispatch(AppCmd::CountStale.into());
        }
        steps(&mut h, 5);
    }
    panic!("still stale: {:?}", h.state().stale);
}

/// Edits from the shell: metadata, the Flow canvas and a grid, each undone and
/// redone with `Command::Undo` / `Redo` (what Ctrl+Z, MIDI and the Edit menu send).
#[test]
fn edits_undo_and_redo() {
    use crate::core::edit::{EditCmd, Field, Value};
    let mut h = start("undo");
    let ids = ids(&h);
    let title = |a: &EarshotApp| a.index.get(ids[0]).unwrap().title.clone();
    let tag_title = title(h.state());

    // Metadata, through the editor's command.
    h.state_mut().dispatch(EditCmd::Editor(vec![ids[0]]).into());
    assert!(h.state().modals.is_open(crate::core::view::Modal::Metadata));
    h.state_mut().dispatch(
        EditCmd::Set {
            tracks: vec![ids[0]],
            values: vec![(Field::Title, Some(Value::Text("Renamed".into())))],
        }
        .into(),
    );
    settle_until(&mut h, |a| {
        title(a) == "Renamed" && a.edits.next_undo().is_some()
    });
    h.state_mut().dispatch(Command::Undo);
    settle_until(&mut h, |a| title(a) == tag_title);
    h.state_mut().dispatch(Command::Redo);
    settle_until(&mut h, |a| title(a) == "Renamed");

    // The Flow canvas: adding two cards is one step.
    h.state_mut()
        .dispatch(FlowCmd::AddTracks(ids[..2].to_vec()).into());
    let cards = |a: &EarshotApp| {
        a.panels
            .flow
            .flow
            .and_then(|f| a.collections.get(f))
            .map_or(0, |c| c.tracks.len())
    };
    settle_until(&mut h, |a| cards(a) == 2);
    h.state_mut().dispatch(Command::Undo);
    settle_until(&mut h, |a| cards(a) == 0);
    h.state_mut().dispatch(Command::Redo);
    settle_until(&mut h, |a| cards(a) == 2);

    // A grid edit on Prepare's player; undo puts the analysed grid back on it too.
    let deck = DeckId::PREPARE;
    h.state_mut()
        .dispatch(ViewCmd::SetMode(Mode::Prepare).into());
    h.state_mut().dispatch(LibraryCmd::Load(ids[1]).into());
    settle_until(&mut h, |a| a.panels.decks[deck.index()].wave.is_some());
    let bpm = |a: &EarshotApp| a.index.get(ids[1]).unwrap().bpm;
    let analysed = bpm(h.state());
    let on_deck = |a: &EarshotApp| {
        let inner = a.player.lock().unwrap();
        inner
            .session(deck)
            .grid
            .as_ref()
            .map(|g| g.mean_bpm().round())
    };
    h.state_mut().dispatch(Command::Deck(
        deck,
        DeckCmd::Grid(crate::core::player::GridOp::SetBpm(140.0)),
    ));
    settle_until(&mut h, |a| bpm(a) == Some(140.0));
    assert_eq!(on_deck(h.state()), Some(140.0));
    h.state_mut().dispatch(Command::Undo);
    settle_until(&mut h, |a| bpm(a) == analysed && on_deck(a) != Some(140.0));
    assert_eq!(on_deck(h.state()), analysed.map(f64::round));

    // A playlist made from the track menu (with its tracks), then deleted: each undone.
    h.state_mut().dispatch(
        CollectionCmd::Create {
            kind: CollectionKind::Playlist,
            parent: None,
            tracks: ids[..2].to_vec(),
            query: None,
        }
        .into(),
    );
    let made = |a: &EarshotApp| {
        a.collections
            .children_in(Section::Playlists, None)
            .into_iter()
            .find(|c| c.kind == CollectionKind::Playlist)
            .map(|c| (c.id, c.tracks.clone()))
    };
    settle_until(&mut h, |a| made(a).is_some_and(|m| m.1 == ids[..2]));
    let (playlist, _) = made(h.state()).unwrap();
    h.state_mut().dispatch(Command::Undo);
    settle_until(&mut h, |a| made(a).is_none());
    h.state_mut().dispatch(Command::Redo);
    settle_until(&mut h, |a| made(a).is_some_and(|m| m.1 == ids[..2]));
    h.state_mut()
        .dispatch(CollectionCmd::Delete(playlist).into());
    settle_until(&mut h, |a| made(a).is_none());
    assert_eq!(
        h.state().edits.next_undo().map(|e| e.label()).as_deref(),
        Some("delete playlist")
    );
    h.state_mut().dispatch(Command::Undo);
    settle_until(&mut h, |a| made(a) == Some((playlist, ids[..2].to_vec())));
}

// ---- the library --------------------------------------------------------------------------

use egui::Pos2;
use egui_kittest::kittest::Queryable;

use crate::core::collections::Section;
use crate::core::view::Source;
use crate::ui::panels::library::drag::DragPayload;

/// A collection made straight in the Store (so nothing starts renaming it), once
/// the app has it.
fn make(h: &mut H, kind: CollectionKind, name: &str, tracks: Vec<i64>) -> i64 {
    let store = h.state().lib.store.clone();
    let id = store.new_collection_id();
    store.send(StoreCmd::Create {
        id,
        parent: None,
        kind,
        name: name.into(),
        query: None,
    });
    let n = tracks.len();
    if n > 0 {
        store.send(StoreCmd::AddTracks { id, tracks });
    }
    settle_until(h, |a| {
        a.collections.get(id).is_some_and(|c| c.tracks.len() == n)
    });
    id
}

fn title(h: &H, id: i64) -> String {
    h.state().index.get(id).unwrap().title.clone()
}

/// Where a label is drawn.
fn at(h: &H, label: &str) -> egui::Rect {
    h.get_by_label(label).rect()
}

/// Press at `from`, move to `to` in a few steps (calling `mid` on the way, the
/// button still down), and let go.
fn drag(h: &mut H, from: Pos2, to: Pos2, mid: impl FnOnce(&mut H)) {
    h.hover_at(from);
    h.step();
    h.drag_at(from);
    h.step();
    for i in 1..=6 {
        h.hover_at(from + (to - from) * (i as f32 / 6.0));
        h.step();
    }
    mid(h);
    h.drop_at(to);
    h.step();
    h.step();
}

/// Select two tracks in the library, drag them onto a crate in the Crates source
/// (with the ghost along the way), then drag a playlist's last row to its top.
#[test]
fn tracks_drag_into_collections_and_playlist_rows_reorder() {
    let mut h = start("library-drag");
    let ids = ids(&h);
    h.state_mut().dispatch(ViewCmd::Aside(None).into());
    let crate_id = make(&mut h, CollectionKind::Crate, "Disco", vec![]);
    h.state_mut()
        .dispatch(ViewCmd::Source(Some(Source::Crates)).into());
    steps(&mut h, 3);

    // Click one, Shift-click another: both are selected.
    let (a, b) = (title(&h, ids[0]), title(&h, ids[1]));
    h.get_by_label(&a).click();
    steps(&mut h, 2);
    h.get_by_label(&b).click_modifiers(egui::Modifiers::SHIFT);
    steps(&mut h, 2);
    assert_eq!(h.state().panels.library.focused_pane, Pane::Library);

    let (from, to) = (at(&h, &a).center(), at(&h, "Disco").center());
    drag(&mut h, from, to, |h| {
        let p = drag::payload(&h.ctx).expect("dragging");
        assert!(
            matches!(p.as_ref(), DragPayload::Tracks { ids: d, .. } if d.len() == 2),
            "the whole selection"
        );
        render(h, "library-drag");
    });
    settle_until(&mut h, |a| {
        a.collections
            .get(crate_id)
            .is_some_and(|c| c.tracks.len() == 2)
    });

    // A playlist in its own order: drag the last row above the first.
    let list = make(&mut h, CollectionKind::Playlist, "Gig", ids[..3].to_vec());
    h.state_mut().dispatch(CollectionCmd::Show(list).into());
    steps(&mut h, 3);
    let last = at(&h, &title(&h, ids[2])).center();
    let first = at(&h, &title(&h, ids[0]));
    drag(
        &mut h,
        last,
        Pos2::new(first.center().x, first.top()),
        |_| {},
    );
    settle_until(&mut h, |a| {
        a.collections.get(list).unwrap().tracks == vec![ids[2], ids[0], ids[1]]
    });
    // One undo step puts the old order back.
    h.state_mut().dispatch(Command::Undo);
    settle_until(&mut h, |a| {
        a.collections.get(list).unwrap().tracks == ids[..3].to_vec()
    });
}

/// Folders in a section: made, nested and their contents listed; a playlist can't
/// go in a crate folder.
#[test]
fn folders_in_each_section() {
    let mut h = start("folders");
    let ids = ids(&h);
    h.state_mut().dispatch(
        CollectionCmd::CreateFolder {
            section: Section::Crates,
            parent: None,
        }
        .into(),
    );
    settle_until(&mut h, |a| a.collections.children(None).len() == 1);
    let folder = h.state().collections.children(None)[0].id;
    assert_eq!(
        h.state().collections.get(folder).unwrap().section,
        Section::Crates
    );
    let inner = make(&mut h, CollectionKind::Crate, "Inner", ids[..2].to_vec());
    h.state_mut()
        .dispatch(CollectionCmd::Move(inner, Some(folder)).into());
    settle_until(&mut h, |a| {
        a.collections.get(inner).unwrap().parent == Some(folder)
    });
    let list = make(&mut h, CollectionKind::Playlist, "P", vec![]);
    assert!(!h.state().collections.can_move(list, Some(folder)));
    // A folder lists everything inside it.
    h.state_mut()
        .dispatch(ViewCmd::Source(Some(Source::Crates)).into());
    h.state_mut().dispatch(CollectionCmd::Show(folder).into());
    steps(&mut h, 3);
    for id in &ids[..2] {
        h.get_by_label(&title(&h, *id));
    }
    assert!(h.query_by_label(&title(&h, ids[2])).is_none());
    // A column filter shows as a chip; the rating and colour columns can be added.
    {
        use crate::ui::panels::library::table::{ColumnId, Filter, TableView};
        let t = h
            .state_mut()
            .panels
            .library
            .tables
            .get_mut(TableView::Folder);
        *t.filter_mut(ColumnId::Bpm) = Filter::Text(">=100".into());
        t.toggle_column(ColumnId::Rating);
        t.toggle_column(ColumnId::Colour);
    }
    steps(&mut h, 3);
    render(&mut h, "folders");
}

/// "Add to Flow ▸ <Flow>" in the track menu adds a card that follows nothing.
#[test]
fn add_to_flow_from_the_track_menu() {
    let mut h = start("add-to-flow");
    let ids = ids(&h);
    h.state_mut().dispatch(ViewCmd::Aside(None).into());
    let flow = make(&mut h, CollectionKind::Flow, "Warm-up", vec![ids[0]]);
    steps(&mut h, 2);
    h.get_by_label(&title(&h, ids[3])).click_secondary();
    steps(&mut h, 2);
    h.get_by_label("Add to Flow ⏵").click();
    steps(&mut h, 2);
    h.get_by_label("Warm-up").click();
    settle_until(&mut h, |a| {
        a.collections.get(flow).unwrap().tracks.len() == 2
    });
    let c = h.state().collections.get(flow).unwrap();
    assert!(c.tracks.contains(&ids[3]));
    assert!(!c.linear, "two starting cards: not linked");
}

/// The FEATURES.md bug: in Prepare, the suggestions' Master is Prepare's player while
/// it plays. Then the suggestions take focus: browsing and "load selected" use them.
#[test]
fn master_in_prepare_is_the_player() {
    let mut h = start("prepare-master");
    let id = ids(&h)[1];
    h.state_mut()
        .dispatch(ViewCmd::SetMode(Mode::Prepare).into());
    h.state_mut().dispatch(LibraryCmd::Load(id).into());
    let prep = DeckId::PREPARE;
    settle_until(&mut h, |a| a.panels.decks[prep.index()].wave.is_some());
    h.state_mut().dispatch(Command::from((
        prep,
        EngineCmd::Play(earshot_engine::Set::On),
    )));
    settle_until(&mut h, |a| {
        a.panels
            .library
            .suggest
            .reference
            .is_some_and(|r| r.id == id && r.deck == Some(prep.index()) && r.playing)
    });
    render(&mut h, "prepare-master");

    let app = h.state_mut();
    app.dispatch(ViewCmd::FocusPane(Pane::Suggest).into());
    app.dispatch(ViewCmd::Browse(1).into());
    let pick = app.panels.library.suggest.focused().expect("a suggestion");
    assert_eq!(
        app.panels.library.focused_track(&app.panels.flow),
        Some(pick)
    );
    let a = DeckId::new(0).unwrap();
    app.dispatch(ViewCmd::LoadSelected(a).into());
    settle_until(&mut h, |app| app.panels.decks[a.index()].wave.is_some());
    assert_eq!(
        h.state().player.lock().unwrap().session(a).track_id,
        Some(pick)
    );
}

mod audio_watch;
mod external;
mod history;
mod metadata;
mod tempo_lock;

/// The text columns fit their contents; clicking a row sizes nothing, and dragging
/// the edge between Title and Artist sizes Title only.
#[test]
fn library_columns_fit_their_contents_until_dragged() {
    use crate::ui::panels::library::table::{ColumnId, TableView, auto_widths};
    let mut h = start("columns");
    h.state_mut().dispatch(ViewCmd::Aside(None).into());
    steps(&mut h, 3);
    render(&mut h, "columns");
    let auto = auto_widths(&h.ctx, &h.state().index);
    let head = at(&h, "Title");
    let artist = at(&h, "Artist");
    // The text columns share the spare room: Title is wider than its fit, and the
    // last column ends near the pane's right edge.
    assert!(
        artist.left() - head.left() > auto[&ColumnId::Title] + 50.0,
        "{head:?} {artist:?} {auto:?}"
    );
    let pane = at(&h, "4 tracks");
    let time = h
        .get_all_by_label("Time")
        .map(|n| n.rect())
        .find(|r| r.top() > pane.bottom())
        .expect("the Time header");
    assert!(pane.right() - time.left() < 120.0, "{time:?} {pane:?}");
    let row = title(&h, ids(&h)[0]);
    h.get_by_label(&row).click();
    steps(&mut h, 3);
    let state = |h: &H| h.state().panels.library.tables.0[&TableView::Library].clone();
    assert!(state(&h).sized.is_empty());

    let edge = Pos2::new(artist.left() - 4.0, artist.center().y);
    drag(&mut h, edge, edge + egui::vec2(60.0, 0.0), |_| {});
    let s = state(&h);
    assert_eq!(
        s.sized.iter().copied().collect::<Vec<_>>(),
        [ColumnId::Title]
    );
    let w = s.columns.iter().find(|c| c.0 == ColumnId::Title).unwrap().1;
    assert!(w > auto[&ColumnId::Title] + 30.0, "{w}");
}

/// Keylock and quantise come back as they were after a restart, per player.
#[test]
fn deck_options_survive_a_restart() {
    use crate::core::settings::{self, DeckOptions};
    let mut h = start_with("deck-options", stand_in);
    let deck1 = DeckId::new(0).unwrap();
    h.state_mut()
        .dispatch((deck1, EngineCmd::Keylock(earshot_engine::Set::On)).into());
    h.state_mut().dispatch(
        (
            DeckId::PREPARE,
            EngineCmd::Quantize(earshot_engine::Set::Off),
        )
            .into(),
    );
    let want = |o: &[DeckOptions]| {
        o.first().is_some_and(|d| d.keylock)
            && o.get(DeckId::PREPARE.index()).is_some_and(|d| !d.quantize)
    };
    settle_until(&mut h, |a| {
        settings::read::<Vec<DeckOptions>>(&a.lib.store.reader, settings::DECK_OPTIONS)
            .is_some_and(|o| want(&o))
    });
    drop(h);

    // The same database, a new app.
    let dir = scratch_dir("deck-options");
    let services = Services {
        db: dir.join(store::FILE_NAME),
        null_audio: true,
        worker: stand_in,
        midi: false,
    };
    let mut h = egui_kittest::Harness::builder()
        .with_size(vec2(1700.0, 1000.0))
        .build_eframe(move |cc| EarshotApp::new(cc, services).expect("app"));
    settle_until(&mut h, |a| {
        a.frame
            .snap
            .as_ref()
            .is_some_and(|s| s.deck(deck1).keylock && !s.deck(DeckId::PREPARE).quantize)
    });
}

/// The deck's tabs switch between the player's controls and the grid editor without
/// moving anything: CUE and the tabs stay put.
#[test]
fn deck_pages_switch_in_place() {
    let mut h = start("deck-pages");
    let id = ids(&h)[0];
    h.state_mut().dispatch(LibraryCmd::Load(id).into());
    settle_until(&mut h, |a| a.panels.decks[0].wave.is_some());
    steps(&mut h, 5);
    let place = |h: &H| {
        let first = |label: &str| h.get_all_by_label(label).next().unwrap().rect();
        (
            first("CUE"),
            first("Player controls"),
            first("Grid controls"),
            first("Stem controls"),
        )
    };
    let before = place(&h);
    h.get_all_by_label("Grid controls").next().unwrap().click();
    steps(&mut h, 5);
    assert!(
        h.query_all_by_label("Anchor 1 here").count() > 0,
        "the grid editor"
    );
    assert_eq!(place(&h), before);
    // Stems have a tab of their own.
    h.get_all_by_label("Stem controls").next().unwrap().click();
    steps(&mut h, 5);
    assert!(h.query_all_by_label("STEMS").count() > 0, "the stems page");
    assert_eq!(place(&h), before);
    render(&mut h, "deck-pages-stems");
    h.get_all_by_label("Player controls")
        .next()
        .unwrap()
        .click();
    steps(&mut h, 5);
    assert_eq!(place(&h), before);
    assert_eq!(
        h.query_all_by_label("STEMS").count(),
        0,
        "not on the player page"
    );
}

/// Vinyl mode turns on and off from its VINYL label only, not a click on the ring.
#[test]
fn only_the_vinyl_label_toggles_vinyl_mode() {
    let mut h = start("vinyl");
    steps(&mut h, 5);
    let deck = DeckId::new(0).unwrap();
    let vinyl = |a: &EarshotApp| a.frame.snap.as_ref().unwrap().deck(deck).vinyl;
    let on = vinyl(h.state());
    // The middle of the ring (its BPM): nothing.
    let label = h.get_all_by_label("VINYL").next().unwrap().rect();
    let ring = label.center() + vec2(0.0, 25.0);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos: ring,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    steps(&mut h, 5);
    assert_eq!(vinyl(h.state()), on);
    h.get_all_by_label("VINYL").next().unwrap().click();
    settle_until(&mut h, |a| vinyl(a) != on);
}

/// The grid editor's BPM box: a BPM typed in is set on Enter.
#[test]
fn typing_a_bpm_in_the_grid_editor_sets_it() {
    let mut h = start("grid-bpm");
    let id = ids(&h)[0];
    h.state_mut().dispatch(LibraryCmd::Load(id).into());
    settle_until(&mut h, |a| a.panels.decks[0].wave.is_some());
    steps(&mut h, 5);
    h.get_all_by_label("Grid controls").next().unwrap().click();
    steps(&mut h, 5);
    let bpm = |a: &EarshotApp| a.index.get(id).unwrap().bpm;
    let shown = format!("{:.2}", bpm(h.state()).unwrap());
    let field = h.get_all_by_value(&shown).next().unwrap();
    field.click();
    steps(&mut h, 2);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.get_all_by_value(&shown)
        .next()
        .unwrap()
        .type_text("128.5");
    steps(&mut h, 2);
    h.key_press(egui::Key::Enter);
    settle_until(&mut h, |a| bpm(a) == Some(128.5));
}

/// Hot cues play on when pressed unless Settings > Decks says "Play while held"; the
/// choice is saved and reaches the player.
#[test]
fn hotcue_mode_setting() {
    use crate::core::settings;
    use crate::core::view::SettingsTab;
    use earshot_engine::HotcueMode;
    let mut h = start("hotcue-mode");
    assert_eq!(
        h.state().player.lock().unwrap().hotcue_mode,
        HotcueMode::PlayOnPress
    );
    h.state_mut()
        .dispatch(ViewCmd::Settings(SettingsTab::Decks).into());
    steps(&mut h, 3);
    render(&mut h, "settings-decks");
    h.get_by_label("Play while held").click();
    steps(&mut h, 3);
    assert_eq!(
        h.state().player.lock().unwrap().hotcue_mode,
        HotcueMode::PlayWhileHeld
    );
    settle_until(&mut h, |a| {
        settings::read::<HotcueMode>(&a.lib.store.reader, settings::HOTCUE_MODE)
            == Some(HotcueMode::PlayWhileHeld)
    });
}

/// Settings > Decks can cue new loads at the track's start instead of its first
/// beat; the choice is saved and reaches the player, and a load then cues at 0 s.
#[test]
fn cue_first_beat_setting() {
    use crate::core::settings;
    use crate::core::view::SettingsTab;
    let mut h = start("cue-first-beat");
    assert!(h.state().player.lock().unwrap().cue_first_beat);
    h.state_mut()
        .dispatch(ViewCmd::Settings(SettingsTab::Decks).into());
    steps(&mut h, 3);
    h.get_by_label("Automatically set initial cue to first detected beat")
        .click();
    steps(&mut h, 3);
    assert!(!h.state().player.lock().unwrap().cue_first_beat);
    settle_until(&mut h, |a| {
        settings::read::<bool>(&a.lib.store.reader, settings::CUE_FIRST_BEAT) == Some(false)
    });

    let id = ids(&h)[1];
    h.state_mut()
        .dispatch(ViewCmd::SetMode(Mode::Prepare).into());
    h.state_mut().dispatch(LibraryCmd::Load(id).into());
    settle_until(&mut h, |a| a.frame.deck(DeckId::PREPARE).grid.is_some());
    let snap = h.state().frame.snap.as_ref().unwrap().prepare;
    assert_eq!(snap.cue_point, 0.0);
}

/// Prepare's Export tab draws: what to export on the left, where to on the right.
#[test]
fn export_tab() {
    use egui_kittest::kittest::Queryable;
    let mut h = start("export_tab");
    h.state_mut()
        .dispatch(ViewCmd::SetMode(Mode::Prepare).into());
    h.state_mut()
        .dispatch(ViewCmd::Tool(crate::core::view::Tool::Export).into());
    steps(&mut h, 3);
    h.get_by_label("From");
    h.get_by_label("To");
    // The tab and its button.
    assert_eq!(h.query_all_by_label("Export").count(), 2);
    render(&mut h, "export_tab");
}

/// A stick plugged in gets an icon of its own, with its light; its pane shows the
/// stick in the middle with its files, and a job holding it turns the light red.
#[test]
fn usb_stick() {
    use egui_kittest::kittest::Queryable;
    let stick = scratch_dir("STICK");
    let _ = std::fs::remove_dir_all(&stick);
    std::fs::create_dir_all(stick.join("Contents/Artist")).unwrap();
    std::fs::write(stick.join("Contents/Artist/a.mp3"), b"not audio").unwrap();
    std::fs::write(stick.join("notes.txt"), b"").unwrap();
    let drive = crate::core::drives::Drive {
        path: stick.clone(),
        label: "STICK".into(),
        device: String::new(),
        fstype: "vfat".into(),
        total_bytes: 32_000_000_000,
        free_bytes: 12_300_000_000,
        writable: true,
        libraries: vec![],
        empty: false,
    };
    let mut h = start("usb_stick");
    let id = drive.id();
    h.state_mut().drives_changed(vec![drive]);
    steps(&mut h, 2);
    h.get_by_label("STICK: safe to remove").click();
    steps(&mut h, 3);
    assert_eq!(
        h.state().prefs.layout().source,
        Some(crate::core::view::Source::Stick(id))
    );
    h.get_by_label("Safe to remove");
    h.get_by_label("📁 Contents").click();
    steps(&mut h, 3);
    h.get_by_label("📁 Artist");
    let hold = h.state().uses.hold(&stick);
    steps(&mut h, 2);
    h.get_by_label("In use");
    render(&mut h, "usb_stick");
    drop(hold);
    let _ = std::fs::remove_dir_all(&stick);
}

/// While tracks are analysed the status line along the bottom says so, with a bar,
/// and the top bar has a jobs pill; once they're done both are gone.
#[test]
fn the_status_line_shows_whats_running() {
    use egui_kittest::kittest::Queryable;
    let mut h = start("status_line");
    let ids = ids(&h);
    h.state_mut().dispatch(
        LibraryCmd::Analyse {
            tracks: ids.clone(),
            dynamic: false,
        }
        .into(),
    );
    settle_until(&mut h, |a| {
        a.jobs.list().iter().any(|j| j.progress.is_some())
    });
    steps(&mut h, 1);
    let jobs = h.state().jobs.list();
    assert!(jobs[0].title.starts_with("Analysing "), "{jobs:?}");
    h.get_by_label_contains(&jobs[0].title);
    let n = jobs.len();
    h.get_by_label(&format!("{n} job{}", if n == 1 { "" } else { "s" }))
        .click();
    steps(&mut h, 2);
    h.get_by_label("Stop analysis");
    render(&mut h, "status_line");
    settle_until(&mut h, |a| a.analysis.busy.is_empty());
    steps(&mut h, 2);
    assert!(h.state().jobs.list().is_empty());
    assert!(h.query_by_label_contains("job").is_none());
}

/// A stick with files on it isn't ready for a library: its pane says so and offers
/// to format it; a playlist's "Send to" opens the add window, which offers to format
/// first, and the format window says what's lost. (The stick here has no device, so
/// formatting it fails, harmlessly.) Its icon's menu has Format and Eject.
#[test]
fn a_stick_with_files_is_formatted_first() {
    use egui_kittest::kittest::Queryable;
    let stick = scratch_dir("FILES");
    let _ = std::fs::remove_dir_all(&stick);
    std::fs::create_dir_all(&stick).unwrap();
    std::fs::write(stick.join("notes.txt"), b"").unwrap();
    let drive = crate::core::drives::Drive {
        path: stick.clone(),
        label: "Files".into(),
        device: String::new(),
        fstype: "vfat".into(),
        total_bytes: 32_000_000_000,
        free_bytes: 12_000_000_000,
        writable: true,
        libraries: vec![],
        empty: false,
    };
    let mut h = start("format_stick");
    let ids = ids(&h);
    let id = drive.id();
    make(&mut h, CollectionKind::Playlist, "Friday", vec![ids[0]]);
    h.state_mut().drives_changed(vec![drive.clone()]);
    h.state_mut()
        .dispatch(ViewCmd::Source(Some(Source::Stick(id))).into());
    steps(&mut h, 3);
    // The pane's and the middle's.
    assert_eq!(
        h.query_all_by_label_contains("It has other files on it.")
            .count(),
        2
    );
    assert_eq!(h.query_all_by_label("Format this stick…").count(), 3);

    h.state_mut()
        .dispatch(ViewCmd::Source(Some(Source::Playlists)).into());
    steps(&mut h, 3);
    h.get_by_label("Friday").click_secondary();
    steps(&mut h, 2);
    h.get_by_label("Send to Files ⏵").click();
    steps(&mut h, 2);
    h.get_by_label("as Rekordbox (format it first)…").click();
    steps(&mut h, 3);
    h.get_by_label_contains("Files isn't ready for a Rekordbox library.");
    h.get_by_label("Format…").click();
    steps(&mut h, 3);
    h.get_by_label_contains("Everything on it is erased (20.0 GB used now)");
    h.get_by_label("Then \"Friday\" is added to it.");
    render(&mut h, "format_stick");
    h.get_by_label("Erase and format").click();
    settle_until(&mut h, |a| {
        a.panels.library.external.format_result().is_some()
    });
    steps(&mut h, 2);
    h.get_by_label_contains("Formatting failed: earshot can't tell which device");
    h.get_by_label("Close").click();
    steps(&mut h, 2);
    assert!(h.query_by_label("Erase and format").is_none());

    // The real drives were listed after formatting: this stick isn't one.
    h.state_mut().drives_changed(vec![drive]);
    steps(&mut h, 2);
    h.get_by_label("Files: safe to remove").click_secondary();
    steps(&mut h, 2);
    h.get_by_label("Eject");
    h.get_by_label("Format this stick…").click();
    steps(&mut h, 2);
    h.get_by_label("Erase and format");
    let _ = std::fs::remove_dir_all(&stick);
}

/// An outside track (another library's, not in earshot's) plays on a deck with its
/// library's grid and cues; edits to it last for the session, and nothing about it
/// is saved but its waveform (in the file cache).
#[test]
fn outside_track_on_a_deck() {
    use crate::core::external::{ExtCue, ExtTrack, Marker};
    let mut h = start("outside_track");
    let file = {
        let a = h.state();
        let id = ids(&h)[0];
        PathBuf::from(&a.index.get(id).unwrap().path)
    };
    let cue = |role: &str, start_s, slot| ExtCue {
        role: role.into(),
        start_s,
        end_s: None,
        slot,
        name: "drop".into(),
        colour: None,
    };
    let ext = ExtTrack {
        id: "mixxx:7".into(),
        location: file.to_string_lossy().into(),
        file: Some(file.clone()),
        title: "From Mixxx".into(),
        artist: "Someone".into(),
        album: String::new(),
        genre: String::new(),
        comment: String::new(),
        bpm: Some(120.0),
        key: Some("Am".into()),
        rating: 0,
        colour: None,
        duration_s: None,
        cues: vec![cue("cue", 2.0, Some(0)), cue("cue", 3.0, None)],
        grid: vec![Marker {
            start_s: 0.25,
            bpm: 120.0,
            beat: 1,
        }],
    };
    let outside = h.state().lib.store.outside.clone();
    let id = outside.register(99, &ext, false).unwrap();
    assert!(id < 0);
    assert_eq!(
        outside.register(99, &ext, false),
        Some(id),
        "the same id each read"
    );

    let deck = h.state().prefs.decks()[0];
    h.state_mut().load(Some(deck), id);
    settle_until(&mut h, |a| a.panels.decks[deck.index()].wave.is_some());
    {
        let inner = h.state().player.lock().unwrap();
        let s = inner.session(deck);
        assert_eq!(s.track_id, Some(id));
        assert_eq!(s.title, "From Mixxx");
    }
    let grid = h
        .state()
        .player
        .lock()
        .unwrap()
        .session(deck)
        .grid
        .clone()
        .unwrap();
    assert!(
        (grid.time_at(0.0) - 0.25).abs() < 1e-6,
        "the library's grid"
    );
    settle_until(&mut h, |a| {
        a.cues
            .get(&id)
            .is_some_and(|c| c.memories.len() == 1 && c.hot_labels[0].as_deref() == Some("drop"))
    });

    // An edit: kept for the session, not saved.
    h.state()
        .lib
        .store
        .send(crate::core::store::StoreCmd::AddMemory {
            track: id,
            start: 5.0,
            end: None,
        });
    settle_until(&mut h, |a| {
        a.cues.get(&id).is_some_and(|c| c.memories.len() == 2)
    });
    let reader = h.state().lib.store.reader.clone();
    let saved: i64 = reader
        .with(|c| {
            Ok(c.query_row(
                "SELECT (SELECT count(*) FROM cues WHERE track_id < 0)
                      + (SELECT count(*) FROM blobs WHERE track_id < 0)
                      + (SELECT count(*) FROM track_features WHERE track_id < 0)",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(saved, 0, "nothing saved for an outside track");
    let key = crate::core::player::file_key(&file).unwrap();
    settle_until(&mut h, |_| reader.file_cache(&key).unwrap().is_some());
}

/// Another library's tracks show in earshot's own track table: one earshot has is
/// that track, one it hasn't is an outside track, which loads onto a deck.
#[test]
fn library_tracks_in_the_table() {
    use std::sync::Arc;

    use crate::core::external::{ExtLibrary, ExtTrack, ExternalEvent, Node, NodeKind};
    use crate::core::sources::{Source as Src, SourceConfig, SourceKind};
    let mut h = start("library_tracks");
    let ours = ids(&h)[0];
    let ours_path = PathBuf::from(&h.state().index.get(ours).unwrap().path);
    // A file earshot hasn't got: a copy of one it has, elsewhere.
    let dir = scratch_dir("library_tracks");
    std::fs::create_dir_all(&dir).unwrap();
    let theirs = dir.join("theirs.wav");
    std::fs::copy(&ours_path, &theirs).unwrap();
    let t = |id: &str, file: &PathBuf, title: &str| ExtTrack {
        id: id.into(),
        location: file.to_string_lossy().into(),
        file: Some(file.clone()),
        title: title.into(),
        artist: "Mixxx person".into(),
        album: String::new(),
        genre: String::new(),
        comment: String::new(),
        bpm: Some(126.0),
        key: Some("8A".into()),
        rating: 0,
        colour: None,
        duration_s: None,
        cues: vec![],
        grid: vec![],
    };
    let lib = ExtLibrary::for_test(
        vec![
            t("1", &ours_path, "Known"),
            t("2", &theirs, "Only in Mixxx"),
        ],
        vec![Node {
            name: "Set".into(),
            kind: NodeKind::Playlist,
            children: vec![],
            tracks: vec!["2".into()],
        }],
    );
    h.state()
        .lib
        .store
        .send(crate::core::store::StoreCmd::AddSource(
            crate::core::sources::NewSource {
                kind: SourceKind::Mixxx,
                name: "Mixxx".into(),
                location: Some(dir.clone()),
                config: SourceConfig::default(),
            },
        ));
    settle_until(&mut h, |a| {
        a.sources.iter().any(|s| s.kind == SourceKind::Mixxx)
    });
    let source: Src = h
        .state()
        .sources
        .iter()
        .find(|s| s.kind == SourceKind::Mixxx)
        .cloned()
        .unwrap();
    h.state_mut().external_event(ExternalEvent::Read {
        source: source.id,
        result: Ok(Arc::new(lib)),
    });
    let outside = h
        .state()
        .index
        .outside()
        .find(|t| t.title == "Only in Mixxx")
        .map(|t| t.id)
        .expect("an outside track");
    assert!(outside < 0);
    assert!(
        h.state().index.tracks().iter().all(|t| t.id >= 0),
        "the library itself has only earshot's tracks"
    );

    h.state_mut().dispatch(
        ViewCmd::Source(Some(crate::core::view::Source::Program(SourceKind::Mixxx))).into(),
    );
    steps(&mut h, 3);
    h.get_by_label("All tracks (2)").click();
    steps(&mut h, 3);
    h.get_by_label("Only in Mixxx");
    // The one earshot has is earshot's track, under its own title.
    let title = h.state().index.get(ours).unwrap().title.clone();
    h.get_by_label(&title);
    render(&mut h, "library_tracks");

    let deck = h.state().prefs.decks()[0];
    h.state_mut()
        .dispatch(Command::Deck(deck, DeckCmd::Load(outside)));
    settle_until(&mut h, |a| {
        a.player.lock().unwrap().session(deck).track_id == Some(outside)
            && a.panels.decks[deck.index()].wave.is_some()
    });

    // "Show in library" from elsewhere comes back to Mixxx's list, with it selected.
    let mixxx = crate::core::view::Source::Program(SourceKind::Mixxx);
    h.state_mut()
        .dispatch(ViewCmd::Source(Some(crate::core::view::Source::AllTracks)).into());
    steps(&mut h, 2);
    h.state_mut().dispatch(ViewCmd::ShowInLibrary(deck).into());
    settle_until(&mut h, |a| {
        a.prefs.layout().source == Some(mixxx)
            && a.panels.library.external.focused() == Some(outside)
    });

    // Into a playlist: it asks, then imports it and the playlist has earshot's track.
    let save = Command::from(crate::core::collections::CollectionCmd::SavePlaylist {
        name: "From Mixxx".into(),
        tracks: vec![outside],
    });
    h.state_mut().dispatch(save.clone());
    steps(&mut h, 2);
    h.get_by_label("Don't ask again");
    h.state_mut().dispatch(
        crate::core::external::ExternalCmd::ImportNow {
            tracks: vec![outside],
            then: Box::new(save),
            remember: false,
        }
        .into(),
    );
    settle_until(&mut h, |a| {
        a.collections.children(None).iter().any(|c| {
            c.name == "From Mixxx"
                && c.tracks.len() == 1
                && c.tracks[0] > 0
                && a.index
                    .get(c.tracks[0])
                    .is_some_and(|t| t.path == theirs.to_string_lossy())
        })
    });

    // Mixxx switched off: its track can't be shown, and the bottom bar says why.
    h.state_mut()
        .dispatch(LibraryCmd::RemoveSource(source.id).into());
    settle_until(&mut h, |a| {
        a.sources.iter().all(|s| s.id != source.id || !s.enabled)
    });
    h.state_mut().dispatch(ViewCmd::ShowInLibrary(deck).into());
    steps(&mut h, 2);
    h.get_by_label_contains("isn't there any more, so it can't be shown.");
    render(&mut h, "notice");
    let _ = std::fs::remove_dir_all(&dir);
}

/// At the narrowest the window can be, nothing in any view goes off its right edge:
/// Perform with two decks or four, and Prepare.
#[test]
fn everything_fits_at_the_narrowest_window() {
    use egui::accesskit;
    use egui_kittest::kittest::{NodeT, Queryable, by};
    let mut h = start("narrowest");
    let ids = ids(&h);
    for (i, d) in DeckId::first(4).enumerate() {
        h.state_mut().load(Some(d), ids[i]);
    }
    settle_until(&mut h, |a| {
        DeckId::first(4).all(|d| a.panels.decks[d.index()].wave.is_some())
    });
    h.set_size(vec2(crate::MIN_WIDTH, 900.0));
    for (mode, four) in [
        (Mode::Perform, false),
        (Mode::Perform, true),
        (Mode::Prepare, false),
    ] {
        h.state_mut().dispatch(ViewCmd::SetMode(mode).into());
        h.state_mut().dispatch(ViewCmd::FourDecks(four).into());
        steps(&mut h, 30);
        // Every widget with a place on screen (text runs are left out: a
        // right-aligned label's is reported further right than it's drawn).
        let past: Vec<String> = h
            .query_all(by())
            .filter(|n| n.accesskit_node().role() != accesskit::Role::TextRun)
            .filter(|n| {
                n.accesskit_node()
                    .bounding_box()
                    .is_some_and(|b| b.x1 as f32 > crate::MIN_WIDTH + 0.5)
            })
            .map(|n| format!("{n:?}"))
            .collect();
        assert!(past.is_empty(), "{mode:?}, four decks {four}: {past:#?}");
    }
}

/// "Show in library" goes back to the list a deck's track was loaded from (the same
/// file can be in many) and selects it there; once that list no longer has it, it's
/// shown in All tracks.
#[test]
fn show_in_library_goes_back_to_where_it_came_from() {
    use crate::ui::panels::library::browser::Scope;
    let mut h = start_with("show-in-library", stand_in);
    let track = ids(&h)[1];
    let boxed = make(&mut h, CollectionKind::Crate, "Box", vec![track]);
    let deck = DeckId::new(0).unwrap();
    h.state_mut()
        .dispatch(ViewCmd::Source(Some(Source::Crates)).into());
    let lib = &mut h.state_mut().panels.library;
    lib.browser.set_scope(Scope::Collection(boxed));
    lib.focused_pane = crate::core::view::Pane::Library;
    h.state_mut()
        .dispatch(Command::Deck(deck, DeckCmd::Load(track)));
    let on = |a: &EarshotApp| a.player.lock().unwrap().session(deck).track_id;
    settle_until(&mut h, |a| on(a) == Some(track));

    // Looking somewhere else meanwhile.
    h.state_mut()
        .dispatch(ViewCmd::Source(Some(Source::AllTracks)).into());
    h.state_mut().panels.library.browser.set_scope(Scope::All);
    steps(&mut h, 2);
    h.state_mut().dispatch(ViewCmd::ShowInLibrary(deck).into());
    let shown_in = |a: &EarshotApp, scope: &Scope, source| {
        let lib = &a.panels.library;
        lib.browser.scope() == scope
            && lib.browser.focused() == Some(track)
            && a.prefs.layout().source == Some(source)
    };
    settle_until(&mut h, |a| {
        shown_in(a, &Scope::Collection(boxed), Source::Crates)
    });

    // Taken out of the crate: All tracks has it.
    h.state().lib.store.send(StoreCmd::RemoveTracks {
        id: boxed,
        tracks: vec![track],
    });
    settle_until(&mut h, |a| {
        a.collections
            .get(boxed)
            .is_some_and(|c| c.tracks.is_empty())
    });
    h.state_mut().dispatch(ViewCmd::ShowInLibrary(deck).into());
    settle_until(&mut h, |a| shown_in(a, &Scope::All, Source::AllTracks));
}

/// Prepare isn't opened while a deck plays (the bottom bar says why); going back to
/// Perform stops Prepare's player; closing the window while a deck plays asks first.
#[test]
fn mode_switches_and_quitting_mind_whats_playing() {
    use egui_kittest::kittest::Queryable;
    let mut h = start_with("mode-switch", stand_in);
    let id = ids(&h)[0];
    let deck = h.state().prefs.decks()[0];
    h.state_mut().load(Some(deck), id);
    settle_until(&mut h, |a| a.panels.decks[deck.index()].wave.is_some());
    let play =
        |d, on: bool| Command::from((d, EngineCmd::Play(earshot_engine::Set::from_bool(on))));
    let playing = |a: &EarshotApp, d| a.frame.snap.as_ref().is_some_and(|sh| sh.deck(d).playing);
    h.state_mut().dispatch(play(deck, true));
    settle_until(&mut h, |a| playing(a, deck));

    h.state_mut()
        .dispatch(ViewCmd::SetMode(Mode::Prepare).into());
    steps(&mut h, 2);
    assert_eq!(h.state().prefs.mode, Mode::Perform);
    h.get_by_label_contains("Stop the playing decks before switching to Prepare.");

    // Closing the window: asked, and Cancel keeps it open.
    h.input_mut()
        .viewports
        .entry(egui::ViewportId::ROOT)
        .or_default()
        .events
        .push(egui::ViewportEvent::Close);
    steps(&mut h, 2);
    h.get_by_label("Do you really want to quit?");
    h.get_by_label("Cancel").click();
    steps(&mut h, 2);
    assert!(h.query_by_label("Do you really want to quit?").is_none());

    h.state_mut().dispatch(play(deck, false));
    settle_until(&mut h, |a| !playing(a, deck));
    h.state_mut()
        .dispatch(ViewCmd::SetMode(Mode::Prepare).into());
    assert_eq!(h.state().prefs.mode, Mode::Prepare);
    let prepare = DeckId::PREPARE;
    h.state_mut().load(Some(prepare), id);
    settle_until(&mut h, |a| a.panels.decks[prepare.index()].wave.is_some());
    h.state_mut().dispatch(play(prepare, true));
    settle_until(&mut h, |a| playing(a, prepare));
    h.state_mut()
        .dispatch(ViewCmd::SetMode(Mode::Perform).into());
    assert_eq!(h.state().prefs.mode, Mode::Perform);
    settle_until(&mut h, |a| !playing(a, prepare));
}

/// A track dropped anywhere on Prepare's player panel loads, as on a Perform deck
/// (not just on its waveform).
#[test]
fn a_track_drops_onto_prepares_player_panel() {
    let mut h = start_prepare("prepare-drop", 0);
    let other = ids(&h)[1];
    h.state_mut()
        .dispatch(ViewCmd::Source(Some(Source::AllTracks)).into());
    steps(&mut h, 3);
    let (from, to) = (at(&h, &title(&h, other)).center(), at(&h, "Q").center());
    drag(&mut h, from, to, |_| {});
    settle_until(&mut h, |a| prepared(a) == Some(other));
}
