//! The whole app, started on the null audio output, a database in a temporary
//! folder, a stand-in analysis worker and a few generated tracks. Each test drives
//! it through commands; with `EARSHOT_SHOT=/tmp/shot.png` set, each also renders the
//! window off-screen and writes it next to that path (`/tmp/shot-perform.png`, …).

use std::path::{Path, PathBuf};
use std::time::Duration;

use egui::vec2;

use super::*;
use crate::core::worker_client::testing::stand_in;

type H = egui_kittest::Harness<'static, EarshotApp>;

/// A folder of its own for one test: the database and the music.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("earshot-app-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("music")).unwrap();
    dir
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
fn start(test: &str) -> H {
    let dir = scratch(test);
    for (title, bpm) in TRACKS {
        write_track(&dir.join("music").join(format!("{title}.wav")), bpm, 24.0);
    }
    let services = Services {
        db: dir.join(store::FILE_NAME),
        null_audio: true,
        worker: stand_in,
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

/// Step frames (in real time: the work happens on other threads) until `ok` holds.
fn settle_until(h: &mut H, ok: impl Fn(&EarshotApp) -> bool) {
    for _ in 0..1000 {
        h.step();
        if ok(h.state()) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out");
}

fn steps(h: &mut H, n: usize) {
    for _ in 0..n {
        h.step();
        std::thread::sleep(Duration::from_millis(16));
    }
}

fn ids(h: &H) -> Vec<i64> {
    h.state().index.tracks().iter().map(|t| t.id).collect()
}

fn render(h: &mut H, test: &str) {
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
    let decks: Vec<DeckId> = DeckId::first(DECKS).collect();
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

/// The Flow tab with two cards and the "+" suggestions open.
#[test]
fn flow() {
    let mut h = start("flow");
    let ids: Vec<i64> = ids(&h).into_iter().take(2).collect();
    h.state_mut().dispatch(FlowCmd::AddTracks(ids).into());
    h.state_mut().flow().fit();
    steps(&mut h, 10);
    h.state_mut().flow().open_first_suggestions();
    steps(&mut h, 5);
    render(&mut h, "flow");
    assert!(h.state().panels.library.flow.open);
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
    app.dispatch(ViewCmd::ToggleModal(crate::core::view::Modal::Audio).into());
    assert!(app.modals.is_open(crate::core::view::Modal::Audio));
    app.dispatch(ViewCmd::CloseModal(crate::core::view::Modal::Audio).into());
    assert!(!app.modals.is_open(crate::core::view::Modal::Audio));
    steps(&mut h, 3);
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
            .library
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
}
