//! A machine that can't keep up with the audio: the top bar says what the audio is
//! doing about it (out of real time, keylock on its lighter setting) and when the
//! system is holding the CPU back; reopening the sound card starts afresh.

use egui_kittest::kittest::Queryable;

use super::*;

/// The next frame looks at the audio's flags again (it does every half second).
fn look(h: &mut H) {
    h.state_mut().hud.last = None;
    steps(h, 2);
}

#[test]
fn the_top_bar_says_when_the_audio_cant_keep_up_and_a_reopened_sound_card_starts_afresh() {
    let mut h = start_with("cpu-strain", stand_in);
    look(&mut h);
    for label in ["audio not real-time", "keylock: lighter", "low-power mode"] {
        assert!(h.query_by_label_contains(label).is_none(), "{label}");
    }

    // The audio thread sets flags (it can't do more); the app notices.
    let stats = |h: &H| h.state().monitor.clone().expect("an engine");
    stats(&h).stats().keylock_cheap.store(true, Relaxed);
    look(&mut h);
    assert!(h.query_by_label_contains("keylock: lighter").is_some());
    assert!(h.query_by_label_contains("audio not real-time").is_none());

    stats(&h).stats().realtime_lost.store(true, Relaxed);
    look(&mut h);
    assert!(h.query_by_label_contains("audio not real-time").is_some());
    assert!(
        h.state()
            .notices
            .shown()
            .iter()
            .any(|n| n.text.contains("can't keep up with the audio")),
        "and a message in the bottom bar, once"
    );
    look(&mut h);
    assert_eq!(h.state().notices.shown().len(), 1, "not said again");

    // The system holding the CPU back, as the power watch reports it.
    h.state_mut().hud.power = Some(crate::core::power::Warning {
        text: "low-power mode".into(),
        detail: "The power profile is low-power.".into(),
    });
    look(&mut h);
    assert!(h.query_by_label_contains("low-power mode").is_some());

    // Picking the device again opens a new engine: real time and the full keylock.
    let settings = h.state().settings.audio.clone();
    h.state_mut()
        .dispatch(Command::Audio(AudioCmd::Restart(settings)));
    look(&mut h);
    assert!(h.query_by_label_contains("audio not real-time").is_none());
    assert!(h.query_by_label_contains("keylock: lighter").is_none());
    assert!(
        h.query_by_label_contains("low-power mode").is_some(),
        "the power profile is the system's, not the sound card's"
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
    assert!(h.query_by_label_contains("analysis: quick only").is_some());

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
    assert!(h.query_by_label_contains("analysis: quick only").is_none());
    settle_until(&mut h, |a| {
        a.index
            .tracks()
            .iter()
            .all(|t| t.grid_source == Some(GridSource::Analysis) && a.index.status(t) == Status::Done)
    });
}
