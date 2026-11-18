//! The outputs earshot can play to, named as people know them: "Speakers",
//! "Headphones", "HDMI / DisplayPort", a Bluetooth or USB device by its own name. Not
//! the sound chip's model ("Family 17h/19h HD Audio Controller Analog Stereo"), which
//! is all the audio library gives.
//!
//! On Linux the names come from PipeWire, which describes each sound card's outputs
//! the way the desktop's sound settings do: the output a card is playing to now (its
//! active "route") says whether that's the speakers, the headphone socket or HDMI. On
//! Windows the system's names are already close ("Speakers (Realtek(R) Audio)"): the
//! part in brackets becomes the detail. macOS's are fine as they are.

use earshot_engine::output::{DeviceSummary, list_devices};

/// What an output is, for how it's shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The computer's own speakers or headphone socket.
    BuiltIn,
    Hdmi,
    /// Fine for listening, too delayed for mixing.
    Bluetooth,
    Usb,
    Other,
}

/// An output to offer.
#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    /// The engine's device id (what [`crate::core::player::AudioSettings::device`]
    /// keeps).
    pub id: String,
    /// What it's called in the list.
    pub label: String,
    /// The hardware behind it, for a tooltip ("Family 17h/19h HD Audio Controller").
    pub detail: Option<String>,
    pub kind: Kind,
    /// The engine's name for it, which [`earshot_engine::OutputInfo::device`] reports.
    pub name: String,
    pub channels: u16,
    /// The system's default output now.
    pub is_default: bool,
}

impl Output {
    /// How it reads in a list: its name, and that it's Bluetooth.
    pub fn shown(&self) -> String {
        match self.kind {
            Kind::Bluetooth => format!("{} (Bluetooth)", self.label),
            _ => self.label.clone(),
        }
    }
}

/// What to call the device the engine opened (`device`, its name for it): the
/// controller's name if it's the controller's sound card, else the output's as the
/// list names it. Asks PipeWire (a moment): for when audio (re)opens, not every frame.
pub fn name_for(
    device: &str,
    controller: Option<&crate::core::controller_audio::ControllerAudio>,
) -> String {
    if let Some(c) = controller
        && device.starts_with(&c.name)
    {
        return c.name.clone();
    }
    list(controller.map(|c| c.card))
        .iter()
        .find(|o| o.name == device)
        .map_or_else(|| device.to_string(), Output::shown)
}

/// The outputs there are now, but the connected controller's sound card
/// (`controller_card`, ALSA's number), which is offered as the controller.
pub fn list(controller_card: Option<i32>) -> Vec<Output> {
    let devices = list_devices();
    #[cfg(target_os = "linux")]
    let dump = std::process::Command::new("pw-dump")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    #[cfg(not(target_os = "linux"))]
    let dump = String::new();
    named(&devices, &dump, controller_card)
}

/// What PipeWire says about one of its outputs.
#[derive(Debug, PartialEq)]
struct Described {
    label: String,
    detail: Option<String>,
    kind: Kind,
    card: Option<i32>,
}

/// `devices` named from PipeWire's description (`dump`, `pw-dump`'s output; empty
/// elsewhere), without `controller_card`'s. Two outputs that would read the same get
/// their hardware added.
fn named(devices: &[DeviceSummary], dump: &str, controller_card: Option<i32>) -> Vec<Output> {
    let described = pipewire(dump);
    let default = default_sink(dump);
    let mut out: Vec<Output> = devices
        .iter()
        .filter_map(|d| {
            let pw =
                d.id.strip_prefix("pipewire:")
                    .and_then(|node| described.iter().find(|(n, _)| n == node))
                    .map(|(_, x)| x);
            if controller_card.is_some() && pw.and_then(|x| x.card) == controller_card {
                return None;
            }
            let (label, detail, kind) = match pw {
                Some(x) => (x.label.clone(), x.detail.clone(), x.kind),
                None => tidy(&d.name),
            };
            Some(Output {
                id: d.id.clone(),
                label,
                detail,
                kind,
                name: d.name.clone(),
                channels: d.max_channels,
                // PipeWire's default is a stand-in to cpal: the real one's in its
                // metadata.
                is_default: d.is_default
                    || (default.is_some() && d.id.strip_prefix("pipewire:") == default.as_deref()),
            })
        })
        .collect();
    let labels: Vec<String> = out.iter().map(|o| o.label.clone()).collect();
    for o in &mut out {
        if labels.iter().filter(|l| **l == o.label).count() > 1
            && let Some(detail) = &o.detail
        {
            o.label = format!("{} · {detail}", o.label);
        }
    }
    out
}

/// A name from the system as it is, but Windows' "Speakers (Realtek(R) Audio)": the
/// output, with the hardware as its detail.
fn tidy(name: &str) -> (String, Option<String>, Kind) {
    let kind = kind_from_name(name);
    if cfg!(windows)
        && let Some(open) = name.find(" (")
        && name.ends_with(')')
    {
        let (label, rest) = name.split_at(open);
        let detail = &rest[2..rest.len() - 1];
        return (label.to_string(), Some(detail.to_string()), kind);
    }
    (name.to_string(), None, kind)
}

fn kind_from_name(name: &str) -> Kind {
    let n = name.to_lowercase();
    if n.contains("bluetooth") || n.contains("airpods") {
        Kind::Bluetooth
    } else if n.contains("hdmi") || n.contains("displayport") {
        Kind::Hdmi
    } else if n.contains("speaker") || n.contains("headphone") {
        Kind::BuiltIn
    } else {
        Kind::Other
    }
}

/// The output PipeWire plays to by default (its node name): `default.audio.sink` in
/// its "default" metadata.
fn default_sink(dump: &str) -> Option<String> {
    let objects: Vec<serde_json::Value> = serde_json::from_str(dump).ok()?;
    objects
        .iter()
        .filter(|o| {
            o.get("props")
                .and_then(|p| p.get("metadata.name"))
                .and_then(|n| n.as_str())
                == Some("default")
        })
        .filter_map(|o| o.get("metadata")?.as_array())
        .flatten()
        .find(|m| m.get("key").and_then(|k| k.as_str()) == Some("default.audio.sink"))
        .and_then(|m| {
            let v = m.get("value")?;
            // A JSON object, or (older PipeWire) that object as a string.
            let v = match v.as_str() {
                Some(s) => serde_json::from_str(s).ok()?,
                None => v.clone(),
            };
            v.get("name")?.as_str().map(str::to_string)
        })
}

/// Each PipeWire output (by node name) as PipeWire's sound settings would name it.
fn pipewire(dump: &str) -> Vec<(String, Described)> {
    let Ok(objects) = serde_json::from_str::<Vec<serde_json::Value>>(dump) else {
        return vec![];
    };
    let props = |o: &serde_json::Value| o.get("info")?.get("props").cloned();
    let class = |o: &serde_json::Value| props(o)?.get("media.class")?.as_str().map(str::to_string);
    let text = |p: &serde_json::Value, k: &str| p.get(k)?.as_str().map(str::to_string);
    let number = |p: &serde_json::Value, k: &str| {
        let v = p.get(k)?;
        v.as_i64().or_else(|| v.as_str()?.parse().ok())
    };
    objects
        .iter()
        .filter(|o| class(o).as_deref() == Some("Audio/Sink"))
        .filter_map(|node| {
            let p = props(node)?;
            let name = text(&p, "node.name")?;
            let fallback = text(&p, "node.description").unwrap_or_else(|| name.clone());
            let card = number(&p, "device.id").and_then(|id| {
                objects
                    .iter()
                    .find(|o| o.get("id").and_then(|i| i.as_i64()) == Some(id))
            });
            let Some(card) = card else {
                let kind = kind_from_name(&fallback);
                return Some((
                    name,
                    Described {
                        label: fallback,
                        detail: None,
                        kind,
                        card: None,
                    },
                ));
            };
            let cp = props(card).unwrap_or_default();
            let hardware = text(&cp, "device.description");
            // The output the card plays to now, for this node.
            let route = card
                .get("info")
                .and_then(|i| i.get("params"))
                .and_then(|p| p.get("Route"))
                .and_then(|r| r.as_array())
                .and_then(|routes| {
                    let outputs = routes
                        .iter()
                        .filter(|r| r.get("direction").and_then(|d| d.as_str()) == Some("Output"));
                    let wanted = number(&p, "card.profile.device");
                    outputs
                        .clone()
                        .find(|r| {
                            wanted.is_some() && r.get("device").and_then(|d| d.as_i64()) == wanted
                        })
                        .or_else(|| outputs.clone().next())
                        .cloned()
                });
            let port = route.as_ref().and_then(|r| {
                // `info` is [count, key, value, key, value, ...].
                let info = r.get("info")?.as_array()?;
                info.windows(2)
                    .find(|w| w[0].as_str() == Some("port.type"))
                    .and_then(|w| w[1].as_str().map(str::to_string))
            });
            let route_name = route
                .as_ref()
                .and_then(|r| r.get("description")?.as_str().map(str::to_string));
            let kind = if text(&cp, "device.api").as_deref() == Some("bluez5") {
                Kind::Bluetooth
            } else if port.as_deref() == Some("hdmi") {
                Kind::Hdmi
            } else if text(&cp, "device.bus").as_deref() == Some("usb") {
                Kind::Usb
            } else if route_name.is_some() {
                Kind::BuiltIn
            } else {
                Kind::Other
            };
            let (label, detail) = match kind {
                // The device's own name ("WH-1000XM4", "USB Audio Device").
                Kind::Bluetooth | Kind::Usb => (hardware.clone().unwrap_or(fallback), None),
                // "Speakers", "Headphones", "HDMI / DisplayPort", with the chip beside.
                _ => (route_name.unwrap_or(fallback), hardware),
            };
            Some((
                name,
                Described {
                    label,
                    detail,
                    kind,
                    card: number(&cp, "api.alsa.card").map(|c| c as i32),
                },
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str, name: &str, default: bool) -> DeviceSummary {
        DeviceSummary {
            id: id.into(),
            name: name.into(),
            max_channels: 2,
            is_default: default,
        }
    }

    /// A laptop (speakers active), its HDMI card, a DDJ-400 (card 2) and Bluetooth
    /// headphones, trimmed from a real `pw-dump`.
    const DUMP: &str = r#"[
      {"id": 51, "info": {"props": {"media.class": "Audio/Device", "api.alsa.card": "1",
          "device.api": "alsa", "device.bus": "pci",
          "device.description": "Family 17h/19h HD Audio Controller"},
        "params": {"Route": [
          {"direction": "Input", "device": 0, "description": "Internal Microphone"},
          {"direction": "Output", "device": 3, "description": "Speakers",
           "info": [4, "port.type", "speaker", "port.availability-group", "Legacy 3"]}]}}},
      {"id": 56, "info": {"props": {"media.class": "Audio/Sink",
          "node.name": "alsa_output.pci-0000_06_00.6.analog-stereo", "device.id": 51,
          "card.profile.device": 3,
          "node.description": "Family 17h/19h HD Audio Controller Analog Stereo"}}},
      {"id": 50, "info": {"props": {"media.class": "Audio/Device", "api.alsa.card": "0",
          "device.api": "alsa", "device.bus": "pci",
          "device.description": "Raven/Raven2/Fenghuang HDMI/DP Audio Controller"},
        "params": {"Route": [
          {"direction": "Output", "device": 3, "description": "HDMI / DisplayPort",
           "info": [4, "port.type", "hdmi", "port.availability-group", "Legacy 1"]}]}}},
      {"id": 57, "info": {"props": {"media.class": "Audio/Sink",
          "node.name": "alsa_output.pci-0000_06_00.1.hdmi-stereo", "device.id": 50,
          "card.profile.device": 3,
          "node.description": "Raven/Raven2/Fenghuang HDMI/DP Audio Controller Digital Stereo (HDMI)"}}},
      {"id": 104, "info": {"props": {"media.class": "Audio/Device", "api.alsa.card": 2,
          "device.api": "alsa", "device.bus": "usb", "device.description": "DDJ-400"},
        "params": {"Route": [{"direction": "Output", "device": 1, "description": "Analog Output"}]}}},
      {"id": 80, "info": {"props": {"media.class": "Audio/Sink",
          "node.name": "alsa_output.usb-Pioneer_DJ_Corporation_DDJ-400_------00.analog-surround-40.8",
          "device.id": 104, "card.profile.device": 1,
          "node.description": "DDJ-400 Analog Surround 4.0"}}},
      {"id": 120, "info": {"props": {"media.class": "Audio/Device", "device.api": "bluez5",
          "device.description": "WH-1000XM4"}}},
      {"id": 121, "info": {"props": {"media.class": "Audio/Sink",
          "node.name": "bluez_output.AA_BB_CC_DD_EE_FF.1", "device.id": 120,
          "node.description": "WH-1000XM4"}}},
      {"id": 36, "type": "PipeWire:Interface:Metadata", "props": {"metadata.name": "default"},
       "metadata": [
         {"subject": 0, "key": "default.configured.audio.sink", "value": {"name": "bluez_output.AA_BB_CC_DD_EE_FF.1"}},
         {"subject": 0, "key": "default.audio.sink", "value": {"name": "alsa_output.pci-0000_06_00.6.analog-stereo"}}]}
    ]"#;

    fn devices() -> Vec<DeviceSummary> {
        vec![
            device(
                "pipewire:alsa_output.pci-0000_06_00.6.analog-stereo",
                "Family 17h/19h HD Audio Controller Analog Stereo",
                // cpal doesn't know: PipeWire's metadata says.
                false,
            ),
            device(
                "pipewire:alsa_output.pci-0000_06_00.1.hdmi-stereo",
                "Raven/Raven2/Fenghuang HDMI/DP Audio Controller Digital Stereo (HDMI)",
                false,
            ),
            device(
                "pipewire:alsa_output.usb-Pioneer_DJ_Corporation_DDJ-400_------00.analog-surround-40.8",
                "DDJ-400 Analog Surround 4.0",
                false,
            ),
            device(
                "pipewire:bluez_output.AA_BB_CC_DD_EE_FF.1",
                "WH-1000XM4",
                false,
            ),
        ]
    }

    /// What this machine's list would show: `cargo test -p earshot-gui
    /// this_machines_outputs -- --ignored --nocapture`.
    #[test]
    #[ignore = "this machine's outputs, for looking at"]
    fn this_machines_outputs() {
        for o in list(None) {
            println!(
                "{:<30} {:<10} {:?}  (default: {}, {} ch, id {})",
                o.label,
                format!("{:?}", o.kind),
                o.detail,
                o.is_default,
                o.channels,
                o.id
            );
        }
    }

    #[test]
    fn outputs_are_named_as_the_desktop_names_them() {
        let got: Vec<(String, Kind)> = named(&devices(), DUMP, None)
            .into_iter()
            .map(|o| (o.label, o.kind))
            .collect();
        assert_eq!(
            got,
            [
                ("Speakers".to_string(), Kind::BuiltIn),
                ("HDMI / DisplayPort".to_string(), Kind::Hdmi),
                ("DDJ-400".to_string(), Kind::Usb),
                ("WH-1000XM4".to_string(), Kind::Bluetooth),
            ]
        );
    }

    #[test]
    fn the_hardware_is_kept_for_the_tooltip() {
        let speakers = &named(&devices(), DUMP, None)[0];
        assert_eq!(
            speakers.detail.as_deref(),
            Some("Family 17h/19h HD Audio Controller")
        );
        assert_eq!(
            speakers.name,
            "Family 17h/19h HD Audio Controller Analog Stereo"
        );
        assert!(speakers.is_default);
    }

    #[test]
    fn the_controllers_card_is_left_out() {
        let labels: Vec<String> = named(&devices(), DUMP, Some(2))
            .into_iter()
            .map(|o| o.label)
            .collect();
        assert!(!labels.iter().any(|l| l.contains("DDJ")), "{labels:?}");
        assert_eq!(labels.len(), 3);
    }

    #[test]
    fn without_pipewire_the_systems_names_stay() {
        let got = named(
            &[device("alsa:hw:0", "HDA Intel PCH, ALC257 Analog", true)],
            "",
            None,
        );
        assert_eq!(got[0].label, "HDA Intel PCH, ALC257 Analog");
        assert_eq!(got[0].detail, None);
    }

    #[test]
    fn two_outputs_that_read_the_same_get_their_hardware() {
        let dump = DUMP.replace("HDMI / DisplayPort", "Speakers");
        let labels: Vec<String> = named(&devices(), &dump, None)
            .into_iter()
            .map(|o| o.label)
            .collect();
        assert_eq!(labels[0], "Speakers · Family 17h/19h HD Audio Controller");
        assert_eq!(
            labels[1],
            "Speakers · Raven/Raven2/Fenghuang HDMI/DP Audio Controller"
        );
    }
}
