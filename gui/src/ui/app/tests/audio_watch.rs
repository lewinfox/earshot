//! A stalled sound card (`core::audio_watch`) is reopened, three times a minute at
//! most, and the top bar says so.

use std::time::Duration;

use egui_kittest::kittest::Queryable;

use super::*;

#[test]
fn a_stall_reopens_the_audio_until_it_has_tried_three_times() {
    let mut h = start_with("audio-stall", stand_in);
    let stall = || {
        Command::Audio(AudioCmd::Stalled {
            quiet: Duration::from_millis(700),
            decks: "A track 1 at 12.00 s playing".into(),
        })
    };
    for tries in 1..=3 {
        h.state_mut().dispatch(stall());
        steps(&mut h, 3);
        assert_eq!(h.state().audio_stalls.len(), tries);
        assert!(h.state().audio.is_ok(), "reopened");
    }
    assert!(
        h.query_by_label_contains("audio stopped for 700 ms; restarted it")
            .is_some(),
        "the top bar says so"
    );
    h.state_mut().dispatch(stall());
    steps(&mut h, 3);
    assert_eq!(
        h.state().audio_stalls.len(),
        3,
        "not reopened a fourth time"
    );
    assert!(h.query_by_label_contains("audio keeps stopping").is_some());

    // Picking the device again starts afresh.
    let settings = h.state().settings.audio.clone();
    h.state_mut()
        .dispatch(Command::Audio(AudioCmd::Restart(settings)));
    steps(&mut h, 3);
    assert!(h.state().audio_stalls.is_empty());
    assert!(h.query_by_label_contains("audio keeps stopping").is_none());
}
