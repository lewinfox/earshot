//! The sampler's window: its 16 slots, the 8 on deck 1's pads beside the 8 on deck
//! 2's. Each slot has its sound (from a file, or the track selected in the library),
//! a button to play or stop it, its volume and its loop switch. What changes is saved
//! as it's changed.

use earshot_engine::{SAMPLER_SLOTS, SampleSlot, SampleSnap};
use egui::{RichText, Ui};

use crate::core::command::Command;
use crate::core::sampler::{Bank, MAX_SECS, PADS, SamplerCmd};
use crate::ui::theme::THEME;

/// The window's title, which is also its id.
pub const TITLE: &str = "Sampler";

/// What the window shows, gathered by the shell.
pub struct SamplerEnv<'a> {
    pub bank: &'a Bank,
    /// The slots as the engine has them (`None` while audio isn't running).
    pub slots: Option<&'a [SampleSnap; SAMPLER_SLOTS]>,
    /// Why each slot's sound couldn't be loaded, if it couldn't.
    pub errors: &'a [Option<String>; SAMPLER_SLOTS],
    /// A track is selected in the library (to put in a slot).
    pub selected: bool,
}

pub fn ui(ui: &mut Ui, env: &SamplerEnv, out: &mut Vec<Command>) {
    ui.label(
        RichText::new(format!(
            "With the pads in SAMPLER mode, a pad plays its slot and SHIFT + pad stops it. \
             Slots 1–8 are on deck 1's pads, 9–16 on deck 2's. A sound is kept whole in \
             memory: the first {} minutes of a longer file.",
            MAX_SECS / 60.0
        ))
        .color(THEME.muted),
    );
    ui.add_space(6.0);
    ui.columns(2, |cols| {
        for (side, ui) in cols.iter_mut().enumerate() {
            ui.label(RichText::new(format!("Deck {} pads", side + 1)).strong());
            egui::Grid::new(("sampler-slots", side))
                .num_columns(6)
                .show(ui, |ui| {
                    for pad in 0..PADS {
                        if let Some(slot) = SampleSlot::new(side * PADS + pad) {
                            row(ui, slot, env, out);
                            ui.end_row();
                        }
                    }
                });
        }
    });
}

/// One slot's row.
fn row(ui: &mut Ui, slot: SampleSlot, env: &SamplerEnv, out: &mut Vec<Command>) {
    let n = slot.index() + 1;
    let saved = env.bank.slot(slot);
    let snap = env.slots.map(|s| s[slot.index()]).unwrap_or_default();
    let error = env.errors[slot.index()].as_deref();
    let mut send = |c: SamplerCmd| out.push(Command::Sampler(c));

    // Play, or stop while it plays.
    let play = if snap.playing {
        format!("■ {n}")
    } else {
        format!("▶ {n}")
    };
    let tip = if snap.playing {
        format!("Stop slot {n}")
    } else {
        format!("Play slot {n} from its start")
    };
    let r = ui.add_enabled(
        snap.loaded,
        egui::Button::new(play).min_size(egui::vec2(44.0, 0.0)),
    );
    if r.on_hover_text(tip).clicked() {
        send(if snap.playing {
            SamplerCmd::Stop(slot)
        } else {
            SamplerCmd::Play(slot)
        });
    }

    // The sound: its name, or why there's none.
    let name = saved.name();
    let (text, colour, tip) = match (&name, error) {
        (None, _) => (
            "empty".to_string(),
            THEME.muted,
            "No sound in this slot".to_string(),
        ),
        (Some(name), Some(e)) => (
            format!("⚠ {name}"),
            THEME.bad,
            format!("Couldn't load it: {e}"),
        ),
        (Some(name), None) if !snap.loaded && env.slots.is_some() => {
            (format!("{name} …"), THEME.muted, "Loading".to_string())
        }
        (Some(name), None) => {
            let file = saved
                .file
                .as_ref()
                .map(|f| f.display().to_string())
                .unwrap_or_default();
            (
                name.clone(),
                THEME.text,
                format!("{file}\n{:.1} s", snap.duration),
            )
        }
    };
    ui.add_sized(
        [170.0, ui.spacing().interact_size.y],
        egui::Label::new(RichText::new(text).color(colour)).truncate(),
    )
    .on_hover_text(tip);

    if ui
        .button("File…")
        .on_hover_text(format!("Pick a sound file for slot {n}"))
        .clicked()
    {
        send(SamplerCmd::PickFile(slot));
    }
    if ui
        .add_enabled(env.selected, egui::Button::new("Selected"))
        .on_hover_text(format!("Put the track selected in the library in slot {n}"))
        .on_disabled_hover_text("Select a track in the library first")
        .clicked()
    {
        send(SamplerCmd::AssignSelected(slot));
    }

    let mut volume = saved.volume;
    let slider = egui::Slider::new(&mut volume, 0.0..=1.0)
        .show_value(false)
        .clamping(egui::SliderClamping::Always);
    ui.spacing_mut().slider_width = 70.0;
    if ui
        .add(slider)
        .on_hover_text(format!("Slot {n}'s volume: {:.0} %", saved.volume * 100.0))
        .changed()
    {
        send(SamplerCmd::Volume(slot, volume));
    }
    let mut looping = saved.looping;
    if ui
        .checkbox(&mut looping, "Loop")
        .on_hover_text("Play round and round until stopped, not once through")
        .changed()
    {
        send(SamplerCmd::Loop(slot, looping));
    }
    if ui
        .add_enabled(name.is_some(), egui::Button::new("Clear"))
        .on_hover_text(format!("Empty slot {n}"))
        .clicked()
    {
        send(SamplerCmd::Clear(slot));
    }
}
