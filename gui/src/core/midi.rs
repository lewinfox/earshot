//! MIDI input. Every message is echoed to the UI's MIDI monitor and looked up in a
//! hardcoded DDJ-400 table. The real thing will load Mixxx's XML + JS mappings instead
//! (see docs/design.md); this table only proves the path from controller to engine.
//!
//! The table is written in Mixxx's `[Group] key` names and turned into registry
//! controls ([`Control::from_mixxx`]) once, when the port opens. A message becomes a
//! [`Command`]: one the engine can take as it is goes straight to it on MIDI's own ring
//! ([`Lane`]), without the player lock; anything else (a beat jump by the deck's step,
//! library browsing) goes to the UI as [`Event::Command`].
//!
//! The note and CC numbers below follow Mixxx's Pioneer DDJ-400 mapping as best I
//! remember it and have NOT been checked against hardware. Use the MIDI monitor to
//! confirm them and correct this table.

use midir::{MidiInput, MidiInputConnection};

use super::bus::{Control, Kind};
use super::command::{self, Command, Lane};
use super::{Event, Sink};

/// How a message's data byte becomes the control's value.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Input {
    /// A note: velocity > 0 is a press (1), 0 a release (0).
    Note,
    /// A 7-bit absolute CC, across the control's travel.
    Cc,
    /// A relative encoder: 1..63 = +n steps, 65..127 = -(128 - v) steps.
    Relative,
}

struct Mapping {
    status: u8,
    data1: u8,
    control: Control,
    input: Input,
    /// `[Group] key`, for the MIDI monitor.
    name: String,
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
                name: format!("{group} {key}"),
            })
        })
        .collect()
}

/// Keeps the input connections open; dropping it closes them.
#[derive(Default)]
pub struct Midi {
    _connections: Vec<MidiInputConnection<()>>,
}

/// Connect to every MIDI input port, sending to the engine on `lane`. Returns the
/// connections and the port names.
pub fn connect_all(sink: &Sink, lane: &Lane) -> Result<(Midi, Vec<String>), String> {
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
        let (sink, lane, port_name, table) = (sink.clone(), lane.clone(), name.clone(), ddj400());
        match input.connect(
            port,
            "earshot-in",
            move |_, bytes, _| on_message(&sink, &lane, &port_name, bytes, &table),
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

/// The mapping a message matches, and the command it makes (if that value does
/// anything).
fn translate<'a>(bytes: &[u8], table: &'a [Mapping]) -> Option<(&'a Mapping, Option<Command>)> {
    if bytes.len() < 3 {
        return None;
    }
    // Treat note-off as note-on with velocity 0.
    let (status, data1, data2) = match bytes[0] & 0xF0 {
        0x80 => (0x90 | (bytes[0] & 0x0F), bytes[1], 0),
        _ => (bytes[0], bytes[1], bytes[2]),
    };
    let mp = table
        .iter()
        .find(|mp| mp.status == status && mp.data1 == data1)?;
    let value = match mp.input {
        Input::Note => (data2 > 0) as u8 as f64,
        Input::Cc => {
            let travel = data2 as f64 / 127.0;
            match mp.control.spec().kind {
                Kind::Continuous(range) => range.from_pos(travel),
                _ => travel,
            }
        }
        Input::Relative if data2 < 64 => data2 as f64,
        Input::Relative => data2 as f64 - 128.0,
    };
    Some((mp, mp.control.command(value)))
}

fn on_message(sink: &Sink, lane: &Lane, port: &str, bytes: &[u8], table: &[Mapping]) {
    let found = translate(bytes, table);
    sink(Event::Midi {
        port: port.into(),
        bytes: bytes.to_vec(),
        mapped: found.as_ref().map(|(mp, _)| mp.name.clone()),
    });
    let Some((_, Some(cmd))) = found else { return };
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

    fn cmd(bytes: impl AsRef<[u8]>) -> Option<Command> {
        translate(bytes.as_ref(), &ddj400())?.1
    }

    #[test]
    fn every_entry_maps_to_a_control() {
        assert_eq!(ddj400().len(), ddj400_names().len());
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
