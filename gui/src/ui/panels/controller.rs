//! Controller mode's Mapping and History tabs, for mapping a controller by pressing
//! its buttons.
//!
//! Mapping lists every message the session knows: the ones mapped (a DDJ-400 starts
//! from the built-in table; anything else from nothing) and every one pressed since.
//! Pressing something runs its command, adds its message to the list if it's new, and
//! scrolls the list to it. Each row's command can be picked from a list, and takes
//! effect at once. The mapping is kept as a draft file as it changes, and read back
//! when earshot starts, so work in progress survives a restart. "New mapping" starts
//! again: empty, from a built-in profile, or from a controller file. "Save JSON…"
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
use crate::core::midi::{self, Input, Learn, Learned, Map, MidiMsg};

/// The draft file's name, in earshot's data folder.
pub const DRAFT_FILE: &str = "controller-draft.json";

/// Where a new mapping starts from.
#[derive(Clone, Copy, Debug, PartialEq)]
enum NewFrom {
    Empty,
    /// One of [`midi::profiles`].
    Profile(usize),
    /// A controller file, picked in a dialog.
    File,
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
    /// The draft file, kept as the mapping changes (the app sets it).
    pub draft: Option<PathBuf>,
    /// The mapping's version when the draft was last written.
    draft_version: u64,
    /// A new mapping asked for, waiting for "replace the current one?".
    confirm_new: Option<NewFrom>,
    /// A controller file read in the open dialog's thread, to put in.
    opened: Arc<Mutex<Option<Map>>>,
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
        // A 14-bit control's fine half is in its coarse half's row.
        keys.retain(|k| !self.learned.is_fine_half(*k));
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
        self.learned.set(key, Learn { control, arg, input });
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

    /// Read the draft back, if there is one: the mapping as it was when earshot last
    /// ran. Call before the ports open (so a DDJ-400's table isn't put on top).
    pub fn load_draft(&mut self) {
        let Some(path) = &self.draft else { return };
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        match midi::from_json(&text) {
            Ok((_, map)) => self.learned.replace(map),
            Err(e) => {
                *self.saved.lock().unwrap() =
                    Some(format!("The draft ({}) couldn't be read: {e}", path.display()))
            }
        }
        self.draft_version = self.learned.version();
    }

    /// Write the draft if the mapping changed since it was last written.
    fn keep_draft(&mut self) {
        let v = self.learned.version();
        if v == self.draft_version {
            return;
        }
        self.draft_version = v;
        let Some(path) = &self.draft else { return };
        if let Err(e) = std::fs::write(path, self.learned.to_json(&self.device())) {
            *self.saved.lock().unwrap() = Some(format!("Couldn't keep the draft: {e}"));
        }
    }

    /// Start a new mapping, in place of the current one.
    fn start_new(&mut self, from: NewFrom, ctx: &egui::Context) {
        match from {
            NewFrom::Empty => self.learned.clear(),
            NewFrom::Profile(i) => {
                if let Some((_, map)) = midi::profiles().into_iter().nth(i) {
                    self.learned.replace(map);
                }
            }
            NewFrom::File => {
                let (opened, note, ctx) = (self.opened.clone(), self.saved.clone(), ctx.clone());
                std::thread::spawn(move || {
                    let Some(path) = rfd::FileDialog::new()
                        .set_title("Start from a controller file")
                        .add_filter("JSON", &["json"])
                        .pick_file()
                    else {
                        return;
                    };
                    let read = std::fs::read_to_string(&path).map_err(|e| e.to_string());
                    match read.and_then(|t| midi::from_json(&t)) {
                        Ok((_, map)) => {
                            *opened.lock().unwrap() = Some(map);
                            *note.lock().unwrap() = Some(format!("Started from {}", path.display()));
                        }
                        Err(e) => {
                            *note.lock().unwrap() =
                                Some(format!("Couldn't read {}: {e}", path.display()))
                        }
                    }
                    ctx.request_repaint();
                });
            }
        }
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
const READINGS: [(Input, &str); 5] = [
    (Input::Note, "button"),
    (Input::Cc, "knob / fader"),
    (Input::Cc14(0), "14-bit controller (MSB + LSB)"),
    (Input::Relative, "steps (1 = +1, 127 = -1)"),
    (Input::Jog(720), "jog wheel (64 = still)"),
];

fn reading_name(i: Input) -> &'static str {
    match i {
        Input::Note => READINGS[0].1,
        Input::Cc => READINGS[1].1,
        Input::Cc14(_) => READINGS[2].1,
        Input::Relative => READINGS[3].1,
        Input::Jog(_) => READINGS[4].1,
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
            ui.label(format!("{mapped} mapped"));
            if ui
                .add_enabled(mapped > 0, egui::Button::new("Save JSON…"))
                .on_hover_text("Write this mapping as a controller file")
                .clicked()
            {
                save(t, ui.ctx());
            }
            let mut asked = None;
            ui.menu_button("New mapping", |ui| {
                if ui.button("Empty").on_hover_text("Map the controller from scratch").clicked() {
                    asked = Some(NewFrom::Empty);
                }
                for (i, (name, _)) in midi::profiles().iter().enumerate() {
                    if ui.button(*name).clicked() {
                        asked = Some(NewFrom::Profile(i));
                    }
                }
                if ui.button("From a file…").on_hover_text("A saved controller file").clicked() {
                    asked = Some(NewFrom::File);
                }
            })
            .response
            .on_hover_text("Start again, empty or from a mapping (this one is replaced)");
            if let Some(from) = asked {
                if mapped > 0 {
                    t.confirm_new = Some(from);
                } else {
                    t.start_new(from, ui.ctx());
                }
            }
            if let Some(note) = t.saved.lock().unwrap().as_ref() {
                ui.label(RichText::new(note).color(cx.theme.muted));
            }
        });
        ui.label(
            RichText::new(
                "Press, turn or move anything on the controller: it runs its command, and \
                 its message shows here. Pick the command it should run.",
            )
            .color(cx.theme.muted),
        );
        ui.separator();
        mapping_table(ui, t, cx);
        confirm_new(ui, t, mapped);
        if let Some(map) = t.opened.lock().unwrap().take() {
            t.learned.replace(map);
        }
        t.keep_draft();
        let now = Instant::now();
        if t.seen.values().any(|s| now - s.at < FLASH) {
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
    }
}

/// "Replace the current mapping?", while a new one is asked for.
fn confirm_new(ui: &mut Ui, t: &mut ControllerUi, mapped: usize) {
    let Some(from) = t.confirm_new else { return };
    let r = egui::Modal::new(egui::Id::new("confirm-new-mapping")).show(ui.ctx(), |ui| {
        ui.heading("Start a new mapping?");
        ui.label(format!(
            "The current one ({mapped} mapped) is replaced. Save JSON first to keep it."
        ));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Start new").clicked() {
                Some(true)
            } else if ui.button("Cancel").clicked() {
                Some(false)
            } else {
                None
            }
        })
        .inner
    });
    match r.inner.or(r.should_close().then_some(false)) {
        Some(true) => {
            t.confirm_new = None;
            t.start_new(from, ui.ctx());
        }
        Some(false) => t.confirm_new = None,
        None => {}
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
const MAPPING_COLUMNS: [&str; 6] =
    ["message", "value", "pressed", "earshot_command", "number", "reads as"];

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
            _ => l.map(|l| Cell::Text(reading_name(l.input).to_string())),
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
                row.col(|ui| command_picker(ui, t, key, l, &names, cx));
                row.col(|ui| {
                    if let Some(l) = l.filter(|l| l.control.takes_number()) {
                        number_field(ui, t, key, &l);
                    }
                });
                row.col(|ui| {
                    let Some(l) = l else { return };
                    egui::ComboBox::from_id_salt(("reads", key))
                        .selected_text(reading_name(l.input))
                        .width(180.0)
                        .show_ui(ui, |ui| {
                            for (input, name) in READINGS {
                                let on = reading_name(l.input) == name;
                                if ui.selectable_label(on, name).clicked() && !on {
                                    // A 14-bit controller starts with MIDI's usual fine half.
                                    let input = match input {
                                        Input::Cc14(_) => Input::Cc14(midi::usual_fine(key)),
                                        i => i,
                                    };
                                    t.learned.set(key, Learn { input, ..l });
                                }
                            }
                        });
                    if let Input::Cc14(fine) = l.input {
                        fine_picker(ui, t, key, fine, &l);
                    }
                });
            });
        });
}

/// A 14-bit controller's fine half: which message on the same channel it is.
fn fine_picker(ui: &mut Ui, t: &mut ControllerUi, key: (u8, u8), fine: u8, l: &Learn) {
    ui.label("fine:");
    egui::ComboBox::from_id_salt(("fine", key))
        .selected_text(RichText::new(format!("{:02X} {fine:02X}", key.0)).monospace())
        .width(70.0)
        .height(300.0)
        .show_ui(ui, |ui| {
            for d in (0..128u8).filter(|d| *d != key.1) {
                let label = RichText::new(format!("{:02X} {d:02X}", key.0)).monospace();
                if ui.selectable_label(d == fine, label).clicked() && d != fine {
                    t.learned.set(key, Learn { input: Input::Cc14(d), ..*l });
                }
            }
        })
        .response
        .on_hover_text("The message that sends this control's fine half (MIDI's usual: +32)");
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

    /// The mapping is kept as a draft as it changes, and read back on the next run;
    /// a new mapping starts from nothing or a profile.
    #[test]
    fn drafts_survive_a_restart() {
        let dir = std::env::temp_dir().join(format!("earshot-draft-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(DRAFT_FILE);
        let _ = std::fs::remove_file(&path);
        let ctx = egui::Context::default();

        let mut t = ControllerUi { draft: Some(path.clone()), ..Default::default() };
        t.load_draft();
        assert!(t.learned.all().is_empty(), "no draft yet");
        t.on_midi(msg(&[0x92, 0x01, 0x7F]));
        t.map_to((0x92, 0x01), "deck_1_play");
        t.keep_draft();
        assert!(path.exists());

        let mut again = ControllerUi { draft: Some(path.clone()), ..Default::default() };
        again.load_draft();
        assert_eq!(again.learned.all(), t.learned.all());

        again.start_new(NewFrom::Profile(0), &ctx);
        assert!(again.learned.all().len() > 50, "the DDJ-400's table");
        again.start_new(NewFrom::Empty, &ctx);
        assert!(again.learned.all().is_empty());
        again.keep_draft();
        let mut third = ControllerUi { draft: Some(path.clone()), ..Default::default() };
        third.load_draft();
        assert!(third.learned.all().is_empty(), "the empty one was kept");

        std::fs::write(&path, "nonsense").unwrap();
        let mut broken = ControllerUi { draft: Some(path), ..Default::default() };
        broken.load_draft();
        assert!(broken.saved.lock().unwrap().as_ref().unwrap().contains("couldn't be read"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Shift + pad: the list goes to Shift, then the pad, and stays on the pad when
    /// Shift (then the pad) is let go.
    /// A 14-bit controller is set by hand, on any fine half: that message's row
    /// goes, and nothing pairs on its own.
    #[test]
    fn a_14_bit_controller_is_set_by_hand() {
        let mut t = ControllerUi::default();
        t.map_to((0xB6, 0x17), "deck_1_filter");
        t.map_to((0xB6, 0x37), "deck_1_filter");
        t.on_midi(msg(&[0xB6, 0x17, 64]));
        t.on_midi(msg(&[0xB6, 0x37, 3]));
        assert_eq!(t.learned.get((0xB6, 0x17)).unwrap().input, Input::Cc);
        assert!(t.keys().contains(&(0xB6, 0x37)), "not paired on its own");
        // Set by hand, on the unit's own fine half.
        let l = t.learned.get((0xB6, 0x17)).unwrap();
        t.learned.set((0xB6, 0x17), Learn { input: Input::Cc14(0x40), ..l });
        assert!(!t.keys().contains(&(0xB6, 0x40)));
        assert!(t.keys().contains(&(0xB6, 0x37)), "the +32 one isn't it");
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
}
