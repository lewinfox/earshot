//! Controllers plugged in and pulled out while earshot runs (`midi::watch`). They're
//! noticed and listed, and nothing is switched by itself, except that the controller
//! in use being pulled out leaves earshot with none. Choosing one (the MIDI pill) is
//! refused while a deck plays: it moves the audio to its sound card.

use super::*;
use crate::core::midi::{Found, Port};

fn device(name: &str) -> Found {
    Found {
        name: name.to_string(),
        key: crate::core::controllers::device_key(name, None),
    }
}

/// As if this device's port were open: the controller in use.
fn in_use(a: &mut EarshotApp, d: &Found) {
    a.midi_devices = vec![Port {
        name: d.name.clone(),
        usb: None,
        mapping: None,
    }];
}

fn said(a: &EarshotApp, what: &str) -> bool {
    a.notices.shown().iter().any(|n| n.text.contains(what))
}

#[test]
fn a_controller_plugged_in_is_listed_and_not_taken_up() {
    let mut h = start("hotplug");
    let ddj = device("DDJ-400:DDJ-400 MIDI 1 20:0");

    // With nothing playing: listed, said, and that's all.
    h.state_mut().midi_devices_changed(vec![ddj.clone()]);
    settle(&mut h);
    assert_eq!(h.state().midi_found, [ddj.clone()]);
    assert!(h.state().midi_devices.is_empty(), "not opened by itself");
    assert_eq!(h.state().prefs.controller_chosen, None);
    assert!(said(
        h.state(),
        "DDJ-400 is plugged in: pick it in the MIDI pill"
    ));

    // The same devices again: nothing.
    let notices = h.state().notices.shown().len();
    h.state_mut().midi_devices_changed(vec![ddj.clone()]);
    assert_eq!(h.state().notices.shown().len(), notices);

    // While a deck plays, choosing it is refused.
    let deck = DeckId::new(0).unwrap();
    let track = ids(&h)[0];
    h.state_mut().load(Some(deck), track);
    settle_until(&mut h, |a| a.panels.decks[0].wave.is_some());
    h.state_mut()
        .dispatch((deck, EngineCmd::Play(earshot_engine::Set::On)).into());
    settle_until(&mut h, |a| a.any_playing());
    h.state_mut()
        .dispatch(AppCmd::UseController(ddj.key.clone()).into());
    assert_eq!(h.state().prefs.controller_chosen, None);
    assert!(said(h.state(), "Stop the decks to change controller"));
    // And the deck stopping doesn't take it up either.
    h.state_mut()
        .dispatch((deck, EngineCmd::Play(earshot_engine::Set::Off)).into());
    play(&mut h, 0.2);
    assert!(!h.state().any_playing());
    assert!(h.state().midi_devices.is_empty());
    assert_eq!(h.state().prefs.controller_chosen, None);

    // With nothing playing a choice is taken, and remembered.
    h.state_mut()
        .dispatch(AppCmd::UseController(ddj.key.clone()).into());
    assert_eq!(h.state().prefs.controller_chosen, Some(ddj.key));
}

#[test]
fn the_controller_in_use_pulled_out_leaves_none() {
    let mut h = start("hotplug-gone");
    let ddj = device("DDJ-400:DDJ-400 MIDI 1 20:0");
    let other = device("Other:Other MIDI 1 24:0");
    h.state_mut().midi_found = vec![ddj.clone(), other.clone()];
    in_use(h.state_mut(), &ddj);

    // A device that isn't in use going changes nothing.
    h.state_mut().midi_devices_changed(vec![ddj.clone()]);
    assert_eq!(h.state().midi_devices.len(), 1);
    h.state_mut()
        .midi_devices_changed(vec![ddj.clone(), other.clone()]);
    assert_eq!(h.state().midi_devices.len(), 1);

    // The one in use going: no controller, and the other isn't taken up instead.
    h.state_mut().midi_devices_changed(vec![other.clone()]);
    settle(&mut h);
    assert!(h.state().midi_devices.is_empty());
    assert_eq!(h.state().midi_found, [other]);
    assert!(said(h.state(), "The controller was unplugged"));
}
