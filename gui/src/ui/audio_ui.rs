//! The Audio settings window: output device, buffer size, and which device channels
//! the master and headphone buses go to (e.g. a controller's 1/2 and 3/4, or a
//! laptop's one pair split left/right for cueing).

use std::sync::{Arc, Mutex};

use earshot_engine::output::{DeviceSummary, list_devices};
use earshot_engine::{Out, OutputInfo, Routing};
use egui::{RichText, Ui};

use super::util;
use crate::core::player::AudioSettings;

const BUFFERS: [u32; 6] = [64, 128, 256, 512, 1024, 2048];

pub enum AudioAction {
    /// Restart audio with a new device or buffer size.
    Restart(AudioSettings),
    /// Change routing live.
    Route(Routing),
}

#[derive(Default)]
pub struct AudioUi {
    pub open: bool,
    /// Filled by a background thread (listing devices can take a moment).
    devices: Arc<Mutex<Option<Vec<DeviceSummary>>>>,
    listed: bool,
    /// Device and buffer as edited, applied with the Apply button.
    draft: Option<AudioSettings>,
}

impl AudioUi {
    pub fn toggle(&mut self) {
        self.open = !self.open;
        if self.open {
            self.listed = false;
            self.draft = None;
        }
    }

    fn list(&mut self, ctx: &egui::Context) {
        self.listed = true;
        *self.devices.lock().unwrap() = None;
        let (out, ctx) = (self.devices.clone(), ctx.clone());
        std::thread::spawn(move || {
            *out.lock().unwrap() = Some(list_devices());
            ctx.request_repaint();
        });
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        saved: &AudioSettings,
        info: &Result<OutputInfo, String>,
        routing: Routing,
    ) -> Option<AudioAction> {
        if !self.open {
            return None;
        }
        if !self.listed {
            self.list(ctx);
        }
        let mut action = None;
        let mut open = true;
        egui::Window::new("Audio")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.set_width(460.0);
                action = self.device_section(ui, saved, info);
                ui.separator();
                let channels = match info {
                    Ok(i) => i.channels,
                    Err(_) => 2,
                };
                if let Some(r) = routing_section(ui, routing, channels) {
                    action = Some(AudioAction::Route(r));
                }
            });
        self.open = open;
        action
    }

    fn device_section(
        &mut self,
        ui: &mut Ui,
        saved: &AudioSettings,
        info: &Result<OutputInfo, String>,
    ) -> Option<AudioAction> {
        let mut action = None;
        match info {
            Ok(i) => ui.label(
                RichText::new(format!(
                    "Now: {} · {} kHz · {} channels · {} frames",
                    i.device,
                    i.sample_rate / 1000,
                    i.channels,
                    i.buffer_frames.map_or("default".into(), |b| b.to_string())
                ))
                .color(if i.null { util::WARN } else { util::MUTED }),
            ),
            Err(e) => ui.label(RichText::new(format!("No audio: {e}")).color(util::BAD)),
        };
        if std::env::var_os("EARSHOT_AUDIO_DEVICE").is_some()
            || std::env::var_os("EARSHOT_BUFFER").is_some()
        {
            ui.label(
                RichText::new("EARSHOT_AUDIO_DEVICE / EARSHOT_BUFFER are set and win over these.")
                    .size(11.0)
                    .color(util::WARN),
            );
        }
        let draft = self.draft.get_or_insert_with(|| saved.clone());
        let devices = self.devices.lock().unwrap().clone();
        egui::Grid::new("audio-device")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Device");
                ui.horizontal(|ui| {
                    let current = match (&draft.device, &devices) {
                        (None, _) => "System default".to_string(),
                        (Some(id), Some(list)) => {
                            list.iter().find(|d| &d.id == id).map_or(id.clone(), label)
                        }
                        (Some(id), None) => id.clone(),
                    };
                    egui::ComboBox::from_id_salt("audio-device-pick")
                        .width(330.0)
                        .selected_text(current)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut draft.device, None, "System default");
                            match &devices {
                                None => {
                                    ui.label(
                                        RichText::new("Looking for devices…").color(util::MUTED),
                                    );
                                }
                                Some(list) => {
                                    for d in list {
                                        ui.selectable_value(
                                            &mut draft.device,
                                            Some(d.id.clone()),
                                            label(d),
                                        );
                                    }
                                }
                            }
                        });
                    if ui.small_button("Refresh").clicked() {
                        self.listed = false;
                    }
                });
                ui.end_row();
                ui.label("Buffer");
                egui::ComboBox::from_id_salt("audio-buffer")
                    .selected_text(
                        draft
                            .buffer_frames
                            .map_or("256 frames".into(), |b| format!("{b} frames")),
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
        let changed = draft.device != saved.device || draft.buffer_frames != saved.buffer_frames;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(changed, egui::Button::new("Apply"))
                .on_hover_text("Restart audio on this device. Loaded tracks stay loaded.")
                .clicked()
            {
                action = Some(AudioAction::Restart(AudioSettings {
                    routing: saved.routing,
                    ..draft.clone()
                }));
            }
            ui.label(
                RichText::new("Smaller buffers respond faster but may crackle.")
                    .size(11.0)
                    .color(util::MUTED),
            );
        });
        action
    }
}

fn label(d: &DeviceSummary) -> String {
    format!(
        "{}{} ({} ch)",
        d.name,
        if d.is_default { ", default" } else { "" },
        d.max_channels
    )
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

fn routing_section(ui: &mut Ui, current: Routing, channels: u16) -> Option<Routing> {
    let mut r = current;
    ui.label(RichText::new("Outputs").strong());
    egui::Grid::new("audio-routing")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            for (name, bus) in [("Master", &mut r.master), ("Headphones", &mut r.phones)] {
                ui.label(name);
                egui::ComboBox::from_id_salt(("audio-out", name))
                    .selected_text(out_name(*bus))
                    .show_ui(ui, |ui| {
                        for o in out_choices(channels) {
                            ui.selectable_value(bus, o, out_name(o));
                        }
                    });
                ui.end_row();
            }
        });
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Presets:").color(util::MUTED));
        if channels >= 4
            && ui
                .button("Controller")
                .on_hover_text("Master on 1/2, headphones on 3/4")
                .clicked()
        {
            r = Routing::default_for(4);
        }
        if ui
            .button("Speakers only")
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
                .button("Split cue")
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
    if r.phones == Out::Off {
        ui.label(
            RichText::new("Headphones are off, so previews and headphone cue are silent.")
                .size(11.0)
                .color(util::WARN),
        );
    }
    (r != current).then_some(r)
}
