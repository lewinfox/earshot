//! The controller's lights. A button's light is the button's own message sent back
//! to the controller: 0x7F lights it, 0x00 puts it out (Pioneer's convention, and
//! Mixxx's). A button is lit when the command it's mapped to is on: BEAT SYNC while
//! sync is on, a hot cue pad while its cue is set. As on rekordbox: LOOP IN / OUT
//! are lit, and nothing else on a deck until a track is loaded; then CUE is lit (flashing
//! while paused away from the cue point) and PLAY flashes, lit once it plays. LOOP IN flashes once pressed, and LOOP IN / OUT
//! together while a loop plays (RELOOP/EXIT lit). BEAT FX ON gets "on" while the
//! effect is on, which the DDJ-400 shows by flashing it (and "off" lit steady).
//!
//! Pads in keyboard and key shift mode light as `core::pads` says. A sampler pad is
//! lit while its slot has a sound, and flashes while it plays (`core::sampler`); with
//! SHIFT (stop) it shows the same.
//!
//! While SHIFT is held a controller may show its shifted buttons' lights (on a
//! DDJ-400, SHIFT + PLAY is its own message, with its own light). The controller file
//! says which message each light is mirrored to (`mirror`, `mirror_status`). One
//! that does nothing of its own in earshot, or has no light of its own (SHIFT + BEAT
//! SYNC: tempo range), gets its button's light, so SHIFT leaves the lights alone; only
//! the ones whose job changes (SHIFT + a hot cue pad clears it, keyboard mode's page
//! pads) show their own.
//!
//! A controller's channel level meters (the mapping's `meters`) take one number each,
//! 0 to 127, and the controller lights its LEDs from it. They show each deck's level
//! before its fader, with the same colours as earshot's meters: red only over full
//! scale ([`meter_value`]).
//!
//! When earshot quits (or the controller is reconnected) the lights that are on go
//! out, and the meters drop to nothing.
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

use super::bus::{Control, DeckControl, FxControl, SamplerControl};
use super::layers::Layers;
use super::midi::{Input, Learned, Meter};
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
                DeckControl::Cue
                    if !deck.playing && (deck.position - deck.cue_point).abs() > 0.01 =>
                {
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
                    flash(
                        looping
                            && loop_beats(deck).is_some_and(|b| (b - beats).abs() < beats * 0.02),
                    )
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
                FxControl::TargetDeck(d) => {
                    on(matches!(fx.target, FxTarget::Deck(t) if t.index() == d as usize))
                }
                FxControl::TargetMaster => on(fx.target == FxTarget::Master),
                FxControl::TempoTap => on(fx.tap_bpm.is_some()),
                _ => Light::Off,
            }
        }
        Control::Sampler(slot, _) => super::sampler::light(&snap.sampler[slot.index()]),
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
        Control::Deck(
            d,
            D::ModePad {
                mode,
                pad,
                shift: true,
            },
        ) => pads[d.index()].shift_job(mode, pad as usize, snap.deck(d)),
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
        Control::Sampler(_, c) => c == SamplerControl::Play,
        Control::Fx(_, c) => matches!(
            c,
            FxControl::On
                | FxControl::Release
                | FxControl::TargetDeck(_)
                | FxControl::TargetMaster
                | FxControl::TempoTap
        ),
        _ => false,
    }
}

/// Every mapped button's message and value, as they should be now; and each message
/// its light is mirrored to (its SHIFT note: [`Learned::mirror_of`]) that hasn't a
/// light of its own, its button's.
pub fn frame(
    learned: &Learned,
    snap: &EngineSnapshot,
    pads: &AllPads,
    layers: &Layers,
    lit_half: bool,
) -> BTreeMap<(u8, u8), u8> {
    // A display-only button is the controller's to light.
    let notes: BTreeMap<(u8, u8), _> = learned
        .all()
        .into_iter()
        .filter(|(_, l)| l.input == Input::Note && !l.display_only)
        .collect();
    // A deck 1 or 2 button shows the deck its hand plays now; the layer switch
    // itself is lit while its hand is on its second deck.
    let shows = |l: &super::midi::Learn| match l.control {
        Control::Deck(d, DeckControl::LayerToggle) => {
            if layers.on(d.index() % 2) {
                Light::On
            } else {
                Light::Off
            }
        }
        c => light(layers.layered(c), l.arg, snap, pads),
    };
    let mut out: BTreeMap<(u8, u8), u8> = notes
        .iter()
        .map(|(&k, l)| (k, value(shows(l), lit_half)))
        .collect();
    for (&k, _) in &notes {
        let Some(twin) = learned.mirror_of(k) else {
            continue;
        };
        if notes
            .get(&twin)
            .is_none_or(|l| !own_light(layers.layered(l.control), snap, pads))
        {
            out.insert(twin, out[&k]);
        }
    }
    out
}

/// The value for a channel level `vu` (linear peak, 1 = full scale) on `meter`,
/// lighting its LEDs as earshot's own meters do (`ui::widgets::meter`): green from
/// -32 dB, amber above -6 dB, red only above full scale.
pub fn meter_value(vu: f64, meter: &Meter) -> u8 {
    const FLOOR_DB: f64 = -32.0;
    const AMBER_DB: f64 = -6.0;
    let (amber_at, red_at) = (meter.amber_at, meter.red_at);
    if vu <= 0.0 {
        return 0;
    }
    let db = 20.0 * vu.log10();
    if db > 0.0 {
        127
    } else if db > AMBER_DB {
        let span = (red_at - 1 - amber_at) as f64;
        amber_at + ((db - AMBER_DB) / -AMBER_DB * span).round() as u8
    } else {
        let span = amber_at.saturating_sub(1) as f64;
        ((db - FLOOR_DB) / (AMBER_DB - FLOOR_DB) * span)
            .round()
            .max(0.0) as u8
    }
}

/// Each meter's message and value now.
pub fn meters(meters: &[Meter], snap: &EngineSnapshot, layers: &Layers) -> Vec<((u8, u8), u8)> {
    meters
        .iter()
        .map(|m| {
            let deck = layers.deck_for(m.deck);
            ((m.status, m.data1), meter_value(snap.deck(deck).vu, m))
        })
        .collect()
}

/// Where the lights thread reads the engine's state and the decks' pads. The app
/// puts the engine's monitor here (again when the sound card is reopened), and the
/// pads whenever one is pressed.
pub type MonitorSlot = Arc<Mutex<LightsState>>;

#[derive(Default)]
pub struct LightsState {
    pub monitor: Option<Monitor>,
    pub pads: AllPads,
    /// Send the controllers their start messages again on the next tick (Debug mode's
    /// button).
    pub resend_start: bool,
    /// Messages to send the controllers as they are, on the next tick (Controller
    /// mode's Send tab). Each is logged.
    pub send: Vec<Vec<u8>>,
    /// Debug builds: the recorder, told every message sent.
    pub recorder: Option<super::debug::Recorder>,
}

/// The lights thread; dropping it puts out the lights earshot lit, and stops it.
///
/// Start-up and quitting follow Mixxx, which a DDJ-400 is happy with: nothing is sent
/// while the sound card starts or stops. Started before audio, it sets every light
/// (to how things are with no audio yet) and asks a DDJ-400 where its controls are,
/// before it returns; once audio runs only the lights that change are sent. Dropped
/// after audio has closed, it puts out only the lights that are on.
pub struct Lights {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Lights {
    /// `outputs`: each port, with its controller's start messages.
    pub fn start(
        mut outputs: Outputs,
        learned: Learned,
        monitor: MonitorSlot,
        layers: Layers,
    ) -> Lights {
        let sent = init(&mut outputs, &learned, &monitor);
        let stop = Arc::new(AtomicBool::new(false));
        let quit = stop.clone();
        let thread = std::thread::Builder::new()
            .name("midi-lights".into())
            .spawn(move || run(outputs, sent, &learned, &monitor, &layers, &quit))
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

/// What a controller is sent as it starts (its file's `start`), and how often again
/// after that, if it needs them repeated to stay in touch (`start_every_ms`: a
/// DDJ-FLX4 stops talking without them).
#[derive(Clone, Debug, Default)]
pub struct Start {
    pub messages: Vec<Vec<u8>>,
    pub every: Option<Duration>,
    last: Option<Instant>,
}

impl Start {
    pub fn new(messages: Vec<Vec<u8>>, every: Option<Duration>) -> Start {
        Start {
            messages,
            every,
            last: None,
        }
    }
}

/// Each output, with its controller's start messages.
pub type Outputs = Vec<(MidiOutputConnection, Start)>;

fn send(
    outputs: &mut Outputs,
    recorder: &Option<super::debug::Recorder>,
    (status, data1): (u8, u8),
    v: u8,
) {
    for (o, _) in outputs.iter_mut() {
        let _ = o.send(&[status, data1, v]);
    }
    if let Some(r) = recorder {
        r.note_midi(true, &[status, data1, v]);
    }
}

/// Send each controller the start messages from its controller file (a DDJ-400's:
/// the question where its controls are).
fn send_start(outputs: &mut Outputs, recorder: &Option<super::debug::Recorder>) {
    let mut sent = 0;
    for (o, start) in outputs.iter_mut() {
        for m in &start.messages {
            if o.send(m).is_ok() {
                sent += 1;
            }
            if let Some(r) = recorder {
                r.note_midi(true, m);
            }
        }
        start.last = Some(Instant::now());
        if let Some(e) = start.every {
            eprintln!("[midi] start messages repeat every {} ms", e.as_millis());
        }
    }
    eprintln!("[midi] sent {sent} start message(s)");
}

/// Send again the start messages of each controller that needs them repeated, when
/// it's time. Not logged or recorded: they go several times a second.
fn keep_alive(outputs: &mut Outputs) {
    for (o, start) in outputs.iter_mut() {
        let Some(every) = start.every else { continue };
        if start.last.is_some_and(|t| t.elapsed() < every) {
            continue;
        }
        for m in &start.messages {
            let _ = o.send(m);
        }
        start.last = Some(Instant::now());
    }
}

/// A message typed in Controller mode's Send tab, to every output, logged.
fn send_by_hand(outputs: &mut Outputs, recorder: &Option<super::debug::Recorder>, m: &[u8]) {
    let hex: Vec<String> = m.iter().map(|b| format!("{b:02X}")).collect();
    let mut sent = 0;
    for (o, _) in outputs.iter_mut() {
        match o.send(m) {
            Ok(()) => sent += 1,
            Err(e) => eprintln!("[midi] sending {} by hand: {e}", hex.join(" ")),
        }
    }
    eprintln!("[midi] sent by hand: {} (to {sent} port(s))", hex.join(" "));
    if let Some(r) = recorder {
        r.note_midi(true, m);
    }
}

/// The controller's start-up, before audio starts: every light as things are with no
/// audio (the decks empty, the meters at nothing), then the question where its
/// controls are (last, as Mixxx's DDJ-400 mapping does). What was sent.
fn init(outputs: &mut Outputs, learned: &Learned, monitor: &MonitorSlot) -> BTreeMap<(u8, u8), u8> {
    let (pads, recorder) = {
        let s = monitor.lock().unwrap();
        (s.pads, s.recorder.clone())
    };
    hello(outputs, &recorder, learned);
    let none = EngineSnapshot::default();
    let mut lights = frame(learned, &none, &pads, &Layers::default(), true);
    lights.extend(meters(&learned.meters(), &none, &Layers::default()));
    for (&k, &v) in &lights {
        send(outputs, &recorder, k, v);
    }
    send_start(outputs, &recorder);
    eprintln!(
        "[init] controller: {} light(s) set, before audio",
        lights.len()
    );
    lights
}

/// What the hot cue pads spell as the controller starts, in binary: a letter on each
/// deck's eight pads (the first pad the highest bit), the next letters after a moment.
const HELLO: &[u8] = b"earshot";
const HELLO_STEP: Duration = Duration::from_millis(150);

/// Each deck's eight hot cue pads, as mapped, for the first two decks that have any.
fn hotcue_pads(learned: &Learned) -> Vec<[Option<(u8, u8)>; 8]> {
    let mut decks: BTreeMap<usize, [Option<(u8, u8)>; 8]> = BTreeMap::new();
    for (k, l) in learned.all() {
        if l.input != Input::Note || l.display_only {
            continue;
        }
        if let Control::Deck(d, DeckControl::Hotcue(n)) = l.control
            && n < 8
        {
            decks.entry(d.index()).or_default()[n as usize].get_or_insert(k);
        }
    }
    decks.into_values().take(2).collect()
}

/// The frames of [`HELLO`]: each pad's light, frame by frame.
fn hello_frames(decks: &[[Option<(u8, u8)>; 8]]) -> Vec<Vec<((u8, u8), u8)>> {
    if decks.is_empty() {
        return vec![];
    }
    HELLO
        .chunks(decks.len())
        .map(|letters| {
            let mut f = vec![];
            for (i, pads) in decks.iter().enumerate() {
                let b = letters.get(i).copied().unwrap_or(0);
                for (j, k) in pads.iter().enumerate() {
                    if let Some(k) = k {
                        f.push((*k, if b & (0x80 >> j) != 0 { 0x7F } else { 0 }));
                    }
                }
            }
            f
        })
        .collect()
}

/// Spell [`HELLO`] on the pads (a moment before audio starts).
fn hello(outputs: &mut Outputs, recorder: &Option<super::debug::Recorder>, learned: &Learned) {
    for f in hello_frames(&hotcue_pads(learned)) {
        for (k, v) in f {
            send(outputs, recorder, k, v);
        }
        std::thread::sleep(HELLO_STEP);
    }
}

fn run(
    mut outputs: Outputs,
    mut sent: BTreeMap<(u8, u8), u8>,
    learned: &Learned,
    monitor: &MonitorSlot,
    layers: &Layers,
    stop: &AtomicBool,
) {
    let start = Instant::now();
    let mut recorder = None;
    let mut by_hand: Vec<Vec<u8>> = vec![];
    while !stop.load(Ordering::Relaxed) {
        let (snap, pads, ask) = {
            let mut s = monitor.lock().unwrap();
            recorder.clone_from(&s.recorder);
            let ask = std::mem::take(&mut s.resend_start);
            by_hand = std::mem::take(&mut s.send);
            (s.monitor.as_ref().map(|m| m.snapshot()), s.pads, ask)
        };
        if ask {
            send_start(&mut outputs, &recorder);
        }
        for m in by_hand.drain(..) {
            send_by_hand(&mut outputs, &recorder, &m);
        }
        keep_alive(&mut outputs);
        let lit_half = (start.elapsed().as_millis() / FLASH.as_millis()) % 2 == 0;
        // No audio (it's reopening): as at start-up, the decks empty.
        let s = snap.unwrap_or_default();
        let mut now = frame(learned, &s, &pads, layers, lit_half);
        now.extend(meters(&learned.meters(), &s, layers));
        for (&k, &v) in &now {
            if sent.get(&k) != Some(&v) {
                send(&mut outputs, &recorder, k, v);
            }
        }
        // A button no longer mapped: put its light out.
        for (&k, _) in sent
            .iter()
            .filter(|(k, v)| **v != 0 && !now.contains_key(k))
        {
            send(&mut outputs, &recorder, k, 0);
        }
        sent = now;
        std::thread::sleep(TICK);
    }
    let off = off_at_exit(&sent);
    for &k in &off {
        send(&mut outputs, &recorder, k, 0);
    }
    eprintln!("[midi] {} light(s) put out on closing", off.len());
}

/// The lights to put out on leaving: only those on (as Mixxx's DDJ-400 mapping puts
/// out the ones it lit). The ones the controller lights itself (its pad mode buttons)
/// it keeps lit, as it does after Mixxx.
fn off_at_exit(sent: &BTreeMap<(u8, u8), u8>) -> Vec<(u8, u8)> {
    sent.iter()
        .filter(|(_, v)| **v != 0)
        .map(|(k, _)| *k)
        .collect()
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
            [
                DeckControl::LoopIn,
                DeckControl::LoopOut,
                DeckControl::Cue,
                DeckControl::Play,
            ]
            .map(|c| light(d(c), None, s, &AllPads::default()))
        };
        use Light::*;
        let mut s = EngineSnapshot::default();
        assert_eq!(lights(&s), [On, On, Off, Off], "nothing loaded");
        assert_eq!(
            light(d(DeckControl::Sync), None, &s, &AllPads::default()),
            Off,
            "nothing else lit"
        );
        s.decks[0].pfl = true;
        assert_eq!(
            light(d(DeckControl::Pfl), None, &s, &AllPads::default()),
            On,
            "but headphone cue"
        );
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
        assert_eq!(
            light(d(DeckControl::LoopIn), None, &s, &AllPads::default()),
            Light::On,
            "ready"
        );
        assert_eq!(
            light(d(DeckControl::Reloop), None, &s, &AllPads::default()),
            Light::Off
        );
        // LOOP IN pressed: it flashes, LOOP OUT stays lit.
        s.decks[0].loop_in = Some(1.0);
        assert_eq!(
            light(d(DeckControl::LoopIn), None, &s, &AllPads::default()),
            Light::Flash
        );
        assert_eq!(
            light(d(DeckControl::LoopOut), None, &s, &AllPads::default()),
            Light::On
        );
        // LOOP OUT pressed: a 4-beat loop at 120 BPM (2 s); both flash.
        s.decks[0].loop_out = Some(3.0);
        s.decks[0].loop_active = true;
        assert_eq!(
            light(d(DeckControl::LoopIn), None, &s, &AllPads::default()),
            Light::Flash
        );
        assert_eq!(
            light(d(DeckControl::LoopOut), None, &s, &AllPads::default()),
            Light::Flash
        );
        assert_eq!(
            light(d(DeckControl::Reloop), None, &s, &AllPads::default()),
            Light::On
        );
        assert_eq!(
            light(
                d(DeckControl::BeatLoop(4.0)),
                Some(4.0),
                &s,
                &AllPads::default()
            ),
            Light::Flash
        );
        assert_eq!(
            light(
                d(DeckControl::BeatLoop(8.0)),
                Some(8.0),
                &s,
                &AllPads::default()
            ),
            Light::Off
        );
    }

    #[test]
    fn pads_sync_and_effects() {
        let mut s = loaded();
        s.decks[0].hotcues[2] = Some(5.0);
        assert_eq!(
            light(d(DeckControl::Hotcue(2)), None, &s, &AllPads::default()),
            Light::On
        );
        assert_eq!(
            light(
                d(DeckControl::HotcueClear(2)),
                None,
                &s,
                &AllPads::default()
            ),
            Light::On
        );
        assert_eq!(
            light(d(DeckControl::Hotcue(3)), None, &s, &AllPads::default()),
            Light::Off
        );
        s.decks[0].sync = true;
        assert_eq!(
            light(d(DeckControl::Sync), None, &s, &AllPads::default()),
            Light::On
        );
        assert_eq!(
            light(d(DeckControl::BeatJump), Some(4.0), &s, &AllPads::default()),
            Light::Off,
            "no state"
        );
        assert_eq!(
            light(Control::Fx(0, FxControl::On), None, &s, &AllPads::default()),
            Light::Off
        );
        s.mixer.fx[0].enabled = true;
        assert_eq!(
            light(Control::Fx(0, FxControl::On), None, &s, &AllPads::default()),
            Light::On,
            "the controller flashes it"
        );
        assert_eq!(
            light(
                Control::Fx(0, FxControl::Release),
                None,
                &s,
                &AllPads::default()
            ),
            Light::On
        );
    }

    /// A sampler pad: dark while its slot is empty, lit with a sound in it, flashing
    /// while it plays; SHIFT + pad (stop) shows the same.
    #[test]
    fn sampler_pads_show_loaded_and_playing() {
        let learned = Learned::default();
        learned.seed_bundled("ddj-400");
        let mut s = EngineSnapshot::default();
        let slot = |i| earshot_engine::SampleSlot::new(i).unwrap();
        let play = |i| Control::Sampler(slot(i), SamplerControl::Play);
        assert_eq!(
            light(play(0), None, &s, &AllPads::default()),
            Light::Off,
            "empty"
        );
        s.sampler[0].loaded = true;
        s.sampler[9].loaded = true;
        s.sampler[9].playing = true;
        assert_eq!(
            light(play(0), None, &s, &AllPads::default()),
            Light::On,
            "loaded"
        );
        assert_eq!(
            light(play(9), None, &s, &AllPads::default()),
            Light::Flash,
            "playing"
        );
        // On the controller: deck 1's pad 1 lit, deck 2's pad 2 flashing, the rest dark,
        // and the same with SHIFT held.
        for lit_half in [true, false] {
            let f = frame(
                &learned,
                &s,
                &AllPads::default(),
                &Layers::default(),
                lit_half,
            );
            assert_eq!(f[&(0x97, 0x30)], 0x7F);
            assert_eq!(f[&(0x98, 0x30)], 0x7F, "with SHIFT");
            assert_eq!(f[&(0x97, 0x31)], 0x00, "an empty slot");
            let flash = if lit_half { 0x7F } else { 0x00 };
            assert_eq!(f[&(0x99, 0x31)], flash);
            assert_eq!(f[&(0x9A, 0x31)], flash, "with SHIFT");
        }
    }

    /// Only the lights on go out at exit: Mixxx sends a DDJ-400 a short list, and a
    /// burst of every mapped button can leave it deaf (#42).
    /// With the left hand on deck 3, deck 1's buttons show deck 3 (deck 2's still deck
    /// 2), and so do its level meter's LEDs.
    #[test]
    fn the_lights_follow_the_hands() {
        let learned = Learned::default();
        learned.seed_bundled("ddj-400");
        let key = |c: Control| {
            learned
                .all()
                .into_iter()
                .find(|(_, l)| l.control == c && l.input == Input::Note)
                .map(|(k, _)| k)
                .expect("mapped")
        };
        let (play1, play2) = (
            key(d(DeckControl::Play)),
            key(Control::Deck(deck(1), DeckControl::Play)),
        );
        let mut s = loaded();
        s.decks_live = 4;
        s.decks[2].track_id = Some(3);
        s.decks[2].playing = true;
        s.decks[2].vu = 1.0;
        let layers = Layers::default();
        layers.set([false, false], true);
        let pads = AllPads::default();
        let lit = |layers: &Layers, k| frame(&learned, &s, &pads, layers, true)[&k];
        // Deck 1 is paused on its cue point (its play flashes: lit this half), but not
        // in the other half; deck 3 plays, so is lit in both.
        let other_half = |layers: &Layers, k| frame(&learned, &s, &pads, layers, false)[&k];
        assert_eq!(other_half(&layers, play1), 0);
        layers.set([true, false], true);
        assert_eq!(lit(&layers, play1), 0x7F);
        assert_eq!(other_half(&layers, play1), 0x7F, "deck 3 is playing");
        assert_eq!(other_half(&layers, play2), 0, "deck 2 isn't");
        let ms = learned.meters();
        if let Some(i) = ms.iter().position(|m| m.deck == deck(0)) {
            let level = |layers: &Layers| meters(&ms, &s, layers)[i].1;
            assert!(level(&layers) > level(&Layers::default()), "deck 3's level");
        }
    }

    #[test]
    fn only_lit_lights_go_out_at_exit() {
        let mut sent = BTreeMap::new();
        sent.insert((0x90, 0x0B), 0x7F); // a play button, lit
        sent.insert((0x90, 0x0C), 0x00); // a cue button, off
        sent.insert((0xB0, 0x02), 0x40); // a level meter, showing
        assert_eq!(off_at_exit(&sent), vec![(0x90, 0x0B), (0xB0, 0x02)]);
    }

    /// The pads spell "earshot" a letter a deck: "e" (0x65) then "a" (0x61) first.
    #[test]
    fn the_pads_spell_earshot() {
        let learned = Learned::default();
        learned.seed_bundled("ddj-400");
        let pads = hotcue_pads(&learned);
        assert_eq!(pads.len(), 2);
        let frames = hello_frames(&pads);
        assert_eq!(frames.len(), 4, "ea rs ho t");
        let lit: Vec<u8> = frames[0].iter().map(|(_, v)| *v).collect();
        let bits = |b: u8| (0..8).map(move |j| if b & (0x80 >> j) != 0 { 0x7F } else { 0 });
        assert_eq!(lit, bits(b'e').chain(bits(b'a')).collect::<Vec<_>>());
        assert!(
            frames[3][8..].iter().all(|(_, v)| *v == 0),
            "nothing after the t"
        );
    }

    /// With no audio (at start-up), every light is set, and the meters show nothing.
    #[test]
    fn start_up_lights_are_set_with_no_audio() {
        let learned = Learned::default();
        learned.seed_bundled("ddj-400");
        let none = EngineSnapshot::default();
        let f = frame(
            &learned,
            &none,
            &AllPads::default(),
            &Layers::default(),
            true,
        );
        assert!(f.contains_key(&(0x90, 0x0B)), "play is set");
        assert!(
            meters(&learned.meters(), &none, &Layers::default())
                .iter()
                .all(|(_, v)| *v == 0)
        );
    }

    /// SHIFT held shows the shifted buttons' lights: those without a job of their own
    /// show their button's, so SHIFT changes only the lights whose job changes.
    #[test]
    fn shift_leaves_the_lights_alone_but_for_changed_jobs() {
        let learned = Learned::default();
        learned.seed_bundled("ddj-400");
        let mut s = loaded();
        s.decks[0].playing = true;
        s.decks[0].sync = true;
        s.decks[0].loop_in = Some(1.0);
        s.decks[0].loop_out = Some(3.0);
        s.decks[0].loop_active = true;
        s.decks[0].hotcues[0] = Some(2.0);
        let f = frame(&learned, &s, &AllPads::default(), &Layers::default(), true);
        for (button, twin, what) in [
            ((0x90, 0x0B), (0x90, 0x47), "PLAY"),
            ((0x90, 0x0C), (0x90, 0x48), "CUE"),
            ((0x90, 0x58), (0x90, 0x60), "BEAT SYNC"),
            ((0x90, 0x10), (0x90, 0x4C), "LOOP IN"),
            (
                (0x97, 0x64),
                (0x98, 0x64),
                "beat loop pad 5 (the 4-beat loop)",
            ),
            (
                (0x97, 0x40),
                (0x98, 0x40),
                "keyboard pad 1, picking (hot cue A set)",
            ),
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
        let rows = r#"{"device":"DDJ-400","inputs":[{"command":"deck_1_beat_sync","message":"90 58"},{"command":"deck_1_tempo_range_next","message":"90 60"}],"mirror":{"90 58":"90 60"}}"#;
        let learned = Learned::default();
        learned.replace(crate::core::midi::from_json(rows).unwrap().1);
        let f = frame(&learned, &s, &AllPads::default(), &Layers::default(), true);
        assert_eq!(f[&(0x90, 0x60)], 0x7F, "tempo range shows BEAT SYNC's");
    }

    #[test]
    fn a_flash_is_on_then_off() {
        assert_eq!(value(Light::Flash, true), 0x7F);
        assert_eq!(value(Light::Flash, false), 0x00);
        assert_eq!(value(Light::On, false), 0x7F);
        assert_eq!(value(Light::Off, true), 0x00);
    }

    #[test]
    fn meters_follow_the_level_in_decibels() {
        let m = Meter {
            deck: deck(1),
            status: 0xB1,
            data1: 0x02,
            amber_at: 87,
            red_at: 119,
        };
        let v = |d: f64| meter_value(10f64.powf(d / 20.0), &m);
        assert_eq!(meter_value(0.0, &m), 0);
        assert_eq!(v(-40.0), 0);
        assert_eq!(v(-6.01), 86, "green up to -6 dB");
        assert_eq!(v(-5.99), 87, "amber above it");
        assert_eq!(v(0.0), 118, "full scale isn't red");
        assert_eq!(v(0.01), 127, "red only over it");
        let mut last = 0;
        for i in 0..=400 {
            let x = v(-40.0 + i as f64 * 0.1);
            assert!(x >= last, "rises with the level");
            last = x;
        }
        let mut s = EngineSnapshot::default();
        s.decks[1].vu = 2.0;
        assert_eq!(meters(&[m], &s, &Layers::default()), [((0xB1, 0x02), 127)]);
    }
}
