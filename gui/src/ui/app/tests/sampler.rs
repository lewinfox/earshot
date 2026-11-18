//! The sampler: a sound assigned to a slot loads in the background and is saved; the
//! pads page labels its pads in sampler mode, and a pad plays its slot (SHIFT + pad
//! stops it); the bank comes back, sounds and settings, when the app starts again.

use egui_kittest::kittest::Queryable;

use super::*;
use crate::core::bus::Control;
use crate::core::sampler::{Bank, SamplerCmd};
use crate::core::view::{Modal, PadMode};

fn slot(i: usize) -> earshot_engine::SampleSlot {
    earshot_engine::SampleSlot::new(i).unwrap()
}

fn sample(h: &H, i: usize) -> earshot_engine::SampleSnap {
    h.state().frame.snap.as_ref().expect("audio").sampler[i]
}

/// The bank as the database has it.
fn saved(h: &H) -> Bank {
    settings::read(&h.state().lib.store.reader, settings::SAMPLER).expect("a saved bank")
}

fn send(h: &mut H, c: SamplerCmd) {
    h.state_mut().dispatch(Command::Sampler(c));
    settle(h);
}

/// A named control pressed, as a key or a controller's button would.
fn press_control(h: &mut H, name: &str) {
    let (c, _) = Control::parse(name, None).unwrap();
    h.state_mut()
        .dispatch(c.command(1.0).expect("a press does something"));
    settle(h);
}

#[test]
fn slots_are_assigned_played_from_the_pads_and_kept() {
    let mut h = start_with("sampler", stand_in);
    h.state_mut().prefs.pretend_controller = true;
    let dir = scratch_dir("sampler");
    // A sound of its own name, outside the library.
    let file = dir.join("Air Horn.wav");
    std::fs::copy(dir.join("music").join("Night Bus.wav"), &file).unwrap();

    // The window: both decks' slots, all empty.
    h.state_mut()
        .dispatch(ViewCmd::ToggleModal(Modal::Sampler).into());
    steps(&mut h, 3);
    h.get_by_label("Deck 1 pads");
    h.get_by_label("Deck 2 pads");
    assert_eq!(h.query_all_by_label("empty").count(), 16);
    assert!(!sample(&h, 2).loaded);

    // A file for slot 3: decoded in the background, in the engine, saved.
    send(&mut h, SamplerCmd::Assign(slot(2), file.clone()));
    assert!(sample(&h, 2).loaded && !sample(&h, 2).playing);
    assert!((sample(&h, 2).duration - 24.0).abs() < 0.1);
    assert_eq!(
        saved(&h).slot(slot(2)).file.as_deref(),
        Some(file.as_path())
    );
    steps(&mut h, 2);
    assert_eq!(h.query_all_by_label("empty").count(), 15);
    h.get_by_label("Air Horn");
    render(&mut h, "sampler-window");
    h.state_mut()
        .dispatch(ViewCmd::CloseModal(Modal::Sampler).into());

    // The pads page in sampler mode: deck 1's pads are slots 1-8, deck 2's 9-16.
    press_control(&mut h, "deck_1_set_pad_mode_sampler");
    press_control(&mut h, "deck_2_set_pad_mode_sampler");
    steps(&mut h, 3);
    assert_eq!(h.state().panels.decks[0].pad_mode, PadMode::Sampler);
    let pads = |h: &H, d: usize| h.state().panels.decks[d].sample_pads.clone();
    assert_eq!(pads(&h, 0)[2].name.as_deref(), Some("Air Horn"));
    assert!(pads(&h, 0)[2].loaded && pads(&h, 0)[0].name.is_none());
    assert_eq!(pads(&h, 1)[0].slot, 8);
    assert_eq!(pads(&h, 1)[7].slot, 15);
    render(&mut h, "sampler-pads");

    // Its pad plays it, with no deck loaded.
    h.get_by_label("Air Horn").click();
    settle_until(&mut h, |a| {
        a.frame.snap.as_ref().unwrap().sampler[2].playing
    });
    steps(&mut h, 2);
    assert!(pads(&h, 0)[2].playing);
    play(&mut h, 0.5);
    let at = sample(&h, 2).position;
    assert!(at > 0.4 && at < 1.0, "{at}");
    // Pressed again (the controller's pad): from its start.
    press_control(&mut h, "sampler_3_play");
    assert!(sample(&h, 2).playing && sample(&h, 2).position < at);
    // SHIFT + pad stops it. SHIFT held shows that on the pads page.
    press_control(&mut h, "deck_1_shift");
    steps(&mut h, 2);
    h.get_by_label("■ 3");
    press_control(&mut h, "sampler_3_stop");
    assert!(!sample(&h, 2).playing);
    let (shift, _) = Control::parse("deck_1_shift", None).unwrap();
    h.state_mut().dispatch(shift.command(0.0).unwrap());
    settle(&mut h);
    // A slot with no sound does nothing.
    press_control(&mut h, "sampler_1_play");
    assert!(!sample(&h, 0).playing);

    // Volume and loop reach the engine and are saved.
    send(&mut h, SamplerCmd::Volume(slot(2), 0.5));
    send(&mut h, SamplerCmd::Loop(slot(2), true));
    assert!(sample(&h, 2).looping && sample(&h, 2).volume == 0.5);
    let bank = saved(&h);
    assert!(bank.slot(slot(2)).looping && bank.slot(slot(2)).volume == 0.5);

    // The track selected in the library goes in a slot by its file.
    h.state_mut().dispatch(ViewCmd::Browse(-100).into());
    steps(&mut h, 3);
    let lib = &h.state().panels.library;
    let id = lib
        .focused_track(&h.state().panels.flow)
        .expect("a track selected");
    send(&mut h, SamplerCmd::AssignSelected(slot(9)));
    assert!(sample(&h, 9).loaded);
    let track = h.state().lib.store.reader.track_path(id).unwrap();
    assert_eq!(saved(&h).slot(slot(9)).file, Some(track));

    // A file that can't be read: the slot says so, and stays silent.
    send(&mut h, SamplerCmd::Assign(slot(4), dir.join("gone.wav")));
    assert!(!sample(&h, 4).loaded);
    assert!(h.state().sample_errors[4].is_some());
    // Emptied: gone from the engine and the database.
    send(&mut h, SamplerCmd::Clear(slot(4)));
    send(&mut h, SamplerCmd::Clear(slot(9)));
    assert!(!sample(&h, 9).loaded);
    assert!(h.state().sample_errors[4].is_none());
    assert_eq!(saved(&h).files().count(), 1);
    drop(h);

    // The same database, a new app: the slot is back, sound and settings.
    let services = Services {
        folders: crate::core::folders::Folders::all_in(&dir),
        null_audio: true,
        worker: stand_in,
        midi: false,
    };
    let mut h = build("sampler", services);
    settle_until(&mut h, |a| {
        a.frame.snap.as_ref().is_some_and(|s| s.sampler[2].loaded)
    });
    assert!(sample(&h, 2).looping && sample(&h, 2).volume == 0.5);
    assert!(!sample(&h, 9).loaded);
    press_control(&mut h, "sampler_3_play");
    assert!(sample(&h, 2).playing);
    // Reopening the sound card keeps it too.
    h.state_mut().reopen_audio();
    settle_until(&mut h, |a| {
        a.frame.snap.as_ref().is_some_and(|s| s.sampler[2].loaded)
    });
    assert!(sample(&h, 2).looping);
}
