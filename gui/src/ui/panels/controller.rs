//! Controller mode's Mapping and History tabs, for mapping a controller by pressing
//! its buttons.
//!
//! Mapping lists every message the session knows: the ones mapped (a controller with
//! a file in `gui/controllers/` starts from it; anything else from nothing) and every
//! one pressed since.
//! Pressing something runs its command, adds its message to the list if it's new, and
//! scrolls the list to it. Each row's command can be picked from a list, and takes
//! effect at once. A mapping of the user's is saved as it changes
//! when earshot starts, so work in progress survives a restart. "New mapping" starts
//! again: empty, from a bundled controller file, or from a file of your own. "Save JSON…"
//! writes the lot as a controller file, to go in the repo.
//!
//! History is the stream of messages as they came in, newest first.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{Align, RichText, Ui};
use egui_extras::{Column, TableBuilder};

use super::{Cx, Panel};
use crate::core::bus::{self, Control};
use crate::core::command::AppCmd;
use crate::core::controllers::{Profile, Store};
use crate::core::midi::{self, Input, Learn, Learned, MidiMsg};

/// The "New mapping" wizard's answers so far.
#[derive(Default)]
struct Wizard {
    /// The maker picked (its folder name); `None` with `new_maker` for a new one.
    maker: Option<String>,
    new_maker: bool,
    maker_name: String,
    model_name: String,
    /// The mapping to start from (a model's key); `None`: empty.
    template: Option<String>,
    /// The MIDI device it's for (by its place in the list); `None`: none yet.
    device: Option<usize>,
    error: Option<String>,
}

/// How many history rows to keep.
const LOG_ROWS: usize = 500;
/// How long a pressed row stays lit.
const FLASH: Duration = Duration::from_millis(600);

/// Controller mode's tabs, under the decks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ControllerTab {
    #[default]
    Mapping,
    History,
    Send,
    Library,
}

/// One line of the history: a run of messages from the same control.
struct Row {
    at: Instant,
    port: usize,
    /// The last message of the run.
    bytes: Vec<u8>,
    count: u32,
    /// The command it ran.
    did: Option<Control>,
}

/// A message pressed this session.
#[derive(Clone, Copy)]
struct Seen {
    count: u32,
    value: u8,
    at: Instant,
}

#[derive(Default)]
pub struct ControllerUi {
    pub tab: ControllerTab,
    /// The MIDI ports open now (the app sets this when it connects).
    pub ports: Vec<String>,
    /// The session's mapping; the MIDI ports read it too.
    pub learned: Learned,
    seen: BTreeMap<(u8, u8), Seen>,
    /// The last message's key, so a stream from one knob scrolls the list once.
    last: Option<(u8, u8)>,
    /// The row to scroll the list to on the next frame.
    scroll_to: Option<(u8, u8)>,
    /// The Mapping table's sort: a column (by its place) and whether it's reversed.
    /// `None`: by message.
    sort: Option<(usize, bool)>,
    /// What's typed in a command list's search box.
    search: String,
    /// A number being typed, and its row.
    value_edit: Option<((u8, u8), String)>,
    log: VecDeque<Row>,
    started: Option<Instant>,
    paused: bool,
    /// What saving or opening a file came to (from the dialogs' threads).
    saved: Arc<Mutex<Option<String>>>,
    /// The open MIDI inputs, with each one's USB id and mapping (the app sets it).
    pub devices: Vec<midi::Port>,
    /// The user's mappings folder (the app sets it; `None`: only the built-in ones).
    pub mine: Option<PathBuf>,
    /// The mapping being shown and edited (the connected controller's); `None`: none.
    active: Option<Profile>,
    /// The mapping's version when it was last saved.
    saved_version: u64,
    /// "New mapping", while it's open.
    wizard: Option<Wizard>,
    /// The controller lights' slot, to send messages typed in the Send tab (the app
    /// sets it).
    pub lights: Option<crate::core::lights::MonitorSlot>,
    /// What's typed in the Send tab, why it isn't a message (if it isn't), and the
    /// messages sent from it, newest first.
    send_text: String,
    send_error: Option<String>,
    sent: VecDeque<Vec<u8>>,
}

/// A command as the history shows it: its name, then its fixed word if any.
fn command_text(c: Control, arg: Option<f64>) -> String {
    let (name, word) = c.name();
    match word.or_else(|| arg.map(|a| a.to_string())) {
        Some(w) => format!("{name} {w}"),
        None => name,
    }
}

/// The word after a mapped command's name (`-4`, `size`), if it has one.
fn word(l: &Learn) -> String {
    l.control
        .name()
        .1
        .or_else(|| l.arg.map(|a| a.to_string()))
        .unwrap_or_default()
}

impl ControllerUi {
    /// A message came in (it has already run its command).
    pub fn on_midi(&mut self, m: MidiMsg) {
        if let Some(key) = midi::key(&m.bytes) {
            let value = if m.bytes[0] & 0xF0 == 0x80 {
                0
            } else {
                m.bytes.get(2).copied().unwrap_or(0)
            };
            // A button let go (a note-off, or a note-on at 0) doesn't light its row or
            // scroll the list: letting go of Shift after Shift + pad leaves the list on
            // the pad.
            let release = key.0 & 0xF0 == 0x90 && value == 0;
            let s = self.seen.entry(key).or_insert(Seen {
                count: 0,
                value,
                at: m.at,
            });
            s.count += 1;
            s.value = value;
            if !release {
                s.at = m.at;
                if self.last != Some(key) {
                    self.scroll_to = Some(key);
                }
                self.last = Some(key);
            }
        }
        if self.paused {
            return;
        }
        self.started.get_or_insert(m.at);
        // A jog wheel or a knob sends a stream: one row per run. A 14-bit knob sends
        // two messages each move (a coarse one and a fine one, alternating), so a
        // message joins either of the newest two rows.
        if let Some(last) = self.log.iter_mut().take(2).find(|r| {
            r.port == m.port && r.bytes.get(..2) == m.bytes.get(..2) && r.bytes.len() == m.bytes.len()
        }) {
            last.count += 1;
            last.at = m.at;
            last.bytes = m.bytes;
            last.did = m.did;
            return;
        }
        self.log.push_front(Row {
            at: m.at,
            port: m.port,
            bytes: m.bytes,
            count: 1,
            did: m.did,
        });
        self.log.truncate(LOG_ROWS);
    }

    /// Every message in the list: the mapped ones and the ones pressed, in order.
    fn keys(&self) -> Vec<(u8, u8)> {
        let mut keys: Vec<(u8, u8)> = self.learned.all().into_iter().map(|(k, _)| k).collect();
        keys.extend(self.seen.keys());
        keys.sort();
        keys.dedup();
        keys
    }

    /// Map `key` to the command called `name`, keeping how it reads if it was mapped.
    /// A command that needs a number gets a first guess (`size`, else 1) to change.
    fn map_to(&mut self, key: (u8, u8), name: &str) {
        let Some((control, arg)) = [None, Some("size"), Some("1")]
            .into_iter()
            .find_map(|w| Control::parse(name, w).ok())
        else {
            return;
        };
        let input = self
            .learned
            .get(key)
            .map_or_else(|| midi::guess_input(key, control), |l| l.input);
        let display_only = self.learned.get(key).is_some_and(|l| l.display_only);
        self.learned.set(key, Learn { control, arg, input, display_only });
    }

    /// Change the number after a mapped command (`-4`, `size`). False if the command
    /// doesn't take it.
    fn set_word(&mut self, key: (u8, u8), w: &str) -> bool {
        let Some(l) = self.learned.get(key) else {
            return false;
        };
        let name = l.control.name().0;
        let w = Some(w.trim()).filter(|w| !w.is_empty());
        match Control::parse(&name, w) {
            Ok((control, arg)) => {
                self.learned.set(key, Learn { control, arg, ..l });
                true
            }
            Err(_) => false,
        }
    }

    fn store(&self) -> Store {
        Store::new(self.mine.clone())
    }

    /// The connected controller's mapping (already in `learned`), now shown and
    /// edited.
    pub fn set_active(&mut self, p: Option<Profile>) {
        self.active = p;
        self.saved_version = self.learned.version();
    }

    /// The mapping can be changed: it's the user's own.
    fn editable(&self) -> bool {
        self.active.as_ref().is_some_and(|p| !p.read_only())
    }

    /// Save the mapping if it's the user's and changed since it was last saved.
    fn keep_mapping(&mut self) {
        let v = self.learned.version();
        if v == self.saved_version {
            return;
        }
        self.saved_version = v;
        let Some(p) = self.active.as_ref().filter(|p| !p.read_only()) else {
            return;
        };
        let store = self.store();
        let own = store.own_part(p, &self.learned.file());
        if let Err(e) = store.save(&p.id, &p.head, &own) {
            *self.saved.lock().unwrap() = Some(format!("Couldn't save the mapping: {e}"));
        }
    }

    /// Make the mapping the wizard describes, and use it: here and now, or for its
    /// device from now on (the command for the app to run, returned).
    fn create(&mut self) -> Result<Option<AppCmd>, String> {
        let w = self.wizard.as_ref().ok_or("no wizard")?;
        let store = self.store();
        let maker = if w.new_maker {
            w.maker_name.clone()
        } else {
            w.maker.clone().ok_or("pick a manufacturer")?
        };
        let template = w
            .template
            .as_deref()
            .and_then(crate::core::controllers::MappingId::parse)
            .and_then(|id| store.get(&id));
        let device = w.device.and_then(|i| self.devices.get(i));
        let id = store.create(
            &maker,
            &w.model_name,
            template.as_ref(),
            device.map(|d| (d.name.as_str(), d.usb.as_deref())),
        )?;
        let cmd = device.map(|d| AppCmd::UseMapping {
            device: crate::core::controllers::device_key(&d.name, d.usb.as_deref()),
            mapping: id.key(),
        });
        let p = store.get(&id).ok_or("the new mapping didn't read back")?;
        self.learned.replace(p.file.clone());
        self.set_active(Some(p));
        *self.saved.lock().unwrap() = store.path(&id).map(|p| format!("Made {}", p.display()));
        self.wizard = None;
        Ok(cmd)
    }

    fn device(&self) -> String {
        self.ports
            .first()
            .and_then(|p| p.split(':').next())
            .unwrap_or("controller")
            .to_string()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
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

/// The bytes, then the message in words: "90 0B 7F  note on ch 1 #11 = 127".
fn named(bytes: &[u8]) -> String {
    match parts(bytes) {
        Some((kind, ch, num, val)) => {
            format!("{:<9} {kind:<8} ch {ch:<2} #{num:<3} = {val}", hex(bytes))
        }
        None => hex(bytes),
    }
}

/// Which control a message comes from, without a value: "90 0B  note ch 1 #11".
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

/// How a message's value reads, as the list names it.
const READINGS: [(Input, &str); 6] = [
    (Input::Note, "button"),
    (Input::Cc, "knob / fader"),
    (Input::Msb, "14-bit coarse (MSB)"),
    (Input::Lsb, "14-bit fine (LSB)"),
    (Input::Relative, "steps (1 = +1, 127 = -1)"),
    (Input::Jog(720), "jog wheel (64 = still)"),
];

fn reading_name(i: Input) -> &'static str {
    match i {
        Input::Note => READINGS[0].1,
        Input::Cc => READINGS[1].1,
        Input::Msb => READINGS[2].1,
        Input::Lsb => READINGS[3].1,
        Input::Relative => READINGS[4].1,
        Input::Jog(_) => READINGS[5].1,
    }
}

pub struct MappingPanel<'a> {
    pub tab: &'a mut ControllerUi,
}

impl Panel for MappingPanel<'_> {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let t = &mut *self.tab;
        let mapped = t.learned.all().len();
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
            }
            ui.add_space(16.0);
            ui.label("Mapping:");
            match &t.active {
                Some(p) => {
                    ui.label(RichText::new(p.label()).strong());
                }
                None => {
                    ui.label(RichText::new("none").color(cx.theme.muted));
                }
            }
            ui.label(format!("({mapped} mapped)"));
            if ui
                .button("New mapping…")
                .on_hover_text("Make a mapping of your own: empty, or starting from one there is")
                .clicked()
            {
                let maker = t.active.as_ref().map(|p| p.id.maker.clone());
                let template = t.active.as_ref().filter(|p| p.id.model.is_some()).map(|p| p.id.key());
                t.wizard = Some(Wizard {
                    maker,
                    template,
                    device: (!t.devices.is_empty()).then_some(0),
                    ..Default::default()
                });
            }
            if ui
                .add_enabled(mapped > 0, egui::Button::new("Save JSON…"))
                .on_hover_text("Write this mapping, as it's used, to a file of your choosing")
                .clicked()
            {
                save(t, ui.ctx());
            }
            if let Some(note) = t.saved.lock().unwrap().as_ref() {
                ui.label(RichText::new(note).color(cx.theme.muted));
            }
        });
        let hint = match &t.active {
            Some(p) if p.read_only() => {
                "A built-in mapping can't be changed. To change it, make a New mapping that starts from it."
            }
            Some(_) => {
                "Press, turn or move anything on the controller: it runs its command, and its \
                 message shows here. Pick the command it should run. Changes are saved as you go."
            }
            None => {
                "No mapping for this controller. Press things to see their messages, and make a \
                 New mapping to map them."
            }
        };
        ui.label(RichText::new(hint).color(cx.theme.muted));
        ui.separator();
        mapping_table(ui, t, cx);
        wizard(ui, t, cx);
        t.keep_mapping();
        let now = Instant::now();
        if t.seen.values().any(|s| now - s.at < FLASH) {
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
    }
}

/// "New mapping": the maker (or a new one), what to start from, the model's name and
/// the device.
fn wizard(ui: &mut Ui, t: &mut ControllerUi, cx: &mut Cx) {
    if t.wizard.is_none() {
        return;
    }
    let store = t.store();
    let makers = store.makers();
    let models = store.models();
    let devices: Vec<String> = t.devices.iter().map(|d| d.name.clone()).collect();
    let mut create = false;
    let mut cancel = false;
    let w = t.wizard.as_mut().expect("open");
    let r = egui::Modal::new(egui::Id::new("new-mapping")).show(ui.ctx(), |ui| {
        ui.set_width(420.0);
        ui.heading("New mapping");
        egui::Grid::new("new-mapping-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("Manufacturer");
            let shown = if w.new_maker {
                "New…".to_string()
            } else {
                w.maker
                    .as_ref()
                    .and_then(|m| makers.iter().find(|(k, _)| k == m))
                    .map_or("Pick one".to_string(), |(_, n)| n.clone())
            };
            egui::ComboBox::from_id_salt("wizard-maker").selected_text(shown).width(240.0).show_ui(ui, |ui| {
                for (k, n) in &makers {
                    if ui.selectable_label(!w.new_maker && w.maker.as_ref() == Some(k), n).clicked() {
                        w.maker = Some(k.clone());
                        w.new_maker = false;
                        w.template = None;
                    }
                }
                if ui.selectable_label(w.new_maker, "New…").clicked() {
                    w.new_maker = true;
                    w.maker = None;
                    w.template = None;
                }
            });
            ui.end_row();
            if w.new_maker {
                ui.label("Its name");
                ui.add(egui::TextEdit::singleline(&mut w.maker_name).hint_text("Pioneer DJ").desired_width(240.0));
                ui.end_row();
            } else if let Some(maker) = &w.maker {
                ui.label("Start from");
                let theirs: Vec<&Profile> = models.iter().filter(|p| p.id.maker == *maker).collect();
                let shown = w
                    .template
                    .as_ref()
                    .and_then(|k| theirs.iter().find(|p| p.id.key() == *k))
                    .map_or("Empty".to_string(), |p| p.label());
                egui::ComboBox::from_id_salt("wizard-template").selected_text(shown).width(240.0).show_ui(ui, |ui| {
                    if ui.selectable_label(w.template.is_none(), "Empty").clicked() {
                        w.template = None;
                    }
                    for p in &theirs {
                        let k = p.id.key();
                        if ui.selectable_label(w.template.as_ref() == Some(&k), p.label()).clicked() {
                            w.template = Some(k);
                        }
                    }
                });
                ui.end_row();
            }
            ui.label("Model");
            ui.add(egui::TextEdit::singleline(&mut w.model_name).hint_text("DDJ-FLX4").desired_width(240.0));
            ui.end_row();
            ui.label("MIDI device");
            let shown = w.device.and_then(|i| devices.get(i)).map_or("None".to_string(), Clone::clone);
            egui::ComboBox::from_id_salt("wizard-device").selected_text(shown).width(240.0).show_ui(ui, |ui| {
                if ui.selectable_label(w.device.is_none(), "None").clicked() {
                    w.device = None;
                }
                for (i, d) in devices.iter().enumerate() {
                    if ui.selectable_label(w.device == Some(i), d).clicked() {
                        w.device = Some(i);
                    }
                }
            });
            ui.end_row();
        });
        ui.label(
            RichText::new(
                "The device uses this mapping from now on. A mapping that starts from another \
                 is a copy: changing one leaves the other alone.",
            )
            .color(cx.theme.muted),
        );
        if let Some(e) = &w.error {
            ui.label(RichText::new(e).color(cx.theme.warn));
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let ready = !w.model_name.trim().is_empty()
                && (w.maker.is_some() || (w.new_maker && !w.maker_name.trim().is_empty()));
            if ui.add_enabled(ready, egui::Button::new("Create")).clicked() {
                create = true;
            }
            if ui.button("Cancel").clicked() {
                cancel = true;
            }
        });
    });
    if cancel || r.should_close() {
        t.wizard = None;
    } else if create {
        match t.create() {
            Ok(cmd) => {
                if let Some(c) = cmd {
                    cx.send(c);
                }
            }
            Err(e) => {
                if let Some(w) = t.wizard.as_mut() {
                    w.error = Some(e);
                }
            }
        }
    }
}

/// Ask where to save the mapping, and save it there.
fn save(t: &ControllerUi, ctx: &egui::Context) {
    let device = t.device();
    let json = t.learned.to_json(&device);
    let (saved, ctx) = (t.saved.clone(), ctx.clone());
    std::thread::spawn(move || {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Save the controller mapping as")
            .set_file_name(format!("{device}.json"))
            .add_filter("JSON", &["json"])
            .save_file()
        else {
            return;
        };
        let note = match std::fs::write(&path, json) {
            Ok(()) => format!("Saved {}", path.display()),
            Err(e) => format!("Couldn't save {}: {e}", path.display()),
        };
        *saved.lock().unwrap() = Some(note);
        ctx.request_repaint();
    });
}

/// The Mapping table's columns.
const MAPPING_COLUMNS: [&str; 7] = [
    "message",
    "value",
    "pressed",
    "earshot_command",
    "number",
    "reads as",
    "display only",
];

/// The rows in the order the table is sorted by. Rows with nothing in the column go
/// last either way; ties stay in message order.
fn sorted(t: &ControllerUi, mut keys: Vec<(u8, u8)>) -> Vec<(u8, u8)> {
    let Some((col, reversed)) = t.sort else {
        return keys;
    };
    #[derive(PartialEq, PartialOrd)]
    enum Cell {
        Num(f64),
        Text(String),
    }
    let cell = |k: &(u8, u8)| -> Option<Cell> {
        let seen = t.seen.get(k);
        let l = t.learned.get(*k);
        match col {
            0 => Some(Cell::Num((k.0 as u32 * 256 + k.1 as u32) as f64)),
            1 => seen.map(|s| Cell::Num(s.value as f64)),
            2 => seen.map(|s| Cell::Num(s.count as f64)),
            3 => l.map(|l| Cell::Text(l.control.name().0)),
            4 => l
                .filter(|l| l.control.takes_number())
                .map(|l| match l.arg {
                    Some(a) => Cell::Num(a),
                    None => Cell::Text(word(&l)),
                }),
            5 => l.map(|l| Cell::Text(reading_name(l.input).to_string())),
            _ => l.map(|l| Cell::Num(l.display_only as u8 as f64)),
        }
    };
    keys.sort_by(|a, b| match (cell(a), cell(b)) {
        (Some(x), Some(y)) => {
            let o = x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal);
            if reversed { o.reverse() } else { o }
        }
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    keys
}

fn mapping_table(ui: &mut Ui, t: &mut ControllerUi, cx: &mut Cx) {
    let editable = t.editable();
    let keys = sorted(t, t.keys());
    if keys.is_empty() {
        ui.label(RichText::new("Nothing yet.").color(cx.theme.muted));
        return;
    }
    let now = Instant::now();
    let names = bus::command_names();
    let mut table = TableBuilder::new(ui)
        .striped(true)
        .resizable(true)
        .auto_shrink(false)
        .column(Column::exact(200.0))
        .column(Column::exact(50.0))
        .column(Column::exact(60.0))
        .column(Column::exact(280.0))
        .column(Column::exact(80.0))
        .column(Column::exact(250.0))
        .column(Column::remainder());
    if let Some(i) = t
        .scroll_to
        .take()
        .and_then(|k| keys.iter().position(|x| *x == k))
    {
        table = table.scroll_to_row(i, Some(Align::Center));
    }
    table
        .header(18.0, |mut h| {
            // Click a heading to sort by it; again to reverse.
            for (i, name) in MAPPING_COLUMNS.into_iter().enumerate() {
                h.col(|ui| {
                    let arrow = match t.sort {
                        Some((c, false)) if c == i => " ⏶",
                        Some((c, true)) if c == i => " ⏷",
                        _ => "",
                    };
                    let r = ui
                        .add(
                            egui::Label::new(
                                RichText::new(format!("{name}{arrow}")).color(cx.theme.muted),
                            )
                            .sense(egui::Sense::click()),
                        )
                        .on_hover_text("Sort by this; click again to reverse");
                    if r.clicked() {
                        t.sort = match t.sort {
                            Some((c, false)) if c == i => Some((i, true)),
                            _ => Some((i, false)),
                        };
                    }
                });
            }
        })
        .body(|body| {
            body.rows(24.0, keys.len(), |mut row| {
                let key = keys[row.index()];
                let seen = t.seen.get(&key).copied();
                let l = t.learned.get(key);
                row.set_selected(seen.is_some_and(|s| now - s.at < FLASH));
                row.col(|ui| {
                    ui.monospace(named_key(key.0, key.1));
                });
                row.col(|ui| {
                    if let Some(s) = seen {
                        ui.monospace(s.value.to_string());
                    }
                });
                row.col(|ui| {
                    if let Some(s) = seen {
                        ui.monospace(s.count.to_string());
                    }
                });
                row.col(|ui| {
                    if !editable {
                        ui.disable();
                    }
                    command_picker(ui, t, key, l, &names, cx)
                });
                row.col(|ui| {
                    if !editable {
                        ui.disable();
                    }
                    if let Some(l) = l.filter(|l| l.control.takes_number()) {
                        number_field(ui, t, key, &l);
                    }
                });
                row.col(|ui| {
                    if !editable {
                        ui.disable();
                    }
                    let Some(l) = l else { return };
                    egui::ComboBox::from_id_salt(("reads", key))
                        .selected_text(reading_name(l.input))
                        .width(180.0)
                        .show_ui(ui, |ui| {
                            for (input, name) in READINGS {
                                let on = reading_name(l.input) == name;
                                if ui.selectable_label(on, name).clicked() && !on {
                                    t.learned.set(key, Learn { input, ..l });
                                }
                            }
                        });
                    // A half with no partner yet: say what it needs.
                    if matches!(l.input, Input::Msb | Input::Lsb)
                        && t.learned.partner(key, &l).is_none()
                    {
                        let other = if l.input == Input::Msb { "fine" } else { "coarse" };
                        ui.label(RichText::new("⚠").color(cx.theme.warn)).on_hover_text(
                            format!("No {other} half yet: map another message to the same command, as its {other} half"),
                        );
                    }
                });
                // Display only: for an output level the controller sets itself.
                row.col(|ui| {
                    if !editable {
                        ui.disable();
                    }
                    let Some(l) = l.filter(|l| crate::core::midi::can_be_display_only(l.control)) else {
                        return;
                    };
                    let mut on = l.display_only;
                    if ui
                        .checkbox(&mut on, "")
                        .on_hover_text(
                            "The controller does this itself (a master level knob, say): \
                             earshot only shows where it is, and leaves the sound alone",
                        )
                        .changed()
                    {
                        t.learned.set_display_only(key, on);
                    }
                });
            });
        });
}

/// The row's command: a list to pick from, with a search box at the top. Picking one
/// maps the message to it at once.
fn command_picker(
    ui: &mut Ui,
    t: &mut ControllerUi,
    key: (u8, u8),
    l: Option<Learn>,
    names: &[String],
    cx: &Cx,
) {
    let current = l.map(|l| l.control.name().0);
    let shown = match &current {
        Some(n) => RichText::new(n).monospace(),
        None => RichText::new("—").color(cx.theme.muted),
    };
    let r = egui::ComboBox::from_id_salt(("command", key))
        .selected_text(shown)
        .width(270.0)
        .height(360.0)
        // Clicking the search box mustn't close it; picking a command does.
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show_ui(ui, |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut t.search)
                    .hint_text("search")
                    .desired_width(250.0),
            )
            .request_focus();
            ui.separator();
            if ui.selectable_label(current.is_none(), "— (nothing)").clicked() {
                t.learned.remove(key);
                t.search.clear();
                ui.close();
            }
            let typed = t.search.trim().to_lowercase();
            for n in names.iter().filter(|n| n.contains(&typed)) {
                let on = current.as_deref() == Some(n.as_str());
                if ui
                    .selectable_label(on, RichText::new(n).monospace())
                    .clicked()
                {
                    t.map_to(key, n);
                    t.search.clear();
                    ui.close();
                }
            }
        });
    if let Some(l) = l {
        r.response.on_hover_text(l.control.describe(l.arg));
    }
}

/// The number after the row's command (`-4`, `8`, `size`), set when the field loses
/// focus.
fn number_field(ui: &mut Ui, t: &mut ControllerUi, key: (u8, u8), l: &Learn) {
    let mut text = match &t.value_edit {
        Some((k, w)) if *k == key => w.clone(),
        _ => word(l),
    };
    let r = ui.add(
        egui::TextEdit::singleline(&mut text)
            .font(egui::TextStyle::Monospace)
            .desired_width(70.0),
    );
    if r.has_focus() {
        t.value_edit = Some((key, text.clone()));
    }
    if r.lost_focus() {
        t.set_word(key, &text);
        t.value_edit = None;
    }
}

/// How many messages the Send tab keeps to send again.
const SENT_ROWS: usize = 30;

/// Hex bytes typed in the Send tab (`90 0B 7F`, `F0 00 40 F7`) as a message, or why
/// they aren't one: a short message needs a status byte and its data, a SysEx starts
/// F0 and ends F7.
pub fn parse_send(text: &str) -> Result<Vec<u8>, String> {
    let bytes: Vec<u8> = text
        .split_whitespace()
        .map(|b| u8::from_str_radix(b, 16).map_err(|_| format!("{b:?} isn't a hex byte")))
        .collect::<Result<_, _>>()?;
    match bytes[..] {
        [] => Err("type the message's bytes in hex, e.g. 90 0B 7F".into()),
        [0xF0, .., 0xF7] => Ok(bytes),
        [0xF0, ..] => Err("a SysEx ends with F7".into()),
        [s, ..] if s < 0x80 => Err("the first byte is a status byte: 80 to FF".into()),
        [s, ref data @ ..] => {
            // How many data bytes the status takes.
            let want = match s & 0xF0 {
                0xC0 | 0xD0 => 1,
                0xF0 => data.len(),
                _ => 2,
            };
            if data.len() != want {
                return Err(format!("{s:02X} takes {want} data byte(s)"));
            }
            if data.iter().any(|&d| d > 0x7F) {
                return Err("data bytes are 00 to 7F".into());
            }
            Ok(bytes)
        }
    }
}

/// The Send tab: type a message, send it to the controller (for testing what a
/// message does). Each one sent is logged (`core::lights`).
pub struct SendPanel<'a> {
    pub tab: &'a mut ControllerUi,
}

impl SendPanel<'_> {
    fn send(t: &mut ControllerUi, bytes: Vec<u8>) {
        if let Some(l) = &t.lights {
            l.lock().unwrap().send.push(bytes.clone());
        }
        t.sent.retain(|b| *b != bytes);
        t.sent.push_front(bytes);
        t.sent.truncate(SENT_ROWS);
    }
}

impl Panel for SendPanel<'_> {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let t = &mut *self.tab;
        if t.ports.is_empty() {
            ui.label(RichText::new("No controller is connected.").color(cx.theme.muted));
            return;
        }
        ui.horizontal(|ui| {
            ui.label("Message (hex)");
            let field = ui.add(
                egui::TextEdit::singleline(&mut t.send_text)
                    .hint_text("90 0B 7F")
                    .font(egui::TextStyle::Monospace)
                    .desired_width(320.0),
            );
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.button("Send").clicked() || enter {
                match parse_send(&t.send_text) {
                    Ok(b) => {
                        t.send_error = None;
                        Self::send(t, b);
                    }
                    Err(e) => t.send_error = Some(e),
                }
                if enter {
                    field.request_focus();
                }
            }
        });
        if let Some(e) = &t.send_error {
            ui.label(RichText::new(e).color(cx.theme.warn));
        }
        ui.label(
            RichText::new(
                "Sent to every open controller port, and logged. A light earshot drives \
                 stays as sent until earshot's own value for it changes.",
            )
            .color(cx.theme.muted),
        );
        ui.separator();
        let mut again = None;
        for b in &t.sent {
            ui.horizontal(|ui| {
                if ui.small_button("Send again").clicked() {
                    again = Some(b.clone());
                }
                ui.label(RichText::new(hex(b)).monospace());
            });
        }
        if let Some(b) = again {
            Self::send(t, b);
        }
    }
}

pub struct HistoryPanel<'a> {
    pub tab: &'a mut ControllerUi,
}

impl Panel for HistoryPanel<'_> {
    fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let t = &mut *self.tab;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Newest first").strong());
            if ui.button("Clear").clicked() {
                t.log.clear();
                t.started = None;
            }
            let pause = if t.paused { "Resume" } else { "Pause" };
            if ui.button(pause).clicked() {
                t.paused = !t.paused;
            }
        });
        if t.log.is_empty() {
            ui.label(
                RichText::new("Press, turn or move something on the controller.")
                    .color(cx.theme.muted),
            );
            return;
        }
        let now = Instant::now();
        let start = t.started.unwrap_or(now);
        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .auto_shrink(false)
            .column(Column::exact(64.0))
            .column(Column::exact(300.0))
            .column(Column::remainder().clip(true))
            .header(18.0, |mut h| {
                for name in ["time", "message", "earshot_command"] {
                    h.col(|ui| {
                        ui.label(RichText::new(name).color(cx.theme.muted));
                    });
                }
            })
            .body(|body| {
                body.rows(18.0, t.log.len(), |mut row| {
                    let r = &t.log[row.index()];
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
                    row.col(|ui| match r.did {
                        Some(c) => {
                            ui.monospace(command_text(c, None));
                        }
                        None => {
                            ui.label(RichText::new("not mapped").color(cx.theme.muted));
                        }
                    });
                });
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::bus::DeckControl;
    use earshot_engine::DeckId;

    fn msg(bytes: &[u8]) -> MidiMsg {
        MidiMsg {
            port: 0,
            bytes: bytes.to_vec(),
            at: Instant::now(),
            did: None,
        }
    }

    /// Press something new: it joins the list and the list scrolls to it. Pick its
    /// command and number; they go in the file.
    #[test]
    fn mapping_by_pressing() {
        let deck1 = DeckId::new(0).unwrap();
        let mut t = ControllerUi::default();
        t.on_midi(msg(&[0x92, 0x01, 0x7F]));
        t.on_midi(msg(&[0x82, 0x01, 0x00]));
        assert_eq!(t.keys(), [(0x92, 0x01)], "a note-off is the same control");
        assert_eq!(t.scroll_to, Some((0x92, 0x01)));
        assert_eq!(t.seen[&(0x92, 0x01)].count, 2);
        assert!(t.learned.get((0x92, 0x01)).is_none());

        // A command that needs a number gets one to change.
        t.map_to((0x92, 0x01), "deck_1_beat_loop");
        let l = t.learned.get((0x92, 0x01)).unwrap();
        assert_eq!(l.control, Control::Deck(deck1, DeckControl::Loop));
        assert_eq!(l.input, Input::Note);
        assert!(t.set_word((0x92, 0x01), "8"));
        assert_eq!(
            t.learned.get((0x92, 0x01)).unwrap().control,
            Control::Deck(deck1, DeckControl::BeatLoop(8.0))
        );
        assert!(!t.set_word((0x92, 0x01), "x"));
        t.map_to((0x92, 0x01), "deck_1_beat_jump");
        assert!(t.set_word((0x92, 0x01), "-4"));
        let l = t.learned.get((0x92, 0x01)).unwrap();
        assert_eq!(l.arg, Some(-4.0));
        assert_eq!(word(&l), "-4");

        // A knob reads as a fader; a new command keeps how it reads.
        t.on_midi(msg(&[0xB5, 0x10, 0x40]));
        assert_eq!(t.scroll_to, Some((0xB5, 0x10)));
        t.map_to((0xB5, 0x10), "mixer_crossfader");
        let l = t.learned.get((0xB5, 0x10)).unwrap();
        assert_eq!(l.input, Input::Cc);
        t.learned.set((0xB5, 0x10), Learn { input: Input::Jog(720), ..l });
        t.map_to((0xB5, 0x10), "deck_2_scratch");
        assert_eq!(t.learned.get((0xB5, 0x10)).unwrap().input, Input::Jog(720));

        let json = t.learned.to_json("Mystery");
        assert!(json.contains("\"deck_1_beat_jump\""), "{json}");
        assert!(json.contains("\"deck_2_scratch\""), "{json}");
    }

    /// A built-in mapping can't be edited; a new one made from it can, and is saved
    /// as it changes (only what differs from the maker's file).
    #[test]
    fn a_new_mapping_is_saved_as_it_changes() {
        let (_, dir) = crate::core::controllers::test_store("panel");
        let ddj = crate::core::controllers::bundled("pioneer", Some("ddj-400"));
        let mut t = ControllerUi { mine: Some(dir.clone()), ..Default::default() };
        t.learned.replace(ddj.file.clone());
        t.set_active(Some(ddj.clone()));
        assert!(!t.editable(), "built-in");

        t.wizard = Some(Wizard {
            maker: Some("pioneer".into()),
            template: Some(ddj.id.key()),
            model_name: "Mine".into(),
            ..Default::default()
        });
        assert!(t.create().unwrap().is_none(), "no device: nothing for the app");
        assert!(t.wizard.is_none());
        assert!(t.editable(), "the new one is the user's");
        assert_eq!(t.learned.all().len(), ddj.file.inputs.len(), "a copy of the DDJ-400's");

        t.map_to((0x92, 0x01), "deck_1_play");
        t.keep_mapping();
        let path = dir.join("pioneer/controllers/mine.json");
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("\"92 01\""), "the new line is saved");
        assert!(!saved.contains("\"90 0B\""), "PLAY is Pioneer's line, not saved again");
        let back = t.store().get(&t.active.as_ref().unwrap().id).unwrap();
        assert_eq!(back.file.inputs.len(), ddj.file.inputs.len() + 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Shift + pad: the list goes to Shift, then the pad, and stays on the pad when
    /// Shift (then the pad) is let go.
    /// A 14-bit controller is two rows on one command, marked coarse and fine by
    /// hand; nothing pairs on its own, and both rows stay in the table.
    #[test]
    fn a_14_bit_controller_is_marked_by_hand() {
        let mut t = ControllerUi::default();
        t.map_to((0xB6, 0x17), "deck_1_filter");
        t.map_to((0xB6, 0x37), "deck_1_filter");
        t.on_midi(msg(&[0xB6, 0x17, 64]));
        t.on_midi(msg(&[0xB6, 0x37, 3]));
        let coarse = t.learned.get((0xB6, 0x17)).unwrap();
        assert_eq!(coarse.input, Input::Cc);
        t.learned.set((0xB6, 0x17), Learn { input: Input::Msb, ..coarse });
        let fine = t.learned.get((0xB6, 0x37)).unwrap();
        t.learned.set((0xB6, 0x37), Learn { input: Input::Lsb, ..fine });
        let coarse = t.learned.get((0xB6, 0x17)).unwrap();
        assert_eq!(t.learned.partner((0xB6, 0x17), &coarse), Some((0xB6, 0x37)));
        assert!(t.keys().contains(&(0xB6, 0x37)), "both rows stay");
    }

    /// The Mapping table sorts by any column, either way; rows with nothing there
    /// go last.
    #[test]
    fn the_mapping_sorts_by_any_column() {
        let mut t = ControllerUi::default();
        for (bytes, n) in [([0x90, 0x01, 0x7F], 3), ([0x90, 0x02, 0x7F], 1)] {
            for _ in 0..n {
                t.on_midi(msg(&bytes));
            }
        }
        t.map_to((0x90, 0x02), "deck_1_play");
        t.map_to((0x91, 0x05), "deck_2_cue");
        let rows = |t: &ControllerUi| sorted(t, t.keys());
        assert_eq!(rows(&t), [(0x90, 0x01), (0x90, 0x02), (0x91, 0x05)]);
        // By times pressed, most first: the one never pressed last.
        t.sort = Some((2, true));
        assert_eq!(rows(&t), [(0x90, 0x01), (0x90, 0x02), (0x91, 0x05)]);
        t.sort = Some((2, false));
        assert_eq!(rows(&t), [(0x90, 0x02), (0x90, 0x01), (0x91, 0x05)]);
        // By command name: deck_1_play, deck_2_cue, then the unmapped one.
        t.sort = Some((3, false));
        assert_eq!(rows(&t), [(0x90, 0x02), (0x91, 0x05), (0x90, 0x01)]);
        t.sort = Some((3, true));
        assert_eq!(rows(&t), [(0x91, 0x05), (0x90, 0x02), (0x90, 0x01)]);
    }

    #[test]
    fn letting_go_doesnt_move_the_list() {
        let mut t = ControllerUi::default();
        t.on_midi(msg(&[0x90, 0x3F, 0x7F]));
        assert_eq!(t.scroll_to.take(), Some((0x90, 0x3F)));
        t.on_midi(msg(&[0x98, 0x00, 0x7F]));
        assert_eq!(t.scroll_to.take(), Some((0x98, 0x00)));
        t.on_midi(msg(&[0x90, 0x3F, 0x00]));
        t.on_midi(msg(&[0x88, 0x00, 0x40]));
        assert_eq!(t.scroll_to, None);
        // A knob still scrolls, at any value.
        t.on_midi(msg(&[0xB0, 0x13, 0x00]));
        assert_eq!(t.scroll_to.take(), Some((0xB0, 0x13)));
        // Pressing Shift again goes back to it.
        t.on_midi(msg(&[0x90, 0x3F, 0x7F]));
        assert_eq!(t.scroll_to, Some((0x90, 0x3F)));
    }

    #[test]
    fn messages_in_words() {
        assert_eq!(named(&[0x90, 0x0B, 0x7F]), "90 0B 7F  note on  ch 1  #11  = 127");
        assert_eq!(named(&[0xB6, 0x40, 0x01]), "B6 40 01  CC       ch 7  #64  = 1");
        assert_eq!(named_key(0x97, 0x00), "97 00  note     ch 8  #0");
        assert_eq!(named_key(0xB0, 0x13), "B0 13  CC       ch 1  #19");
        assert_eq!(named(&[0x90]), "90");
    }

    /// The Send tab reads hex bytes as one message, and says what's wrong otherwise.
    #[test]
    fn typed_messages_are_checked() {
        assert_eq!(parse_send("90 0b 7F"), Ok(vec![0x90, 0x0B, 0x7F]));
        assert_eq!(parse_send("C0 05"), Ok(vec![0xC0, 0x05]));
        assert_eq!(parse_send(" F0 00 40 F7 "), Ok(vec![0xF0, 0x00, 0x40, 0xF7]));
        assert!(parse_send("").is_err());
        assert!(parse_send("90 0B").unwrap_err().contains("2 data byte"));
        assert!(parse_send("90 0B 80").unwrap_err().contains("00 to 7F"));
        assert!(parse_send("0B 7F").unwrap_err().contains("status"));
        assert!(parse_send("F0 00 40").unwrap_err().contains("F7"));
        assert!(parse_send("90 zz 7F").unwrap_err().contains("zz"));
    }

}
