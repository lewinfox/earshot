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

/// The 2 / 4 toggle shows four decks (and their mixer channels), saved in the
/// preferences.
#[test]
fn four_decks() {
    use egui_kittest::kittest::Queryable;
    let mut h = start("four-decks");
    assert!(!h.state().prefs.four_decks);
    h.get_by_label("4 decks").click();
    steps(&mut h, 3);
    assert!(h.state().prefs.four_decks);
    assert_eq!(h.state().prefs.decks().len(), 4);
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
    h.get_all_by_label("Player controls")
        .next()
        .unwrap()
        .click();
    steps(&mut h, 5);
    assert_eq!(place(&h), before);
}

/// Hot cues play on when pressed unless Settings > Decks says "Play while held"; the
/// choice is saved and reaches the player.
#[test]
fn hotcue_mode_setting() {
    use crate::core::settings;
    use crate::core::view::SettingsTab;
    use earshot_engine::HotcueMode;
    let mut h = start("hotcue-mode");
    assert_eq!(h.state().player.lock().unwrap().hotcue_mode, HotcueMode::PlayOnPress);
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
