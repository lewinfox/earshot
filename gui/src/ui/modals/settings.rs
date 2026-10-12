//! The settings window: a tab each for audio, analysis and the keymap. What it changes
//! goes out as commands; the audio tab is [`super::audio`].

use std::path::Path;

use egui::{RichText, Ui};

use super::ModalEnv;
use super::audio::AudioUi;
use crate::core::command::{AppCmd, Command};
use crate::core::keymap::Origin;
use crate::core::view::SettingsTab;
use crate::ui::theme::THEME;

/// The window's title, which is also its id.
pub const TITLE: &str = "Settings";

const TABS: [(SettingsTab, &str); 3] = [
    (SettingsTab::Audio, "Audio"),
    (SettingsTab::Analysis, "Analysis"),
    (SettingsTab::Keymap, "Keymap"),
];

/// The analysis tab's facts, gathered by the shell.
pub struct AnalysisEnv<'a> {
    /// Background workers allowed at once now.
    pub workers: usize,
    /// The most there can be (the CPU cores).
    pub cores: usize,
    pub stale: &'a Stale,
}

/// What's known of the tracks with stale analysis (an older analyser, other audio,
/// failed or missing).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Stale {
    /// Not counted since the window opened: the analysis tab asks.
    #[default]
    Unknown,
    Counting,
    Count(usize),
    /// Queued again, this many.
    Queued(usize),
    Failed(String),
}

/// The keymap tab's facts.
pub struct KeymapEnv<'a> {
    /// Where the user's keymap file goes.
    pub path: &'a Path,
    pub origin: &'a Origin,
}

#[derive(Default)]
pub struct SettingsUi {
    pub tab: SettingsTab,
    audio: AudioUi,
}

impl SettingsUi {
    /// Opening the window: start the audio tab afresh.
    pub fn reset(&mut self) {
        self.audio.reset();
    }

    pub fn show(&mut self, ctx: &egui::Context, env: &ModalEnv, open: &mut bool) -> Vec<Command> {
        let mut out = vec![];
        egui::Window::new(TITLE)
            .open(open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.set_width(460.0);
                ui.horizontal(|ui| {
                    for (tab, name) in TABS {
                        if ui.selectable_label(self.tab == tab, name).clicked() {
                            self.tab = tab;
                        }
                    }
                });
                ui.separator();
                match self.tab {
                    SettingsTab::Audio => {
                        out.extend(
                            self.audio
                                .ui(ui, env.audio_settings, env.audio, env.routing),
                        )
                    }
                    SettingsTab::Analysis => out.extend(analysis(ui, &env.analysis)),
                    SettingsTab::Keymap => out.extend(keymap(ui, &env.keymap_file)),
                }
            });
        out
    }
}

fn note(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(11.0).color(THEME.muted));
}

fn analysis(ui: &mut Ui, env: &AnalysisEnv) -> Vec<Command> {
    let mut out = vec![];
    let mut n = env.workers;
    ui.horizontal(|ui| {
        ui.label("Background workers");
        ui.add(egui::Slider::new(&mut n, 1..=env.cores.max(1)));
    });
    note(
        ui,
        "How many tracks the library analyses at once. Each worker holds its own copy \
         of the analysis models, so more use more memory. While a deck plays in \
         Perform, only one runs. Applies at once.",
    );
    if n != env.workers {
        out.push(Command::App(AppCmd::BackgroundWorkers(n)));
    }
    ui.separator();
    let (count, n) = match env.stale {
        Stale::Unknown => {
            out.push(Command::App(AppCmd::CountStale));
            ("Counting…".to_string(), 0)
        }
        Stale::Counting => ("Counting…".to_string(), 0),
        Stale::Count(0) => ("Every track's analysis is up to date.".to_string(), 0),
        Stale::Count(n) => (format!("{n} track{} out of date.", plural(*n)), *n),
        Stale::Queued(n) => (format!("{n} track{} queued again.", plural(*n)), 0),
        Stale::Failed(e) => (format!("Couldn't count them: {e}"), 0),
    };
    ui.horizontal(|ui| {
        if ui
            .add_enabled(n > 0, egui::Button::new("Re-analyse stale tracks"))
            .on_hover_text("Analyse them again in the background, grids in the mode they have")
            .clicked()
        {
            out.push(Command::App(AppCmd::QueueStale));
        }
        ui.label(count);
    });
    note(
        ui,
        "Stale: analysed by an older version of earshot, the file's audio has changed \
         since, or the analysis failed or never ran.",
    );
    out
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn keymap(ui: &mut Ui, env: &KeymapEnv) -> Option<Command> {
    let mut cmd = None;
    let (status, colour) = match env.origin {
        Origin::BuiltIn => ("Using the built-in keymap.".to_string(), THEME.muted),
        Origin::File => ("Using your keymap file.".to_string(), THEME.ok),
        Origin::Broken(e) => (
            format!("Your keymap file didn't load, so the built-in one is in use:\n{e}"),
            THEME.bad,
        ),
    };
    ui.label(RichText::new(status).color(colour));
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("File");
        ui.add(
            egui::Label::new(RichText::new(env.path.display().to_string()).monospace())
                .selectable(true),
        );
    });
    ui.horizontal(|ui| {
        if ui
            .button("Reload")
            .on_hover_text("Read the file again (the built-in keymap if it isn't there)")
            .clicked()
        {
            cmd = Some(Command::App(AppCmd::ReloadKeymap));
        }
        let missing = *env.origin == Origin::BuiltIn;
        if ui
            .add_enabled(missing, egui::Button::new("Make a copy to edit"))
            .on_hover_text("Write the built-in keymap to the file, and use it")
            .on_disabled_hover_text("The file is already there")
            .clicked()
        {
            cmd = Some(Command::App(AppCmd::CopyKeymap));
        }
    });
    note(
        ui,
        "One binding per line; the built-in file's comments explain the format. \
         Delete the file to go back to the built-in keymap.",
    );
    cmd
}
