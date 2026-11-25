//! Controllers plugged in and pulled out while earshot runs (`midi::watch`), and
//! choosing between two: nothing about the controller changes while a deck plays,
//! since changing it moves the audio to its sound card.

use super::*;
use crate::core::midi::Found;

fn device(name: &str) -> Found {
    Found {
        name: name.to_string(),
        key: crate::core::controllers::device_key(name, None),
    }
}

fn said(a: &EarshotApp, what: &str) -> bool {
    a.notices.shown().iter().any(|n| n.text.contains(what))
}

#[test]
fn a_controller_plugged_in_mid_track_waits_for_the_decks_to_stop() {
    let mut h = start("hotplug");
    let deck = DeckId::new(0).unwrap();
    let track = ids(&h)[0];
    h.state_mut().load(Some(deck), track);
    settle_until(&mut h, |a| a.panels.decks[0].wave.is_some());
    h.state_mut()
        .dispatch((deck, EngineCmd::Play(earshot_engine::Set::On)).into());
    settle_until(&mut h, |a| a.any_playing());

    // Plugged in while it plays: listed, and left alone.
    let ddj = device("DDJ-400:DDJ-400 MIDI 1 20:0");
    h.state_mut().midi_devices_changed(vec![ddj.clone()]);
    assert!(h.state().controller_waiting);
    assert_eq!(h.state().midi_found, [ddj.clone()]);
    assert!(said(h.state(), "DDJ-400 is plugged in"));
    play(&mut h, 0.5);
    assert!(h.state().controller_waiting, "still playing: still waiting");
    assert!(h.state().any_playing());

    // Choosing a controller is refused while it plays, too.
    h.state_mut()
        .dispatch(AppCmd::UseController(ddj.key.clone()).into());
    assert_eq!(h.state().prefs.controller_chosen, None);
    assert!(said(h.state(), "Stop the decks to change controller"));

    // The deck stopped: it's connected. (There's no such device here, so what's
    // found then is what this machine really has.)
    h.state_mut()
        .dispatch((deck, EngineCmd::Play(earshot_engine::Set::Off)).into());
    play(&mut h, 0.2);
    assert!(!h.state().any_playing());
    assert!(!h.state().controller_waiting);

    // With nothing playing a choice is taken, and remembered.
    h.state_mut()
        .dispatch(AppCmd::UseController(ddj.key.clone()).into());
    assert_eq!(h.state().prefs.controller_chosen, Some(ddj.key));
}

#[test]
fn the_same_devices_again_change_nothing() {
    let mut h = start("hotplug-same");
    let keys = device("Keys:Keys MIDI 1 24:0");
    h.state_mut().midi_found = vec![keys.clone()];
    h.state_mut().midi_devices_changed(vec![keys.clone()]);
    // (Not connected again: what's found would be this machine's own.)
    assert_eq!(h.state().midi_found, [keys]);
    assert!(!h.state().controller_waiting);
}
