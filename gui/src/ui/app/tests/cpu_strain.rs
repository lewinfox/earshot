//! A machine that can't keep up with the audio: the top bar says so, and when the
//! system is holding the CPU back; the work in the background (analysis, stems) gives
//! way, and nothing about the sound changes.

use egui_kittest::kittest::Queryable;

use super::*;

/// The next frame looks at the audio's flags again (it does every half second).
fn look(h: &mut H) {
    h.state_mut().hud.last = None;
    steps(h, 2);
}

#[test]
fn the_top_bar_says_when_the_audio_cant_keep_up_and_stems_wait() {
    let mut h = start_with("cpu-strain", stand_in);
    look(&mut h);
    for label in [
        "audio struggling",
        "background work on hold",
        "low-power mode",
    ] {
        assert!(h.query_by_label_contains(label).is_none(), "{label}");
    }

    // The audio thread sets flags (it can't do more); the app notices.
    let stats = |h: &H| h.state().monitor.clone().expect("an engine");
    stats(&h).stats().behind.store(true, Relaxed);
    stats(&h).stats().overruns.store(2, Relaxed);
    look(&mut h);
    assert!(h.query_by_label_contains("audio struggling").is_some());
    assert!(
        h.query_by_label_contains("background work on hold")
            .is_some()
    );
    assert!(
        h.state()
            .notices
            .shown()
            .iter()
            .any(|n| n.text.contains("struggling to keep up with the audio")),
        "and a message in the bottom bar, once"
    );
    look(&mut h);
    assert_eq!(h.state().notices.shown().len(), 1, "not said again");

    // Stems asked for now aren't started.
    let track = ids(&h)[0];
    h.state_mut()
        .dispatch(crate::core::stems::StemsCmd::Calculate(track).into());
    steps(&mut h, 5);
    assert!(h.state().stems.status(track).is_none(), "not started");
    assert!(
        h.state()
            .notices
            .shown()
            .iter()
            .any(|n| n.text.contains("Stems will have to wait"))
    );

    // Room again: the pills go by themselves.
    stats(&h).stats().behind.store(false, Relaxed);
    look(&mut h);
    assert!(h.query_by_label_contains("audio struggling").is_none());
    assert!(
        h.query_by_label_contains("background work on hold")
            .is_none()
    );

    // The system holding the CPU back, as the power watch reports it.
    h.state_mut().hud.power = Some(crate::core::power::Warning {
        text: "low-power mode".into(),
        detail: "The power profile is low-power.".into(),
    });
    look(&mut h);
    assert!(h.query_by_label_contains("low-power mode").is_some());
    assert!(
        h.query_by_label_contains("background work on hold")
            .is_some()
    );
}

/// Short of resources, full analysis waits in its queue (the worker isn't started
/// for it) and the tracks asked for get the quick finder's grid meanwhile; once
/// there's room again the queued analysis runs by itself and replaces it.
#[test]
fn short_of_resources_analysis_is_quick_only_and_the_full_one_runs_once_there_is_room() {
    use crate::core::track::{GridSource, Status};
    let dir = scratch("cpu-strain-analysis");
    link_tracks(&dir.join("music"));
    let services = Services {
        db: dir.join(store::FILE_NAME),
        null_audio: true,
        worker: stand_in,
        midi: false,
    };
    let mut h = build("cpu-strain-analysis", services);
    // No quick grids for new tracks: the tracks start with none.
    h.state_mut()
        .dispatch(AppCmd::QuickAnalyseNew(false).into());
    h.state_mut()
        .dispatch(LibraryCmd::AddRoots(vec![dir.join("music")]).into());
    settle_until(&mut h, |a| a.index.tracks().len() == TRACKS.len());
    assert!(h.state().index.tracks().iter().all(|t| !t.has_grid()));

    h.state_mut().hud.power = Some(crate::core::power::Warning {
        text: "low-power mode".into(),
        detail: "The power profile is low-power.".into(),
    });
    look(&mut h);
    assert!(
        h.query_by_label_contains("background work on hold")
            .is_some()
    );

    let tracks: Vec<i64> = h.state().index.tracks().iter().map(|t| t.id).collect();
    h.state_mut().dispatch(
        LibraryCmd::Analyse {
            tracks: tracks.clone(),
            dynamic: false,
            asked: true,
        }
        .into(),
    );
    settle_until(&mut h, |a| a.index.tracks().iter().all(|t| t.quick()));
    steps(&mut h, 20);
    for t in h.state().index.tracks() {
        assert_eq!(t.grid_source, Some(GridSource::Quick), "{}", t.title);
        assert!(
            h.state().index.analysis(t.id).is_some(),
            "{}: its full analysis is still queued",
            t.title
        );
    }

    // Room again: what was queued runs, and replaces the quick grids.
    h.state_mut().hud.power = None;
    look(&mut h);
    assert!(
        h.query_by_label_contains("background work on hold")
            .is_none()
    );
    settle_until(&mut h, |a| {
        a.index.tracks().iter().all(|t| {
            t.grid_source == Some(GridSource::Analysis) && a.index.status(t) == Status::Done
        })
    });
}
