//! Mixxx's controller mappings, read straight from Mixxx's own files (never copied:
//! they're GPL). Phase 0 of docs/controllers/README.md: find the device's mapping,
//! list its bindings, and say for each what earshot would do with it: send one of
//! its controls, need the mapping's script, or nothing yet (a missing control).
//!
//! Nothing here acts on a message: the hardcoded table in `core::midi` still does
//! that. This is what the Controller mode's Mapping tab looks messages up in.

use std::path::{Path, PathBuf};

use super::bus::Control;

/// Where Mixxx keeps mappings: the user's own first, then the installed ones.
fn dirs() -> Vec<PathBuf> {
    let mut v = vec![];
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        v.push(home.join(".mixxx/controllers"));
        v.push(home.join(".local/share/mixxx/controllers"));
        v.push(home.join("git/mixxx/res/controllers"));
    }
    v.push("/usr/share/mixxx/controllers".into());
    v.push("/usr/local/share/mixxx/controllers".into());
    v.push("/Applications/Mixxx.app/Contents/Resources/controllers".into());
    v
}

/// One `<control>`: a message the controller sends, and what Mixxx does with it.
#[derive(Clone, Debug)]
pub struct Binding {
    /// The mapping author's words, e.g. "PLAY/PAUSE (DECK1) - press - Play/Pause".
    pub description: String,
    pub group: String,
    /// A control name, or a script function (`PioneerDDJ400.jogTurn`).
    pub key: String,
    pub status: u8,
    pub midino: u8,
    /// The `<options>` tags, e.g. `Normal`, `Script-Binding`, `SelectKnob`.
    pub options: Vec<String>,
}

/// What earshot does with a binding, by Mixxx's names alone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Outcome {
    /// One of earshot's controls.
    Control(Control),
    /// A function in the mapping's JS: needs the script engine (phase 2).
    Script,
    /// A control earshot doesn't have yet.
    Missing,
}

impl Outcome {
    pub fn label(self) -> &'static str {
        match self {
            Outcome::Control(_) => "works",
            Outcome::Script => "needs JS",
            Outcome::Missing => "missing",
        }
    }
}

impl Binding {
    pub fn script(&self) -> bool {
        self.options
            .iter()
            .any(|o| o.eq_ignore_ascii_case("Script-Binding"))
    }

    pub fn outcome(&self) -> Outcome {
        if self.script() {
            return Outcome::Script;
        }
        match Control::from_mixxx(&self.group, &self.key) {
            Some(c) => Outcome::Control(c),
            None => Outcome::Missing,
        }
    }

    /// `[Channel1] play`, or the script function.
    pub fn target(&self) -> String {
        if self.script() {
            self.key.clone()
        } else {
            format!("{} {}", self.group, self.key)
        }
    }
}

/// One `.midi.xml` file.
#[derive(Clone, Debug)]
pub struct Mapping {
    pub path: PathBuf,
    /// `<info><name>`, e.g. "Pioneer DDJ-400".
    pub name: String,
    /// `<controller id>`, e.g. "DDJ-400": what Mixxx matches the port name against.
    pub id: String,
    pub bindings: Vec<Binding>,
    /// How many `<output>`s (LEDs) it has.
    pub outputs: usize,
}

/// A mapping file found on disk, before it's read in full.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub path: PathBuf,
    pub name: String,
}

/// Every `.midi.xml` in Mixxx's folders, by name; a user's copy hides the
/// installed one of the same file name.
pub fn find_all() -> Vec<Found> {
    let mut out: Vec<Found> = vec![];
    for dir in dirs() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let path = e.path();
            let file = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !file.ends_with(".midi.xml") {
                continue;
            }
            if out.iter().any(|f| f.path.file_name() == path.file_name()) {
                continue;
            }
            let name = file.trim_end_matches(".midi.xml").to_string();
            out.push(Found { path, name });
        }
    }
    out.sort_by_key(|f| f.name.to_lowercase());
    out
}

/// The mapping for a MIDI port: the port name contains the file's `<controller id>`,
/// its name, or its name without the maker. The longest match wins, so "Pioneer
/// DDJ-400" beats a mapping whose id is just "PIONEER".
pub fn for_port(port: &str, all: &[Found]) -> Option<Mapping> {
    let port = squash(port);
    all.iter()
        .filter_map(|f| read(&f.path).ok())
        .filter_map(|m| {
            let model = m.name.split_once(' ').map_or(&*m.name, |(_, rest)| rest);
            [m.id.as_str(), m.name.as_str(), model]
                .into_iter()
                .map(squash)
                .filter(|k| k.len() >= 3 && port.contains(k.as_str()))
                .map(|k| k.len())
                .max()
                .map(|len| (len, m))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, m)| m)
}

/// Lower case, letters and digits only: "PIONEER DDJ-400 MIDI 1" → "pioneerddj400midi1".
fn squash(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

pub fn read(path: &Path) -> Result<Mapping, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse(&text, path)
}

fn parse(text: &str, path: &Path) -> Result<Mapping, String> {
    let doc = roxmltree::Document::parse(text).map_err(|e| format!("{}: {e}", path.display()))?;
    let root = doc.root_element();
    let child = |n: roxmltree::Node, tag: &str| {
        n.children()
            .find(|c| c.has_tag_name(tag))
            .and_then(|c| c.text())
            .map(|t| t.trim().to_string())
            .unwrap_or_default()
    };
    let name = root
        .children()
        .find(|c| c.has_tag_name("info"))
        .map(|i| child(i, "name"))
        .unwrap_or_default();
    let controller = root
        .children()
        .find(|c| c.has_tag_name("controller"))
        .ok_or("no <controller>")?;
    let id = controller.attribute("id").unwrap_or("").to_string();
    let mut bindings = vec![];
    let mut outputs = 0;
    for n in controller.descendants() {
        if n.has_tag_name("output") {
            outputs += 1;
        }
        if !n.has_tag_name("control") {
            continue;
        }
        let (Some(status), Some(midino)) = (hex(&child(n, "status")), hex(&child(n, "midino")))
        else {
            continue;
        };
        let options = n
            .children()
            .find(|c| c.has_tag_name("options"))
            .map(|o| {
                o.children()
                    .filter(|c| c.is_element())
                    .map(|c| c.tag_name().name().to_string())
                    .collect()
            })
            .unwrap_or_default();
        bindings.push(Binding {
            description: child(n, "description"),
            group: child(n, "group"),
            key: child(n, "key"),
            status,
            midino,
            options,
        });
    }
    Ok(Mapping {
        path: path.to_path_buf(),
        name,
        id,
        bindings,
        outputs,
    })
}

/// `0x9F` or `159`.
fn hex(s: &str) -> Option<u8> {
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u8::from_str_radix(h, 16).ok(),
        None => s.parse().ok(),
    }
}

impl Mapping {
    /// The binding a message matches. A note off (`0x8n`) is looked up as its note on,
    /// as Mixxx does.
    pub fn lookup(&self, bytes: &[u8]) -> Option<(usize, &Binding)> {
        let (&status, &data1) = (bytes.first()?, bytes.get(1)?);
        let find = |s: u8| {
            self.bindings
                .iter()
                .enumerate()
                .find(|(_, b)| b.status == s && b.midino == data1)
        };
        find(status).or_else(|| (status & 0xF0 == 0x80).then(|| find(status | 0x10)).flatten())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0"?>
<MixxxMIDIPreset schemaVersion="1">
  <info><name>Pioneer DDJ-400</name></info>
  <controller id="DDJ-400">
    <controls>
      <control>
        <description>PLAY (DECK1)</description>
        <group>[Channel1]</group><key>play</key>
        <status>0x90</status><midino>0x0B</midino>
        <options><normal/></options>
      </control>
      <control>
        <group>[Channel1]</group><key>PioneerDDJ400.jogTurn</key>
        <status>0xB0</status><midino>0x22</midino>
        <options><Script-Binding/></options>
      </control>
      <control>
        <group>[Channel1]</group><key>slip_enabled</key>
        <status>0x90</status><midino>0x40</midino>
      </control>
    </controls>
    <outputs><output><group>[Channel1]</group></output></outputs>
  </controller>
</MixxxMIDIPreset>"#;

    fn mapping() -> Mapping {
        parse(XML, Path::new("test.midi.xml")).unwrap()
    }

    #[test]
    fn reads_bindings_and_says_what_each_does() {
        let m = mapping();
        assert_eq!((m.name.as_str(), m.id.as_str()), ("Pioneer DDJ-400", "DDJ-400"));
        assert_eq!((m.bindings.len(), m.outputs), (3, 1));
        assert!(matches!(m.bindings[0].outcome(), Outcome::Control(_)));
        assert_eq!(m.bindings[1].outcome(), Outcome::Script);
        assert_eq!(m.bindings[2].outcome(), Outcome::Missing);
    }

    #[test]
    fn note_off_finds_its_note_on() {
        let m = mapping();
        assert_eq!(m.lookup(&[0x90, 0x0B, 0x7F]).unwrap().0, 0);
        assert_eq!(m.lookup(&[0x80, 0x0B, 0x00]).unwrap().0, 0);
        assert!(m.lookup(&[0x91, 0x0B, 0x7F]).is_none());
    }

    #[test]
    fn port_names_match_loosely() {
        assert!(squash("PIONEER DDJ-400:PIONEER DDJ-400 MIDI 1 24:0").contains(&squash("DDJ-400")));
    }
}

/// Against the installed Mixxx files, when there are any.
#[cfg(test)]
mod installed {
    use super::*;

    #[test]
    fn finds_the_ddj400_by_its_port_name() {
        let all = find_all();
        if !all.iter().any(|f| f.name == "Pioneer-DDJ-400") {
            return;
        }
        let t = std::time::Instant::now();
        let m = for_port("PIONEER DDJ-400:PIONEER DDJ-400 MIDI 1 24:0", &all).unwrap();
        eprintln!("picked {} from {} files in {:?}", m.name, all.len(), t.elapsed());
        assert_eq!(m.id, "DDJ-400");
        let count = |o: fn(Outcome) -> bool| m.bindings.iter().filter(|b| o(b.outcome())).count();
        eprintln!(
            "{} bindings: {} work, {} need JS, {} missing; {} outputs",
            m.bindings.len(),
            count(|o| matches!(o, Outcome::Control(_))),
            count(|o| o == Outcome::Script),
            count(|o| o == Outcome::Missing),
            m.outputs
        );
        // Play on deck 1.
        let (_, b) = m.lookup(&[0x90, 0x0B, 0x7F]).unwrap();
        assert_eq!(b.key, "play");
    }
}
