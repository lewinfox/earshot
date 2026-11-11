//! The Tempo lock tab, driven through the window: the track menu opens it, the stand-in
//! worker's chart and straightened file reach it, Straighten refuses a wild BPM, A/B
//! playback keeps its place, and the straightened file is saved into a library folder.

use egui_kittest::kittest::{NodeT, Queryable};

use super::*;
use crate::core::tempo_lock::{TempoLockCmd, Version};
use crate::core::view::Tool;
use crate::ui::panels::tempo_lock::Note;

/// Right-click `title` in the library and choose "Normalise tempo"; wait for the chart.
fn normalise(h: &mut H, title: &str) {
    h.get_all_by_label(title)
        .next()
        .expect("the track's row")
        .click_secondary();
    steps(h, 3);
    h.get_by_label("Normalise tempo").click();
    settle_until(h, |a| a.panels.tempo_lock.chart.is_some());
    steps(h, 3);
}

fn straighten_button<'a>(h: &'a H) -> egui_kittest::Node<'a> {
    h.get_all_by_label("Straighten")
        .find(|n| n.accesskit_node().role() == egui::accesskit::Role::Button)
        .expect("the Straighten button")
}

#[test]
fn normalise_tempo_opens_the_tab_and_analyses() {
    let mut h = start("tempo-menu");
    let id = h
        .state()
        .index
        .tracks()
        .iter()
        .find(|t| t.title == "Night Bus")
        .unwrap()
        .id;
    normalise(&mut h, "Night Bus");
    let app = h.state();
    assert_eq!(app.prefs.mode, Mode::Prepare);
    assert_eq!(app.prefs.layout().tool, Tool::TempoLock);
    assert_eq!(app.tempo_lock.current().track, Some(id));
    let tab = &app.panels.tempo_lock;
    assert_eq!(tab.track, Some(id));
    let a = &tab.chart.as_ref().unwrap().analysis;
    // The tab offers the rounded median, and shows the stand-in's stats and warning.
    assert_eq!(tab.bpm, a.suggested_bpm);
    assert!(tab.busy.is_none());
    assert_eq!(a.tempo_changes.len(), 1);
    h.get_by_label("MEDIAN TEMPO");
    h.get_by_label("A tempo change that looks deliberate:");
}

#[test]
fn a_track_dragged_onto_the_tab_is_analysed() {
    let mut h = start("tempo-drop");
    let app = h.state_mut();
    app.dispatch(ViewCmd::SetMode(Mode::Prepare).into());
    app.dispatch(ViewCmd::Tool(Tool::TempoLock).into());
    steps(&mut h, 3);
    let id = h
        .state()
        .index
        .tracks()
        .iter()
        .find(|t| t.title == "Low Orbit")
        .unwrap()
        .id;
    let from = at(&h, "Low Orbit").center();
    let to = h
        .get_all_by_label_contains("Drag a track here")
        .next()
        .expect("the hint")
        .rect()
        .center();
    drag(&mut h, from, to, |_| {});
    settle_until(&mut h, |a| a.panels.tempo_lock.chart.is_some());
    assert_eq!(h.state().panels.tempo_lock.track, Some(id));
}

#[test]
fn straighten_is_disabled_for_a_bpm_out_of_range() {
    let mut h = start("tempo-range");
    normalise(&mut h, "Glass Steps");
    assert!(!straighten_button(&h).accesskit_node().is_disabled());
    for wild in [300.0, 10.0] {
        h.state_mut().panels.tempo_lock.bpm = wild;
        steps(&mut h, 2);
        assert!(
            straighten_button(&h).accesskit_node().is_disabled(),
            "{wild} BPM"
        );
    }
    // Half-time: twice the detected tempo is fine.
    let median = h
        .state()
        .panels
        .tempo_lock
        .chart
        .as_ref()
        .unwrap()
        .analysis
        .median_bpm;
    let tab = &mut h.state_mut().panels.tempo_lock;
    tab.level = crate::core::tempo_lock::BeatLevel::HalfTime;
    tab.bpm = (median * 2.0).round();
    steps(&mut h, 2);
    assert!(!straighten_button(&h).accesskit_node().is_disabled());
}

/// Straighten, listen to both versions, save into the library. Writes the picture
/// (`EARSHOT_SHOT`) once everything is charted.
#[test]
fn straighten_listen_and_save() {
    let mut h = start("tempo-lock");
    normalise(&mut h, "Basement Hum");
    straighten_button(&h).click();
    settle_until(&mut h, |a| a.panels.tempo_lock.measured.is_some());
    let s = h.state().panels.tempo_lock.straightened.clone().unwrap();
    assert!(s.output.exists());
    assert_eq!(
        s.output.file_name().unwrap().to_string_lossy(),
        format!("Basement Hum (straightened {} bpm).wav", s.bpm)
    );
    steps(&mut h, 3);
    h.get_by_label("OFF THE GRID");

    // The charted track went into Prepare's player.
    let original = h.state().panels.tempo_lock.track.unwrap();
    let in_player = |a: &EarshotApp| {
        a.frame
            .snap
            .as_ref()
            .and_then(|s| s.deck(DeckId::PREPARE).track_id)
    };
    settle_until(&mut h, |a| {
        a.panels.tempo_lock.player == Some(Version::Original)
    });
    assert_eq!(in_player(h.state()), Some(original));
    // Its detected beats over the player's waveform.
    assert!(h.state().panels.decks[DeckId::PREPARE.index()].has_overlay("tempo-lock"));

    // A/B: play the original, then the straightened version from the same place in
    // the music; it plays on.
    let listen = |version, at| Command::TempoLock(TempoLockCmd::Listen { version, at });
    h.state_mut().dispatch(listen(Version::Original, 2.0));
    h.state_mut()
        .dispatch((DeckId::PREPARE, EngineCmd::Play(Set::On)).into());
    settle_until(&mut h, |a| {
        let p = a.frame.snap.as_ref().unwrap().deck(DeckId::PREPARE);
        p.playing && (p.position - 2.0).abs() < 1.0
    });
    let at = s.map_time(20.0, false);
    h.state_mut().dispatch(listen(Version::Straightened, at));
    settle_until(&mut h, |a| {
        let p = a.frame.snap.as_ref().unwrap().deck(DeckId::PREPARE);
        a.panels.tempo_lock.player == Some(Version::Straightened)
            && !a.panels.tempo_lock.loading
            && p.playing
            && (p.position - at).abs() < 1.0
    });
    // On its steady grid, at the BPM it was straightened to.
    let bpm = h
        .state()
        .index
        .get(in_player(h.state()).unwrap())
        .unwrap()
        .bpm;
    assert_eq!(bpm, Some(s.bpm));
    h.state_mut()
        .dispatch((DeckId::PREPARE, EngineCmd::Play(Set::Off)).into());

    // Save: the dropdown lists the library folder; pick it and save.
    let root = h.state().panels.library.browser.folders[0].clone();
    let face = h
        .state()
        .panels
        .tempo_lock
        .root
        .clone()
        .expect("a folder chosen");
    assert_eq!(face, root);
    let short = {
        let p = std::path::Path::new(&root);
        let parent = p.parent().unwrap().file_name().unwrap().to_string_lossy();
        format!("{parent}/{}", p.file_name().unwrap().to_string_lossy())
    };
    h.query_by_label(&short)
        .or_else(|| h.query_by_value(&short))
        .expect("the folder dropdown")
        .click();
    steps(&mut h, 3);
    h.query_by_label(&root)
        .or_else(|| h.query_by_value(&root))
        .expect("the folder in the list")
        .click();
    steps(&mut h, 2);
    h.get_by_label("Save to library").click();
    settle_until(&mut h, |a| {
        matches!(a.panels.tempo_lock.note, Some(Note::Saved(_)))
    });
    let Some(Note::Saved(path)) = h.state().panels.tempo_lock.note.clone() else {
        unreachable!()
    };
    assert_eq!(path.parent().unwrap(), std::path::Path::new(&root));
    settle_until(&mut h, |a| {
        a.index
            .tracks()
            .iter()
            .any(|t| t.path == path.to_string_lossy())
    });

    // Back to the original and the start, for the picture.
    h.state_mut().dispatch(listen(Version::Original, 0.0));
    settle_until(&mut h, |a| {
        a.panels.tempo_lock.player == Some(Version::Original) && !a.panels.tempo_lock.loading
    });
    steps(&mut h, 5);
    render(&mut h, "tempo-lock");
    // Tall enough to show the straightened version's charts too.
    if shot_path("tempo-lock").is_some() {
        h.set_size(vec2(1700.0, 2000.0));
        steps(&mut h, 5);
        render(&mut h, "tempo-lock-tall");
    }
}
