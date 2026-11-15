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
