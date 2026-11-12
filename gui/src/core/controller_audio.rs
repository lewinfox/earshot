//! A controller's own sound card, played to directly. A USB controller with a sound
//! card (a DDJ-400) is one ALSA card with both its MIDI port and its audio device on
//! it, so the card a connected MIDI port is on says which audio device is the
//! controller's. Earshot opens that device itself (`hw:CARD=…`), not through PipeWire,
//! so no system volume or other program comes between earshot and the controller.
//!
//! PipeWire may have the card open. Earshot asks it to let go first, with the
//! standard request JACK uses (the `org.freedesktop.ReserveDevice1` D-Bus name), and
//! holds the card's name until it's done. If PipeWire doesn't let go, [`force_off`]
//! turns the card's PipeWire profile off: the button the top bar offers.

/// A connected controller's sound card.
#[derive(Clone, Debug, PartialEq)]
pub struct ControllerAudio {
    /// ALSA's card number, and its name for it (`DDJ400`).
    pub card: i32,
    pub id: String,
    /// The device for the engine to open (`alsa:hw:CARD=DDJ400,DEV=0`).
    pub device: String,
}

/// The ALSA sequencer client a MIDI port name ends with: midir names ALSA ports
/// `DDJ-400:DDJ-400 MIDI 1 20:0`, client 20, port 0.
pub fn port_client(name: &str) -> Option<i32> {
    let (client, port) = name.rsplit_once(' ')?.1.split_once(':')?;
    port.parse::<i32>().ok()?;
    client.parse().ok()
}

/// The PipeWire device for ALSA card `card` in `pw-dump`'s output, and its profile
/// now: (object id, profile index).
pub fn pipewire_device(dump: &str, card: i32) -> Option<(u64, Option<i64>)> {
    let objects: Vec<serde_json::Value> = serde_json::from_str(dump).ok()?;
    objects.iter().find_map(|o| {
        let info = o.get("info")?;
        let props = info.get("props")?;
        if props.get("media.class")?.as_str()? != "Audio/Device" {
            return None;
        }
        let c = props.get("api.alsa.card")?;
        let c = c.as_i64().or_else(|| c.as_str()?.parse().ok())?;
        if c != card as i64 {
            return None;
        }
        let profile = info
            .get("params")
            .and_then(|p| p.get("Profile"))
            .and_then(|p| p.get(0))
            .and_then(|p| p.get("index"))
            .and_then(|i| i.as_i64());
        Some((o.get("id")?.as_u64()?, profile))
    })
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::process::Command;

    /// The sound card of the first of these MIDI ports that's on a card that plays
    /// audio.
    pub fn find(ports: &[String]) -> Option<ControllerAudio> {
        let seq = alsa::Seq::open(None, None, false).ok()?;
        ports.iter().filter_map(|p| port_client(p)).find_map(|client| {
            let card = seq.get_any_client_info(client).ok()?.get_card().ok()?;
            let dir = std::path::PathBuf::from(format!("/proc/asound/card{card}"));
            if !dir.join("pcm0p").exists() {
                return None;
            }
            let id = std::fs::read_to_string(dir.join("id")).ok()?.trim().to_string();
            Some(ControllerAudio {
                card,
                device: format!("alsa:hw:CARD={id},DEV=0"),
                id,
            })
        })
    }

    /// Holds the card's reservation (see the module doc) until dropped.
    pub struct Reservation {
        _conn: zbus::blocking::Connection,
    }

    struct Holder {
        device: String,
    }

    #[zbus::interface(name = "org.freedesktop.ReserveDevice1")]
    impl Holder {
        /// Earshot doesn't give the card up while it plays to it.
        fn request_release(&self, _priority: i32) -> bool {
            false
        }
        #[zbus(property)]
        fn priority(&self) -> i32 {
            i32::MAX
        }
        #[zbus(property)]
        fn application_name(&self) -> String {
            "earshot".into()
        }
        #[zbus(property)]
        fn application_device_name(&self) -> String {
            self.device.clone()
        }
    }

    /// Take the card's reservation, asking whoever holds it (PipeWire) to let go.
    pub fn reserve(c: &ControllerAudio) -> Result<Reservation, String> {
        use zbus::fdo::{RequestNameFlags, RequestNameReply};
        let e = |e: zbus::Error| e.to_string();
        let name = format!("org.freedesktop.ReserveDevice1.Audio{}", c.card);
        let path = format!("/org/freedesktop/ReserveDevice1/Audio{}", c.card);
        let conn = zbus::blocking::Connection::session().map_err(e)?;
        conn.object_server()
            .at(path.as_str(), Holder { device: format!("hw:{}", c.id) })
            .map_err(e)?;
        let dbus = zbus::blocking::fdo::DBusProxy::new(&conn).map_err(|x| x.to_string())?;
        let wanted = zbus::names::WellKnownName::try_from(name.as_str()).map_err(|x| x.to_string())?;
        let ask = |flags: RequestNameFlags| {
            dbus.request_name(wanted.clone(), flags.into())
                .map_err(|x| x.to_string())
        };
        match ask(RequestNameFlags::DoNotQueue)? {
            RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner => {}
            _ => {
                let holder = zbus::blocking::Proxy::new(
                    &conn,
                    name.as_str(),
                    path.as_str(),
                    "org.freedesktop.ReserveDevice1",
                )
                .map_err(e)?;
                let released: bool = holder.call("RequestRelease", &(i32::MAX)).map_err(e)?;
                if !released {
                    return Err(format!("the program using {} wouldn't let it go", c.id));
                }
                let flags = RequestNameFlags::DoNotQueue | RequestNameFlags::ReplaceExisting;
                match dbus.request_name(wanted, flags).map_err(|x| x.to_string())? {
                    RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner => {}
                    other => return Err(format!("couldn't reserve {}: {other:?}", c.id)),
                }
            }
        }
        Ok(Reservation { _conn: conn })
    }

    /// Turns the card's PipeWire profile off, so PipeWire lets go of it, and back to
    /// what it was when dropped.
    pub struct ForcedOff {
        device: u64,
        was: Option<i64>,
    }

    /// Turn the card's PipeWire profile off (the top bar's button).
    pub fn force_off(c: &ControllerAudio) -> Result<ForcedOff, String> {
        let dump = Command::new("pw-dump")
            .output()
            .map_err(|e| format!("couldn't run pw-dump: {e}"))?;
        let dump = String::from_utf8_lossy(&dump.stdout);
        let (device, was) = pipewire_device(&dump, c.card)
            .ok_or_else(|| format!("PipeWire has no device for {}", c.id))?;
        set_profile(device, 0)?;
        Ok(ForcedOff { device, was })
    }

    fn set_profile(device: u64, index: i64) -> Result<(), String> {
        let ok = Command::new("wpctl")
            .args(["set-profile", &device.to_string(), &index.to_string()])
            .status()
            .map_err(|e| format!("couldn't run wpctl: {e}"))?
            .success();
        if ok { Ok(()) } else { Err("wpctl set-profile failed".into()) }
    }

    impl Drop for ForcedOff {
        fn drop(&mut self) {
            if let Some(i) = self.was.filter(|i| *i != 0) {
                let _ = set_profile(self.device, i);
            }
        }
    }
}

#[cfg(target_os = "linux")]
pub use linux::{ForcedOff, Reservation, find, force_off, reserve};

#[cfg(not(target_os = "linux"))]
mod other {
    use super::ControllerAudio;
    pub struct Reservation;
    pub struct ForcedOff;
    pub fn find(_: &[String]) -> Option<ControllerAudio> {
        None
    }
    pub fn reserve(_: &ControllerAudio) -> Result<Reservation, String> {
        Ok(Reservation)
    }
    pub fn force_off(_: &ControllerAudio) -> Result<ForcedOff, String> {
        Err("only on Linux".into())
    }
}

#[cfg(not(target_os = "linux"))]
pub use other::{ForcedOff, Reservation, find, force_off, reserve};

/// The connected controller's sound card, its reservation, whether its PipeWire
/// profile was turned off, and why earshot couldn't open it (if it couldn't).
#[derive(Default)]
pub struct Hold {
    pub found: Option<ControllerAudio>,
    reservation: Option<Reservation>,
    /// Held for its drop, which turns the PipeWire profile back.
    #[allow(dead_code)]
    forced: Option<ForcedOff>,
    pub error: Option<String>,
}

impl Hold {
    /// The controller now connected (`None`: none with a sound card). Whether that's
    /// a change, so audio should be reopened.
    pub fn set(&mut self, found: Option<ControllerAudio>) -> bool {
        if found == self.found {
            return false;
        }
        *self = Hold { found, ..Hold::default() };
        true
    }

    /// Before opening audio: the device to open, the controller's, with its card
    /// reserved (asked of PipeWire; it may be free anyway, so a refusal only goes to
    /// the log).
    pub fn prepare(&mut self) -> Option<String> {
        let c = self.found.as_ref()?;
        if self.reservation.is_none() {
            match reserve(c) {
                Ok(r) => self.reservation = Some(r),
                Err(e) => eprintln!("audio: reserving {}: {e}", c.id),
            }
        }
        Some(c.device.clone())
    }

    /// Turn the card's PipeWire profile off (the top bar's button), until the
    /// controller goes or earshot quits.
    pub fn force(&mut self) -> Result<(), String> {
        let c = self.found.as_ref().ok_or("no controller sound card")?;
        self.forced = Some(force_off(c)?);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_port_names_its_client() {
        assert_eq!(port_client("DDJ-400:DDJ-400 MIDI 1 20:0"), Some(20));
        assert_eq!(port_client("Midi Through:Midi Through Port-0 14:0"), Some(14));
        assert_eq!(port_client("no numbers"), None);
    }

    #[test]
    fn the_cards_pipewire_device_is_found() {
        let dump = r#"[
            {"id": 31, "info": {"props": {"media.class": "Audio/Device", "api.alsa.card": "0"}}},
            {"id": 79, "info": {"props": {"media.class": "Audio/Device", "api.alsa.card": 1},
                                "params": {"Profile": [{"index": 2}]}}},
            {"id": 77, "info": {"props": {"media.class": "Audio/Sink", "api.alsa.card": 1}}}
        ]"#;
        assert_eq!(pipewire_device(dump, 1), Some((79, Some(2))));
        assert_eq!(pipewire_device(dump, 0), Some((31, None)));
        assert_eq!(pipewire_device(dump, 5), None);
    }
}
