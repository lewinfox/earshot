//! Controller mode's Mapping tab: phase 0 of docs/controllers/README.md. Finds the
//! connected device's Mixxx mapping, then looks up every message it sends: which
//! binding it is (the mapping's own description), what Mixxx would do with it, and
//! whether earshot can (works / needs JS / missing). Messages the mapping doesn't
//! know are listed too. "Copy report" puts the lot on the clipboard.
//!
//! The hardcoded table in `core::midi` is still what drives the engine; its column
//! ("earshot now") shows what it did, so the two can be checked against each other.

use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use egui::{Color32, RichText, Ui};
use egui_extras::{Column, Size, StripBuilder, TableBuilder};

use super::{Cx, Panel};
use crate::core::bus::{Control, deck_letter};
use crate::core::command::AppCmd;
use crate::core::midi::MidiMsg;
use crate::core::mixxx::{self, Found, Mapping, Outcome};

/// How many log rows to keep.
const LOG_ROWS: usize = 500;
/// How long a touched binding stays lit.
const FLASH: Duration = Duration::from_millis(600);

/// Controller mode's tabs, under the decks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ControllerTab {
    Library,
    #[default]
    Mapping,
}

/// One line of the live log: a run of messages from the same control.
struct Row {
    at: Instant,
    port: usize,
    /// The last message of the run.
    bytes: Vec<u8>,
    count: u32,
    /// Its binding in the mapping.
    binding: Option<usize>,
    /// What the hardcoded table did.
    did: Option<Control>,
}

/// A binding's use since the last clear.
#[derive(Clone, Copy, Default)]
struct Hit {
    count: u32,
    value: u8,
    at: Option<Instant>,
    did: Option<Control>,
}

/// A message the mapping doesn't know.
#[derive(Clone, Copy)]
struct Stray {
    count: u32,
    value: u8,
    did: Option<Control>,
}

#[derive(Default)]
pub struct ControllerUi {
    pub tab: ControllerTab,
    /// The MIDI ports open now (the app sets this when it connects).
    pub ports: Vec<String>,
    /// Every Mixxx mapping on disk, once looked for.
    found: Option<Vec<Found>>,
    /// The ports the mapping was last picked for, so it's picked again when they change.
    picked_for: Option<Vec<String>>,
    mapping: Option<Mapping>,
    error: Option<String>,
    log: VecDeque<Row>,
    hits: Vec<Hit>,
    strays: BTreeMap<(u8, u8), Stray>,
    started: Option<Instant>,
    filter: String,
    touched_only: bool,
    paused: bool,
}

impl ControllerUi {
    /// A message came in.
    pub fn on_midi(&mut self, m: MidiMsg) {
        if self.paused {
            return;
        }
        self.started.get_or_insert(m.at);
        let binding = self
            .mapping
            .as_ref()
            .and_then(|map| map.lookup(&m.bytes))
            .map(|(i, _)| i);
        let value = m.bytes.get(2).copied().unwrap_or(0);
        match binding {
            Some(i) => {
                let h = &mut self.hits[i];
                h.count += 1;
                h.value = value;
                h.at = Some(m.at);
                h.did = m.did.or(h.did);
            }
            None if m.bytes.len() >= 2 => {
                let s = self.strays.entry((m.bytes[0], m.bytes[1])).or_insert(Stray {
                    count: 0,
                    value,
                    did: m.did,
                });
                s.count += 1;
                s.value = value;
            }
            None => {}
        }
        // A jog wheel or a knob sends a stream: one row per run. A 14-bit knob sends
        // two messages each move (a coarse one and a fine one, alternating), so a
        // message joins either of the newest two rows.
        if let Some(last) = self.log.iter_mut().take(2).find(|r| {
            r.port == m.port && r.bytes.get(..2) == m.bytes.get(..2) && r.bytes.len() == m.bytes.len()
        }) {
            last.count += 1;
            last.at = m.at;
            last.bytes = m.bytes;
            return;
        }
        self.log.push_front(Row {
            at: m.at,
            port: m.port,
            bytes: m.bytes,
            count: 1,
            binding,
            did: m.did,
        });
        self.log.truncate(LOG_ROWS);
    }

    fn clear(&mut self) {
        self.log.clear();
        self.strays.clear();
        self.started = None;
        self.hits = vec![Hit::default(); self.mapping.as_ref().map_or(0, |m| m.bindings.len())];
    }

    fn set_mapping(&mut self, m: Option<Mapping>) {
        self.mapping = m;
        self.clear();
    }

    /// Pick the mapping for the connected ports, if they've changed since last time.
    fn pick(&mut self) {
        if self.picked_for.as_ref() == Some(&self.ports) {
            return;
        }
        self.picked_for = Some(self.ports.clone());
        let found = self.found.get_or_insert_with(mixxx::find_all);
        let m = self.ports.iter().find_map(|p| mixxx::for_port(p, found));
        self.error = match (&m, self.ports.is_empty(), found.is_empty()) {
            (_, _, true) => Some("No Mixxx mappings found (is Mixxx installed?)".into()),
            (None, false, _) => Some("No Mixxx mapping matches this device: pick one".into()),
            _ => None,
        };
        if m.is_some() || self.mapping.is_none() {
            self.set_mapping(m);
        }
    }

    /// The text "Copy report" puts on the clipboard.
    fn report(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "earshot controller report");
        let _ = writeln!(s, "ports: {}", self.ports.join(", "));
        let Some(map) = &self.mapping else {
            let _ = writeln!(s, "no mapping");
            return s;
        };
        let _ = writeln!(s, "mapping: {} ({})", map.name, map.path.display());
        let _ = writeln!(s, "{}", summary(map, &self.hits, self.strays.len()));
        let _ = writeln!(s, "\n## touched bindings");
        for (b, h) in map.bindings.iter().zip(&self.hits).filter(|(_, h)| h.count > 0) {
            let _ = writeln!(
                s,
                "{:<20} x{:<4} {:<9} {:<45} {}  | earshot now: {}",
                named_key(b.status, b.midino),
                h.count,
                b.outcome().label(),
                b.target(),
                b.description,
                h.did.map_or("-".into(), name),
            );
        }
        let _ = writeln!(s, "\n## messages not in the mapping");
        for ((st, d1), x) in &self.strays {
            let _ = writeln!(
                s,
                "{:<20} x{:<4} last {:3}  | earshot now: {}",
                named_key(*st, *d1),
                x.count,
                x.value,
                x.did.map_or("-".into(), name),
            );
        }
        let _ = writeln!(s, "\n## untouched bindings");
        for (b, _) in map.bindings.iter().zip(&self.hits).filter(|(_, h)| h.count == 0) {
            let _ = writeln!(
                s,
                "{:<20} {:<9} {:<45} {}",
                named_key(b.status, b.midino),
                b.outcome().label(),
                b.target(),
                b.description
            );
        }
        s
    }
}

/// "201 inputs: 70 work, 35 missing, 96 need JS · touched 12 · …".
fn summary(map: &Mapping, hits: &[Hit], strays: usize) -> String {
    let (mut works, mut missing, mut script) = (0, 0, 0);
    let (mut t_works, mut touched) = (0, 0);
    for (b, h) in map.bindings.iter().zip(hits) {
        let o = b.outcome();
        match o {
            Outcome::Control(_) => works += 1,
            Outcome::Missing => missing += 1,
            Outcome::Script => script += 1,
        }
        if h.count > 0 {
            touched += 1;
            t_works += matches!(o, Outcome::Control(_)) as u32;
        }
    }
    format!(
        "{} inputs: {works} work, {missing} missing, {script} need JS · {} LEDs (not yet) · \
         touched {touched} ({t_works} work) · {strays} messages not in the mapping",
        map.bindings.len(),
        map.outputs,
    )
}

/// "deck A: play / pause", "crossfader".
fn name(c: Control) -> String {
    let what = c.describe(None);
    match c {
        Control::Deck(d, _) => format!("deck {}: {what}", deck_letter(d)),
        _ => what,
    }
}

/// A message's kind, channel (1–16), number and value, read by `wmidi`.
fn parts(bytes: &[u8]) -> Option<(&'static str, u8, u8, u16)> {
    use wmidi::MidiMessage as M;
    let v = |x: wmidi::U7| u16::from(u8::from(x));
    Some(match M::try_from(bytes).ok()? {
        M::NoteOn(c, n, x) => ("note on", c.number(), n.into(), v(x)),
        M::NoteOff(c, n, x) => ("note off", c.number(), n.into(), v(x)),
        M::ControlChange(c, f, x) => ("CC", c.number(), f.into(), v(x)),
        M::PitchBendChange(c, x) => ("pitch bend", c.number(), 0, x.into()),
        M::ProgramChange(c, p) => ("program", c.number(), p.into(), 0),
        _ => return None,
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The bytes, then the message in words: "90 0B 7F  note on ch 1 #11 = 127".
fn named(bytes: &[u8]) -> String {
    match parts(bytes) {
        Some((kind, ch, num, val)) => {
            format!("{:<9} {kind:<8} ch {ch:<2} #{num:<3} = {val}", hex(bytes))
        }
        None => hex(bytes),
    }
}

/// Which control a binding listens to, without a value: "90 0B  note ch 1 #11".
fn named_key(status: u8, midino: u8) -> String {
    // A value of 1, so a note reads as a note on (wmidi takes 0 as note off).
    match parts(&[status, midino, 1]) {
        Some((kind, ch, num, _)) => {
            let kind = kind.strip_suffix(" on").unwrap_or(kind);
            format!("{status:02X} {midino:02X}  {kind:<8} ch {ch:<2} #{num}")
        }
        None => hex(&[status, midino]),
    }
}

fn outcome_colour(o: Outcome, cx: &Cx) -> Color32 {
    match o {
        Outcome::Control(_) => cx.theme.ok,
        Outcome::Script => cx.theme.warn,
        Outcome::Missing => cx.theme.bad,
    }
}

pub struct MappingPanel<'a> {
    pub tab: &'a mut ControllerUi,
}

impl Panel for MappingPanel<'_> {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let t = &mut *self.tab;
        t.pick();
        header(ui, t, cx);
        ui.separator();
        let Some(map) = &t.mapping else {
            ui.label("Pick a mapping above to look messages up in.");
            return;
        };
        if t.hits.len() != map.bindings.len() {
            t.hits = vec![Hit::default(); map.bindings.len()];
        }
        ui.label(RichText::new(summary(map, &t.hits, t.strays.len())).color(cx.theme.muted));
        ui.add_space(4.0);
        let now = Instant::now();
        // Two columns, each stacked top to bottom: the log, then the bindings.
        StripBuilder::new(ui)
            .size(Size::relative(0.5))
            .size(Size::remainder())
            .horizontal(|mut strip| {
                strip.cell(|ui| {
                    ui.push_id("log", |ui| log_table(ui, t, now, cx));
                });
                strip.cell(|ui| {
                    ui.push_id("bindings", |ui| bindings_table(ui, t, now, cx));
                });
            });
        // Keep the flashes fading while anything is lit.
        if t.hits.iter().any(|h| h.at.is_some_and(|a| now - a < FLASH)) {
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
    }
}

fn header(ui: &mut Ui, t: &mut ControllerUi, cx: &mut Cx) {
    ui.horizontal(|ui| {
        ui.label("Device:");
        let ports = if t.ports.is_empty() {
            RichText::new("none connected").color(cx.theme.bad)
        } else {
            RichText::new(t.ports.join(", ")).strong()
        };
        ui.label(ports);
        if ui
            .button("Reconnect")
            .on_hover_text("Open every MIDI input again, e.g. after plugging the controller in")
            .clicked()
        {
            cx.send(AppCmd::ReconnectMidi);
            t.picked_for = None;
        }
        ui.add_space(12.0);
        ui.label("Mixxx mapping:");
        let current = t.mapping.as_ref().map(|m| m.path.clone());
        let label = t.mapping.as_ref().map_or("none".into(), |m| m.name.clone());
        let found = t.found.get_or_insert_with(mixxx::find_all).clone();
        let mut chosen = None;
        egui::ComboBox::from_id_salt("mixxx-mapping")
            .selected_text(label)
            .width(220.0)
            .height(400.0)
            .show_ui(ui, |ui| {
                for f in &found {
                    let on = current.as_ref() == Some(&f.path);
                    if ui
                        .selectable_label(on, &f.name)
                        .on_hover_text(f.path.display().to_string())
                        .clicked()
                    {
                        chosen = Some(f.path.clone());
                    }
                }
            });
        if let Some(p) = chosen {
            match mixxx::read(&p) {
                Ok(m) => {
                    t.set_mapping(Some(m));
                    t.error = None;
                }
                Err(e) => t.error = Some(e),
            }
        }
        ui.add_space(12.0);
        if ui.button("Clear").on_hover_text("Forget what's been pressed").clicked() {
            t.clear();
        }
        let pause = if t.paused { "Resume" } else { "Pause" };
        if ui.button(pause).clicked() {
            t.paused = !t.paused;
        }
        if ui
            .button("Copy report")
            .on_hover_text("Everything pressed, what each does, and what's missing, as text")
            .clicked()
        {
            ui.ctx().copy_text(t.report());
        }
    });
    if let Some(e) = &t.error {
        ui.label(RichText::new(e).color(cx.theme.warn));
    }
}

fn log_table(ui: &mut Ui, t: &ControllerUi, now: Instant, cx: &Cx) {
    let map = t.mapping.as_ref().expect("a mapping");
    ui.label(RichText::new("Live (newest first)").strong());
    if t.log.is_empty() {
        ui.label(RichText::new("Press, turn or move something on the controller.").color(cx.theme.muted));
        return;
    }
    let start = t.started.unwrap_or(now);
    TableBuilder::new(ui)
        .striped(true)
        .resizable(true)
        .auto_shrink(false)
        .column(Column::exact(56.0))
        .column(Column::exact(260.0))
        .column(Column::initial(220.0).clip(true))
        .column(Column::exact(64.0))
        .column(Column::remainder().clip(true))
        .header(18.0, |mut h| {
            for name in ["time", "message", "binding", "Mixxx", "earshot now"] {
                h.col(|ui| {
                    ui.label(RichText::new(name).color(cx.theme.muted));
                });
            }
        })
        .body(|body| {
            body.rows(18.0, t.log.len(), |mut row| {
                let r = &t.log[row.index()];
                let b = r.binding.map(|i| &map.bindings[i]);
                row.col(|ui| {
                    ui.monospace(format!("{:6.2}", (r.at - start).as_secs_f64()));
                });
                row.col(|ui| {
                    let x = if r.count > 1 {
                        format!("{}  ×{}", named(&r.bytes), r.count)
                    } else {
                        named(&r.bytes)
                    };
                    ui.monospace(x);
                });
                row.col(|ui| match b {
                    Some(b) => {
                        ui.label(&b.description).on_hover_text(b.target());
                    }
                    None => {
                        ui.label(RichText::new("not in the mapping").color(cx.theme.muted));
                    }
                });
                row.col(|ui| {
                    if let Some(b) = b {
                        let o = b.outcome();
                        ui.label(RichText::new(o.label()).color(outcome_colour(o, cx)));
                    }
                });
                row.col(|ui| {
                    let differs = matches!(
                        (b.map(|b| b.outcome()), r.did),
                        (Some(Outcome::Control(x)), Some(y)) if x != y
                    );
                    let text = r.did.map_or("-".into(), name);
                    let mut l = RichText::new(text);
                    if differs {
                        l = l.color(cx.theme.warn);
                    }
                    let resp = ui.label(l);
                    if differs {
                        resp.on_hover_text("The mapping says otherwise: the hardcoded table may be wrong");
                    }
                });
            });
        });
}

fn bindings_table(ui: &mut Ui, t: &mut ControllerUi, now: Instant, cx: &Cx) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Bindings").strong());
        ui.add(
            egui::TextEdit::singleline(&mut t.filter)
                .hint_text("filter")
                .desired_width(140.0),
        );
        ui.checkbox(&mut t.touched_only, "Touched only");
    });
    let map = t.mapping.as_ref().expect("a mapping");
    let filter = t.filter.to_lowercase();
    let rows: Vec<usize> = (0..map.bindings.len())
        .filter(|&i| !t.touched_only || t.hits[i].count > 0)
        .filter(|&i| {
            let b = &map.bindings[i];
            filter.is_empty()
                || b.description.to_lowercase().contains(&filter)
                || b.target().to_lowercase().contains(&filter)
        })
        .collect();
    TableBuilder::new(ui)
        .striped(true)
        .resizable(true)
        .auto_shrink(false)
        .column(Column::exact(190.0))
        .column(Column::initial(240.0).clip(true))
        .column(Column::initial(180.0).clip(true))
        .column(Column::exact(64.0))
        .column(Column::remainder())
        .header(18.0, |mut h| {
            for name in ["message", "description", "Mixxx target", "earshot", "hits"] {
                h.col(|ui| {
                    ui.label(RichText::new(name).color(cx.theme.muted));
                });
            }
        })
        .body(|body| {
            body.rows(18.0, rows.len(), |mut row| {
                let i = rows[row.index()];
                let (b, h) = (&map.bindings[i], t.hits[i]);
                let lit = h.at.is_some_and(|a| now - a < FLASH);
                row.set_selected(lit);
                row.col(|ui| {
                    ui.monospace(named_key(b.status, b.midino));
                });
                row.col(|ui| {
                    ui.label(&b.description);
                });
                row.col(|ui| {
                    ui.label(b.target());
                });
                row.col(|ui| {
                    let o = b.outcome();
                    let resp = ui.label(RichText::new(o.label()).color(outcome_colour(o, cx)));
                    if let Outcome::Control(c) = o {
                        resp.on_hover_text(name(c));
                    }
                });
                row.col(|ui| {
                    if h.count > 0 {
                        ui.monospace(format!("{} ({})", h.count, h.value));
                    }
                });
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_in_words() {
        assert_eq!(named(&[0x90, 0x0B, 0x7F]), "90 0B 7F  note on  ch 1  #11  = 127");
        assert_eq!(named(&[0xB6, 0x40, 0x01]), "B6 40 01  CC       ch 7  #64  = 1");
        assert_eq!(named_key(0x97, 0x00), "97 00  note     ch 8  #0");
        assert_eq!(named_key(0xB0, 0x13), "B0 13  CC       ch 1  #19");
        assert_eq!(named(&[0x90]), "90");
    }
}
