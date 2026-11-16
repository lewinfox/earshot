//! The controller files earshot ships with (`gui/controllers/`), and which one a
//! connected controller gets.
//!
//! A file can extend another: `pioneer/controllers/ddj-400.json` extends
//! `pioneer/pioneer.json`, which holds what
//! Pioneer's DDJ controllers share. The one on top wins, message by message; its
//! meters and start messages replace the ones under it if it has any.
//!
//! A controller is matched by its USB vendor and product id (`2b73:0026`) where
//! there is one, then by its MIDI port's name, then by its maker's id alone
//! (`2b73:*`), which gets an unknown model what its maker's controllers share.
//! Nothing else in earshot knows which controller it's talking to.

use super::midi::{ControllerFile, from_json};

/// Each bundled file: its id (what `extends` names) and its text. A maker's file is
/// `gui/controllers/<maker>/<maker>.json`, its models' `<maker>/controllers/<model>.json`.
const FILES: &[(&str, &str)] = &[
    ("pioneer", include_str!("../../controllers/pioneer/pioneer.json")),
    ("ddj-400", include_str!("../../controllers/pioneer/controllers/ddj-400.json")),
];

/// A bundled controller file, with what it extends merged under it.
#[derive(Clone, Debug)]
pub struct Profile {
    pub id: &'static str,
    /// The controller's name (`Pioneer DDJ-400`).
    pub name: String,
    /// USB `vendor:product` ids it's for (`2b73:0026`), or a maker's (`2b73:*`).
    pub usb: Vec<String>,
    /// Words in a MIDI port's name that say it's this controller.
    pub names: Vec<String>,
    pub file: ControllerFile,
}

#[derive(serde::Deserialize)]
struct Head {
    #[serde(default)]
    name: String,
    extends: Option<String>,
    #[serde(default)]
    usb: Vec<String>,
    #[serde(default)]
    names: Vec<String>,
}

/// Bundled file `id`, with what it extends under it.
pub fn resolve(id: &str) -> Result<Profile, String> {
    resolve_from(id, 0)
}

fn resolve_from(id: &str, depth: usize) -> Result<Profile, String> {
    if depth > 8 {
        return Err(format!("{id}: extends too deep (a loop?)"));
    }
    let &(id, text) = FILES
        .iter()
        .find(|(i, _)| *i == id)
        .ok_or_else(|| format!("no controller file {id}"))?;
    let head: Head = serde_json::from_str(text).map_err(|e| format!("{id}: {e}"))?;
    let (_, mut file) = from_json(text).map_err(|e| format!("{id}: {e}"))?;
    for s in &mut file.skipped {
        *s = format!("{id}: {s}");
    }
    if let Some(parent) = &head.extends {
        file = merge(resolve_from(parent, depth + 1)?.file, file);
    }
    Ok(Profile {
        id,
        name: head.name,
        usb: head.usb,
        names: head.names,
        file,
    })
}

/// `top` over `base`: its lines in place of the same messages', its meters and start
/// messages in place of the base's if it has any.
fn merge(mut base: ControllerFile, top: ControllerFile) -> ControllerFile {
    base.inputs.extend(top.inputs);
    if !top.meters.is_empty() {
        base.meters = top.meters;
    }
    if !top.start.is_empty() {
        base.start = top.start;
    }
    base.mirror.extend(top.mirror);
    base.mirror_status.extend(top.mirror_status);
    base.skipped.extend(top.skipped);
    base
}

/// Every bundled file, each with what it extends.
pub fn all() -> Vec<Profile> {
    FILES
        .iter()
        .filter_map(|(id, _)| {
            resolve(id)
                .inspect_err(|e| eprintln!("ERROR controller file: {e}"))
                .ok()
        })
        .collect()
}

/// The file for a controller on MIDI port `port`, with USB id `usb` (`2b73:0026`) if
/// it has one: the file with that id, else one whose name is in the port's, else its
/// maker's (`2b73:*`).
pub fn find(port: &str, usb: Option<&str>) -> Option<Profile> {
    let all = all();
    let usb = usb.map(str::to_ascii_lowercase);
    let exact = || {
        let u = usb.as_deref()?;
        all.iter().find(|p| p.usb.iter().any(|x| x.eq_ignore_ascii_case(u)))
    };
    let named = || all.iter().find(|p| p.names.iter().any(|n| port.contains(n.as_str())));
    let maker = || {
        let vendor = usb.as_deref()?.split(':').next()?;
        all.iter().find(|p| {
            p.usb
                .iter()
                .any(|x| x.strip_suffix(":*").is_some_and(|v| v.eq_ignore_ascii_case(vendor)))
        })
    };
    exact().or_else(named).or_else(maker).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every bundled file reads whole: no line left out, and no message on two lines.
    #[test]
    fn every_file_reads_whole() {
        for (id, text) in FILES {
            let p = resolve(id).unwrap();
            assert!(p.file.skipped.is_empty(), "{id}: {:?}", p.file.skipped);
            let (_, own) = from_json(text).unwrap();
            let lines: serde_json::Value = serde_json::from_str(text).unwrap();
            assert_eq!(lines["inputs"].as_array().unwrap().len(), own.inputs.len(), "{id}: a message twice");
        }
    }

    /// The DDJ-400's file by its USB id, or its port's name; another Pioneer device
    /// gets what Pioneer's share; anything else nothing.
    #[test]
    fn a_controller_gets_its_file() {
        let port = "DDJ-400:DDJ-400 MIDI 1 20:0";
        assert_eq!(find("whatever", Some("2b73:0026")).unwrap().id, "ddj-400");
        assert_eq!(find("whatever", Some("2B73:0026")).unwrap().id, "ddj-400");
        assert_eq!(find(port, None).unwrap().id, "ddj-400");
        assert_eq!(find("DDJ-FLX4 MIDI 1", Some("2b73:ffff")).unwrap().id, "pioneer");
        assert!(find("Launchpad", Some("1235:0069")).is_none());
        assert!(find("Midi Through Port-0", None).is_none());
    }

    /// The DDJ-400's file has Pioneer's lines under its own, and its own start
    /// message and meters.
    #[test]
    fn a_file_extends_its_makers() {
        let ddj = resolve("ddj-400").unwrap().file;
        let pioneer = resolve("pioneer").unwrap().file;
        assert!(pioneer.inputs.keys().all(|k| ddj.inputs.contains_key(k)));
        assert!(ddj.inputs.len() > pioneer.inputs.len());
        assert_eq!(ddj.start.len(), 1);
        assert_eq!(ddj.meters.len(), 2);
        assert_eq!(ddj.mirror_of((0x90, 0x0B)), Some((0x90, 0x47)), "PLAY, from Pioneer's");
        assert_eq!(ddj.mirror_of((0x90, 0x10)), Some((0x90, 0x4C)), "LOOP IN, the DDJ-400's");
        assert_eq!(ddj.mirror_of((0x97, 0x31)), Some((0x98, 0x31)), "a pad, a channel up");
        assert_eq!(ddj.mirror_of((0x96, 0x41)), None);
    }
}
