//! The control registry: every control the decks, mixer and library have, with its
//! kind, range, default, how its value reads, how to read it from the engine's
//! snapshot, the [`Command`] it sends, and its Mixxx name. On-screen widgets, the
//! keymap and MIDI all go through here, so a control means the same thing however
//! it's touched.
//!
//! Values are in the engine's units (filter -1..1, EQ linear gain, …). Mixxx's
//! `[Group] key` names exist only for the MIDI edge ([`Control::mixxx`],
//! [`Control::from_mixxx`]).

use earshot_engine::{
    ChannelParam, DeckCmd as EngineCmd, DeckId, DeckSnap, EQ_HIGH, EQ_LOW, EQ_MID, EngineSnapshot,
    MasterParam, ParamId, Press, Set,
};

use super::command::{Command, DeckCmd, ViewCmd};
use super::player::GridOp;

// ---- values -------------------------------------------------------------------------

/// What a knob or fader controls: its range, default and how its value reads.
#[derive(Clone, Copy)]
pub struct Range {
    pub name: &'static str,
    pub min: f64,
    pub max: f64,
    /// Double-click goes back here.
    pub default: f64,
    /// The value at the middle of the travel, for controls whose middle means "as is"
    /// (EQ, filter, balance). The travel either side maps linearly to min..centre and
    /// centre..max, and value arcs start from it.
    pub centre: Option<f64>,
    pub format: fn(f64) -> String,
}

impl Range {
    /// Value to travel, 0..1.
    pub fn to_pos(&self, v: f64) -> f64 {
        let v = v.clamp(self.min, self.max);
        match self.centre {
            Some(c) if v < c => 0.5 * (v - self.min) / (c - self.min),
            Some(c) => 0.5 + 0.5 * (v - c) / (self.max - c),
            None => (v - self.min) / (self.max - self.min),
        }
    }

    /// Travel (0..1) to value.
    pub fn from_pos(&self, p: f64) -> f64 {
        let p = p.clamp(0.0, 1.0);
        match self.centre {
            Some(c) if p < 0.5 => self.min + (c - self.min) * p * 2.0,
            Some(c) => c + (self.max - c) * (p - 0.5) * 2.0,
            None => self.min + (self.max - self.min) * p,
        }
    }

    /// Where value arcs and fills start, as travel.
    pub fn origin(&self) -> f64 {
        self.centre.map_or(0.0, |c| self.to_pos(c))
    }

    pub fn tooltip(&self, v: f64) -> String {
        format!(
            "{}: {}\nDrag (Shift: fine) or scroll · double-click resets to {}",
            self.name,
            (self.format)(v),
            (self.format)(self.default)
        )
    }
}

/// Linear gain as decibels.
pub fn fmt_db(v: f64) -> String {
    if v <= 1e-4 {
        "-∞ dB (kill)".into()
    } else {
        format!("{:+.1} dB", 20.0 * v.log10())
    }
}

pub fn fmt_percent(v: f64) -> String {
    format!("{:.0} %", v * 100.0)
}

/// The filter knob: -1 low-pass .. 0 off .. 1 high-pass.
pub fn fmt_filter(v: f64) -> String {
    if v.abs() < 0.02 {
        "off".into()
    } else {
        format!(
            "{} {:.0} %",
            if v < 0.0 { "low-pass" } else { "high-pass" },
            v.abs() * 100.0
        )
    }
}

/// -1 left .. 1 right.
pub fn fmt_balance(v: f64) -> String {
    if v.abs() < 0.005 {
        "centre".into()
    } else {
        format!(
            "{:.0} % {}",
            v.abs() * 100.0,
            if v < 0.0 { "left" } else { "right" }
        )
    }
}

fn fmt_beats(v: f64) -> String {
    format!("{v} beats")
}

pub const GAIN: Range = Range {
    name: "Gain",
    min: 0.0,
    max: 4.0,
    default: 1.0,
    centre: Some(1.0),
    format: fmt_db,
};
/// Low, mid, high, indexed by `EQ_LOW` etc.
pub const EQ: [Range; 3] = [
    Range {
        name: "Low",
        ..GAIN
    },
    Range {
        name: "Mid",
        ..GAIN
    },
    Range {
        name: "High",
        ..GAIN
    },
];
pub const FILTER: Range = Range {
    name: "Filter",
    min: -1.0,
    max: 1.0,
    default: 0.0,
    centre: Some(0.0),
    format: fmt_filter,
};
pub const VOLUME: Range = Range {
    name: "Volume",
    min: 0.0,
    max: 1.0,
    default: 1.0,
    centre: None,
    format: fmt_percent,
};
pub const CROSSFADER: Range = Range {
    name: "Crossfader",
    min: -1.0,
    max: 1.0,
    default: 0.0,
    centre: Some(0.0),
    format: fmt_balance,
};
pub const MAIN_GAIN: Range = Range {
    name: "Main level",
    min: 0.0,
    max: 5.0,
    default: 1.0,
    centre: Some(1.0),
    format: fmt_db,
};
pub const BALANCE: Range = Range {
    name: "Balance",
    ..CROSSFADER
};
pub const HEAD_GAIN: Range = Range {
    name: "Headphone level",
    ..MAIN_GAIN
};
pub const HEAD_MIX: Range = Range {
    name: "Headphone mix",
    min: -1.0,
    max: 1.0,
    default: -1.0,
    centre: None,
    format: |v| {
        format!(
            "{:.0} % cue, {:.0} % main",
            (1.0 - v) * 50.0,
            (v + 1.0) * 50.0
        )
    },
};
/// The tempo fader, -1..1 across the deck's tempo range (minus at the top).
pub const TEMPO: Range = Range {
    name: "Tempo",
    min: -1.0,
    max: 1.0,
    default: 0.0,
    centre: Some(0.0),
    format: |v| format!("{:+.0} % of the range", v * 100.0),
};
/// How far through the track, 0..1.
pub const POSITION: Range = Range {
    name: "Position",
    min: 0.0,
    max: 1.0,
    default: 0.0,
    centre: None,
    format: fmt_percent,
};
/// Beat jump and loop sizes, in beats.
pub const BEATS: Range = Range {
    name: "Beats",
    min: 1.0 / 32.0,
    max: 64.0,
    default: 4.0,
    centre: Some(4.0),
    format: fmt_beats,
};

/// Loop sizes the halve / double buttons step through, in beats.
pub const LOOP_SIZES: [f64; 9] = [0.125, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0];
/// Beat jump steps to pick from.
pub const JUMP_STEPS: [f64; 6] = [1.0, 2.0, 4.0, 8.0, 16.0, 32.0];
/// How much a nudge button speeds up or slows down while held.
pub const NUDGE: f64 = 0.04;

/// The next loop size up (`dir` 1) or down (-1) in [`LOOP_SIZES`].
pub fn step_size(s: f64, dir: i32) -> f64 {
    let i = LOOP_SIZES
        .iter()
        .position(|&x| x >= s)
        .unwrap_or(LOOP_SIZES.len() - 1) as i32;
    LOOP_SIZES[(i + dir).clamp(0, LOOP_SIZES.len() as i32 - 1) as usize]
}

// ---- controls -----------------------------------------------------------------------

/// How a control takes its value.
#[derive(Clone, Copy)]
pub enum Kind {
    /// 1 on press, 0 on release.
    Button,
    /// On/off. A press flips it, on the audio thread (see `Set::Toggle`), so two
    /// quick presses can't both read a stale state.
    Toggle,
    /// A knob or fader, or a value picked from a list.
    Continuous(Range),
    /// Steps either way, like a browse encoder: the value is how many.
    Relative,
}

/// One control's definition. Parametrised controls (a hot cue pad, a loop size) share
/// one, and take their parameter after the name in the keymap (`hotcue 1`).
#[derive(Clone, Copy)]
pub struct ControlSpec {
    /// Its name in the keymap file.
    pub name: &'static str,
    /// What it does, for the key help.
    pub help: &'static str,
    pub kind: Kind,
}

const fn spec(name: &'static str, help: &'static str, kind: Kind) -> ControlSpec {
    ControlSpec { name, help, kind }
}

/// A control on a particular deck, the mixer or the library.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Control {
    Deck(DeckId, DeckControl),
    Master(MasterParam),
    Library(LibraryControl),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DeckControl {
    Play,
    /// CDJ-style cue: set, or back to it and preview while held.
    Cue,
    /// The cue point at the playhead (snapped when quantising).
    SetCue,
    Sync,
    Quantize,
    Keylock,
    /// Headphone cue.
    Pfl,
    /// A hot cue pad (0-based).
    Hotcue(u8),
    HotcueClear(u8),
    /// Beat jump by the deck's step.
    JumpBack,
    JumpForward,
    /// Beat jump by the value, in beats.
    BeatJump,
    /// The deck's beat jump step, in beats.
    JumpSize,
    /// The loop button: a loop of the deck's loop size, or end the loop (whatever its
    /// size by now).
    Loop,
    /// The size the loop button makes, in beats.
    LoopSize,
    /// A loop this many beats long; the same again ends it, another size resizes it.
    BeatLoop(f64),
    LoopIn,
    LoopOut,
    /// Leave the loop, or go back into the last one.
    Reloop,
    /// Halve or double the loop, or the next loop's size when not looping.
    LoopHalve,
    LoopDouble,
    NudgeDown,
    NudgeUp,
    /// The tempo fader's next range.
    TempoRangeNext,
    Tempo,
    Position,
    Volume,
    Trim,
    /// An EQ band (`EQ_LOW` etc.).
    Eq(usize),
    Filter,
    /// Save the cue point (or the loop while looping) as a memory cue.
    SaveMemory,
    /// Move the deck's beat grid this many milliseconds (negative is earlier).
    GridNudge(f64),
    /// Load the track selected in the library.
    LoadSelected,
    /// Load the previous (-1) or next (1) track in the library's focused list.
    LoadStep(i32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LibraryControl {
    /// Move the selection by the value (negative is up).
    Browse,
    /// Library ↔ suggestions.
    TogglePane,
    /// Put the cursor in the search box.
    Search,
    /// Take back the last edit (metadata, collections, grids, the Flow).
    Undo,
    Redo,
}

impl DeckControl {
    pub fn spec(self) -> ControlSpec {
        use DeckControl::*;
        use Kind::*;
        match self {
            Play => spec("play", "play / pause", Toggle),
            Cue => spec("cue", "cue", Button),
            SetCue => spec("set_cue", "set the cue point here", Button),
            Sync => spec("sync", "sync", Toggle),
            Quantize => spec("quantize", "quantise", Toggle),
            Keylock => spec("keylock", "keylock", Toggle),
            Pfl => spec("pfl", "headphone cue", Toggle),
            Hotcue(_) => spec("hotcue", "hot cue", Button),
            HotcueClear(_) => spec("hotcue_clear", "clear hot cue", Button),
            JumpBack => spec("jump_back", "beat jump back", Button),
            JumpForward => spec("jump_forward", "beat jump forward", Button),
            BeatJump => spec("beatjump", "beat jump", Continuous(BEATS)),
            JumpSize => spec("jump_size", "beat jump step", Continuous(BEATS)),
            Loop => spec("loop", "loop on / off", Button),
            LoopSize => spec("loop_size", "loop size", Continuous(BEATS)),
            BeatLoop(_) => spec("beatloop", "loop", Button),
            LoopIn => spec("loop_in", "loop in", Button),
            LoopOut => spec("loop_out", "loop out", Button),
            Reloop => spec("reloop", "reloop / exit", Button),
            LoopHalve => spec("loop_halve", "halve the loop", Button),
            LoopDouble => spec("loop_double", "double the loop", Button),
            NudgeDown => spec("nudge_down", "nudge slower", Button),
            NudgeUp => spec("nudge_up", "nudge faster", Button),
            TempoRangeNext => spec("tempo_range", "next tempo range", Button),
            Tempo => spec("tempo", "tempo fader", Continuous(TEMPO)),
            Position => spec("position", "go to", Continuous(POSITION)),
            Volume => spec("volume", "channel fader", Continuous(VOLUME)),
            Trim => spec("trim", "gain", Continuous(GAIN)),
            Eq(b) => spec(
                ["eq_low", "eq_mid", "eq_high"][b.min(2)],
                "EQ",
                Continuous(EQ[b.min(2)]),
            ),
            Filter => spec("filter", "filter", Continuous(FILTER)),
            SaveMemory => spec("save_memory", "save the cue (or loop) to memory", Button),
            GridNudge(_) => spec("grid_nudge", "nudge the grid", Button),
            LoadSelected => spec("load_selected", "load the selected track", Button),
            LoadStep(_) => spec("load_step", "load the next track", Button),
        }
    }

    /// Every deck control, with sample parameters for the parametrised ones (for
    /// looking names up and for tests).
    pub const ALL: [DeckControl; 34] = {
        use DeckControl::*;
        [
            Play,
            Cue,
            SetCue,
            Sync,
            Quantize,
            Keylock,
            Pfl,
            Hotcue(0),
            HotcueClear(0),
            JumpBack,
            JumpForward,
            BeatJump,
            JumpSize,
            Loop,
            LoopSize,
            BeatLoop(4.0),
            LoopIn,
            LoopOut,
            Reloop,
            LoopHalve,
            LoopDouble,
            NudgeDown,
            NudgeUp,
            TempoRangeNext,
            Tempo,
            Position,
            Volume,
            Trim,
            Eq(EQ_LOW),
            Eq(EQ_MID),
            Eq(EQ_HIGH),
            Filter,
            SaveMemory,
            GridNudge(10.0),
        ]
    };

    /// By keymap name, with its parameter (`hotcue 1` is pad 0).
    pub fn parse(name: &str, arg: Option<f64>) -> Option<DeckControl> {
        use DeckControl::*;
        Some(match name {
            "hotcue" | "hotcue_clear" => {
                let n = arg.filter(|n| *n >= 1.0 && n.fract() == 0.0)? as u8 - 1;
                if name == "hotcue" {
                    Hotcue(n)
                } else {
                    HotcueClear(n)
                }
            }
            "beatloop" => BeatLoop(arg.filter(|b| *b > 0.0)?),
            "grid_nudge" => GridNudge(arg?),
            "load_selected" => LoadSelected,
            "load_step" => LoadStep(arg.filter(|n| n.fract() == 0.0 && *n != 0.0)? as i32),
            _ => *Self::ALL
                .iter()
                .find(|c| c.spec().name == name && !c.parametrised())?,
        })
    }

    /// Whether the keymap's number is part of the control (the pad, the loop size)
    /// rather than the value it sends.
    pub fn parametrised(self) -> bool {
        matches!(
            self,
            DeckControl::Hotcue(_)
                | DeckControl::HotcueClear(_)
                | DeckControl::BeatLoop(_)
                | DeckControl::GridNudge(_)
                | DeckControl::LoadStep(_)
        )
    }

    /// The control's value in a snapshot, for widgets and controller LEDs. `None` for
    /// controls the engine doesn't hold (the beat jump step and loop size live in the
    /// deck's session) and for plain buttons.
    pub fn get(self, d: &DeckSnap) -> Option<f64> {
        use DeckControl::*;
        let on = |b: bool| Some(b as u8 as f64);
        match self {
            Play => on(d.playing),
            Sync => on(d.sync),
            Quantize => on(d.quantize),
            Keylock => on(d.keylock),
            Pfl => on(d.pfl),
            Hotcue(n) => on(d.hotcues.get(n as usize).is_some_and(Option::is_some)),
            Loop | Reloop => on(d.loop_active),
            Tempo => Some(d.fader),
            Position if d.duration > 0.0 => Some(d.position / d.duration),
            Volume => Some(d.volume),
            Trim => Some(d.pregain),
            Eq(b) => d.eq.get(b).copied(),
            Filter => Some(d.filter),
            _ => None,
        }
    }

    /// What the control sends for `value` (a button's 1 / 0, a toggle's press, a
    /// knob's value). `None` when that value does nothing (most buttons' release).
    pub fn command(self, deck: DeckId, value: f64) -> Option<Command> {
        use DeckControl::*;
        let pressed = value > 0.0;
        let press = if pressed { Press::Down } else { Press::Up };
        let engine = |c| Some(Command::Deck(deck, DeckCmd::Engine(c)));
        let on_press = |c: DeckCmd| pressed.then_some(Command::Deck(deck, c));
        let channel = |p| Some(Command::Param(ParamId::Channel(deck, p), value));
        match self {
            Play if pressed => engine(EngineCmd::Play(Set::Toggle)),
            Sync if pressed => engine(EngineCmd::Sync(Set::Toggle)),
            Quantize if pressed => engine(EngineCmd::Quantize(Set::Toggle)),
            Keylock if pressed => engine(EngineCmd::Keylock(Set::Toggle)),
            Pfl if pressed => engine(EngineCmd::Pfl(Set::Toggle)),
            Play | Sync | Quantize | Keylock | Pfl => None,
            Cue => engine(EngineCmd::Cue(press)),
            SetCue => on_press(DeckCmd::SetCue),
            Hotcue(slot) => engine(EngineCmd::Hotcue { slot, press }),
            HotcueClear(slot) if pressed => engine(EngineCmd::HotcueClear(slot)),
            JumpBack => on_press(DeckCmd::Jump { forward: false }),
            JumpForward => on_press(DeckCmd::Jump { forward: true }),
            BeatJump if value != 0.0 => engine(EngineCmd::BeatJump(value)),
            JumpSize => Some(Command::Deck(deck, DeckCmd::SetJumpSize(value))),
            Loop => on_press(DeckCmd::LoopButton),
            LoopSize => Some(Command::Deck(deck, DeckCmd::SetLoopSize(value))),
            BeatLoop(beats) if pressed => engine(EngineCmd::BeatLoop(beats)),
            LoopIn if pressed => engine(EngineCmd::LoopIn),
            LoopOut if pressed => engine(EngineCmd::LoopOut),
            Reloop if pressed => engine(EngineCmd::Reloop(Set::Toggle)),
            LoopHalve => on_press(DeckCmd::ScaleLoop(0.5)),
            LoopDouble => on_press(DeckCmd::ScaleLoop(2.0)),
            NudgeDown => engine(EngineCmd::Nudge(if pressed { -NUDGE } else { 0.0 })),
            NudgeUp => engine(EngineCmd::Nudge(if pressed { NUDGE } else { 0.0 })),
            TempoRangeNext if pressed => engine(EngineCmd::TempoRangeNext),
            Tempo => engine(EngineCmd::TempoFader(value)),
            Position => Some(Command::Deck(deck, DeckCmd::SeekFraction(value))),
            Volume => channel(ChannelParam::Volume),
            Trim => channel(ChannelParam::Trim),
            Eq(b) => channel(ChannelParam::Eq(b)),
            Filter => channel(ChannelParam::Filter),
            SaveMemory => on_press(DeckCmd::SaveMemory),
            GridNudge(ms) => on_press(DeckCmd::Grid(GridOp::Nudge(ms))),
            LoadSelected if pressed => Some(Command::View(ViewCmd::LoadSelected(deck))),
            LoadStep(n) if pressed => Some(Command::View(ViewCmd::LoadAdjacent(deck, n))),
            _ => None,
        }
    }

    /// Mixxx's key for this control and the group it's in (relative to the deck's
    /// `[ChannelN]`).
    fn mixxx(self) -> Option<(MixxxGroup, String)> {
        use DeckControl::*;
        let key = |k: &str| Some((MixxxGroup::Channel, k.to_string()));
        match self {
            Play => key("play"),
            Cue => key("cue_default"),
            SetCue => key("cue_set"),
            Sync => key("sync_enabled"),
            Quantize => key("quantize"),
            Keylock => key("keylock"),
            Pfl => key("pfl"),
            Hotcue(n) => Some((MixxxGroup::Channel, format!("hotcue_{}_activate", n + 1))),
            HotcueClear(n) => Some((MixxxGroup::Channel, format!("hotcue_{}_clear", n + 1))),
            JumpBack => key("beatjump_backward"),
            JumpForward => key("beatjump_forward"),
            BeatJump => key("beatjump"),
            JumpSize => key("beatjump_size"),
            Loop => key("beatloop_activate"),
            LoopSize => key("beatloop_size"),
            BeatLoop(b) => Some((MixxxGroup::Channel, format!("beatloop_{b}_toggle"))),
            LoopIn => key("loop_in"),
            LoopOut => key("loop_out"),
            Reloop => key("reloop_toggle"),
            LoopHalve => key("loop_halve"),
            LoopDouble => key("loop_double"),
            NudgeDown => key("rate_temp_down"),
            NudgeUp => key("rate_temp_up"),
            TempoRangeNext => key("rateRange"),
            Tempo => key("rate"),
            Position => key("playposition"),
            Volume => key("volume"),
            Trim => key("pregain"),
            Eq(b) => Some((MixxxGroup::Eq, format!("parameter{}", b + 1))),
            // Mixxx's super1 is 0..1 with the filter off at 0.5: the same travel as
            // ours, so a MIDI knob maps onto it the same way.
            Filter => Some((MixxxGroup::QuickEffect, "super1".into())),
            LoadSelected => key("LoadSelectedTrack"),
            SaveMemory | GridNudge(_) | LoadStep(_) => None,
        }
    }
}

impl LibraryControl {
    pub fn spec(self) -> ControlSpec {
        match self {
            LibraryControl::Browse => spec("browse", "move through the list", Kind::Relative),
            LibraryControl::TogglePane => {
                spec("toggle_pane", "library / suggestions", Kind::Button)
            }
            LibraryControl::Search => spec("search", "search", Kind::Button),
            LibraryControl::Undo => spec("undo", "undo", Kind::Button),
            LibraryControl::Redo => spec("redo", "redo", Kind::Button),
        }
    }

    pub const ALL: [LibraryControl; 5] = [
        LibraryControl::Browse,
        LibraryControl::TogglePane,
        LibraryControl::Search,
        LibraryControl::Undo,
        LibraryControl::Redo,
    ];

    pub fn command(self, value: f64) -> Option<Command> {
        let view = |v| Some(Command::View(v));
        match self {
            LibraryControl::Browse if value != 0.0 => view(ViewCmd::Browse(value.round() as i32)),
            LibraryControl::TogglePane if value > 0.0 => view(ViewCmd::TogglePane),
            LibraryControl::Search if value > 0.0 => view(ViewCmd::FocusSearch),
            LibraryControl::Undo if value > 0.0 => Some(Command::Undo),
            LibraryControl::Redo if value > 0.0 => Some(Command::Redo),
            _ => None,
        }
    }
}

fn master_spec(p: MasterParam) -> ControlSpec {
    let c = |name, help, range| spec(name, help, Kind::Continuous(range));
    match p {
        MasterParam::Crossfader => c("crossfader", "crossfader", CROSSFADER),
        MasterParam::Gain => c("main_gain", "main level", MAIN_GAIN),
        MasterParam::Balance => c("balance", "balance", BALANCE),
        MasterParam::HeadGain => c("head_gain", "headphone level", HEAD_GAIN),
        MasterParam::HeadMix => c("head_mix", "headphone mix", HEAD_MIX),
    }
}

const MASTER: [(MasterParam, &str); 5] = [
    (MasterParam::Crossfader, "crossfader"),
    (MasterParam::Gain, "gain"),
    (MasterParam::Balance, "balance"),
    (MasterParam::HeadGain, "headGain"),
    (MasterParam::HeadMix, "headMix"),
];

/// Which Mixxx group a deck control is in.
#[derive(Clone, Copy, PartialEq)]
enum MixxxGroup {
    /// `[Channel1]`, or `[PreviewDeck1]` for Prepare's player.
    Channel,
    /// `[EqualizerRack1_[Channel1]_Effect1]`.
    Eq,
    /// `[QuickEffectRack1_[Channel1]]`.
    QuickEffect,
}

impl MixxxGroup {
    fn name(self, deck: DeckId) -> String {
        let channel = if deck.is_prepare() {
            "[PreviewDeck1]".to_string()
        } else {
            format!("[Channel{}]", deck.index() + 1)
        };
        match self {
            MixxxGroup::Channel => channel,
            MixxxGroup::Eq => format!("[EqualizerRack1_{channel}_Effect1]"),
            MixxxGroup::QuickEffect => format!("[QuickEffectRack1_{channel}]"),
        }
    }

    /// The group and deck a Mixxx group name stands for.
    fn parse(group: &str) -> Option<(MixxxGroup, DeckId)> {
        let (kind, channel) = if let Some(c) = group
            .strip_prefix("[EqualizerRack1_")
            .and_then(|g| g.strip_suffix("_Effect1]"))
        {
            (MixxxGroup::Eq, c)
        } else if let Some(c) = group
            .strip_prefix("[QuickEffectRack1_")
            .and_then(|g| g.strip_suffix(']'))
        {
            (MixxxGroup::QuickEffect, c)
        } else {
            (MixxxGroup::Channel, group)
        };
        let deck = if channel == "[PreviewDeck1]" {
            DeckId::PREPARE
        } else {
            let n: usize = channel
                .strip_prefix("[Channel")?
                .strip_suffix(']')?
                .parse()
                .ok()?;
            DeckId::new(n.checked_sub(1)?)?
        };
        Some((kind, deck))
    }
}

impl Control {
    pub fn spec(self) -> ControlSpec {
        match self {
            Control::Deck(_, c) => c.spec(),
            Control::Master(p) => master_spec(p),
            Control::Library(c) => c.spec(),
        }
    }

    pub fn range(self) -> Option<Range> {
        match self.spec().kind {
            Kind::Continuous(r) => Some(r),
            _ => None,
        }
    }

    /// The value as the engine has it (see [`DeckControl::get`]).
    pub fn get(self, snap: &EngineSnapshot) -> Option<f64> {
        let m = &snap.mixer;
        match self {
            Control::Deck(deck, c) => c.get(snap.deck(deck)),
            Control::Master(p) => Some(match p {
                MasterParam::Crossfader => m.crossfader,
                MasterParam::Gain => m.main_gain,
                MasterParam::Balance => m.balance,
                MasterParam::HeadGain => m.head_gain,
                MasterParam::HeadMix => m.head_mix,
            }),
            Control::Library(_) => None,
        }
    }

    /// What it sends for `value` (see [`DeckControl::command`]).
    pub fn command(self, value: f64) -> Option<Command> {
        match self {
            Control::Deck(deck, c) => c.command(deck, value),
            Control::Master(p) => Some(Command::Param(ParamId::Master(p), value)),
            Control::Library(c) => c.command(value),
        }
    }

    /// What it does, in a few words, with its parameter (`arg`: the keymap's number,
    /// for controls that send one).
    pub fn describe(self, arg: Option<f64>) -> String {
        match self {
            Control::Deck(_, DeckControl::Hotcue(n)) => format!("hot cue {}", n + 1),
            Control::Deck(_, DeckControl::HotcueClear(n)) => format!("clear hot cue {}", n + 1),
            Control::Deck(_, DeckControl::BeatLoop(b)) if b >= 4.0 && b % 4.0 == 0.0 => {
                let bars = b / 4.0;
                format!("loop {bars} bar{}", if bars == 1.0 { "" } else { "s" })
            }
            Control::Deck(_, DeckControl::BeatLoop(b)) => format!("loop {b} beats"),
            Control::Deck(_, DeckControl::GridNudge(ms)) => format!(
                "nudge the grid {} ms {}",
                ms.abs(),
                if ms < 0.0 { "earlier" } else { "later" }
            ),
            Control::Library(LibraryControl::Browse) => match arg {
                Some(v) if v < 0.0 => "previous track".into(),
                Some(_) => "next track".into(),
                None => self.spec().help.into(),
            },
            Control::Deck(_, DeckControl::LoadStep(n)) => {
                format!("load the {} track", if n < 0 { "previous" } else { "next" })
            }
            Control::Deck(deck, DeckControl::LoadSelected) => match deck_letter(deck) {
                Some(l) => format!("load to deck {l}"),
                None => "load the selected track".into(),
            },
            _ => self.spec().help.into(),
        }
    }

    /// Mixxx's `[Group]` and key for this control.
    pub fn mixxx(self) -> Option<(String, String)> {
        match self {
            Control::Deck(deck, c) => {
                let (group, key) = c.mixxx()?;
                Some((group.name(deck), key))
            }
            Control::Master(p) => {
                let key = MASTER.iter().find(|(m, _)| *m == p)?.1;
                Some(("[Master]".into(), key.into()))
            }
            Control::Library(LibraryControl::Browse) => {
                Some(("[Library]".into(), "MoveVertical".into()))
            }
            Control::Library(LibraryControl::TogglePane) => {
                Some(("[Library]".into(), "TogglePane".into()))
            }
            Control::Library(LibraryControl::Search) => None,
            // Mixxx has no undo; these names are earshot's own, for MIDI mappings.
            Control::Library(LibraryControl::Undo) => Some(("[Library]".into(), "Undo".into())),
            Control::Library(LibraryControl::Redo) => Some(("[Library]".into(), "Redo".into())),
        }
    }

    /// The control behind a Mixxx `[Group] key`.
    pub fn from_mixxx(group: &str, key: &str) -> Option<Control> {
        if group == "[Master]" {
            let p = MASTER.iter().find(|(_, k)| *k == key)?.0;
            return Some(Control::Master(p));
        }
        if group == "[Library]" {
            return LibraryControl::ALL
                .into_iter()
                .map(Control::Library)
                .find(|c| c.mixxx().is_some_and(|(_, k)| k == key));
        }
        let (kind, deck) = MixxxGroup::parse(group)?;
        // Parametrised keys: hotcue_N_activate, hotcue_N_clear, beatloop_X_toggle.
        let c = if let Some(rest) = key.strip_prefix("hotcue_") {
            let (n, action) = rest.split_once('_')?;
            let n = n.parse::<u8>().ok()?.checked_sub(1)?;
            match action {
                "activate" => DeckControl::Hotcue(n),
                "clear" => DeckControl::HotcueClear(n),
                _ => return None,
            }
        } else if let Some(size) = key
            .strip_prefix("beatloop_")
            .and_then(|r| r.strip_suffix("_toggle"))
        {
            DeckControl::BeatLoop(size.parse().ok().filter(|b: &f64| *b > 0.0)?)
        } else {
            *DeckControl::ALL
                .iter()
                .chain(&[DeckControl::LoadSelected])
                .filter(|c| !c.parametrised())
                .find(|c| c.mixxx().is_some_and(|(g, k)| g == kind && k == key))?
        };
        Some(Control::Deck(deck, c))
    }
}

/// "A", "B", … for decks; `None` for Prepare's player.
pub fn deck_letter(deck: DeckId) -> Option<char> {
    (!deck.is_prepare()).then(|| (b'A' + deck.index() as u8) as char)
}

#[cfg(test)]
mod tests {
    use super::*;
    use earshot_engine::{MAX_DECKS, TempoRange};

    fn deck(i: usize) -> DeckId {
        DeckId::new(i).unwrap()
    }

    #[test]
    fn ranges_hold_their_defaults() {
        let mut all: Vec<Control> = DeckControl::ALL
            .iter()
            .map(|c| Control::Deck(deck(0), *c))
            .collect();
        all.extend(MASTER.iter().map(|(p, _)| Control::Master(*p)));
        for c in all {
            let Some(r) = c.range() else { continue };
            assert!(r.min < r.max, "{c:?}");
            assert!((r.min..=r.max).contains(&r.default), "{c:?}");
            // Travel and value agree, and the default is where a reset lands.
            for v in [r.min, r.default, r.max] {
                assert!((r.from_pos(r.to_pos(v)) - v).abs() < 1e-9, "{c:?} {v}");
            }
            if let Some(centre) = r.centre {
                assert!((r.to_pos(centre) - 0.5).abs() < 1e-9, "{c:?}");
            }
        }
    }

    #[test]
    fn getters_read_the_snapshot() {
        let mut snap = EngineSnapshot::default();
        let d = &mut snap.decks[1];
        d.playing = true;
        d.volume = 0.25;
        d.eq = [0.0, 1.0, 2.0];
        d.filter = -0.5;
        d.fader = 0.3;
        d.position = 30.0;
        d.duration = 120.0;
        d.hotcues[2] = Some(12.0);
        d.loop_active = true;
        d.range = TempoRange::Ten;
        snap.mixer.crossfader = 0.75;
        snap.mixer.head_mix = 0.5;
        let get = |c: DeckControl| Control::Deck(deck(1), c).get(&snap);
        assert_eq!(get(DeckControl::Play), Some(1.0));
        assert_eq!(get(DeckControl::Sync), Some(0.0));
        assert_eq!(get(DeckControl::Volume), Some(0.25));
        assert_eq!(get(DeckControl::Eq(EQ_HIGH)), Some(2.0));
        assert_eq!(get(DeckControl::Eq(EQ_LOW)), Some(0.0));
        assert_eq!(get(DeckControl::Filter), Some(-0.5));
        assert_eq!(get(DeckControl::Tempo), Some(0.3));
        assert_eq!(get(DeckControl::Position), Some(0.25));
        assert_eq!(get(DeckControl::Hotcue(2)), Some(1.0));
        assert_eq!(get(DeckControl::Hotcue(3)), Some(0.0));
        assert_eq!(get(DeckControl::Loop), Some(1.0));
        // Held by the deck's session, not the engine.
        assert_eq!(get(DeckControl::JumpSize), None);
        // Deck A's values are its own.
        assert_eq!(
            Control::Deck(deck(0), DeckControl::Play).get(&snap),
            Some(0.0)
        );
        assert_eq!(
            Control::Master(MasterParam::Crossfader).get(&snap),
            Some(0.75)
        );
        assert_eq!(Control::Master(MasterParam::HeadMix).get(&snap), Some(0.5));
        snap.prepare.keylock = true;
        assert_eq!(
            Control::Deck(DeckId::PREPARE, DeckControl::Keylock).get(&snap),
            Some(1.0)
        );
    }

    #[test]
    fn toggles_always_send_toggle() {
        for c in [
            DeckControl::Play,
            DeckControl::Sync,
            DeckControl::Quantize,
            DeckControl::Keylock,
            DeckControl::Pfl,
        ] {
            let Some(Command::Deck(_, DeckCmd::Engine(cmd))) = c.command(deck(0), 1.0) else {
                panic!("{c:?}");
            };
            assert!(format!("{cmd:?}").contains("Toggle"), "{c:?}: {cmd:?}");
            assert!(c.command(deck(0), 0.0).is_none(), "{c:?} release");
        }
    }

    #[test]
    fn buttons_send_press_and_release() {
        let d = deck(1);
        let cmd = |c: DeckControl, v| c.command(d, v);
        assert!(matches!(
            cmd(DeckControl::Hotcue(3), 1.0),
            Some(Command::Deck(
                _,
                DeckCmd::Engine(EngineCmd::Hotcue {
                    slot: 3,
                    press: Press::Down
                })
            ))
        ));
        assert!(matches!(
            cmd(DeckControl::Hotcue(3), 0.0),
            Some(Command::Deck(
                _,
                DeckCmd::Engine(EngineCmd::Hotcue {
                    press: Press::Up,
                    ..
                })
            ))
        ));
        assert!(matches!(
            cmd(DeckControl::NudgeUp, 0.0),
            Some(Command::Deck(_, DeckCmd::Engine(EngineCmd::Nudge(0.0))))
        ));
        assert!(cmd(DeckControl::JumpForward, 0.0).is_none());
        assert!(matches!(
            cmd(DeckControl::Volume, 0.5),
            Some(Command::Param(ParamId::Channel(id, ChannelParam::Volume), 0.5)) if id == d
        ));
    }

    #[test]
    fn mixxx_names_round_trip() {
        let decks = DeckId::first(MAX_DECKS).chain([DeckId::PREPARE]);
        for d in decks {
            for c in DeckControl::ALL
                .iter()
                .chain(&[DeckControl::Hotcue(7), DeckControl::BeatLoop(0.5)])
            {
                let c = Control::Deck(d, *c);
                let Some((group, key)) = c.mixxx() else {
                    continue;
                };
                assert_eq!(Control::from_mixxx(&group, &key), Some(c), "{group} {key}");
            }
        }
        for (p, _) in MASTER {
            let c = Control::Master(p);
            let (g, k) = c.mixxx().unwrap();
            assert_eq!(Control::from_mixxx(&g, &k), Some(c));
        }
        assert_eq!(
            Control::from_mixxx("[EqualizerRack1_[Channel2]_Effect1]", "parameter1"),
            Some(Control::Deck(deck(1), DeckControl::Eq(EQ_LOW)))
        );
        assert_eq!(
            Control::from_mixxx("[PreviewDeck1]", "beatloop_16_toggle"),
            Some(Control::Deck(DeckId::PREPARE, DeckControl::BeatLoop(16.0)))
        );
        assert_eq!(
            Control::from_mixxx("[Library]", "MoveVertical"),
            Some(Control::Library(LibraryControl::Browse))
        );
        // Undo from a controller: earshot's own name, as a button press.
        let undo = Control::from_mixxx("[Library]", "Undo").unwrap();
        assert!(matches!(undo.command(1.0), Some(Command::Undo)));
        assert!(undo.command(0.0).is_none());
        let redo = Control::from_mixxx("[Library]", "Redo").unwrap();
        assert!(matches!(redo.command(1.0), Some(Command::Redo)));
        assert_eq!(Control::from_mixxx("[Channel9]", "play"), None);
        assert_eq!(Control::from_mixxx("[Channel1]", "nonsense"), None);
    }

    #[test]
    fn names_parse_back() {
        for c in DeckControl::ALL {
            let arg = match c {
                DeckControl::Hotcue(n) | DeckControl::HotcueClear(n) => Some(n as f64 + 1.0),
                DeckControl::BeatLoop(b) => Some(b),
                DeckControl::GridNudge(ms) => Some(ms),
                _ => None,
            };
            assert_eq!(DeckControl::parse(c.spec().name, arg), Some(c));
        }
        assert_eq!(DeckControl::parse("hotcue", Some(0.0)), None);
        assert_eq!(DeckControl::parse("hotcue", None), None);
        assert_eq!(DeckControl::parse("bogus", None), None);
    }

    #[test]
    fn loop_sizes_step() {
        assert_eq!(step_size(4.0, 1), 8.0);
        assert_eq!(step_size(4.0, -1), 2.0);
        assert_eq!(step_size(32.0, 1), 32.0);
        assert_eq!(step_size(0.125, -1), 0.125);
    }
}
