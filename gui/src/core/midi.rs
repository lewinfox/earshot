//! MIDI input. Every message is echoed to the UI (Controller mode's Mapping and
//! History tabs) and looked up in the session's mapping ([`Learned`]): which command
//! each message runs. The Mapping tab edits it while it runs, and saves it as a
//! controller file.
//!
//! A controller earshot has a file for (`core::controllers`: matched by its USB id or
//! port name) starts from that file; any other starts with nothing mapped.
//!
//! A message becomes a [`Command`]: one the engine can take as it is goes straight to
//! it on MIDI's own ring ([`Lane`]), without the player lock; anything else (a beat
//! jump by the deck's step, library browsing) goes to the UI as [`Event::Command`].

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use midir::{MidiInput, MidiInputConnection, MidiOutput};

use earshot_engine::{DeckId, MasterParam};

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
    /// Half of a 14-bit controller, as MIDI 1.0 names it: the MSB (the coarse
    /// half) or the LSB (the fine half, sent after it). The two halves are the rows
    /// mapped to the same command, one of each; the value goes when the LSB comes.
    /// (MIDI's convention puts the LSB on the MSB's control + 32.) An MSB with no
    /// LSB partner reads as a 7-bit knob.
    Msb,
    Lsb,
    /// A relative encoder: 1..63 = +n steps, 65..127 = -(128 - v) steps.
    Relative,
    /// A jog wheel's turn, centred on 64 (65 = one tick forward, 63 one back), with
    /// this many ticks to a turn. The value is seconds of track: a turn is as far as
    /// a record goes in one turn at 33⅓ rpm, 1.8 s.
    Jog(u32),
}


/// Seconds of a record that pass in one turn at 33⅓ rpm.
const SECS_PER_TURN: f64 = 60.0 / (100.0 / 3.0);

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
    /// The controller acts on this itself (in its own sound card, say: a master
    /// level knob): its messages only move earshot's knob on screen, and earshot's
    /// own control stays at its default, so the sound passes through unchanged.
    pub display_only: bool,
}

/// The controls a mapping can mark display only: the output levels, which have a
/// setting (full) that leaves the sound unchanged, and the headphone mix and master
/// cue, held at the cue side and off (a DDJ-400 puts the master into the headphones
/// itself, from its sound card's channels 1-2, so earshot's headphone channels carry
/// only the cue; Mixxx's mapping leaves all of these to it). A display-only button
/// isn't lit by earshot: the controller lights it.
pub fn can_be_display_only(control: Control) -> bool {
    display_only_value(control).is_some()
}

/// Where earshot holds its own control while a display-only knob stands in for it.
pub fn display_only_value(control: Control) -> Option<f64> {
    match control {
        Control::Master(MasterParam::Gain | MasterParam::HeadGain) => control.range().map(|r| r.default),
        Control::Master(MasterParam::HeadMix | MasterParam::MasterCue) => Some(0.0),
        _ => None,
    }
}

/// The command that puts `control` at `value` (a toggle: on or off, not toggled).
pub fn set_command(control: Control, value: f64) -> Option<Command> {
    match control {
        Control::Master(MasterParam::MasterCue) => Some(Command::Mixer(
            crate::core::command::MixerCmd::MasterCue(earshot_engine::Set::from_bool(value > 0.5)),
        )),
        _ => control.command(value),
    }
}

/// The session's mapping: the command each message runs, by message key, on every
/// port. Edited in the Mapping tab; it lasts until earshot quits (or it's saved as a
/// file).
#[derive(Clone, Default)]
pub struct Learned(Arc<Mutex<Session>>);

#[derive(Default)]
struct Session {
    map: BTreeMap<(u8, u8), Learn>,
    /// The controller's level meters, which earshot drives (`core::lights`).
    meters: Vec<Meter>,
    /// Messages that came in mapped to nothing, already logged (once each a run).
    unmapped: std::collections::HashSet<(u8, u8)>,
    /// The connected controller's lights that go to more than one message (from its
    /// controller file: [`ControllerFile::mirror`]).
    mirror: ControllerFile,
    /// Counts changes, so whoever keeps a copy (the draft file) knows when to.
    version: u64,
    /// Each 14-bit control's MSB, as last sent (by its message), waiting for the
    /// LSB.
    coarse: std::collections::HashMap<(u8, u8), u8>,
}

/// A mapping: the command each message runs, by message key.
pub type Map = BTreeMap<(u8, u8), Learn>;

/// A channel level meter on the controller: a message earshot sends, with a number
/// (0-127) the controller lights its LEDs from. Where its colours start differs from
/// one controller to the next, so the file says: the lowest number that lights the
/// first amber LED, and the red one. Red is only sent over full scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Meter {
    pub deck: DeckId,
    pub status: u8,
    pub data1: u8,
    pub amber_at: u8,
    pub red_at: u8,
}

/// Everything a controller file holds: what each message runs, the meters, what's
/// sent as it starts, and which lights go to more than one message.
#[derive(Clone, Debug, Default)]
pub struct ControllerFile {
    pub inputs: Map,
    pub meters: Vec<Meter>,
    /// Sent to the controller as it starts, after its lights (a DDJ-400's question
    /// where its controls are: `core::lights`).
    pub start: Vec<Vec<u8>>,
    /// Send the start messages again this often (ms) while connected: a controller
    /// that stops talking without them (a DDJ-FLX4).
    pub start_every_ms: Option<u64>,
    /// A button's light is also sent to these messages: the note the same button
    /// sends with SHIFT held, so its light doesn't change while SHIFT is.
    pub mirror: BTreeMap<(u8, u8), (u8, u8)>,
    /// Likewise for every note on a channel (`97` → `98`: a DDJ-400's pads, whose
    /// SHIFT notes are a channel up).
    pub mirror_status: BTreeMap<u8, u8>,
    /// Lines that couldn't be read, and why: they were left out.
    pub skipped: Vec<String>,
}

impl ControllerFile {
    /// The other message `key`'s light also goes to, if any.
    pub fn mirror_of(&self, key: (u8, u8)) -> Option<(u8, u8)> {
        self.mirror
            .get(&key)
            .copied()
            .or_else(|| self.mirror_status.get(&key.0).map(|&s| (s, key.1)))
    }
}

impl Learned {
    pub fn get(&self, key: (u8, u8)) -> Option<Learn> {
        self.0.lock().unwrap().map.get(&key).copied()
    }

    pub fn set(&self, key: (u8, u8), l: Learn) {
        let mut s = self.0.lock().unwrap();
        s.map.insert(key, l);
        s.version += 1;
    }

    /// The other half of the 14-bit control `l` (mapped on `key`) is half of: the
    /// row mapped to the same command as the other kind of half.
    pub fn partner(&self, key: (u8, u8), l: &Learn) -> Option<(u8, u8)> {
        let other = match l.input {
            Input::Msb => Input::Lsb,
            Input::Lsb => Input::Msb,
            _ => return None,
        };
        let s = self.0.lock().unwrap();
        s.map
            .iter()
            .find(|(k, o)| **k != key && o.input == other && o.control == l.control)
            .map(|(k, _)| *k)
    }

    /// Mark `key`'s control display only (or not); a 14-bit control's other half
    /// with it, as they're one knob.
    pub fn set_display_only(&self, key: (u8, u8), on: bool) {
        let Some(l) = self.get(key) else { return };
        let on = on && can_be_display_only(l.control);
        let partner = self.partner(key, &l);
        for k in std::iter::once(key).chain(partner) {
            if let Some(l) = self.get(k) {
                self.set(k, Learn { display_only: on, ..l });
            }
        }
    }

    pub fn remove(&self, key: (u8, u8)) {
        let mut s = self.0.lock().unwrap();
        s.map.remove(&key);
        s.version += 1;
    }

    /// Put mapping `m` in place of the whole mapping, its lights' mirrors too.
    pub fn replace(&self, m: ControllerFile) {
        let mut s = self.0.lock().unwrap();
        s.mirror = ControllerFile {
            mirror: m.mirror,
            mirror_status: m.mirror_status,
            ..Default::default()
        };
        s.map = m.inputs;
        s.meters = m.meters;
        s.version += 1;
    }

    /// The other message `key`'s light also goes to, if any.
    pub fn mirror_of(&self, key: (u8, u8)) -> Option<(u8, u8)> {
        self.0.lock().unwrap().mirror.mirror_of(key)
    }

    /// A message with `key` came in and nothing's mapped to it: true the first time
    /// this run, so it's logged once rather than with every turn of a knob.
    pub fn first_unmapped(&self, key: (u8, u8)) -> bool {
        self.0.lock().unwrap().unmapped.insert(key)
    }

    /// The controller's level meters.
    pub fn meters(&self) -> Vec<Meter> {
        self.0.lock().unwrap().meters.clone()
    }

    /// How many changes so far.
    pub fn version(&self) -> u64 {
        self.0.lock().unwrap().version
    }

    pub fn all(&self) -> Vec<((u8, u8), Learn)> {
        let s = self.0.lock().unwrap();
        s.map.iter().map(|(k, l)| (*k, *l)).collect()
    }

    /// Tests: the bundled mapping for model `model` (`ddj-400`).
    #[cfg(test)]
    pub fn seed_bundled(&self, model: &str) {
        self.replace(super::controllers::bundled("pioneer", Some(model)).file);
    }

    /// The mapping as a controller file: its lines, meters and mirrors.
    pub fn file(&self) -> ControllerFile {
        let s = self.0.lock().unwrap();
        ControllerFile {
            inputs: s.map.clone(),
            meters: s.meters.clone(),
            mirror: s.mirror.mirror.clone(),
            mirror_status: s.mirror.mirror_status.clone(),
            ..Default::default()
        }
    }

    /// The controller file for these bindings, as JSON.
    pub fn to_json(&self, device: &str) -> String {
        let mut head = serde_json::Map::new();
        head.insert("device".into(), serde_json::json!(device));
        file_json(head, &self.file())
    }
}

/// A controller file as JSON: `head` first (its name, USB ids, …), then its lines
/// (a line says only what a message runs; how its value reads is left out where the
/// message says it: a note is a button, a CC a knob or fader), meters, start messages
/// and mirrors (each left out if there are none).
pub fn file_json(head: serde_json::Map<String, serde_json::Value>, f: &ControllerFile) -> String {
    use serde_json::{Value, json};
    let two = |(a, b): (u8, u8)| format!("{a:02X} {b:02X}");
    let inputs: Vec<Value> = f
        .inputs
        .iter()
        .map(|(&(status, data1), l)| {
            let (name, word) = l.control.name();
            let mut o = serde_json::Map::new();
            o.insert("message".into(), json!(two((status, data1))));
            o.insert("command".into(), json!(name));
            if let Some(w) = word.or_else(|| l.arg.map(|a| a.to_string())) {
                o.insert("value".into(), json!(w));
            }
            if l.display_only {
                o.insert("display_only".into(), json!(true));
            }
            if l.input != default_input(status) {
                let (input, ticks) = match l.input {
                    Input::Note => ("button", None),
                    Input::Cc => ("absolute", None),
                    Input::Msb => ("msb", None),
                    Input::Lsb => ("lsb", None),
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
    let mut file = Value::Object(head);
    if !f.start.is_empty() {
        let start: Vec<String> = f
            .start
            .iter()
            .map(|m| m.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" "))
            .collect();
        file["start"] = json!(start);
    }
    if let Some(ms) = f.start_every_ms {
        file["start_every_ms"] = json!(ms);
    }
    file["inputs"] = json!(inputs);
    if !f.mirror.is_empty() {
        let m: serde_json::Map<String, Value> = f.mirror.iter().map(|(&a, &b)| (two(a), json!(two(b)))).collect();
        file["mirror"] = Value::Object(m);
    }
    if !f.mirror_status.is_empty() {
        let m: serde_json::Map<String, Value> =
            f.mirror_status.iter().map(|(a, b)| (format!("{a:02X}"), json!(format!("{b:02X}")))).collect();
        file["mirror_status"] = Value::Object(m);
    }
    let meters: Vec<Value> = f
        .meters
        .iter()
        .map(|m| {
            json!({
                "deck": m.deck.index() + 1,
                "message": two((m.status, m.data1)),
                "amber_at": m.amber_at,
                "red_at": m.red_at,
            })
        })
        .collect();
    if !meters.is_empty() {
        file["meters"] = json!(meters);
    }
    serde_json::to_string_pretty(&file).expect("JSON") + "\n"
}

/// Two hex bytes (`"90 0B"`).
fn two_bytes(text: &str) -> Option<(u8, u8)> {
    match hex(text)?[..] {
        [a, b] => Some((a, b)),
        _ => None,
    }
}

/// Hex bytes separated by spaces (`"F0 00 40 F7"`).
fn hex(text: &str) -> Option<Vec<u8>> {
    text.split_whitespace().map(|b| u8::from_str_radix(b, 16).ok()).collect()
}

/// A controller file (see [`Learned::to_json`]): its device name and mapping. A line
/// that can't be read is left out, and listed with why in `skipped`; only a file
/// that isn't JSON at all is an error.
pub fn from_json(text: &str) -> Result<(String, ControllerFile), String> {
    #[derive(serde::Deserialize)]
    struct File {
        #[serde(default)]
        device: String,
        #[serde(default)]
        inputs: Vec<Line>,
        #[serde(default)]
        meters: Vec<MeterLine>,
        #[serde(default)]
        start: Vec<String>,
        start_every_ms: Option<u64>,
        #[serde(default)]
        mirror: BTreeMap<String, String>,
        #[serde(default)]
        mirror_status: BTreeMap<String, String>,
    }
    #[derive(serde::Deserialize)]
    struct MeterLine {
        deck: usize,
        message: String,
        amber_at: u8,
        red_at: u8,
    }
    #[derive(serde::Deserialize)]
    struct Line {
        message: String,
        command: String,
        value: Option<String>,
        input: Option<String>,
        ticks: Option<u32>,
        #[serde(default)]
        display_only: bool,
    }
    let file: File = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let mut out = ControllerFile::default();
    for (i, line) in file.inputs.iter().enumerate() {
        let at = |e: String| format!("input {} ({}): {e}", i + 1, line.message);
        let Some(k) = two_bytes(&line.message) else {
            out.skipped.push(at("the message isn't two hex bytes".into()));
            continue;
        };
        let k = key(&[k.0, k.1]).expect("two bytes");
        let (control, arg) = match Control::parse(&line.command, line.value.as_deref()) {
            Ok(c) => c,
            Err(e) => {
                out.skipped.push(at(e));
                continue;
            }
        };
        let input = match line.input.as_deref() {
            None => default_input(k.0),
            Some("button") => Input::Note,
            Some("absolute") => Input::Cc,
            Some("msb") => Input::Msb,
            Some("lsb") => Input::Lsb,
            Some("relative") => Input::Relative,
            Some("jog") => Input::Jog(line.ticks.unwrap_or(720)),
            Some(x) => {
                out.skipped.push(at(format!("no input kind {x}")));
                continue;
            }
        };
        out.inputs.insert(
            k,
            Learn {
                control,
                arg,
                input,
                display_only: line.display_only && can_be_display_only(control),
            },
        );
    }
    for (i, m) in file.meters.iter().enumerate() {
        let at = |e: &str| format!("meter {} ({}): {e}", i + 1, m.message);
        let Some((status, data1)) = two_bytes(&m.message) else {
            out.skipped.push(at("the message isn't two hex bytes"));
            continue;
        };
        let Some(deck) = m.deck.checked_sub(1).and_then(DeckId::new) else {
            out.skipped.push(at("no such deck"));
            continue;
        };
        if !(m.amber_at < m.red_at && m.red_at <= 127) {
            out.skipped.push(at("amber_at must be below red_at, and red_at at most 127"));
            continue;
        }
        out.meters.push(Meter {
            deck,
            status,
            data1,
            amber_at: m.amber_at,
            red_at: m.red_at,
        });
    }
    out.start_every_ms = file.start_every_ms.filter(|&ms| ms > 0);
    for m in &file.start {
        match hex(m) {
            Some(b) if !b.is_empty() => out.start.push(b),
            _ => out.skipped.push(format!("start message {m:?}: not hex bytes")),
        }
    }
    for (from, to) in &file.mirror {
        match (two_bytes(from), two_bytes(to)) {
            (Some(f), Some(t)) => {
                out.mirror.insert(f, t);
            }
            _ => out.skipped.push(format!("mirror {from:?}: {to:?}: not two hex bytes each")),
        }
    }
    for (from, to) in &file.mirror_status {
        match (hex(from).as_deref(), hex(to).as_deref()) {
            (Some(&[f]), Some(&[t])) => {
                out.mirror_status.insert(f, t);
            }
            _ => out.skipped.push(format!("mirror_status {from:?}: {to:?}: not a hex byte each")),
        }
    }
    Ok((file.device, out))
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
/// that takes steps, and a knob or fader for the rest. (A 14-bit controller is
/// set by hand.)
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

/// Keeps the input connections open, and the lights going; dropping it puts the
/// lights out and closes them.
#[derive(Default)]
pub struct Midi {
    _connections: Vec<MidiInputConnection<()>>,
    _lights: Option<super::lights::Lights>,
}

/// The MIDI input ports there are (not ALSA's Midi Through), without opening them.
pub fn port_names() -> Vec<String> {
    let Ok(probe) = MidiInput::new("earshot-probe") else {
        return vec![];
    };
    probe
        .ports()
        .iter()
        .filter_map(|p| probe.port_name(p).ok())
        .filter(|n| !n.contains("Midi Through"))
        .collect()
}

/// Connect to every MIDI input port, sending to the engine on `lane`, and to the
/// output ports of the same names, for the controller's lights. Returns the
/// connections and the input port names.
pub fn connect_all(
    sink: &Sink,
    lane: &Lane,
    learned: &Learned,
    monitor: &super::lights::MonitorSlot,
    choose: &dyn Fn(&str, Option<&str>) -> Option<super::controllers::Profile>,
) -> Result<(Midi, Vec<Port>), String> {
    let t0 = std::time::Instant::now();
    let probe = MidiInput::new("earshot-probe").map_err(|e| e.to_string())?;
    let ports = probe.ports();
    let all: Vec<String> = ports.iter().map(|p| probe.port_name(p).unwrap_or_else(|_| "unknown".into())).collect();
    eprintln!("[init] MIDI: {} input port(s): {all:?}", all.len());
    let mut names = Vec::new();
    let mut ports_out = Vec::new();
    let mut mapped = false;
    // Each open port's start messages, from its controller file.
    let mut starts = Vec::new();
    let mut connections = Vec::new();
    for port in &ports {
        let name = probe.port_name(port).unwrap_or_else(|_| "unknown".into());
        if name.contains("Midi Through") {
            continue;
        }
        let input = MidiInput::new("earshot").map_err(|e| e.to_string())?;
        // Which mapping this controller gets (`core::controllers`). The first port
        // with one sets the session's mapping.
        let usb = super::controller_audio::usb_id(&name);
        let profile = choose(&name, usb.as_deref());
        eprintln!(
            "[init] MIDI: {name:?} (USB id {}): mapping {}",
            usb.as_deref().unwrap_or("none"),
            profile.as_ref().map_or("none".to_string(), |p| format!("{} ({})", p.id.key(), p.label()))
        );
        if let Some(p) = &profile
            && !mapped
        {
            learned.replace(p.file.clone());
            mapped = true;
        }
        let start = profile.as_ref().map_or_else(super::lights::Start::default, |p| {
            super::lights::Start::new(
                p.file.start.clone(),
                p.file.start_every_ms.map(std::time::Duration::from_millis),
            )
        });
        let (sink, lane, learned) = (sink.clone(), lane.clone(), learned.clone());
        let index = names.len();
        match input.connect(
            port,
            "earshot-in",
            move |_, bytes, _| on_message(&sink, &lane, bytes, &learned, index),
            (),
        ) {
            Ok(c) => {
                eprintln!("[init] MIDI: opened input {name:?}");
                connections.push(c);
                ports_out.push(Port {
                    name: name.clone(),
                    usb,
                    mapping: profile,
                });
                names.push(name);
                starts.push(start);
            }
            Err(e) => eprintln!("[init] MIDI: could not open {name}: {e}"),
        }
    }
    // No controller with a mapping: nothing mapped (pressing things shows them in
    // Controller mode, to map them).
    if !mapped {
        learned.replace(ControllerFile::default());
    }
    let outputs = connect_outputs(&names, &starts);
    eprintln!(
        "[init] MIDI: {} input(s), {} output(s) open after {:.1} ms; lights {}",
        names.len(),
        outputs.len(),
        t0.elapsed().as_secs_f64() * 1000.0,
        if outputs.is_empty() { "off (no output)" } else { "on" }
    );
    let lights = (!outputs.is_empty())
        .then(|| super::lights::Lights::start(outputs, learned.clone(), monitor.clone()));
    Ok((
        Midi {
            _connections: connections,
            _lights: lights,
        },
        ports_out,
    ))
}

/// An open MIDI input: its name, its device's USB id (if it has one), and the mapping
/// it got.
#[derive(Clone, Debug)]
pub struct Port {
    pub name: String,
    pub usb: Option<String>,
    pub mapping: Option<super::controllers::Profile>,
}

/// The output ports named like the inputs that opened (a controller's two halves),
/// each with its input's start messages (`starts`, by input).
fn connect_outputs(inputs: &[String], starts: &[super::lights::Start]) -> super::lights::Outputs {
    let Ok(probe) = MidiOutput::new("earshot-probe") else {
        return vec![];
    };
    let mut out = vec![];
    for port in probe.ports() {
        let name = probe.port_name(&port).unwrap_or_default();
        let Some(i) = inputs.iter().position(|n| *n == name) else {
            continue;
        };
        let Ok(output) = MidiOutput::new("earshot") else { continue };
        match output.connect(&port, "earshot-lights") {
            Ok(c) => {
                eprintln!("[init] MIDI: opened output {name:?}");
                out.push((c, starts[i].clone()));
            }
            Err(e) => eprintln!("MIDI: could not open {name} for its lights: {e}"),
        }
    }
    out
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
    let travel = |l: &Learn, t: f64| match l.control.spec().kind {
        Kind::Continuous(range) => range.from_pos(t),
        _ => t,
    };
    let l = learned.get(key)?;
    let value = match l.input {
        // A 14-bit control's coarse half: kept for the fine half, which sends the
        // value. Without a fine half, it's a 7-bit knob.
        Input::Msb => {
            if learned.partner(key, &l).is_none() {
                travel(&l, data2 as f64 / 127.0)
            } else {
                learned.0.lock().unwrap().coarse.insert(key, data2);
                return Some((l.control, None));
            }
        }
        Input::Lsb => {
            let coarse = learned.partner(key, &l)?;
            let high = learned.0.lock().unwrap().coarse.get(&coarse).copied();
            let Some(high) = high else {
                return Some((l.control, None));
            };
            travel(&l, ((high as u32) << 7 | data2 as u32) as f64 / 16383.0)
        }
        Input::Note if data2 > 0 => l.control.press_value(l.arg),
        Input::Note => 0.0,
        Input::Cc => travel(&l, data2 as f64 / 127.0),
        Input::Relative if data2 < 64 => data2 as f64,
        Input::Relative => data2 as f64 - 128.0,
        Input::Jog(ticks) => (data2 as f64 - 64.0) * SECS_PER_TURN / ticks as f64,
    };
    // Display only: show it, don't apply it.
    if l.display_only && can_be_display_only(l.control) {
        let cmd = Command::App(crate::core::command::AppCmd::ControllerShows {
            control: l.control,
            value,
        });
        return Some((l.control, Some(cmd)));
    }
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
    // Mapped to nothing: an error, so a button earshot ignores can be tracked down.
    // Once a message (a knob sends a stream); system messages (clock, SysEx) aside.
    if found.is_none()
        && bytes.first().is_some_and(|s| *s < 0xF0)
        && let Some(k) = key(bytes)
        && learned.get(k).is_none()
        && learned.first_unmapped(k)
    {
        let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02X}")).collect();
        eprintln!(
            "ERROR MIDI: {} (port {port}) isn't mapped to anything; logged once a message",
            hex.join(" ")
        );
    }
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
        l.seed_bundled("ddj-400");
        l
    }

    fn cmd(bytes: impl AsRef<[u8]>) -> Option<Command> {
        translate(bytes.as_ref(), &ddj())?.1
    }

    /// A 14-bit control on `status` / `n` moved to `coarse`, `fine`: its two
    /// messages, and what the second sent.
    fn cmd14(status: u8, n: u8, coarse: u8, fine: u8) -> Option<Command> {
        let learned = ddj();
        let first = translate(&[status, n, coarse], &learned)?;
        assert!(first.1.is_none(), "the coarse half waits for the fine one");
        translate(&[status, n + 32, fine], &learned)?.1
    }

    /// A DDJ-400 starts from its controller file, once; any binding can be changed,
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
                display_only: false,
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
                display_only: false,
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
        // Unmapped: does nothing.
        learned.remove((0x90, 0x0B));
        assert!(run(&[0x90, 0x0B, 0x7F]).is_none());
    }

    /// A saved file reads back as the same mapping; a bad line says which.
    #[test]
    fn files_round_trip() {
        let learned = ddj();
        let v = learned.version();
        let (device, map) = from_json(&learned.to_json("DDJ-400")).unwrap();
        assert_eq!(device, "DDJ-400");
        let bundled = super::super::controllers::bundled("pioneer", Some("ddj-400")).file;
        assert_eq!(map.inputs, bundled.inputs);
        assert_eq!(map.meters, bundled.meters, "the meters too");
        assert_eq!(map.mirror, bundled.mirror, "and the mirrors");
        assert_eq!(map.mirror_status, bundled.mirror_status);
        let fresh = Learned::default();
        fresh.replace(map);
        assert!(fresh.version() > 0);
        assert_eq!(learned.version(), v);
        // A line that can't be read is left out, and says which and why; the rest
        // stays.
        let bad = r#"{"inputs": [{"message": "90 0B", "command": "deck_1_play"},
                                 {"message": "90 0C", "command": "deck_1_dance"},
                                 {"message": "90", "command": "deck_1_play"}]}"#;
        let (_, m) = from_json(bad).unwrap();
        assert_eq!(m.inputs.len(), 1);
        assert_eq!(m.skipped.len(), 2);
        assert!(m.skipped[0].contains("input 2 (90 0C)") && m.skipped[0].contains("no command deck_1_dance"));
        assert!(m.skipped[1].contains("input 3 (90)"));
        assert!(from_json("nonsense").is_err());
    }

    #[test]
    fn every_entry_maps_to_a_control() {
        assert!(translate(&[0x90, 0x0B, 0x7F], &Learned::default()).is_none(), "nothing mapped");
        let l = Learned::default();
        assert!(l.first_unmapped((0x90, 0x0B)), "logged the first time");
        assert!(!l.first_unmapped((0x90, 0x0B)), "not again");
        assert!(l.first_unmapped((0x90, 0x0C)), "another message is");
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

    /// The pad mode buttons tell the pads page which mode the pads are in.
    #[test]
    fn pad_mode_buttons() {
        use crate::core::view::{PadMode, ViewCmd};
        for (bytes, d, mode) in [
            ([0x90, 0x1B, 0x7F], 0, PadMode::HotCue),
            ([0x90, 0x69, 0x7F], 0, PadMode::Keyboard),
            ([0x91, 0x20, 0x7F], 1, PadMode::BeatJump),
            ([0x91, 0x6F, 0x7F], 1, PadMode::KeyShift),
        ] {
            assert!(
                matches!(cmd(bytes), Some(Command::View(ViewCmd::PadMode(x, m))) if x == deck(d) && m == mode),
                "{bytes:02X?}"
            );
        }
        assert!(cmd([0x90, 0x69, 0x00]).is_none(), "nothing on release");
    }

    /// Keyboard and key shift pads go to the app (which knows the page and the hot
    /// cue), on press and release, SHIFT's on the next channel.
    #[test]
    fn keyboard_and_key_shift_pads() {
        use crate::core::view::{PadMode, ViewCmd};
        for (bytes, d, m, p, sh, dn) in [
            ([0x97, 0x44, 0x7F], 0, PadMode::Keyboard, 4, false, true),
            ([0x87, 0x44, 0x00], 0, PadMode::Keyboard, 4, false, false),
            ([0x9A, 0x46, 0x7F], 1, PadMode::Keyboard, 6, true, true),
            ([0x99, 0x70, 0x7F], 1, PadMode::KeyShift, 0, false, true),
            ([0x98, 0x77, 0x7F], 0, PadMode::KeyShift, 7, true, true),
        ] {
            assert!(
                matches!(
                    cmd(bytes),
                    Some(Command::View(ViewCmd::ModePad { deck: x, mode, pad, shift, down }))
                        if x == deck(d) && mode == m && pad == p && shift == sh && down == dn
                ),
                "{bytes:02X?}"
            );
        }
    }

    /// Pad FX pads: bank 1 on 0x10-0x17 and bank 2 on 0x50-0x57, deck 1 on 0x97 and
    /// deck 2 on 0x99, SHIFT's on the next channel; a press and a release each.
    #[test]
    fn pad_fx_pads() {
        use crate::core::view::{PadMode, ViewCmd};
        for (bytes, d, m, p, sh, dn) in [
            ([0x97, 0x10, 0x7F], 0, PadMode::PadFx1, 0, false, true),
            ([0x87, 0x10, 0x00], 0, PadMode::PadFx1, 0, false, false),
            ([0x97, 0x17, 0x7F], 0, PadMode::PadFx1, 7, false, true),
            ([0x98, 0x13, 0x7F], 0, PadMode::PadFx1, 3, true, true),
            ([0x88, 0x13, 0x00], 0, PadMode::PadFx1, 3, true, false),
            ([0x99, 0x12, 0x7F], 1, PadMode::PadFx1, 2, false, true),
            ([0x97, 0x50, 0x7F], 0, PadMode::PadFx2, 0, false, true),
            ([0x99, 0x57, 0x7F], 1, PadMode::PadFx2, 7, false, true),
            ([0x89, 0x57, 0x00], 1, PadMode::PadFx2, 7, false, false),
            ([0x9A, 0x54, 0x7F], 1, PadMode::PadFx2, 4, true, true),
        ] {
            assert!(
                matches!(cmd(bytes),
                    Some(Command::View(ViewCmd::ModePad { deck: x, mode, pad, shift, down }))
                        if x == deck(d) && mode == m && pad == p && shift == sh && down == dn
                ),
                "{bytes:02X?}"
            );
        }
        // Every pad of both banks is mapped, with and without SHIFT, on both decks.
        let all = ddj().all();
        let fx = all.iter().filter(|(_, l)| {
            let name = l.control.name().0;
            name.contains("_pad_fx_") && !name.contains("set_pad_mode")
        });
        assert_eq!(fx.count(), 2 * 2 * 2 * 8);
    }

    /// Sampler mode's pads play slots 1-8 (deck 1) and 9-16 (deck 2), straight to the
    /// engine; SHIFT's, a channel up, stop them. Nothing on release.
    #[test]
    fn sampler_pads() {
        use crate::core::sampler::SamplerCmd;
        let slot = |i| earshot_engine::SampleSlot::new(i).unwrap();
        for (bytes, want) in [
            ([0x97, 0x30, 0x7F], SamplerCmd::Play(slot(0))),
            ([0x97, 0x37, 0x7F], SamplerCmd::Play(slot(7))),
            ([0x99, 0x30, 0x7F], SamplerCmd::Play(slot(8))),
            ([0x99, 0x37, 0x7F], SamplerCmd::Play(slot(15))),
            ([0x98, 0x32, 0x7F], SamplerCmd::Stop(slot(2))),
            ([0x9A, 0x35, 0x7F], SamplerCmd::Stop(slot(13))),
        ] {
            let got = cmd(bytes);
            assert!(matches!(&got, Some(Command::Sampler(c)) if *c == want), "{bytes:02X?}: {got:?}");
            assert!(command::direct(got.unwrap()).is_ok(), "{bytes:02X?} goes direct");
        }
        assert!(cmd([0x87, 0x30, 0x00]).is_none(), "nothing on release");
        assert!(cmd([0x97, 0x30, 0x00]).is_none(), "nor on a note-on at velocity 0");
        let (c, _) = translate(&[0x99, 0x31, 0x7F], &ddj()).unwrap();
        assert_eq!(c.name().0, "sampler_10_play");
        // The SAMPLER mode button still tells the pads page.
        use crate::core::view::{PadMode, ViewCmd};
        assert!(matches!(
            cmd([0x90, 0x22, 0x7F]),
            Some(Command::View(ViewCmd::PadMode(d, PadMode::Sampler))) if d == deck(0)
        ));
    }

    /// Pads in beat loop and beat jump mode: their own notes, each a fixed size.
    #[test]
    fn loop_and_jump_pads() {
        assert!(matches!(
            cmd([0x97, 0x60, 0x7F]),
            Some(Command::Deck(d, DeckCmd::Engine(EngineCmd::BeatLoop(b)))) if d == deck(0) && b == 0.25
        ));
        assert!(matches!(
            cmd([0x99, 0x67, 0x7F]),
            Some(Command::Deck(d, DeckCmd::Engine(EngineCmd::BeatLoop(b)))) if d == deck(1) && b == 32.0
        ));
        assert!(matches!(
            cmd([0x97, 0x20, 0x7F]),
            Some(Command::Deck(_, DeckCmd::Engine(EngineCmd::BeatJump(b)))) if b == -1.0
        ));
        assert!(matches!(
            cmd([0x99, 0x27, 0x7F]),
            Some(Command::Deck(d, DeckCmd::Engine(EngineCmd::BeatJump(b)))) if d == deck(1) && b == 8.0
        ));
        assert!(cmd([0x87, 0x20, 0x00]).is_none(), "nothing on release");
        // Shift only tells the pads page it's held (and let go).
        use crate::core::view::ViewCmd;
        let shift = translate(&[0x91, 0x3F, 0x7F], &ddj()).unwrap();
        assert_eq!(shift.0.name().0, "deck_2_shift");
        assert!(matches!(shift.1, Some(Command::View(ViewCmd::Shift(d, true))) if d == deck(1)));
        assert!(matches!(cmd([0x81, 0x3F, 0x00]), Some(Command::View(ViewCmd::Shift(_, false)))));
    }

    /// The BEAT FX section: CH SELECT puts the effect where it points (moving off a
    /// position does nothing), ON/OFF toggles, Shift + ON/OFF releases.
    #[test]
    fn beat_fx() {
        use earshot_engine::{FxCmd, FxTarget};
        let fx = |bytes| match cmd(bytes) {
            Some(Command::Fx(0, c)) => Some(c),
            _ => None,
        };
        assert_eq!(fx([0x94, 0x10, 0x7F]), Some(FxCmd::Target(FxTarget::Deck(deck(0)))));
        assert_eq!(fx([0x94, 0x11, 0x7F]), Some(FxCmd::Target(FxTarget::Deck(deck(1)))));
        assert_eq!(fx([0x94, 0x14, 0x7F]), Some(FxCmd::Target(FxTarget::Master)));
        assert_eq!(fx([0x94, 0x10, 0x00]), None);
        assert!(matches!(fx([0x94, 0x47, 0x7F]), Some(FxCmd::Enabled(_))));
        assert_eq!(fx([0x94, 0x43, 0x7F]), Some(FxCmd::Release));
        assert!(matches!(
            cmd([0x90, 0x5C, 0x7F]),
            Some(Command::Mixer(crate::core::command::MixerCmd::SyncLeader(Some(d)))) if d == deck(0)
        ));
        assert!(matches!(
            cmd([0x90, 0x14, 0x7F]),
            Some(Command::Deck(d, DeckCmd::Engine(EngineCmd::BeatLoop(b)))) if d == deck(0) && b == 4.0
        ));
        assert!(matches!(
            cmd([0x91, 0x50, 0x7F]),
            Some(Command::Deck(d, DeckCmd::ToggleActiveLoop)) if d == deck(1)
        ));
        assert_eq!(fx([0x94, 0x4A, 0x7F]), Some(FxCmd::BeatsStep(-1)));
        assert_eq!(fx([0x94, 0x63, 0x7F]), Some(FxCmd::NextKind));
        assert_eq!(fx([0x94, 0x64, 0x7F]), Some(FxCmd::PrevKind));
        assert!(matches!(cmd14(0xB4, 0x02, 0x7F, 0x7F), Some(Command::Fx(0, FxCmd::Level(l))) if l == 1.0));
    }

    /// A display-only knob says where it is, and runs nothing itself.
    #[test]
    fn a_display_only_knob_is_shown_not_applied() {
        let l = Learned::default();
        let c = Control::Master(MasterParam::Gain);
        let half = |input| Learn { control: c, arg: None, input, display_only: false };
        l.set((0xB6, 0x08), half(Input::Msb));
        l.set((0xB6, 0x28), half(Input::Lsb));
        // Ticked on one half: both are display only.
        l.set_display_only((0xB6, 0x28), true);
        assert!(l.get((0xB6, 0x08)).unwrap().display_only);
        assert!(translate(&[0xB6, 0x08, 127], &l).and_then(|t| t.1).is_none());
        match translate(&[0xB6, 0x28, 127], &l).and_then(|t| t.1) {
            Some(Command::App(crate::core::command::AppCmd::ControllerShows { control, value })) => {
                assert_eq!(control, c);
                assert_eq!(value, 1.0);
            }
            other => panic!("{other:?}"),
        }
        let (_, back) = from_json(&l.to_json("x")).unwrap();
        assert!(back.inputs[&(0xB6, 0x08)].display_only && back.inputs[&(0xB6, 0x28)].display_only);
    }

    /// A DDJ-400's MASTER LEVEL, HEADPHONES LEVEL and HEADPHONES MIXING are display
    /// only from the start (the controller does them itself); only the output levels and the headphone mix
    /// can be display only.
    #[test]
    fn the_ddj_master_level_is_display_only() {
        let l = ddj();
        assert!(l.get((0xB6, 0x08)).unwrap().display_only);
        assert!(l.get((0xB6, 0x28)).unwrap().display_only);
        assert!(l.get((0xB6, 0x0D)).unwrap().display_only, "the headphone level: the DDJ sets it");
        // HEADPHONES MIXING too: the controller blends its master into the headphones,
        // and earshot's headphone channels carry only the cue.
        assert!(l.get((0xB6, 0x0C)).unwrap().display_only);
        assert!(l.get((0xB6, 0x2C)).unwrap().display_only);
        assert_eq!(display_only_value(Control::Master(MasterParam::HeadMix)), Some(0.0));
        // MASTER CUE too: the controller puts the master in the headphones itself.
        assert!(l.get((0x96, 0x63)).unwrap().display_only);
        assert!(matches!(
            translate(&[0x96, 0x63, 0x7F], &l),
            Some((_, Some(Command::App(crate::core::command::AppCmd::ControllerShows { .. }))))
        ));
        // The crossfader can't be: it has no setting that leaves the sound alone.
        l.set_display_only((0xB6, 0x1F), true);
        assert!(!l.get((0xB6, 0x1F)).unwrap().display_only);
    }

    #[test]
    fn faders_cover_their_range() {
        assert!(matches!(
            cmd14(0xB0, 0x13, 127, 127),
            Some(Command::Param(ParamId::Channel(_, ChannelParam::Volume), v)) if v == 1.0
        ));
        assert!(matches!(
            cmd14(0xB6, 0x1F, 0, 0),
            Some(Command::Param(ParamId::Master(MasterParam::Crossfader), v)) if v == -1.0
        ));
        assert!(matches!(
            cmd14(0xB1, 0x00, 127, 127),
            Some(Command::Deck(d, DeckCmd::Engine(EngineCmd::TempoFader(v)))) if v == 1.0 && d == deck(1)
        ));
    }

    /// A 14-bit knob's two halves make one fine value.
    #[test]
    fn fourteen_bit_knobs() {
        use crate::core::bus::DeckControl;
        let filter = |c: Option<Command>| match c {
            Some(Command::Param(ParamId::Channel(_, ChannelParam::Filter), v)) => v,
            other => panic!("{other:?}"),
        };
        // Coarse 64, fine 0: half way (the filter's middle, 0).
        let mid = filter(cmd14(0xB6, 0x17, 64, 0));
        assert!(mid.abs() < 1e-3, "{mid}");
        // One fine step on: a hair more, not a jump.
        let a = filter(cmd14(0xB6, 0x17, 64, 1));
        assert!(a > mid && a - mid < 1e-3, "{a}");
        // The fine half alone, before any coarse half: nothing.
        let learned = ddj();
        assert!(translate(&[0xB6, 0x37, 5], &learned).and_then(|t| t.1).is_none());
        // Two rows on one command, one marked coarse and one fine, are a pair,
        // whatever their numbers; a coarse half alone is a 7-bit knob.
        let l = Learned::default();
        let c = Control::Deck(deck(0), DeckControl::Filter);
        l.set((0xB6, 0x17), Learn { control: c, arg: None, input: Input::Msb, display_only: false });
        let alone = translate(&[0xB6, 0x17, 127], &l).and_then(|t| t.1);
        assert!(matches!(alone, Some(Command::Param(_, v)) if v == 1.0), "{alone:?}");
        l.set((0xB6, 0x40), Learn { control: c, arg: None, input: Input::Lsb, display_only: false });
        assert_eq!(l.partner((0xB6, 0x17), &l.get((0xB6, 0x17)).unwrap()), Some((0xB6, 0x40)));
        assert!(translate(&[0xB6, 0x17, 127], &l).and_then(|t| t.1).is_none());
        let both = translate(&[0xB6, 0x40, 127], &l).and_then(|t| t.1);
        assert!(matches!(both, Some(Command::Param(_, v)) if v == 1.0), "{both:?}");
        let json = l.to_json("x");
        assert!(json.contains("\"msb\"") && json.contains("\"lsb\""), "{json}");
        let (_, back) = from_json(&json).unwrap();
        assert_eq!(back.inputs[&(0xB6, 0x40)].input, Input::Lsb);
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
        // With SHIFT the top searches: two ticks forward, 16 times 2/720 of 1.8 s.
        let Some(Command::Deck(d, DeckCmd::Engine(EngineCmd::Search(by)))) = cmd([0xB1, 0x29, 66]) else {
            panic!("not a search")
        };
        assert_eq!(d, deck(1));
        assert!((by - 0.08).abs() < 1e-9, "{by}");
        assert!(command::direct(cmd([0xB0, 0x29, 62]).unwrap()).is_ok());
    }

    #[test]
    fn engine_commands_go_direct_and_the_rest_to_the_ui() {
        let play = cmd([0x90, 0x0B, 0x7F]).unwrap();
        assert!(command::direct(play).is_ok());
        let volume = cmd14(0xB0, 0x13, 64, 0).unwrap();
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
    fn sending_without_audio_waits_for_it() {
        // No engine yet (the controller answers before audio starts): the lane keeps
        // the commands, a bounded number, for the engine to come, without blocking.
        let lane = Lane::default();
        for _ in 0..command::LANE_WAITING + 10 {
            let c = command::direct(cmd([0x90, 0x0B, 0x7F]).unwrap()).unwrap();
            assert!(lane.send(c).is_ok());
        }
    }
}

