//! The controller's lights. A button's light is the button's own message sent back
//! to the controller: 0x7F lights it, 0x00 puts it out (Pioneer's convention, and
//! Mixxx's). A button is lit when the command it's mapped to is on: BEAT SYNC while
//! sync is on, a hot cue pad while its cue is set. As on rekordbox: LOOP IN / OUT
//! are lit, and nothing else on a deck until a track is loaded; then CUE is lit (flashing
//! while paused away from the cue point) and PLAY flashes, lit once it plays. LOOP IN flashes once pressed, and LOOP IN / OUT
//! together while a loop plays (RELOOP/EXIT lit). BEAT FX ON gets "on" while the
//! effect is on, which the DDJ-400 shows by flashing it (and "off" lit steady).
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
/// engine's state. Buttons whose command has no on or off stay dark.
pub fn light(control: Control, arg: Option<f64>, snap: &EngineSnapshot) -> Light {
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

/// Every mapped button's message and value, as they should be now.
pub fn frame(learned: &Learned, snap: &EngineSnapshot, lit_half: bool) -> BTreeMap<(u8, u8), u8> {
    learned
        .all()
        .into_iter()
        .filter(|(_, l)| l.input == Input::Note)
        .map(|(k, l)| (k, value(light(l.control, l.arg, snap), lit_half)))
        .collect()
}

/// Where the lights thread reads the engine's state. The app puts the engine's
/// monitor here, and again when the sound card is reopened.
pub type MonitorSlot = Arc<Mutex<Option<Monitor>>>;

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
        let snap = monitor.lock().unwrap().as_ref().map(|m| m.snapshot());
        let lit_half = (start.elapsed().as_millis() / FLASH.as_millis()) % 2 == 0;
        let now = match snap {
            Some(s) => frame(learned, &s, lit_half),
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
    for (&k, &v) in &sent {
        if v != 0 {
            send(&mut outputs, k, 0);
        }
    }
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
                .map(|c| light(d(c), None, s))
        };
        use Light::*;
        let mut s = EngineSnapshot::default();
        assert_eq!(lights(&s), [On, On, Off, Off], "nothing loaded");
        assert_eq!(light(d(DeckControl::Sync), None, &s), Off, "nothing else lit");
        s.decks[0].pfl = true;
        assert_eq!(light(d(DeckControl::Pfl), None, &s), On, "but headphone cue");
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
        assert_eq!(light(d(DeckControl::LoopIn), None, &s), Light::On, "ready");
        assert_eq!(light(d(DeckControl::Reloop), None, &s), Light::Off);
        // LOOP IN pressed: it flashes, LOOP OUT stays lit.
        s.decks[0].loop_in = Some(1.0);
        assert_eq!(light(d(DeckControl::LoopIn), None, &s), Light::Flash);
        assert_eq!(light(d(DeckControl::LoopOut), None, &s), Light::On);
        // LOOP OUT pressed: a 4-beat loop at 120 BPM (2 s); both flash.
        s.decks[0].loop_out = Some(3.0);
        s.decks[0].loop_active = true;
        assert_eq!(light(d(DeckControl::LoopIn), None, &s), Light::Flash);
        assert_eq!(light(d(DeckControl::LoopOut), None, &s), Light::Flash);
        assert_eq!(light(d(DeckControl::Reloop), None, &s), Light::On);
        assert_eq!(light(d(DeckControl::BeatLoop(4.0)), Some(4.0), &s), Light::Flash);
        assert_eq!(light(d(DeckControl::BeatLoop(8.0)), Some(8.0), &s), Light::Off);
    }

    #[test]
    fn pads_sync_and_effects() {
        let mut s = loaded();
        s.decks[0].hotcues[2] = Some(5.0);
        assert_eq!(light(d(DeckControl::Hotcue(2)), None, &s), Light::On);
        assert_eq!(light(d(DeckControl::HotcueClear(2)), None, &s), Light::On);
        assert_eq!(light(d(DeckControl::Hotcue(3)), None, &s), Light::Off);
        s.decks[0].sync = true;
        assert_eq!(light(d(DeckControl::Sync), None, &s), Light::On);
        assert_eq!(light(d(DeckControl::BeatJump), Some(4.0), &s), Light::Off, "no state");
        assert_eq!(light(Control::Fx(0, FxControl::On), None, &s), Light::Off);
        s.mixer.fx[0].enabled = true;
        assert_eq!(light(Control::Fx(0, FxControl::On), None, &s), Light::On, "the controller flashes it");
        assert_eq!(light(Control::Fx(0, FxControl::Release), None, &s), Light::On);
    }

    #[test]
    fn a_flash_is_on_then_off() {
        assert_eq!(value(Light::Flash, true), 0x7F);
        assert_eq!(value(Light::Flash, false), 0x00);
        assert_eq!(value(Light::On, false), 0x7F);
        assert_eq!(value(Light::Off, true), 0x00);
    }
}
