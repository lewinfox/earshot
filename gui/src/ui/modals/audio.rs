//! The settings window's Audio tab: output device, buffer size, and which device
//! channels the master and headphone buses go to (e.g. a controller's 1/2 and 3/4, or
//! a laptop's one pair split left/right for cueing).

use std::sync::{Arc, Mutex};

use earshot_engine::output::can_split;
use earshot_engine::{Out, OutputInfo, Routing};
use egui::{RichText, Ui};

use crate::core::command::{AudioCmd, Command};
use crate::core::controller_audio::ControllerAudio;
use crate::core::outputs::{self, Kind, Output};
use crate::core::player::{AudioSettings, DEFAULT_BUFFER};
use crate::ui::theme::THEME;
use crate::ui::widgets::{Button, muted};

/// An output picker's width, when there's room.
const PICK_W: f32 = 330.0;

const BUFFERS: [u32; 6] = [64, 128, 256, 512, 1024, 2048];

#[derive(Default)]
pub struct AudioUi {
    /// Filled by a background thread (listing devices can take a moment).
    devices: Arc<Mutex<Option<Vec<Output>>>>,
    listed: bool,
    /// Device and buffer as edited, applied with the Apply button.
    draft: Option<AudioSettings>,
}

impl AudioUi {
    /// Opening the settings: list the devices again and drop an old draft.
    pub fn reset(&mut self) {
        self.listed = false;
        self.draft = None;
    }

    fn list(&mut self, ctx: &egui::Context, controller: Option<&ControllerAudio>) {
        self.listed = true;
        *self.devices.lock().unwrap() = None;
        let (out, ctx) = (self.devices.clone(), ctx.clone());
        let card = controller.map(|c| c.card);
        std::thread::spawn(move || {
            *out.lock().unwrap() = Some(outputs::list(card));
            ctx.request_repaint();
        });
    }

    pub fn ui(
        &mut self,
        ui: &mut Ui,
        saved: &AudioSettings,
        info: &Result<OutputInfo, String>,
        controller: Option<&ControllerAudio>,
        routing: Routing,
    ) -> Option<Command> {
        if !self.listed {
            self.list(ui.ctx(), controller);
        }
        let mut action = self.device_section(ui, saved, info, controller);
        ui.separator();
        let (channels, phones) = match info {
            Ok(i) => (i.channels, i.phones.as_ref().map(|p| p.1)),
            Err(_) => (2, None),
        };
        if let Some(r) = routing_section(ui, routing, channels, phones) {
            action = Some(Command::Audio(AudioCmd::Route(r)));
        }
        action
    }

    fn device_section(
        &mut self,
        ui: &mut Ui,
        saved: &AudioSettings,
        info: &Result<OutputInfo, String>,
        controller: Option<&ControllerAudio>,
    ) -> Option<Command> {
        let mut action = None;
        let devices = self.devices.lock().unwrap().clone();
        match info {
            Ok(i) => ui.label(
                RichText::new(format!(
                    "Now: {} · {} kHz · {} channels · {} frames",
                    now_name(&i.device, controller, &devices),
                    i.sample_rate / 1000,
                    i.channels,
                    i.buffer_frames.map_or("default".into(), |b| b.to_string())
                ))
                .color(if i.null { THEME.warn } else { THEME.muted }),
            ),
            Err(e) => ui.label(RichText::new(format!("No audio: {e}")).color(THEME.bad)),
        };
        if let Ok(i) = info {
            if let Some((name, ch)) = &i.phones {
                ui.label(
                    RichText::new(format!("Headphones: {name} · {ch} channels")).color(THEME.muted),
                );
            }
            if let Some(e) = &i.phones_error {
                ui.label(
                    RichText::new(format!("Headphones are on the master's device: {e}"))
                        .color(THEME.warn),
                );
            }
        }
        if std::env::var_os("EARSHOT_AUDIO_DEVICE").is_some()
            || std::env::var_os("EARSHOT_BUFFER").is_some()
        {
            ui.label(
                RichText::new("EARSHOT_AUDIO_DEVICE / EARSHOT_BUFFER are set and win over these.")
                    .size(11.0)
                    .color(THEME.warn),
            );
        }
        let draft = self.draft.get_or_insert_with(|| saved.clone());
        let on_controller = controller.is_some() && draft.use_controller;
        egui::Grid::new("audio-device")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Output");
                ui.horizontal(|ui| {
                    output_pick(ui, draft, &devices, controller);
                    if ui.add(Button::subtle("Refresh").small()).clicked() {
                        self.listed = false;
                    }
                });
                ui.end_row();
                if !on_controller
                    && let Some(o) = picked(&draft.device, &devices)
                    && o.kind == Kind::Bluetooth
                {
                    ui.label("");
                    ui.label(
                        RichText::new(
                            "Bluetooth adds about a fifth of a second of delay: fine for \
                             listening, too late for mixing.",
                        )
                        .size(11.0)
                        .color(THEME.warn),
                    );
                    ui.end_row();
                }
                // The controller has its own headphone socket.
                if can_split() && !on_controller {
                    ui.label("Headphones");
                    device_pick(
                        ui,
                        "audio-phones-pick",
                        &mut draft.phones_device,
                        &devices,
                        "Same device as master",
                    )
                    .on_hover_text(
                        "Put the headphones on a device of their own, e.g. the laptop's \
                         speakers for the master and a USB dongle for the headphones",
                    );
                    ui.end_row();
                }
                ui.label("Buffer");
                egui::ComboBox::from_id_salt("audio-buffer")
                    .selected_text(
                        draft
                            .buffer_frames
                            .map_or(format!("{DEFAULT_BUFFER} frames"), |b| {
                                format!("{b} frames")
                            }),
                    )
                    .show_ui(ui, |ui| {
                        for b in BUFFERS {
                            ui.selectable_value(
                                &mut draft.buffer_frames,
                                Some(b),
                                format!("{b} frames"),
                            );
                        }
                    });
                ui.end_row();
            });
        let changed = draft.device != saved.device
            || draft.use_controller != saved.use_controller
            || draft.buffer_frames != saved.buffer_frames
            || draft.phones_device != saved.phones_device;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(changed, Button::primary("Apply"))
                .on_hover_text("Restart audio on this device. Loaded tracks stay loaded.")
                .clicked()
            {
                action = Some(Command::Audio(AudioCmd::Restart(AudioSettings {
                    routing: saved.routing,
                    ..draft.clone()
                })));
            }
            muted(ui, "Smaller buffers respond faster but may crackle.");
        });
        action
    }
}

/// The output chosen in `device`, if it's one there is now.
fn picked<'a>(device: &Option<String>, devices: &'a Option<Vec<Output>>) -> Option<&'a Output> {
    let id = device.as_ref()?;
    devices.as_ref()?.iter().find(|d| &d.id == id)
}

fn label(o: &Output) -> String {
    o.shown()
}

/// Its hardware and channels, for hovering over it.
fn hover(o: &Output) -> String {
    let channels = format!("{} channels", o.channels);
    match &o.detail {
        Some(d) => format!("{d} · {channels}"),
        None => channels,
    }
}

/// "Same as the system", and which output that is now.
fn system_label(devices: &Option<Vec<Output>>) -> String {
    match devices
        .as_ref()
        .and_then(|l| l.iter().find(|d| d.is_default))
    {
        Some(d) => format!("Same as the system (now: {})", label(d)),
        None => "Same as the system".into(),
    }
}

/// What the engine says it's playing to, by the name it has in the list.
fn now_name(
    device: &str,
    controller: Option<&ControllerAudio>,
    devices: &Option<Vec<Output>>,
) -> String {
    if let Some(c) = controller
        && device.starts_with(&c.name)
    {
        return c.name.clone();
    }
    devices
        .as_ref()
        .and_then(|l| l.iter().find(|d| d.name == device))
        .map_or_else(|| device.to_string(), label)
}

/// Where the master goes: the system's output, the controller's sound card (by its
/// own name) or one of the outputs. Choosing other than the controller with one
/// connected turns [`AudioSettings::use_controller`] off.
fn output_pick(
    ui: &mut Ui,
    draft: &mut AudioSettings,
    devices: &Option<Vec<Output>>,
    controller: Option<&ControllerAudio>,
) {
    let on_controller = controller.is_some() && draft.use_controller;
    let current = match (controller, on_controller) {
        (Some(c), true) => c.name.clone(),
        _ => match (&draft.device, devices) {
            (None, _) => system_label(devices),
            (Some(id), _) => picked(&draft.device, devices).map_or(id.clone(), label),
        },
    };
    egui::ComboBox::from_id_salt("audio-device-pick")
        // (No wider than the window has room for.)
        .width(PICK_W.min(ui.available_width()))
        .selected_text(current)
        .show_ui(ui, |ui| {
            if let Some(c) = controller
                && ui
                    .selectable_label(on_controller, &c.name)
                    .on_hover_text(
                        "Master on its master outputs, headphones on its headphone socket",
                    )
                    .clicked()
            {
                draft.use_controller = true;
            }
            if ui
                .selectable_label(
                    !on_controller && draft.device.is_none(),
                    system_label(devices),
                )
                .on_hover_text("Whatever the system plays to: it follows the system's choice")
                .clicked()
            {
                draft.device = None;
                draft.use_controller = false;
            }
            match devices {
                None => {
                    ui.label(RichText::new("Looking for outputs…").color(THEME.muted));
                }
                Some(list) => {
                    for d in list {
                        let chosen = !on_controller && draft.device.as_ref() == Some(&d.id);
                        if ui
                            .selectable_label(chosen, label(d))
                            .on_hover_text(hover(d))
                            .clicked()
                        {
                            draft.device = Some(d.id.clone());
                            draft.use_controller = false;
                        }
                    }
                }
            }
        });
}

/// An output for the headphones of their own, or `none` (on the master's).
fn device_pick(
    ui: &mut Ui,
    id: &str,
    pick: &mut Option<String>,
    devices: &Option<Vec<Output>>,
    none: &str,
) -> egui::Response {
    let current = match &*pick {
        None => none.to_string(),
        Some(id) => picked(pick, devices).map_or(id.clone(), label),
    };
    egui::ComboBox::from_id_salt(id)
        // (No wider than the window has room for.)
        .width(PICK_W.min(ui.available_width()))
        .selected_text(current)
        .show_ui(ui, |ui| {
            ui.selectable_value(pick, None, none);
            match devices {
                None => {
                    ui.label(RichText::new("Looking for outputs…").color(THEME.muted));
                }
                Some(list) => {
                    for d in list {
                        ui.selectable_value(pick, Some(d.id.clone()), label(d))
                            .on_hover_text(hover(d));
                    }
                }
            }
        })
        .response
}

fn out_name(o: Out) -> String {
    match o {
        Out::Off => "Off".into(),
        Out::Stereo(c) => format!("{}/{}", c + 1, c + 2),
        Out::Mono(c) => format!("{} (mono)", c + 1),
    }
}

fn out_choices(channels: u16) -> Vec<Out> {
    let mut v = vec![Out::Off];
    v.extend((0..channels.saturating_sub(1)).step_by(2).map(Out::Stereo));
    v.extend((0..channels).map(Out::Mono));
    v
}

/// Which channels the master and headphones go to: of the one device, or with the
/// headphones on their own (`phones`: its channels), of each one's.
fn routing_section(
    ui: &mut Ui,
    current: Routing,
    channels: u16,
    phones: Option<u16>,
) -> Option<Routing> {
    let mut r = current;
    ui.label(RichText::new("Outputs").strong());
    egui::Grid::new("audio-routing")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            for (name, bus, ch) in [
                ("Master", &mut r.master, channels),
                ("Headphones", &mut r.phones, phones.unwrap_or(channels)),
            ] {
                ui.label(name);
                egui::ComboBox::from_id_salt(("audio-out", name))
                    .selected_text(out_name(*bus))
                    .show_ui(ui, |ui| {
                        for o in out_choices(ch) {
                            ui.selectable_value(bus, o, out_name(o));
                        }
                    });
                ui.end_row();
            }
        });
    if phones.is_some() {
        muted(
            ui,
            "The headphones are on their own device: these are its channels.",
        );
    }
    if phones.is_none() {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Presets:").color(THEME.muted));
            if channels >= 4
                && ui
                    .add(Button::new("Controller"))
                    .on_hover_text("Master on 1/2, headphones on 3/4")
                    .clicked()
            {
                r = Routing::default_for(4);
            }
            if ui
                .add(Button::new("Speakers only"))
                .on_hover_text("Master on 1/2, no headphones")
                .clicked()
            {
                r = Routing {
                    master: Out::Stereo(0),
                    phones: Out::Off,
                };
            }
            if channels >= 2
                && ui
                    .add(Button::new("Split cue"))
                    .on_hover_text(
                        "One stereo output, e.g. laptop headphones: master in the right ear, \
                     cue and preview in the left",
                    )
                    .clicked()
            {
                r = Routing {
                    master: Out::Mono(1),
                    phones: Out::Mono(0),
                };
            }
        });
    }
    if r.phones == Out::Off {
        ui.label(
            RichText::new("Headphones are off, so previews and headphone cue are silent.")
                .size(11.0)
                .color(THEME.warn),
        );
    }
    (r != current).then_some(r)
}
