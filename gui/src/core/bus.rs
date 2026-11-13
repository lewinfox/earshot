//! The control registry: every control the decks, mixer and library have, with its
//! kind, range, default, how its value reads, how to read it from the engine's
//! snapshot, the [`Command`] it sends, and its Mixxx name. On-screen widgets, the
//! keymap and MIDI all go through here, so a control means the same thing however
//! it's touched.
//!
//! Each control has a name that goes from the general to the particular: where it is,
//! then what it is, then the part, then what to do to it (`deck_1_hot_cue_c_clear`,
//! `mixer_crossfader`, `fx_1_on`). A number or letter in the name says *which* (the
//! deck, the hot cue); a number after it is the value (`deck_1_beat_jump -4`). `docs/commands.md`
//! lists them all, made from here ([`commands_markdown`]).
//!
//! Values are in the engine's units (filter -1..1, EQ linear gain, …). Mixxx's
//! `[Group] key` names exist only for the MIDI edge ([`Control::mixxx`],
//! [`Control::from_mixxx`]).

use std::time::Instant;

use earshot_engine::{
    ChannelParam, DeckCmd as EngineCmd, DeckId, DeckSnap, EQ_HIGH, EQ_LOW, EQ_MID, EngineSnapshot,
    FX_UNITS, FxCmd, FxTarget, HOTCUES, MAX_DECKS, MasterParam, ParamId, Press, Set,
};

use super::command::{AppCmd, Command, DeckCmd, MixerCmd, ViewCmd};
use super::player::GridOp;
use super::view::PadMode;

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
    /// (EQ, filter, balance, headphone mix). The travel either side maps linearly to min..centre and
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
/// The master output: none to full, no boost, no centre.
pub const MASTER_GAIN: Range = Range {
    name: "Master level",
    min: 0.0,
    max: 1.0,
    default: 1.0,
    centre: None,
    format: fmt_db,
};
pub const BALANCE: Range = Range {
    name: "Balance",
    ..CROSSFADER
};
/// The headphones: as the master.
pub const HEAD_GAIN: Range = Range {
    name: "Headphone level",
    ..MASTER_GAIN
};
pub const HEAD_MIX: Range = Range {
    name: "Headphone mix",
    min: 0.0,
    max: 1.0,
    default: 0.5,
    centre: Some(0.5),
    format: |v| format!("{:.0} % cue, {:.0} % master", (1.0 - v) * 100.0, v * 100.0),
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
/// An effect unit's level: how loud the echo's repeats are, how deep Trans cuts.
pub const FX_LEVEL: Range = Range {
    name: "Effect level",
    min: 0.0,
    max: 1.0,
    default: 0.5,
    centre: None,
    format: fmt_percent,
};
/// A deck's key shift, in semitones.
pub const SEMITONES: Range = Range {
    name: "Key shift",
    min: -earshot_engine::KEY_SHIFT_MAX,
    max: earshot_engine::KEY_SHIFT_MAX,
    default: 0.0,
    centre: Some(0.0),
    format: |v| format!("{:+.0} semitones", v),
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

/// The word after a control's name.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Word {
    Num(f64),
    /// The deck's configured beat jump or loop size (`size`, or `-size` for back).
    Size { back: bool },
}

/// A control on a particular deck, the mixer or the library.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Control {
    Deck(DeckId, DeckControl),
    Master(MasterParam),
    /// An effect unit (0-based, below `FX_UNITS`).
    Fx(usize, FxControl),
    Library(LibraryControl),
    /// What's on screen.
    Ui(UiControl),
}

/// A control over what's on screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum UiControl {
    /// Hide the decks so the library gets their room (the waveforms stay), or bring
    /// them back: Mixxx's maximised library, Pioneer's VIEW.
    LibraryMaximize,
}

impl UiControl {
    pub const ALL: [UiControl; 1] = [UiControl::LibraryMaximize];

    pub fn spec(self) -> ControlSpec {
        match self {
            UiControl::LibraryMaximize => spec(
                "library_maximize_toggle",
                "hide the decks for a bigger library, or show them",
                Kind::Button,
            ),
        }
    }

    pub fn command(self, value: f64) -> Option<Command> {
        match self {
            UiControl::LibraryMaximize if value > 0.0 => {
                Some(Command::View(ViewCmd::ToggleLibraryMaximized))
            }
            UiControl::LibraryMaximize => None,
        }
    }

    /// Mixxx's `[Skin]` key.
    fn mixxx(self) -> &'static str {
        match self {
            UiControl::LibraryMaximize => "show_maximized_library",
        }
    }
}

/// A control on an effect unit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FxControl {
    On,
    Level,
    /// Halve or double the effect's length in beats.
    BeatsDown,
    BeatsUp,
    /// The next effect, or the one before.
    Next,
    Prev,
    /// Put the unit on the next deck, or the master mix.
    Target,
    /// Put the unit on this deck (0-based), or on the master mix: Pioneer's CH SELECT
    /// switch.
    TargetDeck(u8),
    TargetMaster,
    /// Turn the effect off and play the release effect (Pioneer's Release FX).
    Release,
    /// Follow the target's tempo (BPM auto mode), or run on a tempo tapped in on this
    /// button (BPM tap mode).
    TempoAuto,
    TempoTap,
}

impl FxControl {
    pub const ALL: [FxControl; 15] = [
        FxControl::On,
        FxControl::Release,
        FxControl::Level,
        FxControl::BeatsDown,
        FxControl::BeatsUp,
        FxControl::Next,
        FxControl::Prev,
        FxControl::Target,
        FxControl::TargetDeck(0),
        FxControl::TargetDeck(1),
        FxControl::TargetDeck(2),
        FxControl::TargetDeck(3),
        FxControl::TargetMaster,
        FxControl::TempoAuto,
        FxControl::TempoTap,
    ];

    pub fn spec(self) -> ControlSpec {
        use FxControl::*;
        match self {
            On => spec("on", "effect on / off", Kind::Toggle),
            Level => spec("level", "effect level", Kind::Continuous(FX_LEVEL)),
            BeatsDown => spec("beats_halve", "halve the effect's length", Kind::Button),
            BeatsUp => spec("beats_double", "double the effect's length", Kind::Button),
            Next => spec("effect_next", "next effect", Kind::Button),
            Prev => spec("effect_prev", "previous effect", Kind::Button),
            Target => spec("channel_next", "effect on the next deck / master", Kind::Button),
            TargetDeck(0) => spec("channel_deck_1", "effect on deck 1", Kind::Button),
            TargetDeck(1) => spec("channel_deck_2", "effect on deck 2", Kind::Button),
            TargetDeck(2) => spec("channel_deck_3", "effect on deck 3", Kind::Button),
            TargetDeck(_) => spec("channel_deck_4", "effect on deck 4", Kind::Button),
            TargetMaster => spec("channel_master", "effect on the master mix", Kind::Button),
            Release => spec("release", "effect off, with the release effect", Kind::Button),
            TempoAuto => spec("tempo_auto", "effect follows the track's tempo", Kind::Button),
            TempoTap => spec("tempo_tap", "tap the effect's tempo", Kind::Button),
        }
    }

    pub fn command(self, unit: usize, value: f64) -> Option<Command> {
        let fx = |c| Some(Command::Fx(unit, c));
        let pressed = value > 0.0;
        match self {
            FxControl::Level => fx(FxCmd::Level(value)),
            _ if !pressed => None,
            FxControl::On => fx(FxCmd::Enabled(Set::Toggle)),
            FxControl::BeatsDown => fx(FxCmd::BeatsStep(-1)),
            FxControl::BeatsUp => fx(FxCmd::BeatsStep(1)),
            FxControl::Next => fx(FxCmd::NextKind),
            FxControl::Prev => fx(FxCmd::PrevKind),
            FxControl::Target => fx(FxCmd::NextTarget),
            FxControl::TargetDeck(d) => fx(FxCmd::Target(FxTarget::Deck(DeckId::new(d as usize)?))),
            FxControl::TargetMaster => fx(FxCmd::Target(FxTarget::Master)),
            FxControl::Release => fx(FxCmd::Release),
            FxControl::TempoAuto => fx(FxCmd::TempoAuto),
            // Timed here: on the MIDI thread, as the message comes in.
            FxControl::TempoTap => Some(Command::App(AppCmd::FxTap {
                unit,
                at: std::time::Instant::now(),
            })),
        }
    }

    /// Mixxx's key, in `[EffectRack1_EffectUnitN]`. Mixxx has no beat lengths or
    /// targets to cycle; those names are earshot's own.
    fn mixxx(self) -> &'static str {
        match self {
            FxControl::On => "enabled",
            FxControl::Level => "mix",
            FxControl::Next => "next_chain",
            FxControl::Prev => "prev_chain",
            FxControl::BeatsDown => "beats_down",
            FxControl::BeatsUp => "beats_up",
            FxControl::Target => "next_target",
            // Mixxx sets group_[ChannelN]_enable per channel; these are earshot's own.
            FxControl::TargetDeck(0) => "target_deck_1",
            FxControl::TargetDeck(1) => "target_deck_2",
            FxControl::TargetDeck(2) => "target_deck_3",
            FxControl::TargetDeck(_) => "target_deck_4",
            FxControl::TargetMaster => "target_master",
            FxControl::Release => "release",
            FxControl::TempoAuto => "tempo_auto",
            FxControl::TempoTap => "tempo_tap",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DeckControl {
    Play,
    /// CDJ-style cue: set, or back to it and preview while held.
    Cue,
    /// The cue point at the playhead (snapped when quantising).
    SetCue,
    Sync,
    /// Make this deck the sync master (the one synced decks follow).
    SyncSetMaster,
    Quantize,
    Keylock,
    /// Shift the key to this many semitones (rounded), tempo untouched.
    KeyShift,
    /// Shift the key a semitone up or down, or back to the track's own.
    KeyShiftUp,
    KeyShiftDown,
    KeyShiftReset,
    /// Headphone cue.
    Pfl,
    /// A hot cue pad (0-based).
    Hotcue(u8),
    HotcueClear(u8),
    /// Beat jump by the deck's configured size (`beat_jump -size`, `beat_jump size`).
    JumpBack,
    JumpForward,
    /// Beat jump by the value, in beats (negative: back).
    BeatJump,
    /// A loop of the deck's configured loop size, or end the loop (whatever its size
    /// by now): `beat_loop size`.
    Loop,
    /// A loop this many beats long; the same again ends it, another size resizes it.
    BeatLoop(f64),
    LoopIn,
    LoopOut,
    /// Leave the loop, or go back into the last one.
    Reloop,
    /// SHIFT + RELOOP/EXIT: make the memory loop just called (or the nearest) the
    /// track's active loop, which turns itself on when played onto; or stop it being.
    LoopActiveToggle,
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
    /// The controller's pads are in this mode now (its pad mode buttons): the deck's
    /// pads page shows what they do.
    SetPadMode(PadMode),
    /// A performance pad (0-based) in a pad mode that keeps state of its own
    /// (keyboard, key shift: see `core::pads`), pressed or let go, with SHIFT held
    /// or not. Its own message per mode, as the DDJ-400 sends.
    ModePad { mode: PadMode, pad: u8, shift: bool },
    /// CUE/LOOP CALL ◀ / ▶: go to the memory cue before (after) the playhead; while
    /// looping, halve (double) the loop instead.
    CueJumpPrev,
    CueJumpNext,
    /// Delete the memory cue (or loop) just called: the one at the cue point.
    CueDelete,
    /// The controller's Shift button, held (1) or let go (0). It does nothing itself:
    /// a controller that sends its own messages for shifted buttons (the DDJ-400)
    /// needs no more, but its Shift still sends a message, so it has a name.
    Shift,
    /// Vinyl mode: the jog wheels' tops (and the mouse on the waveform) scratch. Off,
    /// they only bend.
    Vinyl,
    /// A hand on the record: the jog wheel's top touched (1) or let go (0). Only in
    /// vinyl mode.
    JogTouch,
    /// The jog wheel's top turned this many seconds of track (negative: back):
    /// scratching while touched in vinyl mode, else a bend.
    JogTurn,
    /// The jog wheel's side turned this many seconds of track: speed up or slow down
    /// while it turns.
    JogBend,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LibraryControl {
    /// Move the selection by the value (negative is up).
    Browse,
    /// Focus the next pane (library or Flow → suggestions → …), or the one before.
    PaneNext,
    PanePrev,
    /// Put the cursor in the search box.
    Search,
    /// Open the suggestions (tracks related to what's playing) on their top track,
    /// or close them.
    RelatedTracks,
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
            SetCue => spec("cue_set", "set the cue point here", Button),
            Sync => spec("beat_sync", "beat sync", Toggle),
            SyncSetMaster => spec("sync_set_master", "make this deck the sync master", Button),
            Quantize => spec("quantize", "quantise", Toggle),
            Keylock => spec("key_lock", "key lock", Toggle),
            KeyShift => spec("key_shift", "key shift", Continuous(SEMITONES)),
            KeyShiftUp => spec("key_shift_up", "key up a semitone", Button),
            KeyShiftDown => spec("key_shift_down", "key down a semitone", Button),
            KeyShiftReset => spec("key_shift_reset", "key back to the track's own", Button),
            Pfl => spec("headphone_cue", "headphone cue", Toggle),
            Hotcue(_) => spec("hot_cue", "hot cue", Button),
            HotcueClear(_) => spec("hot_cue_clear", "clear hot cue", Button),
            JumpBack => spec("beat_jump", "beat jump back", Button),
            JumpForward => spec("beat_jump", "beat jump forward", Button),
            BeatJump => spec("beat_jump", "beat jump", Continuous(BEATS)),
            Loop => spec("beat_loop", "loop on / off", Button),
            BeatLoop(_) => spec("beat_loop", "loop", Button),
            LoopIn => spec("loop_in", "loop in", Button),
            LoopOut => spec("loop_out", "loop out", Button),
            Reloop => spec("loop_exit", "leave the loop / reloop", Button),
            LoopActiveToggle => spec(
                "loop_active_toggle",
                "make the nearest memory loop the active loop, or not",
                Button,
            ),
            LoopHalve => spec("loop_halve", "halve the loop", Button),
            LoopDouble => spec("loop_double", "double the loop", Button),
            NudgeDown => spec("pitch_bend_down", "slow down while held", Button),
            NudgeUp => spec("pitch_bend_up", "speed up while held", Button),
            TempoRangeNext => spec("tempo_range_next", "next tempo range", Button),
            Tempo => spec("tempo", "tempo fader", Continuous(TEMPO)),
            Position => spec("position", "go to", Continuous(POSITION)),
            Volume => spec("channel_fader", "channel fader", Continuous(VOLUME)),
            Trim => spec("trim", "trim (gain)", Continuous(GAIN)),
            Eq(b) => spec(
                ["eq_low", "eq_mid", "eq_high"][b.min(2)],
                "EQ",
                Continuous(EQ[b.min(2)]),
            ),
            Filter => spec("filter", "filter", Continuous(FILTER)),
            SaveMemory => spec("cue_save_as_memory", "save the cue (or loop) to memory", Button),
            CueJumpPrev => spec(
                "cue_jump_to_prev",
                "previous memory cue (looping: halve the loop)",
                Button,
            ),
            CueJumpNext => {
                spec("cue_jump_to_next", "next memory cue (looping: double the loop)", Button)
            }
            CueDelete => spec("cue_delete", "delete the memory cue just called", Button),
            GridNudge(_) => spec("grid_nudge", "nudge the grid", Button),
            LoadSelected => spec("load_selected", "load the selected track", Button),
            LoadStep(_) => spec("load_step", "load the next track", Button),
            Shift => spec("shift", "shift (held)", Button),
            SetPadMode(m) => spec(
                match m {
                    PadMode::HotCue => "set_pad_mode_hot_cue",
                    PadMode::BeatLoop => "set_pad_mode_beat_loop",
                    PadMode::BeatJump => "set_pad_mode_beat_jump",
                    PadMode::Sampler => "set_pad_mode_sampler",
                    PadMode::Keyboard => "set_pad_mode_keyboard",
                    PadMode::PadFx1 => "set_pad_mode_pad_fx_1",
                    PadMode::PadFx2 => "set_pad_mode_pad_fx_2",
                    PadMode::KeyShift => "set_pad_mode_key_shift",
                },
                "pad mode",
                Button,
            ),
            ModePad { mode, shift, .. } => spec(
                match (mode, shift) {
                    (PadMode::KeyShift, false) => "key_shift_pad",
                    (PadMode::KeyShift, true) => "key_shift_pad_shift",
                    (_, false) => "keyboard_pad",
                    (_, true) => "keyboard_pad_shift",
                },
                if shift {
                    "pad with SHIFT: page before (7) / after (8); keyboard: pick again (1-6)"
                } else if mode == PadMode::KeyShift {
                    "key shift pad: set the key"
                } else {
                    "keyboard pad: pick the hot cue, then play it in this key"
                },
                Button,
            ),
            Vinyl => spec("vinyl_mode", "vinyl mode (jog wheels scratch)", Toggle),
            JogTouch => spec("scratch_hold", "hand on the record (scratch)", Button),
            JogTurn => spec("scratch", "scratch", Relative),
            JogBend => spec("pitch_bend", "bend the speed", Relative),
        }
    }

    /// Every deck control, with sample parameters for the parametrised ones (for
    /// looking names up and for tests).
    pub const ALL: [DeckControl; 58] = {
        use DeckControl::*;
        [
            Play,
            Cue,
            SetCue,
            Sync,
            SyncSetMaster,
            Quantize,
            Keylock,
            KeyShift,
            KeyShiftUp,
            KeyShiftDown,
            KeyShiftReset,
            Pfl,
            Hotcue(0),
            HotcueClear(0),
            JumpBack,
            JumpForward,
            BeatJump,
            Loop,
            BeatLoop(4.0),
            LoopIn,
            LoopOut,
            Reloop,
            LoopActiveToggle,
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
            CueJumpPrev,
            CueJumpNext,
            CueDelete,
            GridNudge(10.0),
            Vinyl,
            JogTouch,
            JogTurn,
            JogBend,
            SetPadMode(PadMode::HotCue),
            SetPadMode(PadMode::BeatLoop),
            SetPadMode(PadMode::BeatJump),
            SetPadMode(PadMode::Sampler),
            SetPadMode(PadMode::Keyboard),
            SetPadMode(PadMode::PadFx1),
            SetPadMode(PadMode::PadFx2),
            SetPadMode(PadMode::KeyShift),
            ModePad { mode: PadMode::Keyboard, pad: 0, shift: false },
            ModePad { mode: PadMode::Keyboard, pad: 0, shift: true },
            ModePad { mode: PadMode::KeyShift, pad: 0, shift: false },
            ModePad { mode: PadMode::KeyShift, pad: 0, shift: true },
            Shift,
        ]
    };

    /// By its name after the deck (`play`, `hot_cue_c`) and the word after that.
    fn parse(name: &str, word: Option<Word>) -> Option<DeckControl> {
        use DeckControl::*;
        let num = match word {
            Some(Word::Num(n)) => Some(n),
            _ => None,
        };
        if let Some(rest) = name.strip_prefix("hot_cue_") {
            let (n, clear) = match rest.strip_suffix("_clear") {
                Some(n) => (n, true),
                None => (rest, false),
            };
            let n = hot_cue_slot(n)?;
            return word.is_none().then_some(if clear { HotcueClear(n) } else { Hotcue(n) });
        }
        Some(match (name, word) {
            ("beat_jump", Some(Word::Size { back })) => {
                if back {
                    JumpBack
                } else {
                    JumpForward
                }
            }
            ("beat_jump", _) => BeatJump,
            ("beat_loop", Some(Word::Size { back: false })) => Loop,
            ("beat_loop", _) => BeatLoop(num.filter(|b| *b > 0.0)?),
            ("grid_nudge", _) => GridNudge(num?),
            ("load_step", _) => LoadStep(num.filter(|n| n.fract() == 0.0 && *n != 0.0)? as i32),
            ("keyboard_pad" | "keyboard_pad_shift" | "key_shift_pad" | "key_shift_pad_shift", _) => {
                let pad = num.filter(|n| n.fract() == 0.0 && (1.0..=8.0).contains(n))? as u8 - 1;
                ModePad {
                    mode: if name.starts_with("keyboard") {
                        PadMode::Keyboard
                    } else {
                        PadMode::KeyShift
                    },
                    pad,
                    shift: name.ends_with("_shift"),
                }
            }
            (_, Some(Word::Size { .. })) => return None,
            _ => *Self::ALL
                .iter()
                .chain(&[LoadSelected])
                .find(|c| c.spec().name == name && !c.parametrised() && !c.sized())?,
        })
    }

    /// Whether it acts by the deck's configured size (`size` after its name).
    fn sized(self) -> bool {
        matches!(
            self,
            DeckControl::JumpBack | DeckControl::JumpForward | DeckControl::Loop
        )
    }

    /// Its name after the deck (`hot_cue_c`), and the fixed word after that, if it
    /// has one (`-size`, the loop's `8`).
    fn name(self) -> (String, Option<String>) {
        use DeckControl::*;
        let name = self.spec().name;
        match self {
            Hotcue(n) => (format!("hot_cue_{}", hot_cue_letter(n).to_ascii_lowercase()), None),
            HotcueClear(n) => (
                format!("hot_cue_{}_clear", hot_cue_letter(n).to_ascii_lowercase()),
                None,
            ),
            JumpBack => (name.into(), Some("-size".into())),
            JumpForward | Loop => (name.into(), Some("size".into())),
            BeatLoop(b) => (name.into(), Some(b.to_string())),
            GridNudge(ms) => (name.into(), Some(ms.to_string())),
            LoadStep(n) => (name.into(), Some(n.to_string())),
            ModePad { pad, .. } => (name.into(), Some((pad + 1).to_string())),
            _ => (name.into(), None),
        }
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
                | DeckControl::ModePad { .. }
        )
    }

    /// The control's value in a snapshot, for widgets and controller LEDs. `None` for
    /// controls the engine doesn't hold and for plain buttons.
    pub fn get(self, d: &DeckSnap) -> Option<f64> {
        use DeckControl::*;
        let on = |b: bool| Some(b as u8 as f64);
        match self {
            Play => on(d.playing),
            Sync => on(d.sync),
            Quantize => on(d.quantize),
            Keylock => on(d.keylock),
            KeyShift => Some(d.key_shift),
            KeyShiftReset => on(d.key_shift != 0.0),
            Vinyl => on(d.vinyl),
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
            SyncSetMaster => pressed.then_some(Command::Mixer(MixerCmd::SyncLeader(Some(deck)))),
            Quantize if pressed => engine(EngineCmd::Quantize(Set::Toggle)),
            Keylock if pressed => engine(EngineCmd::Keylock(Set::Toggle)),
            Vinyl if pressed => engine(EngineCmd::Vinyl(Set::Toggle)),
            Pfl if pressed => engine(EngineCmd::Pfl(Set::Toggle)),
            Play | Sync | Quantize | Keylock | Pfl | Vinyl => None,
            KeyShift => engine(EngineCmd::KeyShift(value.round())),
            KeyShiftUp if pressed => engine(EngineCmd::KeyShiftBy(1.0)),
            KeyShiftDown if pressed => engine(EngineCmd::KeyShiftBy(-1.0)),
            KeyShiftReset if pressed => engine(EngineCmd::KeyShift(0.0)),
            Cue => engine(EngineCmd::Cue(press)),
            SetCue => on_press(DeckCmd::SetCue),
            Hotcue(slot) => engine(EngineCmd::Hotcue { slot, press }),
            HotcueClear(slot) if pressed => engine(EngineCmd::HotcueClear(slot)),
            JumpBack => on_press(DeckCmd::Jump { forward: false }),
            JumpForward => on_press(DeckCmd::Jump { forward: true }),
            BeatJump if value != 0.0 => engine(EngineCmd::BeatJump(value)),
            Loop => on_press(DeckCmd::LoopButton),
            BeatLoop(beats) if pressed => engine(EngineCmd::BeatLoop(beats)),
            LoopIn if pressed => engine(EngineCmd::LoopIn),
            LoopOut if pressed => engine(EngineCmd::LoopOut),
            Reloop if pressed => engine(EngineCmd::Reloop(Set::Toggle)),
            LoopActiveToggle => on_press(DeckCmd::ToggleActiveLoop),
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
            CueJumpPrev => on_press(DeckCmd::CallMemory { forward: false }),
            CueJumpNext => on_press(DeckCmd::CallMemory { forward: true }),
            CueDelete => on_press(DeckCmd::DeleteCalledMemory),
            GridNudge(ms) => on_press(DeckCmd::Grid(GridOp::Nudge(ms))),
            LoadSelected if pressed => Some(Command::View(ViewCmd::LoadSelected(deck))),
            LoadStep(n) if pressed => Some(Command::View(ViewCmd::LoadAdjacent(deck, n))),
            SetPadMode(m) if pressed => Some(Command::View(ViewCmd::PadMode(deck, m))),
            ModePad { mode, pad, shift } => Some(Command::View(ViewCmd::ModePad {
                deck,
                mode,
                pad,
                shift,
                down: pressed,
            })),
            JogTouch => engine(EngineCmd::Hand(pressed)),
            JogTurn if value != 0.0 => engine(EngineCmd::HandMove {
                by: value,
                at: Instant::now(),
            }),
            JogBend if value != 0.0 => engine(EngineCmd::Bend {
                by: value,
                at: Instant::now(),
            }),
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
            SyncSetMaster => key("sync_leader"),
            Quantize => key("quantize"),
            Keylock => key("keylock"),
            KeyShift => key("pitch_adjust"),
            KeyShiftUp => key("pitch_up"),
            KeyShiftDown => key("pitch_down"),
            KeyShiftReset => key("reset_key"),
            Pfl => key("pfl"),
            Hotcue(n) => Some((MixxxGroup::Channel, format!("hotcue_{}_activate", n + 1))),
            HotcueClear(n) => Some((MixxxGroup::Channel, format!("hotcue_{}_clear", n + 1))),
            JumpBack => key("beatjump_backward"),
            JumpForward => key("beatjump_forward"),
            BeatJump => key("beatjump"),
            Loop => key("beatloop_activate"),
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
            // Mixxx's mappings scratch through script functions; these names are
            // earshot's own, for MIDI mappings.
            Vinyl => key("vinyl"),
            JogTouch => key("jog_touch"),
            JogTurn => key("jog_turn"),
            JogBend => key("jog_bend"),
            SaveMemory | CueJumpPrev | CueJumpNext | CueDelete | LoopActiveToggle
            | GridNudge(_) | LoadStep(_) | SetPadMode(_) | ModePad { .. } | Shift => None,
        }
    }
}

impl LibraryControl {
    pub fn spec(self) -> ControlSpec {
        match self {
            LibraryControl::Browse => spec("browse", "move through the list", Kind::Relative),
            LibraryControl::PaneNext => spec("pane_next", "next pane", Kind::Button),
            LibraryControl::PanePrev => spec("pane_prev", "previous pane", Kind::Button),
            LibraryControl::Search => spec("search", "search", Kind::Button),
            LibraryControl::RelatedTracks => {
                spec(
                    "related_tracks_toggle",
                    "open the suggestions on their top track, or close them",
                    Kind::Button,
                )
            }
            LibraryControl::Undo => spec("undo", "undo", Kind::Button),
            LibraryControl::Redo => spec("redo", "redo", Kind::Button),
        }
    }

    pub const ALL: [LibraryControl; 7] = [
        LibraryControl::Browse,
        LibraryControl::PaneNext,
        LibraryControl::PanePrev,
        LibraryControl::Search,
        LibraryControl::RelatedTracks,
        LibraryControl::Undo,
        LibraryControl::Redo,
    ];

    pub fn command(self, value: f64) -> Option<Command> {
        let view = |v| Some(Command::View(v));
        match self {
            LibraryControl::Browse if value != 0.0 => view(ViewCmd::Browse(value.round() as i32)),
            LibraryControl::PaneNext if value > 0.0 => view(ViewCmd::PaneStep(1)),
            LibraryControl::PanePrev if value > 0.0 => view(ViewCmd::PaneStep(-1)),
            LibraryControl::Search if value > 0.0 => view(ViewCmd::FocusSearch),
            LibraryControl::RelatedTracks if value > 0.0 => view(ViewCmd::RelatedTracks),
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
        MasterParam::Gain => c("master_level", "master level", MASTER_GAIN),
        MasterParam::Balance => c("master_balance", "master balance", BALANCE),
        MasterParam::HeadGain => c("headphone_level", "headphone level", HEAD_GAIN),
        MasterParam::HeadMix => c("headphone_mix", "headphone mix", HEAD_MIX),
        MasterParam::MasterCue => spec(
            "master_headphone_cue",
            "master cue: the master mix in the headphones",
            Kind::Toggle,
        ),
    }
}

/// The mixer's controls, with their Mixxx names (Mixxx has no master cue: that name
/// is earshot's).
const MASTER: [(MasterParam, &str); 6] = [
    (MasterParam::Crossfader, "crossfader"),
    (MasterParam::Gain, "gain"),
    (MasterParam::Balance, "balance"),
    (MasterParam::HeadGain, "headGain"),
    (MasterParam::HeadMix, "headMix"),
    (MasterParam::MasterCue, "master_cue"),
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
        let channel = format!("[Channel{}]", deck.index() + 1);
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
        let n: usize = channel
            .strip_prefix("[Channel")?
            .strip_suffix(']')?
            .parse()
            .ok()?;
        let deck = DeckId::new(n.checked_sub(1)?)?;
        Some((kind, deck))
    }
}

impl Control {
    pub fn spec(self) -> ControlSpec {
        match self {
            Control::Deck(_, c) => c.spec(),
            Control::Master(p) => master_spec(p),
            Control::Fx(_, c) => c.spec(),
            Control::Library(c) => c.spec(),
            Control::Ui(c) => c.spec(),
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
                MasterParam::Gain => m.master_gain,
                MasterParam::Balance => m.balance,
                MasterParam::HeadGain => m.head_gain,
                MasterParam::HeadMix => m.head_mix,
                MasterParam::MasterCue => m.master_cue as u8 as f64,
            }),
            Control::Fx(u, c) => {
                let s = m.fx.get(u)?;
                match c {
                    FxControl::On => Some(s.enabled as u8 as f64),
                    FxControl::Level => Some(s.level),
                    _ => None,
                }
            }
            Control::Library(_) | Control::Ui(_) => None,
        }
    }

    /// What it sends for `value` (see [`DeckControl::command`]).
    pub fn command(self, value: f64) -> Option<Command> {
        match self {
            Control::Deck(deck, c) => c.command(deck, value),
            // A toggle, like the decks' headphone cue: a press flips it.
            Control::Master(MasterParam::MasterCue) => (value > 0.5)
                .then_some(Command::Mixer(MixerCmd::MasterCue(earshot_engine::Set::Toggle))),
            Control::Master(p) => Some(Command::Param(ParamId::Master(p), value)),
            Control::Fx(u, c) if u < FX_UNITS => c.command(u, value),
            Control::Fx(..) => None,
            Control::Library(c) => c.command(value),
            Control::Ui(c) => c.command(value),
        }
    }

    /// Its full name (`deck_1_hot_cue_c`) and the fixed word after it, if it has one
    /// (`-size`, a loop's `8`). Controls that take a value (a knob, `beat_jump`) have
    /// none: the value comes when it's sent.
    pub fn name(self) -> (String, Option<String>) {
        match self {
            Control::Deck(d, c) => {
                let (name, word) = c.name();
                (format!("deck_{}_{name}", d.index() + 1), word)
            }
            Control::Master(p) => (format!("mixer_{}", master_spec(p).name), None),
            Control::Fx(u, c) => (format!("fx_{}_{}", u + 1, c.spec().name), None),
            Control::Library(c) => (format!("library_{}", c.spec().name), None),
            Control::Ui(c) => (format!("ui_{}", c.spec().name), None),
        }
    }

    /// The control a name and the word after it stand for, with that word's number
    /// (`deck_1_beat_jump -4`: beat jump, -4). The word is a number, or `size` /
    /// `-size` for the deck's configured beat jump or loop size.
    pub fn parse(name: &str, word: Option<&str>) -> Result<(Control, Option<f64>), String> {
        let word = match word {
            None => None,
            Some("size") => Some(Word::Size { back: false }),
            Some("-size") => Some(Word::Size { back: true }),
            Some(n) => Some(Word::Num(
                n.parse::<f64>().map_err(|_| format!("{n} isn't a number"))?,
            )),
        };
        let num = match word {
            Some(Word::Num(n)) => Some(n),
            _ => None,
        };
        let unknown = || format!("no command {name}");
        let numbered = |rest: &str, count: usize| -> Option<(usize, String)> {
            let (n, thing) = rest.split_once('_')?;
            let n = n.parse::<usize>().ok()?.checked_sub(1).filter(|n| *n < count)?;
            Some((n, thing.to_string()))
        };
        let size_only_on_decks = || match word {
            Some(Word::Size { .. }) => Err(format!("{name} has no configured size")),
            _ => Ok(()),
        };
        let control = if let Some(thing) = name.strip_prefix("mixer_") {
            size_only_on_decks()?;
            MASTER
                .iter()
                .map(|(p, _)| *p)
                .find(|p| master_spec(*p).name == thing)
                .map(Control::Master)
        } else if let Some(thing) = name.strip_prefix("ui_") {
            size_only_on_decks()?;
            UiControl::ALL
                .into_iter()
                .find(|c| c.spec().name == thing)
                .map(Control::Ui)
        } else if let Some(thing) = name.strip_prefix("library_") {
            size_only_on_decks()?;
            LibraryControl::ALL
                .into_iter()
                .find(|c| c.spec().name == thing)
                .map(Control::Library)
        } else if let Some(rest) = name.strip_prefix("fx_") {
            size_only_on_decks()?;
            numbered(rest, FX_UNITS).and_then(|(u, thing)| {
                let c = FxControl::ALL.into_iter().find(|c| c.spec().name == thing)?;
                Some(Control::Fx(u, c))
            })
        } else if let Some(rest) = name.strip_prefix("deck_") {
            numbered(rest, MAX_DECKS).and_then(|(d, thing)| {
                Some(Control::Deck(
                    DeckId::new(d)?,
                    DeckControl::parse(&thing, word)?,
                ))
            })
        } else {
            None
        };
        Ok((control.ok_or_else(unknown)?, num))
    }

    /// Whether a button mapped to it needs a number after its name (how far to jump,
    /// the loop's beats).
    pub fn takes_number(self) -> bool {
        use DeckControl::*;
        matches!(
            self,
            Control::Deck(
                _,
                BeatJump | JumpBack | JumpForward | Loop | BeatLoop(_) | GridNudge(_) | LoadStep(_)
            ) | Control::Library(LibraryControl::Browse)
        )
    }

    /// The value a button press sends, given the word after the control's name: that
    /// number, unless it's part of the control (a loop's beats), else 1.
    pub fn press_value(self, arg: Option<f64>) -> f64 {
        match self {
            Control::Deck(_, c) if c.parametrised() || c.sized() => 1.0,
            _ => arg.unwrap_or(1.0),
        }
    }

    /// What it does, in a few words, with its parameter (`arg`: the keymap's number,
    /// for controls that send one).
    pub fn describe(self, arg: Option<f64>) -> String {
        match self {
            Control::Deck(_, DeckControl::Hotcue(n)) => format!("hot cue {}", hot_cue_letter(n)),
            Control::Deck(_, DeckControl::HotcueClear(n)) => {
                format!("clear hot cue {}", hot_cue_letter(n))
            }
            Control::Deck(_, DeckControl::BeatLoop(b)) if b >= 4.0 && b % 4.0 == 0.0 => {
                let bars = b / 4.0;
                format!("loop {bars} bar{}", if bars == 1.0 { "" } else { "s" })
            }
            Control::Deck(_, DeckControl::BeatLoop(b)) => format!("loop {b} beats"),
            Control::Deck(_, DeckControl::BeatJump) => match arg {
                Some(b) => format!("beat jump {} {}", b.abs(), if b < 0.0 { "back" } else { "forward" }),
                None => self.spec().help.into(),
            },
            Control::Deck(_, DeckControl::GridNudge(ms)) => format!(
                "nudge the grid {} ms {}",
                ms.abs(),
                if ms < 0.0 { "earlier" } else { "later" }
            ),
            Control::Fx(u, c) => format!("FX{} {}", u + 1, c.spec().help),
            Control::Library(LibraryControl::Browse) => match arg {
                Some(v) if v < 0.0 => "previous track".into(),
                Some(_) => "next track".into(),
                None => self.spec().help.into(),
            },
            Control::Deck(_, DeckControl::LoadStep(n)) => {
                format!("load the {} track", if n < 0 { "previous" } else { "next" })
            }
            Control::Deck(deck, DeckControl::LoadSelected) => {
                format!("load to deck {}", deck_letter(deck))
            }
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
            Control::Fx(u, c) => Some((
                format!("[EffectRack1_EffectUnit{}]", u + 1),
                c.mixxx().into(),
            )),
            Control::Library(LibraryControl::Browse) => {
                Some(("[Library]".into(), "MoveVertical".into()))
            }
            Control::Library(LibraryControl::PaneNext) => {
                Some(("[Library]".into(), "MoveFocusForward".into()))
            }
            Control::Library(LibraryControl::PanePrev) => {
                Some(("[Library]".into(), "MoveFocusBackward".into()))
            }
            Control::Library(LibraryControl::Search) => None,
            // Mixxx has no undo; these names are earshot's own, for MIDI mappings.
            Control::Library(LibraryControl::Undo) => Some(("[Library]".into(), "Undo".into())),
            Control::Library(LibraryControl::Redo) => Some(("[Library]".into(), "Redo".into())),
            // Mixxx has no such list; earshot's own name.
            Control::Library(LibraryControl::RelatedTracks) => {
                Some(("[Library]".into(), "RelatedTracks".into()))
            }
            Control::Ui(c) => Some(("[Skin]".into(), c.mixxx().into())),
        }
    }

    /// The control behind a Mixxx `[Group] key`.
    pub fn from_mixxx(group: &str, key: &str) -> Option<Control> {
        if group == "[Master]" {
            let p = MASTER.iter().find(|(_, k)| *k == key)?.0;
            return Some(Control::Master(p));
        }
        if let Some(n) = group
            .strip_prefix("[EffectRack1_EffectUnit")
            .and_then(|g| g.strip_suffix(']'))
        {
            let u = n
                .parse::<usize>()
                .ok()?
                .checked_sub(1)
                .filter(|u| *u < FX_UNITS)?;
            let c = FxControl::ALL.into_iter().find(|c| c.mixxx() == key)?;
            return Some(Control::Fx(u, c));
        }
        if group == "[Skin]" {
            return UiControl::ALL.into_iter().find(|c| c.mixxx() == key).map(Control::Ui);
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

/// Every command's name, once each (`deck_1_beat_jump`, `deck_1_hot_cue_c`), in the
/// registry's order, for picking one from a list.
pub fn command_names() -> Vec<String> {
    let mut all: Vec<Control> = vec![];
    for d in DeckId::first(MAX_DECKS) {
        for c in DeckControl::ALL.iter().chain(&[DeckControl::LoadSelected]) {
            match c {
                DeckControl::Hotcue(_) => {
                    all.extend((0..HOTCUES as u8).map(|n| Control::Deck(d, DeckControl::Hotcue(n))))
                }
                DeckControl::HotcueClear(_) => all.extend(
                    (0..HOTCUES as u8).map(|n| Control::Deck(d, DeckControl::HotcueClear(n))),
                ),
                c => all.push(Control::Deck(d, *c)),
            }
        }
    }
    all.extend(MASTER.iter().map(|(p, _)| Control::Master(*p)));
    for u in 0..FX_UNITS {
        all.extend(FxControl::ALL.iter().map(|c| Control::Fx(u, *c)));
    }
    all.extend(LibraryControl::ALL.iter().map(|c| Control::Library(*c)));
    all.extend(UiControl::ALL.iter().map(|c| Control::Ui(*c)));
    let mut names: Vec<String> = vec![];
    for c in all {
        let name = c.name().0;
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// What the number after a command is, for `docs/commands.md`.
#[cfg(test)]
fn value_doc(c: Control) -> String {
    use DeckControl::*;
    match c {
        Control::Deck(_, BeatJump) => "beats (negative: back), or `size` / `-size`".into(),
        Control::Deck(_, BeatLoop(_)) => "beats, or `size`".into(),
        Control::Deck(_, GridNudge(_)) => "milliseconds (negative: earlier)".into(),
        Control::Deck(_, LoadStep(_)) => "tracks (negative: previous)".into(),
        Control::Deck(_, ModePad { .. }) => "the pad, 1 to 8".into(),
        Control::Deck(_, JogTurn | JogBend) => "seconds of track (negative: back)".into(),
        Control::Library(LibraryControl::Browse) => "rows (negative: up)".into(),
        _ => match c.spec().kind {
            Kind::Button => "pressed (1) or let go (0)".into(),
            Kind::Toggle => "—".into(),
            Kind::Relative => "steps (negative: back)".into(),
            Kind::Continuous(r) => format!("{} to {}", r.min, r.max),
        },
    }
}

/// `docs/commands.md`: every command, made from the registry.
#[cfg(test)]
pub fn commands_markdown() -> String {
    let mut out = String::from(
        "# Commands\n\n\
         Everything earshot can be told to do, by a key (`keymap.txt`), a controller or its \
         own buttons. Made from `gui/src/core/bus.rs` by a test: don't edit by hand \
         (`EARSHOT_WRITE_DOCS=1 cargo test commands_doc` rewrites it).\n\n\
         A name goes from the general to the particular: where, what, which part, what to \
         do. A number or letter in the name says which (`deck_2`, `hot_cue_c`); a number after it is \
         the value (`deck_1_beat_jump -4`). `size` stands for the deck's configured beat \
         jump or loop size.\n",
    );
    let mut table = |title: &str, rows: Vec<(String, String, String)>| {
        out.push_str(&format!("\n## {title}\n\n| Command | Number | What it does |\n|---|---|---|\n"));
        for (name, value, help) in rows {
            out.push_str(&format!("| `{name}` | {value} | {help} |\n"));
        }
    };
    let row = |c: Control, pattern: Option<String>| {
        let name = pattern.unwrap_or_else(|| c.name().0);
        (name, value_doc(c), c.spec().help.to_string())
    };
    let d = DeckId::new(0).expect("deck 1");
    let deck_rows = DeckControl::ALL
        .iter()
        .chain(&[DeckControl::LoadSelected])
        .filter(|c| !c.sized())
        .map(|&c| {
            let pattern = match c {
                DeckControl::Hotcue(_) => Some("deck_N_hot_cue_X".to_string()),
                DeckControl::HotcueClear(_) => Some("deck_N_hot_cue_X_clear".to_string()),
                _ => Some(Control::Deck(d, c).name().0.replacen("deck_1_", "deck_N_", 1)),
            };
            row(Control::Deck(d, c), pattern)
        })
        .collect();
    table(
        &format!(
            "Decks: `deck_N_`, N = 1 to {MAX_DECKS}; hot cues X = a to {}",
            hot_cue_letter(HOTCUES as u8 - 1).to_ascii_lowercase()
        ),
        deck_rows,
    );
    table(
        "Mixer: `mixer_`",
        MASTER.iter().map(|(p, _)| row(Control::Master(*p), None)).collect(),
    );
    table(
        &format!("Effect units: `fx_N_`, N = 1 to {FX_UNITS}"),
        FxControl::ALL
            .iter()
            .map(|&c| {
                let name = Control::Fx(0, c).name().0.replacen("fx_1_", "fx_N_", 1);
                row(Control::Fx(0, c), Some(name))
            })
            .collect(),
    );
    table(
        "Library: `library_`",
        LibraryControl::ALL.iter().map(|&c| row(Control::Library(c), None)).collect(),
    );
    table(
        "What's on screen: `ui_`",
        UiControl::ALL.iter().map(|&c| row(Control::Ui(c), None)).collect(),
    );
    out
}

/// A hot cue's letter, as Pioneer names them: A for pad 0, B, … H.
pub fn hot_cue_letter(slot: u8) -> char {
    (b'A' + slot) as char
}

/// The hot cue a letter names (`a` or `A`: pad 0).
fn hot_cue_slot(letter: &str) -> Option<u8> {
    let &[c] = letter.as_bytes() else {
        return None;
    };
    let slot = c.to_ascii_uppercase().checked_sub(b'A')?;
    ((slot as usize) < HOTCUES).then_some(slot)
}

/// "A", "B", … for decks 1, 2, ….
pub fn deck_letter(deck: DeckId) -> char {
    (b'A' + deck.index() as u8) as char
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
        // Plain buttons have no value.
        assert_eq!(get(DeckControl::JumpBack), None);
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
        snap.decks[0].keylock = true;
        assert_eq!(
            Control::Deck(DeckId::new(0).unwrap(), DeckControl::Keylock).get(&snap),
            Some(1.0)
        );
    }

    #[test]
    fn key_shift_controls() {
        let d = deck(0);
        let sends = |c: DeckControl, v: f64| match c.command(d, v) {
            Some(Command::Deck(x, DeckCmd::Engine(e))) if x == d => Some(e),
            _ => None,
        };
        assert!(
            matches!(sends(DeckControl::KeyShift, 2.6), Some(EngineCmd::KeyShift(n)) if n == 3.0)
        );
        assert!(
            matches!(sends(DeckControl::KeyShiftUp, 1.0), Some(EngineCmd::KeyShiftBy(n)) if n == 1.0)
        );
        assert!(
            matches!(sends(DeckControl::KeyShiftDown, 1.0), Some(EngineCmd::KeyShiftBy(n)) if n == -1.0)
        );
        assert!(
            matches!(sends(DeckControl::KeyShiftReset, 1.0), Some(EngineCmd::KeyShift(n)) if n == 0.0)
        );
        assert!(
            sends(DeckControl::KeyShiftUp, 0.0).is_none(),
            "nothing on release"
        );
        let (c, _) = Control::parse("deck_2_key_shift_up", None).unwrap();
        assert_eq!(c, Control::Deck(deck(1), DeckControl::KeyShiftUp));
        let mut snap = EngineSnapshot::default();
        snap.decks[0].key_shift = -2.0;
        assert_eq!(
            Control::Deck(d, DeckControl::KeyShift).get(&snap),
            Some(-2.0)
        );
        assert_eq!(
            Control::Deck(d, DeckControl::KeyShiftReset).get(&snap),
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
        for d in DeckId::first(MAX_DECKS) {
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
            Control::from_mixxx("[Channel1]", "beatloop_16_toggle"),
            Some(Control::Deck(deck(0), DeckControl::BeatLoop(16.0)))
        );
        assert_eq!(Control::from_mixxx("[PreviewDeck1]", "play"), None);
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
        for u in 0..FX_UNITS {
            for c in FxControl::ALL {
                let c = Control::Fx(u, c);
                let (g, k) = c.mixxx().unwrap();
                assert_eq!(Control::from_mixxx(&g, &k), Some(c), "{g} {k}");
            }
        }
        assert_eq!(
            Control::from_mixxx("[EffectRack1_EffectUnit1]", "mix"),
            Some(Control::Fx(0, FxControl::Level))
        );
        assert_eq!(
            Control::from_mixxx("[EffectRack1_EffectUnit9]", "mix"),
            None
        );
        assert_eq!(Control::from_mixxx("[Channel9]", "play"), None);
        assert_eq!(Control::from_mixxx("[Channel1]", "nonsense"), None);
    }

    #[test]
    fn names_parse_back() {
        let mut all: Vec<Control> = vec![];
        for d in DeckId::first(MAX_DECKS) {
            for c in DeckControl::ALL.iter().chain(&[
                DeckControl::Hotcue(7),
                DeckControl::LoadSelected,
                DeckControl::LoadStep(-1),
            ]) {
                all.push(Control::Deck(d, *c));
            }
        }
        all.extend(MASTER.iter().map(|(p, _)| Control::Master(*p)));
        all.extend(FxControl::ALL.iter().map(|c| Control::Fx(0, *c)));
        all.extend(LibraryControl::ALL.iter().map(|c| Control::Library(*c)));
        all.extend(UiControl::ALL.iter().map(|c| Control::Ui(*c)));
        for c in all {
            let (name, word) = c.name();
            let (back, _) = Control::parse(&name, word.as_deref()).unwrap();
            assert_eq!(back, c, "{name} {word:?}");
        }
        let parse = |n: &str, w: Option<&str>| Control::parse(n, w).map(|(c, _)| c);
        let d = deck(1);
        assert_eq!(
            parse("deck_2_hot_cue_c_clear", None),
            Ok(Control::Deck(d, DeckControl::HotcueClear(2)))
        );
        assert_eq!(
            Control::parse("deck_2_beat_jump", Some("-4")),
            Ok((Control::Deck(d, DeckControl::BeatJump), Some(-4.0)))
        );
        assert_eq!(
            parse("deck_2_beat_jump", Some("-size")),
            Ok(Control::Deck(d, DeckControl::JumpBack))
        );
        assert_eq!(
            parse("deck_2_beat_loop", Some("size")),
            Ok(Control::Deck(d, DeckControl::Loop))
        );
        assert_eq!(
            parse("mixer_crossfader", None),
            Ok(Control::Master(MasterParam::Crossfader))
        );
        for bad in [
            "deck_1_hot_cue_i",
            "deck_1_hot_cue_1",
            "deck_1_hot_cue_ab",
            "deck_5_play",
            "deck_1_dance",
            "fx_2_on",
            "mixer_play",
            "play",
        ] {
            assert!(parse(bad, None).is_err(), "{bad}");
        }
        assert!(parse("deck_1_beat_loop", None).is_err(), "needs its beats");
        assert!(parse("deck_1_beat_loop", Some("-size")).is_err());
        assert!(parse("deck_1_play", Some("size")).is_err());
        assert!(parse("mixer_crossfader", Some("size")).is_err());
        assert!(parse("deck_1_hot_cue_a", Some("2")).is_err());
        assert!(parse("deck_1_beat_jump", Some("x")).unwrap_err().contains("isn't a number"));
    }

    /// `docs/commands.md` is the registry as it is.
    #[test]
    fn commands_doc() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/commands.md");
        let want = commands_markdown();
        if std::env::var_os("EARSHOT_WRITE_DOCS").is_some() {
            std::fs::write(&path, &want).unwrap();
        }
        let have = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            have == want,
            "docs/commands.md is out of date: EARSHOT_WRITE_DOCS=1 cargo test commands_doc"
        );
    }

    #[test]
    fn effect_controls() {
        let fx = |c: FxControl, v| Control::Fx(0, c).command(v);
        assert!(matches!(
            fx(FxControl::On, 1.0),
            Some(Command::Fx(0, FxCmd::Enabled(Set::Toggle)))
        ));
        assert!(fx(FxControl::On, 0.0).is_none());
        assert!(matches!(
            fx(FxControl::BeatsUp, 1.0),
            Some(Command::Fx(0, FxCmd::BeatsStep(1)))
        ));
        assert!(matches!(
            fx(FxControl::Level, 0.25),
            Some(Command::Fx(0, FxCmd::Level(0.25)))
        ));
        assert!(Control::Fx(FX_UNITS, FxControl::On).command(1.0).is_none());
        let mut snap = EngineSnapshot::default();
        snap.mixer.fx[0].enabled = true;
        snap.mixer.fx[0].level = 0.8;
        assert_eq!(Control::Fx(0, FxControl::On).get(&snap), Some(1.0));
        assert_eq!(Control::Fx(0, FxControl::Level).get(&snap), Some(0.8));
    }

    #[test]
    fn loop_sizes_step() {
        assert_eq!(step_size(4.0, 1), 8.0);
        assert_eq!(step_size(4.0, -1), 2.0);
        assert_eq!(step_size(32.0, 1), 32.0);
        assert_eq!(step_size(0.125, -1), 0.125);
    }
}
