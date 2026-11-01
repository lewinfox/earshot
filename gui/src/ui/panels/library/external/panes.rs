//! Each place music comes from, as an icon in the library's left stack and the pane
//! it opens: a DJ program's library (always listed: one not found has a button to
//! say where it is), another library added by hand, each USB stick while it's
//! plugged in, and Manage (the music folders, adding libraries).
//!
//! What a source can do shows only where it can: every library has its playlists
//! and Import; a stick also has its light, Eject, its files and Convert, and takes
//! playlists dropped on it.

use std::borrow::Cow;

use egui::{RichText, Ui};

use super::ExternalUi;
use crate::core::command::Command;
use crate::core::drives::Drive;
use crate::core::external::ExternalCmd;
use crate::core::sources::{Source, SourceKind};
use crate::core::view::Source as Pane;
use crate::ui::panels::CoreView;
use crate::ui::panels::export::short_kind;
use crate::ui::theme::{THEME, fs};
use crate::ui::widgets::StackItem;

/// What the middle shows for a source's pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Middle {
    /// The library's (or a playlist's) tracks.
    Tracks,
    /// The stick: what it is, its libraries and its files.
    Stick,
}

/// A library's icon: letters for each program.
fn icon(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::RekordboxDb | SourceKind::RekordboxUsb => "RB",
        SourceKind::RekordboxXml => "XML",
        SourceKind::Serato | SourceKind::SeratoUsb => "S",
        SourceKind::Mixxx => "MX",
        SourceKind::Folder => "📁",
    }
}

impl ExternalUi {
    /// The sources' icons, after earshot's own views: the programs, other libraries,
    /// the sticks, then Manage.
    pub fn stack_items(&self, v: &CoreView) -> Vec<StackItem<Pane>> {
        let mut out = vec![];
        for (i, (kind, source)) in self.entries().into_iter().enumerate() {
            let (id, tip) = match (&source, kind.is_program()) {
                (Some(s), true) => (Pane::Program(kind), format!("{}: {}", kind.label(), s.name)),
                (None, _) => (
                    Pane::Program(kind),
                    format!("{}: not found (click to say where it is)", kind.label()),
                ),
                (Some(s), false) => (Pane::Library(s.id), s.name.clone()),
            };
            out.push(StackItem {
                id,
                icon: Cow::Borrowed(icon(kind)),
                tip: Cow::Owned(tip),
                enabled: true,
                light: None,
                gap: i == 0,
            });
        }
        for (i, d) in self.drives.iter().enumerate() {
            let busy = self.stick_busy(v, d);
            out.push(StackItem {
                id: Pane::Stick(d.id()),
                icon: Cow::Borrowed("💾"),
                tip: Cow::Owned(format!(
                    "{}: {}",
                    d.label,
                    if busy {
                        "in use, don't pull it out"
                    } else {
                        "safe to remove"
                    }
                )),
                enabled: true,
                light: Some(if busy { THEME.bad } else { THEME.ok }),
                gap: i == 0,
            });
        }
        let mut manage = StackItem::new(
            Pane::Manage,
            "+",
            "Manage sources: music folders, other libraries",
        );
        manage.gap = true;
        out.push(manage);
        out
    }

    /// The source a pane shows is gone (a stick pulled out, a library removed).
    pub fn pane_gone(&self, pane: Pane) -> bool {
        match pane {
            Pane::Stick(id) => !self.drives.iter().any(|d| d.id() == id),
            Pane::Library(id) => !self.listed(id),
            _ => false,
        }
    }

    /// What the middle shows for `pane`, if it's one of ours and something is picked.
    pub fn middle(&self, pane: Pane) -> Option<Middle> {
        let open_in = |s: &Source| match pane {
            Pane::Program(k) => s.kind == k,
            Pane::Library(id) => s.id == id,
            Pane::Stick(id) => self.on_a_stick(s).is_some_and(|d| d.id() == id),
            _ => false,
        };
        if let Pane::Stick(id) = pane
            && let Some(d) = self.drives.iter().find(|d| d.id() == id)
            && self.stick.open.as_ref() == Some(&d.path)
        {
            return Some(Middle::Stick);
        }
        let (open, _) = self.open.as_ref()?;
        self.source(*open)
            .is_some_and(|s| open_in(s))
            .then_some(Middle::Tracks)
    }

    /// A pane: the source's name, then what it has.
    pub fn pane(&mut self, ui: &mut Ui, pane: Pane, v: &CoreView, out: &mut Vec<Command>) {
        if !self.searched {
            self.searched = true;
            out.push(ExternalCmd::Find.into());
        }
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| match pane {
                Pane::Program(kind) => self.program_pane(ui, kind, out),
                Pane::Library(id) => {
                    if let Some(s) = self.source(id).cloned() {
                        title(ui, &s.name);
                        self.library(ui, &s, out);
                    }
                }
                Pane::Stick(id) => {
                    if let Some(d) = self.drives.iter().find(|d| d.id() == id).cloned() {
                        self.stick_pane(ui, &d, v, out);
                    }
                }
                _ => {}
            });
        self.status_box(ui, out);
    }

    fn program_pane(&mut self, ui: &mut Ui, kind: SourceKind, out: &mut Vec<Command>) {
        title(ui, kind.label());
        let found = self.entries().into_iter().find(|(k, _)| *k == kind);
        match found {
            Some((_, Some(source))) => self.library(ui, &source, out),
            _ => {
                muted(
                    ui,
                    format!(
                        "No {} library found in its usual place. If it's somewhere else \
                         (another drive, Wine, a container), say where.",
                        kind.label()
                    ),
                );
                ui.add_space(4.0);
                if ui.button("+ Add library location").clicked() {
                    out.push(ExternalCmd::SetLocation { kind }.into());
                }
                if ui
                    .button("Look again")
                    .on_hover_text("Look in each program's usual place")
                    .clicked()
                {
                    out.push(ExternalCmd::Find.into());
                }
                if let Some(e) = &self.find_error {
                    ui.label(RichText::new(e).size(fs(11.0)).color(THEME.bad));
                }
            }
        }
    }

    /// A stick's pane: its light and Eject, then each library on it. Picking it
    /// shows the stick in the middle; playlists dropped anywhere on the pane go
    /// onto it.
    fn stick_pane(&mut self, ui: &mut Ui, d: &Drive, v: &CoreView, out: &mut Vec<Command>) {
        // The stick in the middle, unless one of its playlists is.
        let showing_one = self
            .open
            .as_ref()
            .and_then(|(id, _)| self.source(*id))
            .is_some_and(|s| self.on_a_stick(s).is_some_and(|x| x.path == d.path));
        if !showing_one && self.stick.open.as_ref() != Some(&d.path) {
            self.stick.open(d);
        }
        ui.horizontal(|ui| {
            self.light(ui, v, d);
            let selected = self.stick.open.as_ref() == Some(&d.path);
            if ui
                .selectable_label(selected, RichText::new(&d.label).strong().size(fs(14.0)))
                .on_hover_text("What's on it, and its files")
                .clicked()
            {
                self.stick.open(d);
                self.open = None;
            }
            self.eject_button(ui, v, d, out);
        });
        if let Some(e) = &self.stick.eject_error {
            ui.label(
                RichText::new(format!("Couldn't eject: {e}"))
                    .size(fs(11.0))
                    .color(THEME.bad),
            );
        }
        ui.add_space(4.0);
        let libraries = self.stick_libraries(d);
        if libraries.is_empty() {
            muted(ui, "No DJ library on it. Drop a playlist here to make one.");
        }
        for s in libraries {
            ui.label(RichText::new(short_kind(s.kind)).color(THEME.muted));
            self.library(ui, &s, out);
            ui.add_space(6.0);
        }
        self.stick_drop(ui, ui.max_rect(), v, d);
    }

    /// Managing sources, after the music folders (drawn by the library pane):
    /// adding another library or a copy of a stick.
    pub fn manage(&mut self, ui: &mut Ui, out: &mut Vec<Command>) {
        if let Some(e) = &self.find_error {
            ui.label(RichText::new(e).size(fs(11.0)).color(THEME.bad));
        }
        ui.horizontal(|ui| {
            if ui
                .button("+ Add a library…")
                .on_hover_text("A Rekordbox XML, or a folder holding a stick's library")
                .clicked()
            {
                out.push(ExternalCmd::Pick.into());
            }
            if ui
                .button("Look again")
                .on_hover_text("Look for libraries in each program's usual place")
                .clicked()
            {
                out.push(ExternalCmd::Find.into());
            }
        });
        muted(
            ui,
            "Rekordbox, Serato and Mixxx have their own icons, and so does each USB stick \
             while it's plugged in.",
        );
    }

    /// The stick is in use (red).
    pub(super) fn stick_busy(&self, v: &CoreView, d: &Drive) -> bool {
        v.uses.busy(&d.path) || self.stick.ejecting.as_ref() == Some(&d.path)
    }
}

fn title(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).strong().size(fs(14.0)));
    ui.add_space(2.0);
}

fn muted(ui: &mut Ui, text: impl Into<String>) {
    ui.label(RichText::new(text.into()).size(fs(11.0)).color(THEME.muted));
}
