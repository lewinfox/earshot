//! USB sticks in the Sources pane: one entry per stick plugged in, with a light
//! (green: safe to pull out; red: earshot is using it) and an eject button, and the
//! DJ libraries on it below. Clicking a stick shows it in the middle: what it is
//! and what's on it, its libraries (import, convert to the other format) and its
//! files, to copy into earshot.
//!
//! Drops: a playlist (earshot's, or another library's) on a stick adds it there,
//! in the stick's format, after a window that says what will happen; another
//! library's playlist on Playlists or Crates imports it (`import_drop`).

use std::collections::BTreeSet;
use std::io;
use std::path::PathBuf;

use egui::{Rect, RichText, Stroke, Ui};

use super::{Dialog, ExternalUi, Status};
use crate::core::collections::CollectionId;
use crate::core::command::Command;
use crate::core::drives::{self, Copied, Drive, Entry};
use crate::core::export::{
    Export, ExportCmd, ExportEvent, ExportOptions, From, Target as ExportTarget, TargetKind,
    Written, picked,
};
use crate::core::external::{ExternalCmd, ExternalEvent, ImportOptions, NodePath, Target};
use crate::core::sources::Source;
use crate::ui::panels::CoreView;
use crate::ui::panels::export::short_kind;
use crate::ui::panels::library::drag::{self, DragPayload};
use crate::ui::theme::{THEME, fs};

/// What was dropped on a stick.
#[derive(Clone, Debug)]
enum Dropped {
    Earshot(CollectionId),
    Library {
        source: Source,
        path: NodePath,
        name: String,
    },
}

/// The add-to-stick window, open.
struct ToStick {
    drive: Drive,
    what: Dropped,
    /// The format to write (the stick's, or chosen when it has none or both).
    kind: TargetKind,
}

#[derive(Default)]
pub(super) struct StickUi {
    /// The stick the middle shows.
    pub(super) open: Option<PathBuf>,
    /// The folder its file browser is in, and what's in it.
    dir: PathBuf,
    listing: Option<(PathBuf, io::Result<Vec<Entry>>)>,
    /// Files and folders ticked, to copy.
    ticked: BTreeSet<PathBuf>,
    /// The music folder copies go into.
    copy_to: String,
    copying: Option<String>,
    copy_report: Option<Result<Copied, String>>,
    /// A stick being ejected, and why the last eject failed.
    pub(super) ejecting: Option<PathBuf>,
    pub(super) eject_error: Option<String>,
    /// A drop being written to a stick, and how the last one went.
    sending: Option<(PathBuf, String)>,
    send_report: Option<(PathBuf, Result<Written, String>)>,
    to_stick: Option<ToStick>,
}

impl StickUi {
    pub(super) fn drives_changed(&mut self, drives: &[Drive]) {
        let there = |p: &PathBuf| drives.iter().any(|d| &d.path == p);
        if self.open.as_ref().is_some_and(|p| !there(p)) {
            self.open = None;
            self.listing = None;
            self.ticked.clear();
        }
        if self.ejecting.as_ref().is_some_and(|p| !there(p)) {
            self.ejecting = None;
        }
    }

    pub(super) fn event(&mut self, e: ExternalEvent) {
        match e {
            ExternalEvent::CopyProgress(m) => self.copying = Some(m),
            ExternalEvent::Copied(r) => {
                self.copying = None;
                self.ticked.clear();
                self.copy_report = Some(r);
            }
            // Ejected: it leaves the drive list, which clears `ejecting`.
            ExternalEvent::Ejected { drive, result } => {
                if let Err(e) = result
                    && self.ejecting.as_ref() == Some(&drive)
                {
                    self.ejecting = None;
                    self.eject_error = Some(e);
                }
            }
            _ => {}
        }
    }

    pub(super) fn open(&mut self, d: &Drive) {
        if self.open.as_ref() != Some(&d.path) {
            self.open = Some(d.path.clone());
            self.dir = d.path.clone();
            self.listing = None;
            self.ticked.clear();
        }
    }
}

impl ExternalUi {
    /// The libraries on a stick, as sources.
    pub(super) fn stick_libraries(&self, d: &Drive) -> Vec<Source> {
        self.sources
            .iter()
            .filter(|s| s.enabled && s.kind.on_stick())
            .filter(|s| s.location.as_deref().is_some_and(|at| d.holds(at)))
            .cloned()
            .collect()
    }

    /// Green: nothing of earshot's is using the stick (and what it wrote is on it).
    pub(super) fn light(&self, ui: &mut Ui, v: &CoreView, d: &Drive) -> bool {
        let busy = self.stick_busy(v, d);
        let (colour, tip) = if busy {
            (THEME.bad, "In use: don't pull it out yet")
        } else {
            (THEME.ok, "Safe to remove")
        };
        let size = fs(9.0);
        let (rect, r) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
        ui.painter()
            .circle_filled(rect.center(), size / 2.0, colour);
        r.on_hover_text(tip);
        busy
    }

    pub(super) fn eject_button(
        &mut self,
        ui: &mut Ui,
        v: &CoreView,
        d: &Drive,
        out: &mut Vec<Command>,
    ) {
        if self.stick.ejecting.as_ref() == Some(&d.path) {
            ui.spinner();
            return;
        }
        let busy = v.uses.busy(&d.path);
        if ui
            .add_enabled(!busy, egui::Button::new("⏏").small())
            .on_hover_text("Eject")
            .on_disabled_hover_text("earshot is using it")
            .clicked()
        {
            self.stick.ejecting = Some(d.path.clone());
            self.stick.eject_error = None;
            out.push(ExternalCmd::Eject(d.clone()).into());
        }
    }

    // ---- the middle ---------------------------------------------------------------

    /// The stick picked: what it is, its libraries and its files.
    pub fn stick_view(&mut self, ui: &mut Ui, v: &CoreView, out: &mut Vec<Command>) {
        let Some(d) = self
            .stick
            .open
            .as_ref()
            .and_then(|p| self.drives.iter().find(|d| &d.path == p))
            .cloned()
        else {
            return;
        };
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("💾 {}", d.label))
                    .strong()
                    .size(fs(15.0)),
            );
            let busy = self.light(ui, v, &d);
            ui.label(
                RichText::new(if busy { "In use" } else { "Safe to remove" })
                    .size(fs(11.0))
                    .color(THEME.muted),
            );
            self.eject_button(ui, v, &d, out);
        });
        let gb = |b: u64| b as f64 / 1e9;
        muted(
            ui,
            format!(
                "{} · {} · {:.1} GB free of {:.1} GB{}",
                d.path.display(),
                if d.fstype.is_empty() { "?" } else { &d.fstype },
                gb(d.free_bytes),
                gb(d.total_bytes),
                if d.writable { "" } else { " · read-only" }
            ),
        );
        if let Some(n) = d.notes() {
            muted(ui, n);
        }
        ui.add_space(6.0);

        let libraries = self.stick_libraries(&d);
        if libraries.is_empty() {
            muted(
                ui,
                "No DJ library on it. Drop playlists on the stick's pane (left) to \
                 make one, or use Prepare › Export.",
            );
        }
        for s in &libraries {
            self.read_once(s, out);
            ui.horizontal(|ui| {
                ui.label(RichText::new(short_kind(s.kind)).strong());
                let info = match self.status.get(&s.id) {
                    Some(Status::Ready(lib)) => {
                        let lists: usize = lib.playlists.iter().map(count).sum();
                        format!("{} tracks, {lists} playlists", lib.tracks.len())
                    }
                    Some(Status::Reading(_)) => "reading…".into(),
                    Some(Status::Failed(e)) => e.clone(),
                    None => String::new(),
                };
                muted(ui, info);
                if ui.button("Show tracks").clicked() {
                    self.show(s, None, out);
                }
                if ui
                    .add_enabled(self.importing.is_none(), egui::Button::new("Import all…"))
                    .on_hover_text("Its tracks, copied into a music folder, with its playlists")
                    .clicked()
                {
                    self.open_import(s, Target::Library);
                }
                if let Some(to) = TargetKind::converting(s.kind)
                    && ui
                        .button(format!(
                            "Convert to {}…",
                            short_kind(to.stick_kind().unwrap())
                        ))
                        .on_hover_text("Opens Prepare › Export, set to convert the whole stick")
                        .clicked()
                {
                    out.push(ExportCmd::Convert(s.clone()).into());
                }
            });
        }
        if let Some((at, m)) = &self.stick.sending
            && at == &d.path
        {
            ui.horizontal(|ui| {
                ui.spinner();
                muted(
                    ui,
                    if m.is_empty() {
                        "Writing to the stick…"
                    } else {
                        m
                    },
                );
            });
        }
        if let Some((at, r)) = &self.stick.send_report
            && at == &d.path
        {
            match r {
                Ok(w) => {
                    ui.label(
                        RichText::new(format!(
                            "Added {} tracks in {} playlists; the stick has {} tracks now.",
                            w.tracks, w.playlists, w.on_target
                        ))
                        .color(THEME.ok),
                    );
                    for line in &w.warnings {
                        muted(ui, line);
                    }
                }
                Err(e) => {
                    ui.label(
                        RichText::new(format!("Writing to the stick failed: {e}")).color(THEME.bad),
                    );
                }
            }
        }
        self.status_box(ui, out);
        ui.separator();
        self.files(ui, &d, out);
    }

    /// The stick's files: a folder at a time, ticked to copy into earshot.
    fn files(&mut self, ui: &mut Ui, d: &Drive, out: &mut Vec<Command>) {
        let st = &mut self.stick;
        if st.listing.as_ref().is_none_or(|(at, _)| at != &st.dir) {
            st.listing = Some((st.dir.clone(), drives::read_dir(&st.dir)));
        }
        // Where we are: the stick, then each folder down, each a button back up.
        ui.horizontal_wrapped(|ui| {
            let mut at = d.path.clone();
            if ui.small_button(&d.label).clicked() {
                st.dir = d.path.clone();
            }
            if let Ok(rel) = st.dir.clone().strip_prefix(&d.path) {
                for c in rel.components() {
                    at.push(c);
                    ui.label("›");
                    if ui.small_button(c.as_os_str().to_string_lossy()).clicked() {
                        st.dir = at.clone();
                    }
                }
            }
        });
        ui.horizontal(|ui| {
            let n = st.ticked.len();
            if !self.music.contains(&st.copy_to) {
                st.copy_to = self.music.first().cloned().unwrap_or_default();
            }
            let can = n > 0 && st.copying.is_none() && !self.music.is_empty();
            if ui
                .add_enabled(can, egui::Button::new(format!("Copy {n} to earshot")))
                .on_hover_text(
                    "Into the music folder, in a folder named after the stick. Audio \
                     earshot has already isn't copied again.",
                )
                .on_disabled_hover_text(if self.music.is_empty() {
                    "Add a music folder first (Sources › Folders)"
                } else {
                    "Tick files or folders to copy"
                })
                .clicked()
            {
                st.copying = Some(String::new());
                st.copy_report = None;
                out.push(
                    ExternalCmd::CopyIn {
                        files: st.ticked.iter().cloned().collect(),
                        drive: d.clone(),
                        to: PathBuf::from(&st.copy_to),
                    }
                    .into(),
                );
            }
            if self.music.len() > 1 {
                muted(ui, "into");
                egui::ComboBox::from_id_salt("stick-copy-to")
                    .selected_text(st.copy_to.as_str())
                    .show_ui(ui, |ui| {
                        for m in &self.music {
                            ui.selectable_value(&mut st.copy_to, m.clone(), m);
                        }
                    });
            }
            if let Some(m) = &st.copying {
                ui.spinner();
                muted(ui, m);
            }
        });
        match &st.copy_report {
            Some(Ok(c)) => {
                ui.label(RichText::new(copied_text(c)).color(THEME.ok));
                for f in c.failed.iter().take(5) {
                    muted(ui, f);
                }
            }
            Some(Err(e)) => {
                ui.label(RichText::new(e).color(THEME.bad));
            }
            None => {}
        }
        let entries = match &st.listing {
            Some((_, Ok(e))) => e.clone(),
            Some((_, Err(e))) => {
                ui.label(RichText::new(e.to_string()).color(THEME.bad));
                return;
            }
            None => return,
        };
        egui::ScrollArea::vertical()
            .id_salt("stick-files")
            .auto_shrink(false)
            .show(ui, |ui| {
                if entries.is_empty() {
                    muted(ui, "Empty");
                }
                for e in &entries {
                    ui.horizontal(|ui| {
                        let mut on = st.ticked.contains(&e.path);
                        let enabled = e.dir || e.audio;
                        if ui
                            .add_enabled(enabled, egui::Checkbox::without_text(&mut on))
                            .changed()
                        {
                            if on {
                                st.ticked.insert(e.path.clone());
                            } else {
                                st.ticked.remove(&e.path);
                            }
                        }
                        if e.dir {
                            if ui.link(format!("📁 {}", e.name)).clicked() {
                                st.dir = e.path.clone();
                            }
                        } else {
                            let text = RichText::new(&e.name);
                            ui.label(if e.audio {
                                text
                            } else {
                                text.color(THEME.muted)
                            });
                            muted(ui, size(e.size));
                        }
                    });
                }
            });
    }

    // ---- drops ----------------------------------------------------------------------

    /// Open the import window for a library or one of its nodes.
    pub(super) fn open_import(&mut self, source: &Source, target: Target) {
        self.dialog = Some(Dialog {
            source: source.clone(),
            target,
            options: ImportOptions {
                memory_cues: source.kind.has_memory_cues(),
                ..ImportOptions::default()
            },
            copy_to: source
                .kind
                .on_stick()
                .then(|| self.music.first().cloned().unwrap_or_default()),
        });
    }

    /// Another library's playlist dropped in `rect` (the Playlists or Crates pane):
    /// the import window, for it.
    pub fn import_drop(&mut self, ui: &Ui, rect: Rect) {
        let Some((source, path)) = dropped_external(ui, rect) else {
            return;
        };
        if let Some(s) = self.sources.iter().find(|s| s.id == source).cloned() {
            let target = if path.is_empty() {
                Target::Library
            } else {
                Target::Node(path)
            };
            self.open_import(&s, target);
        }
    }

    /// A playlist dropped on a stick's entry: the add-to-stick window.
    pub(super) fn stick_drop(&mut self, ui: &Ui, rect: Rect, v: &CoreView, d: &Drive) {
        if !ui.rect_contains_pointer(rect) {
            return;
        }
        let Some(p) = drag::payload(ui.ctx()) else {
            return;
        };
        let what = match p.as_ref() {
            DragPayload::Collection(id) => Dropped::Earshot(*id),
            DragPayload::External { source, path, name } => {
                let Some(s) = self.sources.iter().find(|s| s.id == *source) else {
                    return;
                };
                // A stick's own playlist, back onto it: nothing to do.
                if s.location.as_deref().is_some_and(|at| d.holds(at)) {
                    return;
                }
                Dropped::Library {
                    source: s.clone(),
                    path: path.clone(),
                    name: name.clone(),
                }
            }
            DragPayload::Tracks { .. } => return,
        };
        if !d.writable {
            return;
        }
        ui.painter().rect_stroke(
            rect,
            3,
            Stroke::new(1.0_f32, THEME.accent),
            egui::StrokeKind::Inside,
        );
        if !ui.input(|i| i.pointer.any_released()) {
            return;
        }
        egui::DragAndDrop::clear_payload(ui.ctx());
        if let Dropped::Earshot(id) = &what
            && v.collections.get(*id).is_none()
        {
            return;
        }
        let kind = d
            .libraries
            .first()
            .and_then(|&k| TargetKind::for_stick(k))
            .unwrap_or(TargetKind::RekordboxUsb);
        self.stick.to_stick = Some(ToStick {
            drive: d.clone(),
            what,
            kind,
        });
    }

    /// The add-to-stick window: what will be written, in which format, and go.
    pub(super) fn to_stick_window(
        &mut self,
        ctx: &egui::Context,
        v: &CoreView,
        out: &mut Vec<Command>,
    ) {
        let Some(t) = &mut self.stick.to_stick else {
            return;
        };
        let (name, from_kind) = match &t.what {
            Dropped::Earshot(id) => (
                v.collections
                    .get(*id)
                    .map_or("?".into(), |c| c.name.clone()),
                None,
            ),
            Dropped::Library { source, name, .. } => (name.clone(), Some(source.kind)),
        };
        let busy = self.stick.sending.is_some();
        let (mut go, mut close, mut open) = (false, false, true);
        egui::Window::new(format!("Add to {}", t.drive.label))
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!("\"{name}\""));
                let choices: Vec<TargetKind> = match t.drive.libraries.as_slice() {
                    [one] => TargetKind::for_stick(*one).into_iter().collect(),
                    _ => vec![TargetKind::RekordboxUsb, TargetKind::SeratoUsb],
                };
                if choices.len() > 1 {
                    ui.horizontal(|ui| {
                        ui.label("As:");
                        for k in choices {
                            ui.radio_value(&mut t.kind, k, short_kind(k.stick_kind().unwrap()));
                        }
                    });
                }
                let to = short_kind(t.kind.stick_kind().unwrap());
                let text = match from_kind.map(short_kind) {
                    Some(from) if from != to => format!(
                        "This is a {from} playlist: it will be converted to {to}'s format \
                         (cues, loops and beat grids included)."
                    ),
                    Some(_) => format!("Added to the stick's {to} library."),
                    None => {
                        format!("earshot's tracks, cues and beat grids, written in {to}'s format.")
                    }
                };
                muted(ui, text);
                muted(
                    ui,
                    "Tracks not on the stick are copied onto it. Its other playlists stay; \
                     one of the same name is replaced.",
                );
                if t.drive.libraries.is_empty() {
                    muted(
                        ui,
                        format!("The stick has no DJ library yet: this makes a {to} one."),
                    );
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    go = ui
                        .add_enabled(!busy, egui::Button::new("Add"))
                        .on_disabled_hover_text("Something is being written already")
                        .clicked();
                    close = ui.button("Cancel").clicked();
                });
            });
        if go {
            let t = self.stick.to_stick.take().expect("open");
            let from = match t.what {
                Dropped::Earshot(id) => From::Earshot(picked(v.collections, &[id])),
                Dropped::Library { source, path, .. } => From::Library {
                    source,
                    nodes: (!path.is_empty()).then(|| vec![path]),
                },
            };
            self.stick.sending = Some((t.drive.path.clone(), String::new()));
            self.stick.send_report = None;
            out.push(
                ExportCmd::Run(Box::new(Export {
                    from,
                    target: ExportTarget {
                        kind: t.kind,
                        path: t.drive.path.clone(),
                        volume_root: None,
                    },
                    options: ExportOptions::for_kind(t.kind),
                    replace: false,
                    merge: true,
                }))
                .into(),
            );
        } else if close || !open {
            self.stick.to_stick = None;
        }
    }

    /// News of a write: the stick view shows its own (a drop), not the Export tab's.
    pub fn export_event(&mut self, e: &ExportEvent) {
        let Some((at, m)) = &mut self.stick.sending else {
            return;
        };
        match e {
            ExportEvent::Progress(p) => *m = p.clone(),
            ExportEvent::Done(r) => {
                let at = at.clone();
                // The stick's library changed: read it again when it's next shown.
                let on_it: Vec<_> = self
                    .sources
                    .iter()
                    .filter(|s| s.location.as_deref().is_some_and(|l| l.starts_with(&at)))
                    .map(|s| s.id)
                    .collect();
                for id in on_it {
                    self.status.remove(&id);
                }
                self.stick.sending = None;
                self.stick.send_report = Some((at, r.clone()));
            }
            ExportEvent::Picked(_) => {}
        }
    }
}

/// Another library's node being dropped in `rect` now: (its source, its path).
fn dropped_external(ui: &Ui, rect: Rect) -> Option<(crate::core::sources::SourceId, NodePath)> {
    if !ui.rect_contains_pointer(rect) {
        return None;
    }
    let p = drag::payload(ui.ctx())?;
    let DragPayload::External { source, path, .. } = p.as_ref() else {
        return None;
    };
    ui.painter().rect_stroke(
        rect.shrink(2.0),
        3,
        Stroke::new(1.0_f32, THEME.accent),
        egui::StrokeKind::Inside,
    );
    if !ui.input(|i| i.pointer.any_released()) {
        return None;
    }
    egui::DragAndDrop::clear_payload(ui.ctx());
    Some((*source, path.clone()))
}

/// Playlists and crates in a node, itself included.
fn count(n: &crate::core::external::Node) -> usize {
    match n.kind {
        crate::core::external::NodeKind::Folder => n.children.iter().map(count).sum(),
        _ => 1,
    }
}

fn muted(ui: &mut Ui, text: impl Into<String>) {
    ui.label(RichText::new(text.into()).size(fs(11.0)).color(THEME.muted));
}

fn size(bytes: u64) -> String {
    match bytes {
        b if b >= 1_000_000_000 => format!("{:.1} GB", b as f64 / 1e9),
        b if b >= 1_000_000 => format!("{:.1} MB", b as f64 / 1e6),
        b => format!("{} KB", b.div_ceil(1000)),
    }
}

/// "Copied 12 tracks; 3 earshot had already."
pub(super) fn copied_text(c: &Copied) -> String {
    let mut s = format!("Copied {} tracks", c.copied);
    if c.already > 0 {
        s += &format!("; {} earshot had already", c.already);
    }
    if !c.failed.is_empty() {
        s += &format!("; {} couldn't be copied", c.failed.len());
    }
    s + "."
}
