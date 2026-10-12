//! The keyboard map: which key does what, read from a data file (`keymap.txt`, built
//! in), as controls from the registry ([`super::bus`]). The key help is made from it.
//!
//! Keys are kept as names here (core doesn't know egui); the UI turns them into its
//! keys once, when it loads the map.

use earshot_engine::{DeckId, MasterParam};

use super::bus::{Control, DeckControl, Kind, LibraryControl, deck_letter};
use super::command::Command;

/// The built-in keymap.
pub const BUILT_IN: &str = include_str!("keymap.txt");

/// Which of the app's modes a binding works in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Perform,
    Prepare,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    /// `None`: both modes.
    pub mode: Option<Mode>,
    /// The key's name as the file has it (`A`, `;`, `Left`).
    pub key: String,
    /// The key as the UI names it (see [`Keymap::spell_keys`]); to begin with, as the
    /// file has it.
    pub code: String,
    pub shift: bool,
    pub control: Control,
    /// The number after the control, if any.
    pub arg: Option<f64>,
}

impl Binding {
    /// The value a press sends: the number, unless that's the control's parameter.
    fn value(&self) -> f64 {
        match self.control {
            Control::Deck(_, c) if c.parametrised() => 1.0,
            _ => self.arg.unwrap_or(1.0),
        }
    }

    /// What pressing the key sends.
    pub fn press(&self) -> Option<Command> {
        self.control.command(self.value())
    }

    /// What letting go sends: only buttons that act while held (cue, hot cues,
    /// nudges) say anything.
    pub fn release(&self) -> Option<Command> {
        match self.control.spec().kind {
            Kind::Button => self.control.command(0.0),
            _ => None,
        }
    }

    /// Whether holding the key down repeats it (browsing does; decks don't).
    pub fn repeats(&self) -> bool {
        matches!(self.control.spec().kind, Kind::Relative)
    }

    pub fn describe(&self) -> String {
        self.control.describe(self.arg)
    }

    /// The key as the help shows it: `Shift ←`.
    pub fn label(&self) -> String {
        let key = match self.key.as_str() {
            "Left" => "←",
            "Right" => "→",
            "Up" => "↑",
            "Down" => "↓",
            k => k,
        };
        if self.shift {
            format!("Shift {key}")
        } else {
            key.to_string()
        }
    }

    fn applies(&self, mode: Mode) -> bool {
        self.mode.is_none_or(|m| m == mode)
    }

    /// Whether `other` is the same key in a mode this one works in too.
    fn clashes(&self, other: &Binding) -> bool {
        self.code == other.code
            && self.shift == other.shift
            && (self.mode.is_none() || other.mode.is_none() || self.mode == other.mode)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Keymap {
    pub bindings: Vec<Binding>,
}

/// One column of the key help: a title and (key, what it does) rows.
pub struct HelpSection {
    pub title: String,
    pub deck: Option<DeckId>,
    pub rows: Vec<(String, String)>,
}

impl Keymap {
    /// The map built into the app.
    pub fn built_in() -> Keymap {
        Keymap::parse(BUILT_IN).expect("the built-in keymap parses")
    }

    pub fn parse(text: &str) -> Result<Keymap, String> {
        let mut bindings = vec![];
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let b = parse_line(line).map_err(|e| format!("keymap line {}: {e}", n + 1))?;
            if bindings.iter().any(|o: &Binding| o.clashes(&b)) {
                return Err(format!(
                    "keymap line {}: {} is bound twice",
                    n + 1,
                    b.label()
                ));
            }
            bindings.push(b);
        }
        Ok(Keymap { bindings })
    }

    /// Name every key as the UI does (`name` gives the UI's name for a key the file
    /// names, e.g. `;` → `Semicolon`), so [`Keymap::find`] can take the UI's names.
    /// Fails on a key the UI doesn't know, or two names for one key bound twice.
    pub fn spell_keys(mut self, name: impl Fn(&str) -> Option<String>) -> Result<Keymap, String> {
        for i in 0..self.bindings.len() {
            let b = &self.bindings[i];
            let code = name(&b.key).ok_or_else(|| format!("keymap: no key called {}", b.key))?;
            self.bindings[i].code = code;
            let b = &self.bindings[i];
            if self.bindings[..i].iter().any(|o| o.clashes(b)) {
                return Err(format!("keymap: {} is bound twice", b.label()));
            }
        }
        Ok(self)
    }

    /// What a key does in `mode`. A shifted key with no binding of its own does what
    /// it does unshifted.
    pub fn find(&self, mode: Mode, key: &str, shift: bool) -> Option<&Binding> {
        let find = |shift| {
            self.bindings
                .iter()
                .find(|b| b.applies(mode) && b.shift == shift && b.code == key)
        };
        find(shift).or_else(|| if shift { find(false) } else { None })
    }

    /// The key help for `mode`: one section per deck (and the library), in the
    /// file's order. Shift + a hot cue key clearing it goes in a note, not a row each.
    pub fn help(&self, mode: Mode) -> Vec<HelpSection> {
        let mut sections: Vec<HelpSection> = vec![];
        for b in self.bindings.iter().filter(|b| b.applies(mode)) {
            if let Control::Deck(_, DeckControl::HotcueClear(n)) = b.control
                && b.shift
                && self.bindings.iter().any(|o| {
                    o.applies(mode)
                        && !o.shift
                        && o.key == b.key
                        && matches!(o.control, Control::Deck(_, DeckControl::Hotcue(m)) if m == n)
                })
            {
                continue;
            }
            let (title, deck) = match b.control {
                Control::Deck(d, _) => match deck_letter(d) {
                    Some(l) => (format!("Deck {l}"), Some(d)),
                    None => ("Player".to_string(), Some(d)),
                },
                Control::Master(_) => ("Mixer".to_string(), None),
                Control::Library(_) => ("Library".to_string(), None),
            };
            let row = (b.label(), b.describe());
            match sections.iter_mut().find(|s| s.title == title) {
                Some(s) => s.rows.push(row),
                None => sections.push(HelpSection {
                    title,
                    deck,
                    rows: vec![row],
                }),
            }
        }
        sections
    }
}

fn parse_line(line: &str) -> Result<Binding, String> {
    let words: Vec<&str> = line.split_whitespace().collect();
    let [mode, key, target, control, rest @ ..] = words.as_slice() else {
        return Err("expected <mode> <key> <target> <control> [<number>]".into());
    };
    let mode = match *mode {
        "perform" => Some(Mode::Perform),
        "prepare" => Some(Mode::Prepare),
        "any" => None,
        m => return Err(format!("unknown mode {m}")),
    };
    let (shift, key) = match key.strip_prefix("Shift+") {
        Some(k) => (true, k),
        None => (false, *key),
    };
    let arg = match rest {
        [] => None,
        [n] => Some(
            n.parse::<f64>()
                .map_err(|_| format!("{n} isn't a number"))?,
        ),
        _ => return Err("too many words".into()),
    };
    let unknown = || format!("no control {control} on {target}");
    let control = match *target {
        "library" => Control::Library(
            [
                LibraryControl::Browse,
                LibraryControl::TogglePane,
                LibraryControl::Search,
            ]
            .into_iter()
            .find(|c| c.spec().name == *control)
            .ok_or_else(unknown)?,
        ),
        "mixer" => Control::Master(
            [
                MasterParam::Crossfader,
                MasterParam::Gain,
                MasterParam::Balance,
                MasterParam::HeadGain,
                MasterParam::HeadMix,
            ]
            .into_iter()
            .find(|p| Control::Master(*p).spec().name == *control)
            .ok_or_else(unknown)?,
        ),
        t => {
            let deck = match t {
                "prepare" => DeckId::PREPARE,
                _ => t
                    .strip_prefix("deck")
                    .and_then(|n| n.parse::<usize>().ok())
                    .and_then(|n| DeckId::new(n.checked_sub(1)?))
                    .ok_or_else(|| format!("unknown target {t}"))?,
            };
            Control::Deck(deck, DeckControl::parse(control, arg).ok_or_else(unknown)?)
        }
    };
    Ok(Binding {
        mode,
        key: key.to_string(),
        code: key.to_string(),
        shift,
        control,
        arg,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::command::{DeckCmd, ViewCmd};
    use crate::core::player::GridOp;
    use earshot_engine::{DeckCmd as EngineCmd, Press, Set};

    fn deck(i: usize) -> DeckId {
        DeckId::new(i).unwrap()
    }

    fn press(mode: Mode, key: &str, shift: bool) -> Option<Command> {
        Keymap::built_in().find(mode, key, shift)?.press()
    }

    #[test]
    fn keys_are_spelt_as_the_ui_names_them() {
        let ui_name = |n: &str| egui::Key::from_name(n).map(|k| k.name().to_string());
        let km = Keymap::built_in().spell_keys(ui_name).unwrap();
        assert!(km.find(Mode::Perform, "Semicolon", false).is_some());
        assert!(km.find(Mode::Perform, "Comma", false).is_some());
        assert!(km.find(Mode::Perform, ";", false).is_none());
        // Two spellings of one key are still one key.
        let twice = Keymap::parse("perform ; deck1 play\nperform Semicolon deck2 play").unwrap();
        assert!(
            twice
                .spell_keys(ui_name)
                .unwrap_err()
                .contains("bound twice")
        );
        let bad = Keymap::parse("perform Hyper deck1 play").unwrap();
        assert!(
            bad.spell_keys(ui_name)
                .unwrap_err()
                .contains("no key called Hyper")
        );
    }

    #[test]
    fn perform_keys_play_the_decks() {
        use Mode::Perform;
        assert!(matches!(
            press(Perform, "S", false),
            Some(Command::Deck(d, DeckCmd::Engine(EngineCmd::Play(Set::Toggle)))) if d == deck(0)
        ));
        assert!(matches!(
            press(Perform, ";", false),
            Some(Command::Deck(d, DeckCmd::Engine(EngineCmd::Pfl(Set::Toggle)))) if d == deck(1)
        ));
        assert!(matches!(
            press(Perform, "W", false),
            Some(Command::Deck(d, DeckCmd::Jump { forward: true })) if d == deck(0)
        ));
        assert!(matches!(
            press(Perform, "E", false),
            Some(Command::Deck(_, DeckCmd::LoopButton))
        ));
        // A hot cue acts while held; Shift clears it.
        let km = Keymap::built_in();
        let b = km.find(Perform, "8", false).unwrap();
        assert!(matches!(
            b.press(),
            Some(Command::Deck(d, DeckCmd::Engine(EngineCmd::Hotcue { slot: 1, press: Press::Down }))) if d == deck(1)
        ));
        assert!(matches!(
            b.release(),
            Some(Command::Deck(
                _,
                DeckCmd::Engine(EngineCmd::Hotcue {
                    slot: 1,
                    press: Press::Up
                })
            ))
        ));
        assert!(matches!(
            press(Perform, "8", true),
            Some(Command::Deck(_, DeckCmd::Engine(EngineCmd::HotcueClear(1))))
        ));
        // Toggles say nothing on release.
        assert!(km.find(Perform, "S", false).unwrap().release().is_none());
        // Shift with no binding of its own: the unshifted key.
        assert!(matches!(
            press(Perform, "S", true),
            Some(Command::Deck(
                _,
                DeckCmd::Engine(EngineCmd::Play(Set::Toggle))
            ))
        ));
        assert!(matches!(
            press(Perform, "Right", true),
            Some(Command::View(ViewCmd::LoadSelected(d))) if d == deck(1)
        ));
        assert!(press(Perform, "C", false).is_none(), "C is Prepare's");
    }

    #[test]
    fn prepare_keys() {
        use Mode::Prepare;
        let p = DeckId::PREPARE;
        assert!(matches!(
            press(Prepare, "Space", false),
            Some(Command::Deck(d, DeckCmd::Engine(EngineCmd::Play(Set::Toggle)))) if d == p
        ));
        assert!(matches!(
            press(Prepare, "Left", false),
            Some(Command::Deck(d, DeckCmd::Jump { forward: false })) if d == p
        ));
        assert!(matches!(
            press(Prepare, "Left", true),
            Some(Command::Deck(_, DeckCmd::Grid(GridOp::Nudge(ms)))) if ms == -10.0
        ));
        assert!(matches!(
            press(Prepare, "C", false),
            Some(Command::Deck(_, DeckCmd::Engine(EngineCmd::SetCue(None))))
        ));
        assert!(matches!(
            press(Prepare, "M", false),
            Some(Command::Deck(_, DeckCmd::SaveMemory))
        ));
        // 6-9: loops of 1, 2, 4 and 8 bars.
        for (key, beats) in [("6", 4.0), ("7", 8.0), ("8", 16.0), ("9", 32.0)] {
            assert!(
                matches!(
                    press(Prepare, key, false),
                    Some(Command::Deck(d, DeckCmd::Engine(EngineCmd::BeatLoop(b)))) if b == beats && d == p
                ),
                "{key}"
            );
        }
        assert!(matches!(
            press(Prepare, "Enter", false),
            Some(Command::View(ViewCmd::LoadSelected(d))) if d == p
        ));
        // Browsing repeats while held, in both modes.
        let km = Keymap::built_in();
        for mode in [Mode::Prepare, Mode::Perform] {
            let b = km.find(mode, "Down", false).unwrap();
            assert!(b.repeats());
            assert!(matches!(b.press(), Some(Command::View(ViewCmd::Browse(1)))));
            assert!(matches!(
                km.find(mode, "Up", false).unwrap().press(),
                Some(Command::View(ViewCmd::Browse(-1)))
            ));
        }
        assert!(!km.find(Prepare, "Space", false).unwrap().repeats());
    }

    #[test]
    fn help_comes_from_the_map() {
        let km = Keymap::built_in();
        let help = km.help(Mode::Perform);
        let titles: Vec<&str> = help.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["Deck A", "Deck B", "Library"]);
        let a = &help[0].rows;
        assert!(a.contains(&("S".into(), "play / pause".into())));
        assert!(a.contains(&("4".into(), "hot cue 4".into())));
        assert!(a.contains(&("Shift ←".into(), "load to deck A".into())));
        assert!(!a.iter().any(|r| r.1.starts_with("clear")), "in the note");
        let help = km.help(Mode::Prepare);
        let player = &help[0].rows;
        assert_eq!(help[0].title, "Player");
        assert!(player.contains(&("6".into(), "loop 1 bar".into())));
        assert!(player.contains(&("9".into(), "loop 8 bars".into())));
        assert!(player.contains(&("Shift →".into(), "nudge the grid 10 ms later".into())));
        assert!(player.contains(&("M".into(), "save the cue (or loop) to memory".into())));
        assert!(
            help[1]
                .rows
                .contains(&("↑".into(), "previous track".into()))
        );
    }

    #[test]
    fn bad_lines_say_where() {
        for (text, wants) in [
            ("perform A deck1", "expected"),
            ("sometimes A deck1 play", "unknown mode"),
            ("perform A deck9 play", "unknown target"),
            ("perform A deck1 dance", "no control dance"),
            ("perform A deck1 hotcue", "no control hotcue"),
            ("perform A deck1 beatloop x", "isn't a number"),
            ("perform A deck1 play\nany A library search", "bound twice"),
        ] {
            let e = Keymap::parse(text).unwrap_err();
            assert!(e.contains(wants), "{text}: {e}");
            assert!(e.starts_with("keymap line"), "{e}");
        }
        // The same key in two modes is fine.
        assert!(Keymap::parse("perform C deck1 play\nprepare C prepare set_cue").is_ok());
    }
}
