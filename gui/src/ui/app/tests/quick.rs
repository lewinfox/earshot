//! Quick analysis of new tracks (`library::QuickAnalyser`): a track the library
//! hasn't seen before gets a quick grid, saved as `GridSource::Quick`; the full
//! analysis is left to the user and replaces it, as a hand fix does.

use super::*;
use crate::core::track::{GridSource, Status};

/// The fixture track's tempo, by its title.
fn fixture_bpm(title: &str) -> f64 {
    TRACKS
        .iter()
        .find(|(t, _)| title.starts_with(t))
        .unwrap_or_else(|| panic!("{title} isn't in the fixture"))
        .1
}

/// How Prepare's player names its grid's source.
fn deck_source(a: &EarshotApp) -> String {
    let inner = a.player.lock().unwrap();
    inner.session(DeckId::new(0).unwrap()).grid_source.clone()
}

#[test]
fn new_tracks_get_a_quick_grid_and_full_analysis_replaces_it() {
    // (Scans the fixture: every track is new.)
    let mut h = start_with("quick", stand_in);
    settle_until(&mut h, |a| a.index.tracks().iter().all(|t| t.quick()));
    for t in h.state().index.tracks() {
        let bpm = t.bpm.expect("the quick grid's BPM");
        assert!(
            (bpm - fixture_bpm(&t.title)).abs() < 0.05,
            "{}: {bpm}",
            t.title
        );
        assert_eq!(t.grid_source, Some(GridSource::Quick));
        assert!(!t.analysed(), "not the full analysis");
        assert_eq!(h.state().index.status(t), Status::Quick);
    }

    // On a deck it plays on the saved quick grid, and isn't analysed by that.
    let [first, second, ..] = ids(&h)[..] else {
        panic!("the fixture has four tracks");
    };
    h.state_mut()
        .dispatch(ViewCmd::SetMode(Mode::Prepare).into());
    h.state_mut().dispatch(LibraryCmd::Load(first).into());
    settle_until(&mut h, |a| {
        prepared(a) == Some(first) && deck_source(a) == GridSource::Quick.label()
    });
    assert!(h.state().index.get(first).unwrap().quick());
    assert!(h.state().index.analysis(first).is_none(), "nothing queued");

    // Asked for, the full analysis replaces it, on the deck too.
    h.state_mut().dispatch(
        LibraryCmd::Analyse {
            tracks: vec![first],
            dynamic: false,
            asked: true,
        }
        .into(),
    );
    settle_until(&mut h, |a| {
        let t = a.index.get(first).unwrap();
        t.grid_source == Some(GridSource::Analysis) && a.index.status(t) == Status::Done
    });
    assert_eq!(deck_source(h.state()), GridSource::Analysis.label());
    assert!(
        h.state().index.get(second).unwrap().quick(),
        "only that one"
    );

    // A quick grid fixed by hand is the user's.
    h.state_mut().dispatch(LibraryCmd::Load(second).into());
    settle_until(&mut h, |a| {
        prepared(a) == Some(second) && deck_source(a) == GridSource::Quick.label()
    });
    steps(&mut h, 20);
    press(&mut h, egui::Modifiers::SHIFT, Key::ArrowRight);
    settle_until(&mut h, |a| {
        let t = a.index.get(second).unwrap();
        t.grid_source == Some(GridSource::User) && a.index.status(t) == Status::Done
    });
}

#[test]
fn the_setting_turns_quick_analysis_of_new_tracks_off() {
    let dir = scratch("quick-off");
    link_tracks(&dir.join("music"));
    let services = Services {
        folders: crate::core::folders::Folders::all_in(&dir),
        null_audio: true,
        worker: stand_in,
        midi: false,
        secrets: std::sync::Arc::new(crate::core::bucket::MemorySecrets::default()),
    };
    let mut h = build("quick-off", services);
    assert!(h.state().settings.quick_analyse_new, "on by default");
    h.state_mut()
        .dispatch(AppCmd::QuickAnalyseNew(false).into());
    h.state_mut()
        .dispatch(LibraryCmd::AddRoots(vec![dir.join("music")]).into());
    settle_until(&mut h, |a| {
        a.index.tracks().len() == TRACKS.len() && a.index.tracks().iter().all(|t| !t.has_grid())
    });
    assert_eq!(
        settings::read::<bool>(&h.state().lib.store.reader, settings::QUICK_ANALYSE_NEW),
        Some(false),
        "saved"
    );

    // On again: the next new track gets its quick grid. The ones found while it was
    // off aren't new any more, and stay as they are.
    h.state_mut().dispatch(AppCmd::QuickAnalyseNew(true).into());
    write_track(&dir.join("music/Late Arrival.wav"), 120.0, 24.0);
    h.state_mut().dispatch(LibraryCmd::Rescan.into());
    settle_until(&mut h, |a| a.index.tracks().len() == TRACKS.len() + 1);
    for t in h.state().index.tracks() {
        if t.title.starts_with("Late Arrival") {
            assert_eq!(t.grid_source, Some(GridSource::Quick));
            assert!((t.bpm.unwrap() - 120.0).abs() < 0.05, "{:?}", t.bpm);
        } else {
            assert!(!t.has_grid(), "{}", t.title);
        }
    }
}
