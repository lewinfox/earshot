//! MIDI input. Every message is echoed to the UI's MIDI monitor and looked up in a
//! hardcoded DDJ-400 table. The real thing will load Mixxx's XML + JS mappings instead
//! (see docs/design.md); this table only proves the path from controller to engine.
//!
//! The note and CC numbers below follow Mixxx's Pioneer DDJ-400 mapping as best I
//! remember it and have NOT been checked against hardware. Use the MIDI monitor to
//! confirm them and correct this table.

use midir::{MidiInput, MidiInputConnection};

use super::controls;
use super::player::Player;
use super::{Event, Sink};

#[derive(Clone, Copy)]
enum Kind {
    /// Note: velocity > 0 = 1 (press), 0 = release.
    Button,
    /// Button that flips the control's current on/off state on press.
    Toggle,
    /// 7-bit absolute CC scaled to `lo..hi`.
    Absolute(f64, f64),
    /// Relative encoder: 1..63 = +n steps, 65..127 = -(128 - v) steps.
    Relative,
}

struct Mapping {
    status: u8,
    data1: u8,
    group: &'static str,
    key: &'static str,
    kind: Kind,
}

const fn m(status: u8, data1: u8, group: &'static str, key: &'static str, kind: Kind) -> Mapping {
    Mapping {
        status,
        data1,
        group,
        key,
        kind,
    }
}

const CH: [&str; 2] = ["[Channel1]", "[Channel2]"];

fn ddj400() -> Vec<Mapping> {
    let mut v = Vec::new();
    for d in 0..2u8 {
        let g = CH[d as usize];
        let note = 0x90 + d;
        let cc = 0xB0 + d;
        v.push(m(note, 0x0B, g, "play", Kind::Toggle));
        v.push(m(note, 0x0C, g, "cue_default", Kind::Button));
        v.push(m(note, 0x58, g, "sync_enabled", Kind::Toggle));
        v.push(m(note, 0x54, g, "pfl", Kind::Toggle));
        v.push(m(note, 0x10, g, "loop_in", Kind::Button));
        v.push(m(note, 0x11, g, "loop_out", Kind::Button));
        v.push(m(note, 0x4D, g, "reloop_toggle", Kind::Button));
        // Tempo slider: as Mixxx's mapping reads it, a raw 0 is the slowest end (the
        // top of the fader is minus on Pioneer gear).
        v.push(m(cc, 0x00, g, "rate", Kind::Absolute(-1.0, 1.0)));
        v.push(m(cc, 0x13, g, "volume", Kind::Absolute(0.0, 1.0)));
        // Performance pads in hot cue mode, unshifted / shifted.
        const HOTCUE_ACTIVATE: [&str; 8] = [
            "hotcue_1_activate",
            "hotcue_2_activate",
            "hotcue_3_activate",
            "hotcue_4_activate",
            "hotcue_5_activate",
            "hotcue_6_activate",
            "hotcue_7_activate",
            "hotcue_8_activate",
        ];
        const HOTCUE_CLEAR: [&str; 8] = [
            "hotcue_1_clear",
            "hotcue_2_clear",
            "hotcue_3_clear",
            "hotcue_4_clear",
            "hotcue_5_clear",
            "hotcue_6_clear",
            "hotcue_7_clear",
            "hotcue_8_clear",
        ];
        for k in 0..8u8 {
            v.push(m(
                0x97 + 2 * d,
                k,
                g,
                HOTCUE_ACTIVATE[k as usize],
                Kind::Button,
            ));
            v.push(m(
                0x98 + 2 * d,
                k,
                g,
                HOTCUE_CLEAR[k as usize],
                Kind::Button,
            ));
        }
    }
    v.push(m(
        0xB6,
        0x1F,
        "[Master]",
        "crossfader",
        Kind::Absolute(-1.0, 1.0),
    ));
    v.push(m(0xB6, 0x40, "[Library]", "MoveVertical", Kind::Relative));
    v.push(m(0x96, 0x41, "[Library]", "TogglePane", Kind::Button));
    v.push(m(
        0x96,
        0x46,
        "[Channel1]",
        "LoadSelectedTrack",
        Kind::Button,
    ));
    v.push(m(
        0x96,
        0x47,
        "[Channel2]",
        "LoadSelectedTrack",
        Kind::Button,
    ));
    v
}

/// Keeps the input connections open; dropping it closes them.
#[derive(Default)]
pub struct Midi {
    _connections: Vec<MidiInputConnection<()>>,
}

/// Connect to every MIDI input port. Returns the connections and the port names.
pub fn connect_all(sink: &Sink, player: &Player) -> Result<(Midi, Vec<String>), String> {
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
        let (sink, player, port_name, table) =
            (sink.clone(), player.clone(), name.clone(), ddj400());
        match input.connect(
            port,
            "earshot-in",
            move |_, bytes, _| on_message(&sink, &player, &port_name, bytes, &table),
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

fn on_message(sink: &Sink, player: &Player, port: &str, bytes: &[u8], table: &[Mapping]) {
    if bytes.len() < 3 {
        return;
    }
    // Treat note-off as note-on with velocity 0.
    let (status, data1, data2) = match bytes[0] & 0xF0 {
        0x80 => (0x90 | (bytes[0] & 0x0F), bytes[1], 0),
        _ => (bytes[0], bytes[1], bytes[2]),
    };
    let mapping = table
        .iter()
        .find(|mp| mp.status == status && mp.data1 == data1);
    sink(Event::Midi {
        port: port.into(),
        bytes: bytes.to_vec(),
        mapped: mapping.map(|mp| format!("{} {}", mp.group, mp.key)),
    });
    let Some(mp) = mapping else { return };

    let mut inner = player.lock().unwrap();
    let value = match mp.kind {
        Kind::Button => (data2 > 0) as u8 as f64,
        Kind::Toggle => {
            if data2 == 0 {
                return;
            }
            let on = controls::current(&inner, mp.group, mp.key).unwrap_or(false);
            (!on) as u8 as f64
        }
        Kind::Absolute(lo, hi) => lo + (hi - lo) * data2 as f64 / 127.0,
        Kind::Relative => {
            if data2 < 64 {
                data2 as f64
            } else {
                data2 as f64 - 128.0
            }
        }
    };
    match controls::apply(&mut inner, mp.group, mp.key, value) {
        Ok(controls::Outcome::Ui) => {
            drop(inner);
            sink(Event::UiControl {
                group: mp.group.into(),
                key: mp.key.into(),
                value,
            });
        }
        Ok(_) => {}
        Err(e) => eprintln!("MIDI: {e}"),
    }
}
