//! The controller's lights. A button's light is the button's own message sent back
//! to the controller: 0x7F lights it, 0x00 puts it out (Pioneer's convention, and
//! Mixxx's). A button is lit when the command it's mapped to is on: BEAT SYNC while
//! sync is on, a hot cue pad while its cue is set. As on rekordbox: LOOP IN / OUT
//! are lit, and nothing else on a deck until a track is loaded; then CUE is lit (flashing
//! while paused away from the cue point) and PLAY flashes, lit once it plays. LOOP IN flashes once pressed, and LOOP IN / OUT
//! together while a loop plays (RELOOP/EXIT lit). BEAT FX ON gets "on" while the
//! effect is on, which the DDJ-400 shows by flashing it (and "off" lit steady).
//!
//! Pads in keyboard and key shift mode light as `core::pads` says.
//!
//! While SHIFT is held the DDJ-400 shows its shifted buttons' lights (SHIFT + PLAY
//! is its own message, with its own light). A shifted button that does nothing of its
//! own in earshot, or has no light of its own (SHIFT + BEAT SYNC: tempo range), gets
//! its unshifted twin's light, so SHIFT leaves the lights alone; only the ones whose
//! job changes (SHIFT + a hot cue pad clears it, keyboard mode's page pads) show their
//! own. See `shift_twin`.
//!
//! When earshot quits (or the controller is reconnected) every mapped light goes out.
//!
//! A thread works out every mapped button's light from the engine's latest state,
//! 20 times a second, and sends only the ones that changed. The controller can't
//! flash a light itself, so the thread does, half a second on and half off.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use earshot_engine::{EngineSnapshot, FxTarget, Monitor};
use midir::MidiOutputConnection;

use super::bus::{Control, DeckControl, FxControl};
use super::midi::{Input, Learned};
use super::pads::AllPads;

/// How a light shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Light {
    Off,
    On,
    /// Half a second on, half a second off.
    Flash,
}

/// How often the thread looks, and how long a flash stays on (and off).
const TICK: Duration = Duration::from_millis(50);
const FLASH: Duration = Duration::from_millis(500);

/// The light for a button mapped to `control` (with its number, `arg`), given the
/// engine's state and the decks' pads. Buttons whose command has no on or off stay
/// dark.
pub fn light(control: Control, arg: Option<f64>, snap: &EngineSnapshot, pads: &AllPads) -> Light {
    let on = |b: bool| if b { Light::On } else { Light::Off };
    let flash = |b: bool| if b { Light::Flash } else { Light::Off };
    match control {
        Control::Deck(d, c) => {
            let deck = snap.deck(d);
            let loaded = deck.track_id.is_some();
            let looping = deck.loop_active && deck.loop_in.is_some() && deck.loop_out.is_some();
            // LOOP IN pressed, LOOP OUT not yet.
            let loop_started = deck.loop_in.is_some() && deck.loop_out.is_none();
            match c {
                // Lit, ready for a loop, even with nothing loaded; flashing once it's
                // pressed (LOOP IN) and while the loop plays (both, together).
                DeckControl::LoopIn if looping || loop_started => Light::Flash,
                DeckControl::LoopOut if looping => Light::Flash,
                DeckControl::LoopIn | DeckControl::LoopOut => Light::On,
                // Headphone cue is the mixer channel's, track or not.
                DeckControl::Pfl => on(deck.pfl),
                // With nothing loaded, nothing else on the deck is lit.
                _ if !loaded => Light::Off,
                // Flashing while paused, lit while playing.
                DeckControl::Play if deck.playing => Light::On,
                DeckControl::Play => Light::Flash,
                // Flashing while paused away from the cue point (a press sets it there).
                DeckControl::Cue if !deck.playing && (deck.position - deck.cue_point).abs() > 0.01 => {
                    Light::Flash
                }
                DeckControl::Cue => Light::On,
                DeckControl::Sync => on(deck.sync),
                DeckControl::Hotcue(n) | DeckControl::HotcueClear(n) => {
                    on(deck.hotcues.get(n as usize).is_some_and(|c| c.is_some()))
                }
                DeckControl::Reloop | DeckControl::Loop => on(looping),
                DeckControl::LoopActiveToggle => on(deck.active_loop.is_some()),
                DeckControl::BeatLoop(_) => {
                    let beats = arg.unwrap_or(4.0);
                    flash(looping && loop_beats(deck).is_some_and(|b| (b - beats).abs() < beats * 0.02))
                }
                DeckControl::Quantize => on(deck.quantize),
                DeckControl::Keylock => on(deck.keylock),
                DeckControl::Vinyl => on(deck.vinyl),
                DeckControl::KeyShiftReset => on(deck.key_shift != 0.0),
                DeckControl::ModePad { mode, pad, shift } => {
                    pads[d.index()].light(mode, pad as usize, shift, deck)
                }
                _ => Light::Off,
            }
        }
        Control::Master(earshot_engine::MasterParam::MasterCue) => on(snap.mixer.master_cue),
        Control::Fx(u, c) => {
            let Some(fx) = snap.mixer.fx.get(u) else {
                return Light::Off;
            };
            match c {
                // The DDJ-400 flashes BEAT FX ON itself while it's "on" (7F), and
                // shows it steady at 00; Mixxx sends the same to its SHIFT message
                // (Release FX), so it doesn't change while SHIFT is held.
                FxControl::On | FxControl::Release => on(fx.enabled),
                FxControl::TargetDeck(d) => on(matches!(fx.target, FxTarget::Deck(t) if t.index() == d as usize)),
                FxControl::TargetMaster => on(fx.target == FxTarget::Master),
                FxControl::TempoTap => on(fx.tap_bpm.is_some()),
                _ => Light::Off,
            }
        }
        _ => Light::Off,
    }
}

/// The loop's length in beats of the track's own tempo.
fn loop_beats(deck: &earshot_engine::DeckSnap) -> Option<f64> {
    let (start, end) = (deck.loop_in?, deck.loop_out?);
    let track_bpm = deck.bpm? / deck.tempo.max(1e-6);
    Some((end - start) * track_bpm / 60.0)
}

/// The value to send for `light`, in the flash's on half (`lit_half`) or off half.
pub fn value(light: Light, lit_half: bool) -> u8 {
    match light {
        Light::On => 0x7F,
        Light::Flash if lit_half => 0x7F,
        _ => 0x00,
    }
}

/// Whether a button mapped to `control` has a light of its own: one `light` works out
/// from the state. A shifted pad in keyboard or key shift mode has one only where
/// SHIFT gives it a job.
fn own_light(control: Control, snap: &EngineSnapshot, pads: &AllPads) -> bool {
    use DeckControl as D;
    match control {
        Control::Deck(d, D::ModePad { mode, pad, shift: true }) => {
            pads[d.index()].shift_job(mode, pad as usize, snap.deck(d))
        }
        Control::Deck(_, c) => matches!(
            c,
            D::LoopIn
                | D::LoopOut
                | D::Pfl
                | D::Play
                | D::Cue
                | D::Sync
                | D::Hotcue(_)
                | D::HotcueClear(_)
                | D::Reloop
                | D::Loop
                | D::LoopActiveToggle
                | D::BeatLoop(_)
                | D::Quantize
                | D::Keylock
                | D::Vinyl
                | D::KeyShiftReset
                | D::ModePad { .. }
        ),
        Control::Master(earshot_engine::MasterParam::MasterCue) => true,
        Control::Fx(_, c) => matches!(
            c,
            FxControl::On | FxControl::Release | FxControl::TargetDeck(_) | FxControl::TargetMaster | FxControl::TempoTap
        ),
        _ => false,
    }
}

/// A DDJ-400 button's message with SHIFT held: on a deck's channel, PLAY, CUE, LOOP
/// IN / OUT, RELOOP/EXIT, BEAT SYNC and CHANNEL CUE each send another note (Mixxx's
/// mapping); a pad sends the same note a channel up.
pub fn shift_twin((status, data1): (u8, u8)) -> Option<(u8, u8)> {
    match status {
        0x90 | 0x91 => {
            let shifted = match data1 {
                0x0B => 0x47, // PLAY/PAUSE
                0x0C => 0x48, // CUE
                0x10 => 0x4C, // LOOP IN
                0x11 => 0x4E, // LOOP OUT
                0x4D => 0x50, // RELOOP/EXIT
                0x58 => 0x60, // BEAT SYNC
                0x54 => 0x68, // CHANNEL CUE
                _ => return None,
            };
            Some((status, shifted))
        }
        0x97 | 0x99 => Some((status + 1, data1)),
        _ => None,
    }
}

/// Every mapped button's message and value, as they should be now; and each shifted
/// twin without a light of its own, its button's (see `shift_twin`).
pub fn frame(learned: &Learned, snap: &EngineSnapshot, pads: &AllPads, lit_half: bool) -> BTreeMap<(u8, u8), u8> {
    let notes: BTreeMap<(u8, u8), _> =
        learned.all().into_iter().filter(|(_, l)| l.input == Input::Note).collect();
    let mut out: BTreeMap<(u8, u8), u8> = notes
        .iter()
        .map(|(&k, l)| (k, value(light(l.control, l.arg, snap, pads), lit_half)))
        .collect();
    for (&k, _) in &notes {
        let Some(twin) = shift_twin(k) else { continue };
        if notes.get(&twin).is_none_or(|l| !own_light(l.control, snap, pads)) {
            out.insert(twin, out[&k]);
        }
    }
    out
}

/// Where the lights thread reads the engine's state and the decks' pads. The app
/// puts the engine's monitor here (again when the sound card is reopened), and the
/// pads whenever one is pressed.
pub type MonitorSlot = Arc<Mutex<LightsState>>;

#[derive(Default)]
pub struct LightsState {
    pub monitor: Option<Monitor>,
    pub pads: AllPads,
}

/// The lights thread; dropping it puts every light out and stops it.
pub struct Lights {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Lights {
    pub fn start(outputs: Vec<MidiOutputConnection>, learned: Learned, monitor: MonitorSlot) -> Lights {
        let stop = Arc::new(AtomicBool::new(false));
        let quit = stop.clone();
        let thread = std::thread::Builder::new()
            .name("midi-lights".into())
            .spawn(move || run(outputs, &learned, &monitor, &quit))
            .ok();
        Lights { stop, thread }
    }
}

impl Drop for Lights {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(mut outputs: Vec<MidiOutputConnection>, learned: &Learned, monitor: &MonitorSlot, stop: &AtomicBool) {
    let start = Instant::now();
    let mut sent: BTreeMap<(u8, u8), u8> = BTreeMap::new();
    let send = |outputs: &mut Vec<MidiOutputConnection>, (status, data1): (u8, u8), v: u8| {
        for o in outputs.iter_mut() {
            let _ = o.send(&[status, data1, v]);
        }
    };
    while !stop.load(Ordering::Relaxed) {
        let (snap, pads) = {
            let s = monitor.lock().unwrap();
            (s.monitor.as_ref().map(|m| m.snapshot()), s.pads)
        };
        let lit_half = (start.elapsed().as_millis() / FLASH.as_millis()) % 2 == 0;
        let now = match snap {
            Some(s) => frame(learned, &s, &pads, lit_half),
            None => BTreeMap::new(),
        };
        for (&k, &v) in &now {
            if sent.get(&k) != Some(&v) {
                send(&mut outputs, k, v);
            }
        }
        // A button no longer mapped: put its light out.
        for (&k, _) in sent.iter().filter(|(k, v)| **v != 0 && !now.contains_key(k)) {
            send(&mut outputs, k, 0);
        }
        sent = now;
        std::thread::sleep(TICK);
    }
    // Every mapped button's light out, not only the ones lit here: the controller
    // lights some itself (its pad mode buttons), and shouldn't be left lit.
    for k in off_at_exit(learned, &sent) {
        send(&mut outputs, k, 0);
    }
}

/// The lights to put out on leaving: every mapped button's, and any lit here since.
fn off_at_exit(learned: &Learned, sent: &BTreeMap<(u8, u8), u8>) -> std::collections::BTreeSet<(u8, u8)> {
    let mapped = learned.all().into_iter().filter(|(_, l)| l.input == Input::Note).map(|(k, _)| k);
    mapped.chain(sent.iter().filter(|(_, v)| **v != 0).map(|(k, _)| *k)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use earshot_engine::DeckId;

    fn deck(i: usize) -> DeckId {
        DeckId::new(i).unwrap()
    }

    fn d(c: DeckControl) -> Control {
        Control::Deck(deck(0), c)
    }

    /// A deck with a track, paused on its cue point.
    fn loaded() -> EngineSnapshot {
        let mut s = EngineSnapshot::default();
        s.decks_live = 2;
        s.decks[0].track_id = Some(1);
        s.decks[0].bpm = Some(120.0);
        s.decks[0].tempo = 1.0;
        s
    }

    #[test]
    fn play_and_cue_follow_the_transport() {
        let lights = |s: &EngineSnapshot| {
            [DeckControl::LoopIn, DeckControl::LoopOut, DeckControl::Cue, DeckControl::Play]
                .map(|c| light(d(c), None, s, &AllPads::default()))
        };
        use Light::*;
        let mut s = EngineSnapshot::default();
        assert_eq!(lights(&s), [On, On, Off, Off], "nothing loaded");
        assert_eq!(light(d(DeckControl::Sync), None, &s, &AllPads::default()), Off, "nothing else lit");
        s.decks[0].pfl = true;
        assert_eq!(light(d(DeckControl::Pfl), None, &s, &AllPads::default()), On, "but headphone cue");
        s = loaded();
        assert_eq!(lights(&s), [On, On, On, Flash], "paused on the cue point");
        s.decks[0].playing = true;
        assert_eq!(lights(&s), [On, On, On, On], "playing");
        s.decks[0].position = 10.0;
        s.decks[0].playing = false;
        assert_eq!(lights(&s), [On, On, Flash, Flash], "paused away from it");
    }

    #[test]
    fn loops_flash_their_buttons() {
        let mut s = loaded();
        s.decks[0].playing = true;
        assert_eq!(light(d(DeckControl::LoopIn), None, &s, &AllPads::default()), Light::On, "ready");
        assert_eq!(light(d(DeckControl::Reloop), None, &s, &AllPads::default()), Light::Off);
        // LOOP IN pressed: it flashes, LOOP OUT stays lit.
        s.decks[0].loop_in = Some(1.0);
        assert_eq!(light(d(DeckControl::LoopIn), None, &s, &AllPads::default()), Light::Flash);
        assert_eq!(light(d(DeckControl::LoopOut), None, &s, &AllPads::default()), Light::On);
        // LOOP OUT pressed: a 4-beat loop at 120 BPM (2 s); both flash.
        s.decks[0].loop_out = Some(3.0);
        s.decks[0].loop_active = true;
        assert_eq!(light(d(DeckControl::LoopIn), None, &s, &AllPads::default()), Light::Flash);
        assert_eq!(light(d(DeckControl::LoopOut), None, &s, &AllPads::default()), Light::Flash);
        assert_eq!(light(d(DeckControl::Reloop), None, &s, &AllPads::default()), Light::On);
        assert_eq!(light(d(DeckControl::BeatLoop(4.0)), Some(4.0), &s, &AllPads::default()), Light::Flash);
        assert_eq!(light(d(DeckControl::BeatLoop(8.0)), Some(8.0), &s, &AllPads::default()), Light::Off);
    }

    #[test]
    fn pads_sync_and_effects() {
        let mut s = loaded();
        s.decks[0].hotcues[2] = Some(5.0);
        assert_eq!(light(d(DeckControl::Hotcue(2)), None, &s, &AllPads::default()), Light::On);
        assert_eq!(light(d(DeckControl::HotcueClear(2)), None, &s, &AllPads::default()), Light::On);
        assert_eq!(light(d(DeckControl::Hotcue(3)), None, &s, &AllPads::default()), Light::Off);
        s.decks[0].sync = true;
        assert_eq!(light(d(DeckControl::Sync), None, &s, &AllPads::default()), Light::On);
        assert_eq!(light(d(DeckControl::BeatJump), Some(4.0), &s, &AllPads::default()), Light::Off, "no state");
        assert_eq!(light(Control::Fx(0, FxControl::On), None, &s, &AllPads::default()), Light::Off);
        s.mixer.fx[0].enabled = true;
        assert_eq!(light(Control::Fx(0, FxControl::On), None, &s, &AllPads::default()), Light::On, "the controller flashes it");
        assert_eq!(light(Control::Fx(0, FxControl::Release), None, &s, &AllPads::default()), Light::On);
    }

    #[test]
    fn every_mapped_light_goes_out_at_exit() {
        let learned = Learned::default();
        learned.seed_ddj400();
        let mut sent = BTreeMap::new();
        // Something lit here that's since been unmapped still goes out.
        sent.insert((0x90, 0x7E), 0x7F);
        let off = off_at_exit(&learned, &sent);
        assert!(off.contains(&(0x90, 0x69)), "a pad mode button, never lit by earshot");
        assert!(off.contains(&(0x97, 0x44)), "a keyboard pad");
        assert!(off.contains(&(0x90, 0x7E)));
        assert!(!off.iter().any(|(s, _)| s & 0xF0 == 0xB0), "knobs have no lights");
    }

    /// SHIFT held shows the shifted buttons' lights: those without a job of their own
    /// show their button's, so SHIFT changes only the lights whose job changes.
    #[test]
    fn shift_leaves_the_lights_alone_but_for_changed_jobs() {
        let learned = Learned::default();
        learned.seed_ddj400();
        let mut s = loaded();
        s.decks[0].playing = true;
        s.decks[0].sync = true;
        s.decks[0].loop_in = Some(1.0);
        s.decks[0].loop_out = Some(3.0);
        s.decks[0].loop_active = true;
        s.decks[0].hotcues[0] = Some(2.0);
        let f = frame(&learned, &s, &AllPads::default(), true);
        for (button, twin, what) in [
            ((0x90, 0x0B), (0x90, 0x47), "PLAY"),
            ((0x90, 0x0C), (0x90, 0x48), "CUE"),
            ((0x90, 0x58), (0x90, 0x60), "BEAT SYNC"),
            ((0x90, 0x10), (0x90, 0x4C), "LOOP IN"),
            ((0x97, 0x64), (0x98, 0x64), "beat loop pad 5 (the 4-beat loop)"),
            ((0x97, 0x40), (0x98, 0x40), "keyboard pad 1, picking (hot cue A set)"),
            ((0x97, 0x70), (0x98, 0x70), "key shift pad 1"),
        ] {
            assert_eq!(f[&button], 0x7F, "{what} lit");
            assert_eq!(f.get(&twin), Some(&0x7F), "{what} with SHIFT");
        }
        // Jobs that change keep their own: hot cue B unset clears nothing; key shift's
        // pad 8 turns to page 3, pad 7 to page 1.
        assert_eq!(f[&(0x98, 0x01)], 0x00, "SHIFT + hot cue B");
        assert_eq!(f[&(0x98, 0x76)], 0x7F, "SHIFT + key shift pad 7");
        assert_eq!(f[&(0x97, 0x76)], 0x7F, "key shift pad 7");
        // A shifted button with no light of its own (SHIFT + BEAT SYNC: tempo range).
        let rows = r#"{"device":"DDJ-400","inputs":[{"command":"deck_1_beat_sync","message":"90 58"},{"command":"deck_1_tempo_range_next","message":"90 60"}]}"#;
        let learned = Learned::default();
        learned.replace(crate::core::midi::from_json(rows).unwrap().1);
        let f = frame(&learned, &s, &AllPads::default(), true);
        assert_eq!(f[&(0x90, 0x60)], 0x7F, "tempo range shows BEAT SYNC's");
    }

    #[test]
    fn a_flash_is_on_then_off() {
        assert_eq!(value(Light::Flash, true), 0x7F);
        assert_eq!(value(Light::Flash, false), 0x00);
        assert_eq!(value(Light::On, false), 0x7F);
        assert_eq!(value(Light::Off, true), 0x00);
    }
}
