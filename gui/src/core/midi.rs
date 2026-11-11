//! MIDI input. Every message is echoed to the UI (Controller mode's Mapping and
//! History tabs) and looked up in the session's mapping ([`Learned`]): which command
//! each message runs. The Mapping tab edits it while it runs, and saves it as a
//! controller file.
//!
//! When a DDJ-400 is connected the mapping starts from a built-in table, written in
//! Mixxx's `[Group] key` names and turned into registry controls
//! ([`Control::from_mixxx`]); any other controller starts with nothing mapped.
//!
//! A message becomes a [`Command`]: one the engine can take as it is goes straight to
//! it on MIDI's own ring ([`Lane`]), without the player lock; anything else (a beat
//! jump by the deck's step, library browsing) goes to the UI as [`Event::Command`].

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use midir::{MidiInput, MidiInputConnection};

use super::bus::{Control, Kind};
use super::command::{self, Command, Lane};
use super::{Event, Sink};

/// How a message's data byte becomes the control's value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Input {
    /// A note: velocity > 0 is a press (1), 0 a release (0).
    Note,
    /// A 7-bit absolute CC, across the control's travel.
    Cc,
    /// A relative encoder: 1..63 = +n steps, 65..127 = -(128 - v) steps.
    Relative,
    /// A jog wheel's turn, centred on 64 (65 = one tick forward, 63 one back), with
    /// this many ticks to a turn. The value is seconds of track: a turn is as far as
    /// a record goes in one turn at 33⅓ rpm, 1.8 s.
    Jog(u32),
}

/// Seconds of a record that pass in one turn at 33⅓ rpm.
const SECS_PER_TURN: f64 = 60.0 / (100.0 / 3.0);

struct Mapping {
    status: u8,
    data1: u8,
    control: Control,
    input: Input,
}

/// The DDJ-400's two decks, its mixer and browse knob, in Mixxx's names.
fn ddj400_names() -> Vec<(u8, u8, String, String, Input)> {
    let mut v = vec![];
    let mut m = |status, data1, group: &str, key: &str, input| {
        v.push((status, data1, group.to_string(), key.to_string(), input))
    };
    // The controller has two decks, on MIDI channels 1 and 2.
    for d in 0..2u8 {
        let g = format!("[Channel{}]", d + 1);
        let (note, cc) = (0x90 + d, 0xB0 + d);
        m(note, 0x0B, &g, "play", Input::Note);
        m(note, 0x0C, &g, "cue_default", Input::Note);
        m(note, 0x58, &g, "sync_enabled", Input::Note);
        m(note, 0x54, &g, "pfl", Input::Note);
        m(note, 0x10, &g, "loop_in", Input::Note);
        m(note, 0x11, &g, "loop_out", Input::Note);
        m(note, 0x4D, &g, "reloop_toggle", Input::Note);
        // Tempo slider: as Mixxx's mapping reads it, a raw 0 is the slowest end (the
        // top of the fader is minus on Pioneer gear).
        m(cc, 0x00, &g, "rate", Input::Cc);
        m(cc, 0x13, &g, "volume", Input::Cc);
        // The jog wheel (Mixxx's mapping, 720 ticks a turn): its top touched, the top
        // turned while touched (scratch in vinyl mode), the top turned untouched (still
        // spinning) and the side turned (both bend).
        m(note, 0x36, &g, "jog_touch", Input::Note);
        m(cc, 0x22, &g, "jog_turn", Input::Jog(720));
        m(cc, 0x23, &g, "jog_bend", Input::Jog(720));
        m(cc, 0x21, &g, "jog_bend", Input::Jog(720));
        // Performance pads in hot cue mode, unshifted / shifted.
        for k in 0..8u8 {
            let n = k + 1;
            m(
                0x97 + 2 * d,
                k,
                &g,
                &format!("hotcue_{n}_activate"),
                Input::Note,
            );
            m(
                0x98 + 2 * d,
                k,
                &g,
                &format!("hotcue_{n}_clear"),
                Input::Note,
            );
        }
        m(0x96, 0x46 + d, &g, "LoadSelectedTrack", Input::Note);
    }
    m(0xB6, 0x1F, "[Master]", "crossfader", Input::Cc);
    m(0xB6, 0x40, "[Library]", "MoveVertical", Input::Relative);
    m(0x96, 0x41, "[Library]", "TogglePane", Input::Note);
    v
}

fn ddj400() -> Vec<Mapping> {
    ddj400_names()
        .into_iter()
        .filter_map(|(status, data1, group, key, input)| {
            let Some(control) = Control::from_mixxx(&group, &key) else {
                eprintln!("MIDI: no control {group} {key}");
                return None;
            };
            Some(Mapping {
                status,
                data1,
                control,
                input,
            })
        })
        .collect()
}

/// A message's key: its status byte (a note-off counted as a note-on) and first data
/// byte, which together say which control on the controller sent it.
pub fn key(bytes: &[u8]) -> Option<(u8, u8)> {
    let (status, data1) = (*bytes.first()?, *bytes.get(1)?);
    Some(match status & 0xF0 {
        0x80 => (0x90 | (status & 0x0F), data1),
        _ => (status, data1),
    })
}

/// One binding set by hand in the Mapping tab: the command a message runs, with the
/// fixed word after its name (`deck_1_beat_jump -4`: `arg` -4).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Learn {
    pub control: Control,
    pub arg: Option<f64>,
    pub input: Input,
}

/// The session's mapping: the command each message runs, by message key, on every
/// port. Edited in the Mapping tab; it lasts until earshot quits (or it's saved as a
/// file).
#[derive(Clone, Default)]
pub struct Learned(Arc<Mutex<Session>>);

#[derive(Default)]
struct Session {
    map: BTreeMap<(u8, u8), Learn>,
    /// The built-in table has been put in once (so clearing it sticks).
    seeded: bool,
}

impl Learned {
    pub fn get(&self, key: (u8, u8)) -> Option<Learn> {
        self.0.lock().unwrap().map.get(&key).copied()
    }

    pub fn set(&self, key: (u8, u8), l: Learn) {
        self.0.lock().unwrap().map.insert(key, l);
    }

    pub fn remove(&self, key: (u8, u8)) {
        self.0.lock().unwrap().map.remove(&key);
    }

    pub fn clear(&self) {
        self.0.lock().unwrap().map.clear();
    }

    pub fn all(&self) -> Vec<((u8, u8), Learn)> {
        let s = self.0.lock().unwrap();
        s.map.iter().map(|(k, l)| (*k, *l)).collect()
    }

    /// Start from the built-in DDJ-400 table, the first time one is connected.
    fn seed_ddj400(&self) {
        let mut s = self.0.lock().unwrap();
        if std::mem::replace(&mut s.seeded, true) {
            return;
        }
        for mp in ddj400() {
            let l = Learn {
                control: mp.control,
                arg: None,
                input: mp.input,
            };
            s.map.entry((mp.status, mp.data1)).or_insert(l);
        }
    }

    /// The controller file for these bindings, as JSON. A line says only what a
    /// message runs; how its value reads is left out where the message says it (a
    /// note is a button, a CC a knob or fader).
    pub fn to_json(&self, device: &str) -> String {
        use serde_json::{Value, json};
        let inputs: Vec<Value> = self
            .all()
            .into_iter()
            .map(|((status, data1), l)| {
                let (name, word) = l.control.name();
                let mut o = serde_json::Map::new();
                o.insert("message".into(), json!(format!("{status:02X} {data1:02X}")));
                o.insert("command".into(), json!(name));
                if let Some(w) = word.or_else(|| l.arg.map(|a| a.to_string())) {
                    o.insert("value".into(), json!(w));
                }
                if l.input != default_input(status) {
                    let (input, ticks) = match l.input {
                        Input::Note => ("button", None),
                        Input::Cc => ("absolute", None),
                        Input::Relative => ("relative", None),
                        Input::Jog(t) => ("jog", Some(t)),
                    };
                    o.insert("input".into(), json!(input));
                    if let Some(t) = ticks {
                        o.insert("ticks".into(), json!(t));
                    }
                }
                Value::Object(o)
            })
            .collect();
        let file = json!({ "device": device, "inputs": inputs });
        serde_json::to_string_pretty(&file).expect("JSON") + "\n"
    }
}

/// How a message reads unless a binding says otherwise: a note is a button, anything
/// else a knob or fader.
fn default_input(status: u8) -> Input {
    match status & 0xF0 {
        0x80 | 0x90 => Input::Note,
        _ => Input::Cc,
    }
}

/// How a newly mapped message reads: as a button for a note, steps for a control
/// that takes steps, and a knob or fader for the rest.
pub fn guess_input(key: (u8, u8), control: Control) -> Input {
    match (default_input(key.0), control.spec().kind) {
        (Input::Note, _) => Input::Note,
        (_, Kind::Relative) => Input::Relative,
        _ => Input::Cc,
    }
}

/// A message as it came in, for the Controller mode's Mapping tab.
#[derive(Clone, Debug)]
pub struct MidiMsg {
    /// Which port, by its place in [`connect_all`]'s names.
    pub port: usize,
    pub bytes: Vec<u8>,
    pub at: std::time::Instant,
    /// The command it ran, if it's mapped.
    pub did: Option<Control>,
}

/// Keeps the input connections open; dropping it closes them.
#[derive(Default)]
pub struct Midi {
    _connections: Vec<MidiInputConnection<()>>,
}

/// Connect to every MIDI input port, sending to the engine on `lane`. Returns the
/// connections and the port names.
pub fn connect_all(
    sink: &Sink,
    lane: &Lane,
    learned: &Learned,
) -> Result<(Midi, Vec<String>), String> {
    let probe = MidiInput::new("earshot-probe").map_err(|e| e.to_string())?;
    let ports = probe.ports();
    let mut names = Vec::new();
    let mut connections = Vec::new();
    for port in &ports {
        let name = probe.port_name(port).unwrap_or_else(|_| "unknown".into());
        if name.contains("Midi Through") {
            continue;
        }
        let input = MidiInput::new("earshot").map_err(|e| e.to_string())?;
        if name.contains("DDJ-400") {
            learned.seed_ddj400();
        }
        let (sink, lane, learned) = (sink.clone(), lane.clone(), learned.clone());
        let index = names.len();
        match input.connect(
            port,
            "earshot-in",
            move |_, bytes, _| on_message(&sink, &lane, bytes, &learned, index),
            (),
        ) {
            Ok(c) => {
                connections.push(c);
                names.push(name);
            }
            Err(e) => eprintln!("MIDI: could not open {name}: {e}"),
        }
    }
    Ok((
        Midi {
            _connections: connections,
        },
        names,
    ))
}

/// The control a message runs, and the command it makes (if that value does
/// anything).
fn translate(bytes: &[u8], learned: &Learned) -> Option<(Control, Option<Command>)> {
    if bytes.len() < 3 {
        return None;
    }
    let key = key(bytes)?;
    // A note-off is a note-on with velocity 0.
    let data2 = if bytes[0] & 0xF0 == 0x80 { 0 } else { bytes[2] };
    let l = learned.get(key)?;
    let value = match l.input {
        Input::Note if data2 > 0 => l.control.press_value(l.arg),
        Input::Note => 0.0,
        Input::Cc => {
            let travel = data2 as f64 / 127.0;
            match l.control.spec().kind {
                Kind::Continuous(range) => range.from_pos(travel),
                _ => travel,
            }
        }
        Input::Relative if data2 < 64 => data2 as f64,
        Input::Relative => data2 as f64 - 128.0,
        Input::Jog(ticks) => (data2 as f64 - 64.0) * SECS_PER_TURN / ticks as f64,
    };
    Some((l.control, l.control.command(value)))
}

fn on_message(
    sink: &Sink,
    lane: &Lane,
    bytes: &[u8],
    learned: &Learned,
    port: usize,
) {
    let found = translate(bytes, learned);
    sink(Event::Midi(MidiMsg {
        port,
        bytes: bytes.to_vec(),
        at: std::time::Instant::now(),
        did: found.as_ref().map(|(c, _)| *c),
    }));
    let Some((_, Some(cmd))) = found else {
        return;
    };
    match command::direct(cmd) {
        Ok(c) => {
            if let Err(e) = lane.send(c) {
                eprintln!("MIDI: {e}");
            }
        }
        Err(cmd) => sink(Event::Command(cmd)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::command::{DeckCmd, ViewCmd};
    use earshot_engine::{
        ChannelParam, DeckCmd as EngineCmd, DeckId, MasterParam, ParamId, Press, Set,
    };

    fn deck(i: usize) -> DeckId {
        DeckId::new(i).unwrap()
    }

    fn ddj() -> Learned {
        let l = Learned::default();
        l.seed_ddj400();
        l
    }

    fn cmd(bytes: impl AsRef<[u8]>) -> Option<Command> {
        translate(bytes.as_ref(), &ddj())?.1
    }

    /// A DDJ-400 starts from the built-in table, once; any binding can be changed,
    /// and a button can send a fixed value.
    #[test]
    fn the_session_mapping() {
        use crate::core::bus::DeckControl;
        let learned = ddj();
        let run = |b: &[u8]| translate(b, &learned).and_then(|(_, c)| c);
        // Deck 1's play button jumps back 4 beats instead.
        let jump = Control::Deck(deck(0), DeckControl::BeatJump);
        learned.set(
            (0x90, 0x0B),
            Learn {
                control: jump,
                arg: Some(-4.0),
                input: guess_input((0x90, 0x0B), jump),
            },
        );
        assert!(matches!(
            run(&[0x90, 0x0B, 0x7F]),
            Some(Command::Deck(_, DeckCmd::Engine(EngineCmd::BeatJump(b)))) if b == -4.0
        ));
        assert!(run(&[0x80, 0x0B, 0x00]).is_none(), "nothing on release");
        let scratch = Control::Deck(deck(1), DeckControl::JogTurn);
        assert_eq!(guess_input((0xB5, 0x01), scratch), Input::Relative);
        assert_eq!(guess_input((0x95, 0x01), scratch), Input::Note);
        assert_eq!(
            guess_input((0xB5, 0x01), Control::Master(MasterParam::Crossfader)),
            Input::Cc
        );
        learned.set(
            (0xB0, 0x22),
            Learn {
                control: scratch,
                arg: None,
                input: Input::Jog(720),
            },
        );
        let json = learned.to_json("DDJ-400");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["device"], "DDJ-400");
        let line = |m: &str| {
            v["inputs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|i| i["message"] == m)
                .cloned()
                .unwrap()
        };
        assert_eq!(
            line("90 0B"),
            serde_json::json!({ "message": "90 0B", "command": "deck_1_beat_jump", "value": "-4" })
        );
        assert_eq!(
            line("B0 22"),
            serde_json::json!({ "message": "B0 22", "command": "deck_2_scratch", "input": "jog", "ticks": 720 })
        );
        assert_eq!(line("B0 13")["command"], "deck_1_channel_fader");
        // Unmapped: does nothing. Cleared, it stays cleared on the next connect.
        learned.remove((0x90, 0x0B));
        assert!(run(&[0x90, 0x0B, 0x7F]).is_none());
        learned.clear();
        learned.seed_ddj400();
        assert!(learned.all().is_empty());
    }

    #[test]
    fn every_entry_maps_to_a_control() {
        assert_eq!(ddj400().len(), ddj400_names().len());
        assert!(translate(&[0x90, 0x0B, 0x7F], &Learned::default()).is_none(), "nothing mapped");
    }

    #[test]
    fn buttons_and_toggles() {
        // Play on deck 2 toggles, on press only.
        assert!(matches!(
            cmd([0x91, 0x0B, 0x7F]),
            Some(Command::Deck(d, DeckCmd::Engine(EngineCmd::Play(Set::Toggle)))) if d == deck(1)
        ));
        assert!(cmd([0x91, 0x0B, 0x00]).is_none());
        // Cue is held: a note-off is its release.
        assert!(matches!(
            cmd([0x80, 0x0C, 0x40]),
            Some(Command::Deck(d, DeckCmd::Engine(EngineCmd::Cue(Press::Up)))) if d == deck(0)
        ));
        // Shifted pad 3 on deck 1 clears hot cue 3.
        assert!(matches!(
            cmd([0x98, 0x02, 0x7F]),
            Some(Command::Deck(_, DeckCmd::Engine(EngineCmd::HotcueClear(2))))
        ));
        assert!(matches!(
            cmd([0x99, 0x07, 0x7F]),
            Some(Command::Deck(d, DeckCmd::Engine(EngineCmd::Hotcue { slot: 7, press: Press::Down }))) if d == deck(1)
        ));
        assert!(cmd([0x90, 0x7E, 0x7F]).is_none(), "unmapped");
        assert!(cmd([0x90, 0x0B]).is_none(), "too short");
    }

    #[test]
    fn faders_cover_their_range() {
        assert!(matches!(
            cmd([0xB0, 0x13, 127]),
            Some(Command::Param(ParamId::Channel(_, ChannelParam::Volume), v)) if v == 1.0
        ));
        assert!(matches!(
            cmd([0xB6, 0x1F, 0]),
            Some(Command::Param(ParamId::Master(MasterParam::Crossfader), v)) if v == -1.0
        ));
        assert!(matches!(
            cmd([0xB1, 0x00, 127]),
            Some(Command::Deck(d, DeckCmd::Engine(EngineCmd::TempoFader(v)))) if v == 1.0 && d == deck(1)
        ));
    }

    #[test]
    fn the_jog_wheel_scratches() {
        assert!(matches!(
            cmd([0x91, 0x36, 0x7F]),
            Some(Command::Deck(d, DeckCmd::Engine(EngineCmd::Hand(true)))) if d == deck(1)
        ));
        assert!(matches!(
            cmd([0x80, 0x36, 0x00]),
            Some(Command::Deck(_, DeckCmd::Engine(EngineCmd::Hand(false))))
        ));
        // Two ticks back: 2/720 of 1.8 s.
        let Some(Command::Deck(_, DeckCmd::Engine(EngineCmd::HandMove { by, .. }))) =
            cmd([0xB0, 0x22, 62])
        else {
            panic!("not a hand move")
        };
        assert!((by + 0.005).abs() < 1e-9, "{by}");
        assert!(command::direct(cmd([0xB0, 0x22, 65]).unwrap()).is_ok());
        // The side bends.
        assert!(matches!(
            cmd([0xB1, 0x21, 66]),
            Some(Command::Deck(d, DeckCmd::Engine(EngineCmd::Bend { by, .. }))) if d == deck(1) && by > 0.0
        ));
    }

    #[test]
    fn engine_commands_go_direct_and_the_rest_to_the_ui() {
        let play = cmd([0x90, 0x0B, 0x7F]).unwrap();
        assert!(command::direct(play).is_ok());
        let volume = cmd([0xB0, 0x13, 64]).unwrap();
        assert!(command::direct(volume).is_ok());
        // Browsing and loading need the library: the UI does those.
        let browse = cmd([0xB6, 0x40, 0x7E]).unwrap();
        assert!(matches!(
            command::direct(browse),
            Err(Command::View(ViewCmd::Browse(-2)))
        ));
        let load = cmd([0x96, 0x47, 0x7F]).unwrap();
        assert!(matches!(
            command::direct(load),
            Err(Command::View(ViewCmd::LoadSelected(d))) if d == deck(1)
        ));
    }

    #[test]
    fn sending_without_audio_fails_quietly() {
        // No engine yet: the lane has no ring, and says so instead of blocking.
        let lane = Lane::default();
        let c = command::direct(cmd([0x90, 0x0B, 0x7F]).unwrap()).unwrap();
        assert!(lane.send(c).is_err());
    }
}
