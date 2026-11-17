//! Controller mappings: the ones earshot ships with (`gui/controllers/`, found by
//! `build.rs` and compiled in) and the user's own (`controllers/` in the data
//! folder), and which one a connected controller gets.
//!
//! Both are laid out the same way: a maker's file `<maker>/<maker>.json`, and its
//! models' `<maker>/controllers/<model>.json`. A model's mapping is its maker's file
//! with its own on top, message by message (its meters and start messages replace the
//! maker's if it has any). The maker's file is the user's own if they have one, else
//! the bundled one.
//!
//! Bundled mappings can't be changed in the app: a new mapping can start from one (a
//! copy), and is the user's own.
//!
//! A controller gets: the mapping chosen for it before (the MIDI pill), else one of
//! the user's own whose USB id (`2b73:0026`) or port name matches, else a bundled
//! one's, else its maker's file alone (by the maker's USB id, `2b73:*`). Nothing else
//! in earshot knows which controller it's talking to.

use std::path::PathBuf;

use super::midi::{ControllerFile, file_json, from_json};

/// Each bundled file: its maker, its model (`None`: the maker's own file) and its text.
const BUNDLED: &[(&str, Option<&str>, &str)] =
    include!(concat!(env!("OUT_DIR"), "/bundled_controllers.rs"));

/// Where a mapping comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    /// Shipped with earshot: read only.
    Bundled,
    /// The user's own.
    Mine,
}

/// Which mapping: where it's from, its maker, and its model (`None`: the maker's
/// own file).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MappingId {
    pub source: Source,
    pub maker: String,
    pub model: Option<String>,
}

impl MappingId {
    /// As text, to remember (`bundled:pioneer/ddj-400`, `mine:pioneer/flx4`).
    pub fn key(&self) -> String {
        let source = match self.source {
            Source::Bundled => "bundled",
            Source::Mine => "mine",
        };
        match &self.model {
            Some(m) => format!("{source}:{}/{m}", self.maker),
            None => format!("{source}:{}", self.maker),
        }
    }

    pub fn parse(key: &str) -> Option<MappingId> {
        let (source, rest) = key.split_once(':')?;
        let source = match source {
            "bundled" => Source::Bundled,
            "mine" => Source::Mine,
            _ => return None,
        };
        let (maker, model) = match rest.split_once('/') {
            Some((a, b)) => (a, Some(b.to_string())),
            None => (rest, None),
        };
        Some(MappingId {
            source,
            maker: maker.to_string(),
            model,
        })
    }
}

/// What a file says about the controller it's for.
#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
pub struct Head {
    /// Its name (`Pioneer DDJ-400`).
    #[serde(default)]
    pub name: String,
    /// USB `vendor:product` ids it's for (`2b73:0026`), or a maker's (`2b73:*`).
    #[serde(default)]
    pub usb: Vec<String>,
    /// Words in a MIDI port's name that say it's this controller.
    #[serde(default)]
    pub names: Vec<String>,
}

/// A mapping, read.
#[derive(Clone, Debug)]
pub struct Profile {
    pub id: MappingId,
    pub head: Head,
    /// What its file holds.
    pub own: ControllerFile,
    /// With its maker's file under it (a model's), as it's used.
    pub file: ControllerFile,
}

impl Profile {
    /// Bundled: can't be changed in the app.
    pub fn read_only(&self) -> bool {
        self.id.source == Source::Bundled
    }

    /// Its name, and where it's from, for a list.
    pub fn label(&self) -> String {
        let name = if self.head.name.is_empty() {
            self.id.key()
        } else {
            self.head.name.clone()
        };
        match self.id.source {
            Source::Bundled => format!("{name} (built-in)"),
            Source::Mine => name,
        }
    }
}

/// A name as a file name: lower case, a dash for anything but letters and digits
/// (`Pioneer DJ` → `pioneer-dj`).
pub fn slug(name: &str) -> String {
    let mut s = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c.to_ascii_lowercase());
        } else if !s.ends_with('-') {
            s.push('-');
        }
    }
    s.trim_matches('-').to_string()
}

/// What a connected device is remembered by: its USB id if it has one, else its
/// port's name without ALSA's numbers (`DDJ-400:DDJ-400 MIDI 1 20:0` →
/// `DDJ-400:DDJ-400 MIDI 1`).
pub fn device_key(port: &str, usb: Option<&str>) -> String {
    if let Some(u) = usb {
        return format!("usb:{u}");
    }
    format!("port:{}", port_name(port))
}

/// A port's name without ALSA's client and port numbers.
fn port_name(port: &str) -> &str {
    match port.rsplit_once(' ') {
        Some((head, tail))
            if tail
                .split(':')
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())) =>
        {
            head
        }
        _ => port,
    }
}

/// The mappings: the bundled ones, and the user's in `mine` (if there's a folder).
pub struct Store {
    mine: Option<PathBuf>,
}

impl Store {
    pub fn new(mine: Option<PathBuf>) -> Store {
        Store { mine }
    }

    /// Where the user's file for `id` goes (none for a bundled one).
    pub fn path(&self, id: &MappingId) -> Option<PathBuf> {
        if id.source != Source::Mine {
            return None;
        }
        let dir = self.mine.as_ref()?.join(&id.maker);
        Some(match &id.model {
            Some(m) => dir.join("controllers").join(format!("{m}.json")),
            None => dir.join(format!("{}.json", id.maker)),
        })
    }

    /// Every file: its id and text.
    fn texts(&self) -> Vec<(MappingId, String)> {
        let mut out: Vec<(MappingId, String)> = BUNDLED
            .iter()
            .map(|(maker, model, text)| {
                let id = MappingId {
                    source: Source::Bundled,
                    maker: maker.to_string(),
                    model: model.map(str::to_string),
                };
                (id, text.to_string())
            })
            .collect();
        let Some(root) = &self.mine else { return out };
        let mut makers: Vec<PathBuf> = std::fs::read_dir(root)
            .map(|d| {
                d.filter_map(Result::ok)
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect()
            })
            .unwrap_or_default();
        makers.sort();
        for dir in makers {
            let maker = dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let mut files = vec![(None, dir.join(format!("{maker}.json")))];
            let mut models: Vec<PathBuf> = std::fs::read_dir(dir.join("controllers"))
                .map(|d| d.filter_map(Result::ok).map(|e| e.path()).collect())
                .unwrap_or_default();
            models.sort();
            for p in models {
                if p.extension().is_some_and(|e| e == "json") {
                    let model = p
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    files.push((Some(model), p));
                }
            }
            for (model, path) in files {
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let id = MappingId {
                    source: Source::Mine,
                    maker: maker.clone(),
                    model,
                };
                out.push((id, text));
            }
        }
        out
    }

    /// Every mapping, makers' files and models', each read (one that isn't JSON is
    /// logged and left out; lines that can't be read are logged and left out).
    pub fn all(&self) -> Vec<Profile> {
        let read: Vec<(MappingId, Head, ControllerFile)> = self
            .texts()
            .into_iter()
            .filter_map(|(id, text)| {
                let head: Head = serde_json::from_str(&text).ok()?;
                match from_json(&text) {
                    Ok((_, file)) => {
                        for s in &file.skipped {
                            eprintln!("ERROR controller file {}: left out {s}", id.key());
                        }
                        Some((id, head, file))
                    }
                    Err(e) => {
                        eprintln!("ERROR controller file {}: {e}", id.key());
                        None
                    }
                }
            })
            .collect();
        read.iter()
            .map(|(id, head, own)| {
                let file = match (&id.model, maker_own(&read, &id.maker)) {
                    (Some(_), Some(base)) => merge(base.clone(), own.clone()),
                    _ => own.clone(),
                };
                Profile {
                    id: id.clone(),
                    head: head.clone(),
                    own: own.clone(),
                    file,
                }
            })
            .collect()
    }

    /// The models' mappings (not the makers' own files): the user's first, then by
    /// maker and name.
    pub fn models(&self) -> Vec<Profile> {
        let mut v: Vec<Profile> = self
            .all()
            .into_iter()
            .filter(|p| p.id.model.is_some())
            .collect();
        v.sort_by_key(|p| {
            (
                std::cmp::Reverse(p.id.source),
                p.id.maker.clone(),
                p.label(),
            )
        });
        v
    }

    /// The makers: their folder name and name, bundled or the user's.
    pub fn makers(&self) -> Vec<(String, String)> {
        let all = self.all();
        let mut out: Vec<(String, String)> = vec![];
        for p in &all {
            if out.iter().any(|(m, _)| *m == p.id.maker) {
                continue;
            }
            let named = all.iter().find(|q| {
                q.id.maker == p.id.maker && q.id.model.is_none() && !q.head.name.is_empty()
            });
            let name = named.map_or_else(|| p.id.maker.clone(), |q| q.head.name.clone());
            out.push((p.id.maker.clone(), name));
        }
        out.sort();
        out
    }

    pub fn get(&self, id: &MappingId) -> Option<Profile> {
        self.all().into_iter().find(|p| p.id == *id)
    }

    /// The mapping for a controller on MIDI port `port`, with USB id `usb` if it has
    /// one: `chosen` (a [`MappingId::key`] remembered for it) if it's there, else the
    /// user's own by USB id then name, else a bundled one's likewise, else its maker's
    /// file by the maker's USB id (`2b73:*`).
    pub fn find(&self, port: &str, usb: Option<&str>, chosen: Option<&str>) -> Option<Profile> {
        let all = self.all();
        if let Some(id) = chosen.and_then(MappingId::parse)
            && let Some(p) = all.iter().find(|p| p.id == id)
        {
            return Some(p.clone());
        }
        let usb = usb.map(str::to_ascii_lowercase);
        let by_usb = |p: &&Profile| {
            usb.as_deref()
                .is_some_and(|u| p.head.usb.iter().any(|x| x.eq_ignore_ascii_case(u)))
        };
        let by_name = |p: &&Profile| p.head.names.iter().any(|n| port.contains(n.as_str()));
        let by_maker = |p: &&Profile| {
            let Some(vendor) = usb.as_deref().and_then(|u| u.split(':').next()) else {
                return false;
            };
            p.head.usb.iter().any(|x| {
                x.strip_suffix(":*")
                    .is_some_and(|v| v.eq_ignore_ascii_case(vendor))
            })
        };
        for s in [Source::Mine, Source::Bundled] {
            let models = || {
                all.iter()
                    .filter(move |p| p.id.source == s && p.id.model.is_some())
            };
            if let Some(p) = models().find(by_usb).or_else(|| models().find(by_name)) {
                return Some(p.clone());
            }
        }
        for s in [Source::Mine, Source::Bundled] {
            if let Some(p) = all
                .iter()
                .filter(|p| p.id.source == s && p.id.model.is_none())
                .find(by_maker)
            {
                return Some(p.clone());
            }
        }
        None
    }

    /// Write the user's file for `id`.
    pub fn save(
        &self,
        id: &MappingId,
        head: &Head,
        own: &ControllerFile,
    ) -> Result<PathBuf, String> {
        let path = self.path(id).ok_or("a built-in mapping can't be changed")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let head = match serde_json::to_value(head) {
            Ok(serde_json::Value::Object(m)) => m,
            _ => serde_json::Map::new(),
        };
        std::fs::write(&path, file_json(head, own))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(path)
    }

    /// What of `full` (a model's whole mapping, as edited) goes in its own file: the
    /// lines its maker's file doesn't have the same, its meters if they differ, and its
    /// own start messages and mirrors.
    pub fn own_part(&self, p: &Profile, full: &ControllerFile) -> ControllerFile {
        let base = match &p.id.model {
            Some(_) => {
                let all = self.all();
                let read: Vec<_> = all.into_iter().map(|q| (q.id, q.head, q.own)).collect();
                maker_own(&read, &p.id.maker).cloned().unwrap_or_default()
            }
            None => ControllerFile::default(),
        };
        ControllerFile {
            inputs: full
                .inputs
                .iter()
                .filter(|(k, l)| base.inputs.get(k) != Some(l))
                .map(|(k, l)| (*k, *l))
                .collect(),
            meters: if full.meters == base.meters {
                vec![]
            } else {
                full.meters.clone()
            },
            start: p.own.start.clone(),
            start_every_ms: p.own.start_every_ms,
            mirror: p.own.mirror.clone(),
            mirror_status: p.own.mirror_status.clone(),
            skipped: vec![],
        }
    }

    /// A new mapping of the user's: model `model` of maker `maker` (both as the user
    /// typed them), starting from `template`'s own lines (a model of the same maker)
    /// or empty, for the device on `port` (with USB id `usb`). A new maker gets its own
    /// (empty) file, matched by the device's maker id. The new mapping's id.
    pub fn create(
        &self,
        maker: &str,
        model: &str,
        template: Option<&Profile>,
        device: Option<(&str, Option<&str>)>,
    ) -> Result<MappingId, String> {
        let (maker_slug, model_slug) = (slug(maker), slug(model));
        if maker_slug.is_empty() || model_slug.is_empty() {
            return Err("give the maker and the model a name".into());
        }
        let id = MappingId {
            source: Source::Mine,
            maker: maker_slug.clone(),
            model: Some(model_slug),
        };
        if self.path(&id).is_some_and(|p| p.exists()) {
            return Err(format!(
                "you have a mapping called {} already",
                model.trim()
            ));
        }
        let usb = device.and_then(|(_, u)| u).map(str::to_ascii_lowercase);
        let known = self.makers().into_iter().find(|(m, _)| *m == maker_slug);
        // A maker earshot doesn't know: its own file, for what its models share.
        if known.is_none() {
            let vendor = usb
                .as_deref()
                .and_then(|u| u.split(':').next())
                .map(|v| format!("{v}:*"));
            let head = Head {
                name: maker.trim().to_string(),
                usb: vendor.into_iter().collect(),
                names: vec![],
            };
            self.save(
                &MappingId {
                    model: None,
                    ..id.clone()
                },
                &head,
                &ControllerFile::default(),
            )?;
        }
        let maker_name = known.map_or_else(|| maker.trim().to_string(), |(_, n)| n);
        let head = Head {
            name: format!("{maker_name} {}", model.trim()),
            usb: usb.into_iter().collect(),
            names: device
                .map(|(port, _)| port_name(port).to_string())
                .into_iter()
                .collect(),
        };
        let own = template.map(|t| t.own.clone()).unwrap_or_default();
        self.save(&id, &head, &own)?;
        Ok(id)
    }
}

/// Maker `maker`'s own file among `read`: the user's if they have one, else the
/// bundled one.
fn maker_own<'a>(
    read: &'a [(MappingId, Head, ControllerFile)],
    maker: &str,
) -> Option<&'a ControllerFile> {
    let of = |s| {
        read.iter()
            .find(|(i, _, _)| i.source == s && i.maker == maker && i.model.is_none())
    };
    of(Source::Mine)
        .or_else(|| of(Source::Bundled))
        .map(|(_, _, f)| f)
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
        base.start_every_ms = top.start_every_ms;
    }
    base.mirror.extend(top.mirror);
    base.mirror_status.extend(top.mirror_status);
    base.skipped.extend(top.skipped);
    base
}

/// A folder of its own for a test's mappings, emptied first.
#[cfg(test)]
pub fn test_store(name: &str) -> (Store, PathBuf) {
    let dir =
        std::env::temp_dir().join(format!("earshot-controllers-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    (Store::new(Some(dir.clone())), dir)
}

/// The bundled mapping for `maker`/`model`, read (tests).
#[cfg(test)]
pub fn bundled(maker: &str, model: Option<&str>) -> Profile {
    let id = MappingId {
        source: Source::Bundled,
        maker: maker.into(),
        model: model.map(str::to_string),
    };
    Store::new(None).get(&id).expect("a bundled mapping")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every bundled file reads whole: no line left out, and no message on two lines.
    #[test]
    fn every_bundled_file_reads_whole() {
        assert!(BUNDLED.len() >= 2, "build.rs found the files");
        for (maker, model, text) in BUNDLED {
            let (_, own) = from_json(text).unwrap();
            assert!(
                own.skipped.is_empty(),
                "{maker} {model:?}: {:?}",
                own.skipped
            );
            let lines: serde_json::Value = serde_json::from_str(text).unwrap();
            let n = lines["inputs"].as_array().map_or(0, Vec::len);
            assert_eq!(n, own.inputs.len(), "{maker} {model:?}: a message twice");
        }
    }

    /// The DDJ-400 by its USB id or its port's name; another Pioneer device gets what
    /// Pioneer's share; anything else nothing.
    #[test]
    fn a_controller_gets_its_mapping() {
        let s = Store::new(None);
        let port = "DDJ-400:DDJ-400 MIDI 1 20:0";
        let model = |p: Option<Profile>| p.map(|p| p.id.model);
        assert_eq!(
            model(s.find("x", Some("2b73:0026"), None)),
            Some(Some("ddj-400".into()))
        );
        assert_eq!(
            model(s.find("x", Some("2B73:0026"), None)),
            Some(Some("ddj-400".into()))
        );
        assert_eq!(
            model(s.find(port, None, None)),
            Some(Some("ddj-400".into()))
        );
        assert_eq!(
            model(s.find("DDJ-SB3 MIDI 1", Some("2b73:ffff"), None)),
            Some(None),
            "Pioneer's file"
        );
        assert!(s.find("Launchpad", Some("1235:0069"), None).is_none());
        assert!(s.find("Midi Through Port-0", None, None).is_none());
    }

    /// A model's mapping has its maker's lines under its own, and its own start
    /// message, meters and mirrors.
    #[test]
    fn a_model_builds_on_its_maker() {
        let ddj = bundled("pioneer", Some("ddj-400")).file;
        let pioneer = bundled("pioneer", None).file;
        assert!(pioneer.inputs.keys().all(|k| ddj.inputs.contains_key(k)));
        assert!(ddj.inputs.len() > pioneer.inputs.len());
        assert_eq!(ddj.start.len(), 1);
        assert_eq!(ddj.meters.len(), 2);
        assert_eq!(
            ddj.mirror_of((0x90, 0x0B)),
            Some((0x90, 0x47)),
            "PLAY, from Pioneer's"
        );
        assert_eq!(
            ddj.mirror_of((0x90, 0x10)),
            Some((0x90, 0x4C)),
            "LOOP IN, the DDJ-400's"
        );
        assert_eq!(
            ddj.mirror_of((0x97, 0x31)),
            Some((0x98, 0x31)),
            "a pad, a channel up"
        );
        assert_eq!(ddj.mirror_of((0x96, 0x41)), None);
    }

    /// A new mapping from the DDJ-400 is the user's, has its lines, matches the
    /// device, and wins over the bundled one; a remembered choice wins over both.
    #[test]
    fn a_new_mapping_from_a_template() {
        let (s, dir) = test_store("template");
        let ddj = bundled("pioneer", Some("ddj-400"));
        let port = "DDJ-400:DDJ-400 MIDI 1 20:0";
        let id = s
            .create(
                "Pioneer",
                "My DDJ",
                Some(&ddj),
                Some((port, Some("2b73:0026"))),
            )
            .unwrap();
        assert_eq!(id.key(), "mine:pioneer/my-ddj");
        assert!(dir.join("pioneer/controllers/my-ddj.json").exists());
        assert!(
            !dir.join("pioneer/pioneer.json").exists(),
            "Pioneer has a file already"
        );
        let mine = s.get(&id).unwrap();
        assert!(!mine.read_only());
        assert_eq!(mine.file.inputs, ddj.file.inputs);
        assert_eq!(mine.file.start, ddj.file.start);
        assert_eq!(mine.head.name, "Pioneer DJ My DDJ");
        assert_eq!(
            s.find(port, Some("2b73:0026"), None).unwrap().id,
            id,
            "mine first"
        );
        let chosen = ddj.id.key();
        assert_eq!(
            s.find(port, Some("2b73:0026"), Some(&chosen)).unwrap().id,
            ddj.id
        );
        assert!(
            s.create("Pioneer", "My DDJ", None, None).is_err(),
            "one of that name"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A new maker gets its own empty file, matched by the device's maker id; its
    /// first model starts empty.
    #[test]
    fn a_new_maker() {
        let (s, dir) = test_store("maker");
        let id = s
            .create(
                "Acme Audio",
                "Mixer 1",
                None,
                Some(("Acme Mixer 1 24:0", Some("1234:0001"))),
            )
            .unwrap();
        assert_eq!(id.key(), "mine:acme-audio/mixer-1");
        let maker = s
            .get(&MappingId {
                model: None,
                ..id.clone()
            })
            .unwrap();
        assert_eq!(maker.head.name, "Acme Audio");
        assert_eq!(maker.head.usb, vec!["1234:*".to_string()]);
        let p = s.get(&id).unwrap();
        assert!(p.file.inputs.is_empty());
        assert_eq!(p.head.names, vec!["Acme Mixer 1".to_string()]);
        assert_eq!(
            s.find("Acme Mixer 1 24:0", Some("1234:0001"), None)
                .unwrap()
                .id,
            id
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Saving keeps only what differs from the maker's file.
    #[test]
    fn own_part_leaves_the_makers_lines_out() {
        let (s, dir) = test_store("own");
        let ddj = bundled("pioneer", Some("ddj-400"));
        let id = s.create("Pioneer", "Copy", Some(&ddj), None).unwrap();
        let p = s.get(&id).unwrap();
        let own = s.own_part(&p, &p.file);
        assert_eq!(own.inputs, ddj.own.inputs);
        assert_eq!(own.meters, ddj.own.meters);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The DDJ-FLX4 by its port's name: Pioneer's lines under its own, its start
    /// message repeated every 200 ms, and its own sampler pad layout and BEAT FX.
    #[test]
    fn the_flx4() {
        let s = Store::new(None);
        let p = s.find("DDJ-FLX4:DDJ-FLX4 MIDI 1 24:0", None, None).unwrap();
        assert_eq!(p.id.model.as_deref(), Some("ddj-flx4"));
        assert_eq!(p.file.start_every_ms, Some(200));
        assert_eq!(
            p.file.start,
            vec![vec![
                0xF0, 0x00, 0x40, 0x05, 0x00, 0x00, 0x04, 0x05, 0x00, 0x50, 0x02, 0xF7
            ]]
        );
        let cmd = |k: (u8, u8)| p.file.inputs.get(&k).map(|l| l.control.name().0);
        assert_eq!(
            cmd((0x90, 0x0B)).as_deref(),
            Some("deck_1_play"),
            "Pioneer's"
        );
        assert_eq!(cmd((0x97, 0x34)).as_deref(), Some("sampler_9_play"));
        assert_eq!(cmd((0x99, 0x30)).as_deref(), Some("sampler_5_play"));
        assert_eq!(cmd((0x9A, 0x37)).as_deref(), Some("sampler_16_stop"));
        assert_eq!(cmd((0x95, 0x11)).as_deref(), Some("fx_1_channel_deck_2"));
        assert_eq!(cmd((0x94, 0x11)), None, "not on the FLX4");
        assert_eq!(cmd((0xB6, 0x0D)), None, "no headphone level");
        // Written back, it reads the same.
        let (_, again) = from_json(&file_json(serde_json::Map::new(), &p.own)).unwrap();
        assert_eq!(again.start_every_ms, Some(200));
        assert_eq!(again.inputs, p.own.inputs);
    }

    #[test]
    fn keys_and_names() {
        let id = MappingId::parse("mine:pioneer/flx4").unwrap();
        assert_eq!(
            (id.source, id.maker.as_str(), id.model.as_deref()),
            (Source::Mine, "pioneer", Some("flx4"))
        );
        assert_eq!(id.key(), "mine:pioneer/flx4");
        assert_eq!(MappingId::parse("bundled:pioneer").unwrap().model, None);
        assert!(MappingId::parse("nonsense").is_none());
        assert_eq!(slug(" Pioneer DJ!  FLX4 "), "pioneer-dj-flx4");
        assert_eq!(
            device_key("DDJ-400:DDJ-400 MIDI 1 20:0", None),
            "port:DDJ-400:DDJ-400 MIDI 1"
        );
        assert_eq!(device_key("x", Some("2b73:0026")), "usb:2b73:0026");
    }
}
