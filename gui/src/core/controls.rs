//! The control bus: Mixxx-style `[Group] key = value` controls mapped onto engine
//! commands. Keyboard, on-screen controls and MIDI all come through here.
//!
//! Buttons send 1 on press and 0 on release. Controls that the UI owns (library
//! navigation, loading the selected track) are handed back to the caller to forward.

use earshot_engine::{
    ChannelParam, Command, DeckCmd, DeckId, EQ_HIGH, EQ_LOW, EQ_MID, EngineSnapshot, MasterParam,
    ParamId, Press, Set,
};

use crate::core::player::{Inner, deck_id};

pub enum Outcome {
    Done,
    /// A control for the UI (e.g. `[Library] MoveVertical`); forward it as an event.
    Ui,
}

/// Mixxx's groups for deck A, deck B and Prepare's player (Mixxx's preview deck).
pub const CHANNELS: [&str; 3] = ["[Channel1]", "[Channel2]", "[PreviewDeck1]"];
/// Mixxx's groups for each channel's EQ (`parameter1..3` = low, mid, high) and its
/// quick effect, the filter knob (`super1`).
pub const EQ_GROUPS: [&str; 2] = [
    "[EqualizerRack1_[Channel1]_Effect1]",
    "[EqualizerRack1_[Channel2]_Effect1]",
];
pub const FILTER_GROUPS: [&str; 2] = [
    "[QuickEffectRack1_[Channel1]]",
    "[QuickEffectRack1_[Channel2]]",
];

fn deck_of(group: &str) -> Option<DeckId> {
    deck_id(CHANNELS.iter().position(|g| *g == group)?).ok()
}

fn deck_in(groups: &[&str], group: &str) -> Option<DeckId> {
    deck_id(groups.iter().position(|g| *g == group)?).ok()
}

/// EQ band (0 low, 1 mid, 2 high) for Mixxx's `parameter1..3`.
fn eq_band(key: &str) -> Option<usize> {
    match key {
        "parameter1" => Some(EQ_LOW),
        "parameter2" => Some(EQ_MID),
        "parameter3" => Some(EQ_HIGH),
        _ => None,
    }
}

pub fn apply(inner: &mut Inner, group: &str, key: &str, value: f64) -> Result<Outcome, String> {
    if group == "[Library]" || group == "[Suggest]" || key == "LoadSelectedTrack" {
        return Ok(Outcome::Ui);
    }
    if group == "[Master]" {
        let p = match key {
            "crossfader" => MasterParam::Crossfader,
            "gain" => MasterParam::Gain,
            "balance" => MasterParam::Balance,
            "headGain" => MasterParam::HeadGain,
            "headMix" => MasterParam::HeadMix,
            _ => return Err(format!("unknown control {group} {key}")),
        };
        inner.send(Command::Param(ParamId::Master(p), value))?;
        return Ok(Outcome::Done);
    }
    let channel = |deck, p| Command::Param(ParamId::Channel(deck, p), value);
    if let Some(deck) = deck_in(&EQ_GROUPS, group) {
        let band = eq_band(key).ok_or_else(|| format!("unknown control {group} {key}"))?;
        inner.send(channel(deck, ChannelParam::Eq(band)))?;
        return Ok(Outcome::Done);
    }
    if let Some(deck) = deck_in(&FILTER_GROUPS, group) {
        if key != "super1" {
            return Err(format!("unknown control {group} {key}"));
        }
        // Mixxx's super1 is 0..1 with the filter off at 0.5.
        let value = value * 2.0 - 1.0;
        inner.send(Command::Param(
            ParamId::Channel(deck, ChannelParam::Filter),
            value,
        ))?;
        return Ok(Outcome::Done);
    }
    let deck = deck_of(group).ok_or_else(|| format!("unknown group {group}"))?;
    let d = deck.index();
    let pressed = value > 0.0;
    let press = if pressed { Press::Down } else { Press::Up };
    let set = Set::from_bool(pressed);
    let dc = |c| Command::Deck(deck, c);

    // Parametrised keys first: hotcue_N_activate, hotcue_N_clear, beatloop_X_toggle.
    if let Some(rest) = key.strip_prefix("hotcue_") {
        let (n, action) = rest.split_once('_').ok_or("bad hotcue control")?;
        let slot = n
            .parse::<u8>()
            .map_err(|e| e.to_string())?
            .checked_sub(1)
            .ok_or("hotcues start at 1")?;
        let cmd = match action {
            "activate" => dc(DeckCmd::Hotcue { slot, press }),
            "clear" if pressed => dc(DeckCmd::HotcueClear(slot)),
            "clear" => return Ok(Outcome::Done),
            _ => return Err(format!("unknown control {group} {key}")),
        };
        inner.send(cmd)?;
        return Ok(Outcome::Done);
    }
    if let Some(size) = key
        .strip_prefix("beatloop_")
        .and_then(|r| r.strip_suffix("_toggle"))
    {
        if pressed {
            let beats = size.parse::<f64>().map_err(|e| e.to_string())?;
            inner.send(dc(DeckCmd::BeatLoop(beats)))?;
        }
        return Ok(Outcome::Done);
    }

    let cmd = match key {
        "play" => dc(DeckCmd::Play(set)),
        "cue_default" => dc(DeckCmd::Cue(press)),
        "sync_enabled" => dc(DeckCmd::Sync(set)),
        "quantize" => dc(DeckCmd::Quantize(set)),
        // Put the cue point at the playhead (snapped when quantising).
        "cue_set" if pressed => dc(DeckCmd::SetCue(None)),
        "cue_set" => return Ok(Outcome::Done),
        "keylock" => dc(DeckCmd::Keylock(set)),
        "pfl" => dc(DeckCmd::Pfl(set)),
        "volume" => channel(deck, ChannelParam::Volume),
        "pregain" => channel(deck, ChannelParam::Trim),
        // Mixxx's `rate` is the fader, -1..1 across its range. The engine decides
        // whether it moves the tempo (see `DeckCmd::TempoFader`).
        "rate" => dc(DeckCmd::TempoFader(value)),
        // Step to the next range (±6, ±10, ±16, WIDE), like a CDJ's tempo range button.
        "rateRange" if pressed => dc(DeckCmd::TempoRangeNext),
        "rateRange" => return Ok(Outcome::Done),
        "rate_temp_up" => dc(DeckCmd::Nudge(if pressed { 0.04 } else { 0.0 })),
        "rate_temp_down" => dc(DeckCmd::Nudge(if pressed { -0.04 } else { 0.0 })),
        // Mixxx's 0..1 through the track; below 0 is the lead-in before it.
        "playposition" => {
            let duration = inner.snapshot().map_or(0.0, |s| s.deck(deck).duration);
            dc(DeckCmd::Seek(value.min(1.0) * duration))
        }
        "beatjump_size" => {
            inner.jump_size[d] = value;
            return Ok(Outcome::Done);
        }
        "beatloop_size" => {
            inner.loop_size[d] = value;
            return Ok(Outcome::Done);
        }
        "beatjump" if value != 0.0 => dc(DeckCmd::BeatJump(value)),
        "beatjump_forward" if pressed => dc(DeckCmd::BeatJump(inner.jump_size[d])),
        "beatjump_backward" if pressed => dc(DeckCmd::BeatJump(-inner.jump_size[d])),
        // The loop button: starts a loop of the chosen size, or ends the current one
        // (whatever its size by now).
        "beatloop_activate" if pressed => {
            let looping = inner.snapshot().is_some_and(|s| s.deck(deck).loop_active);
            if looping {
                dc(DeckCmd::Reloop(Set::Off))
            } else {
                dc(DeckCmd::BeatLoop(inner.loop_size[d]))
            }
        }
        "loop_in" if pressed => dc(DeckCmd::LoopIn),
        "loop_out" if pressed => dc(DeckCmd::LoopOut),
        "reloop_toggle" if pressed => dc(DeckCmd::Reloop(Set::Toggle)),
        "loop_halve" if pressed => dc(DeckCmd::LoopScale(0.5)),
        "loop_double" if pressed => dc(DeckCmd::LoopScale(2.0)),
        "beatjump" | "beatjump_forward" | "beatjump_backward" | "beatloop_activate" | "loop_in"
        | "loop_out" | "reloop_toggle" | "loop_halve" | "loop_double" => return Ok(Outcome::Done), // releases
        _ => return Err(format!("unknown control {group} {key}")),
    };
    inner.send(cmd)?;
    Ok(Outcome::Done)
}

/// Flip a toggle-style control (play, sync, quantize, keylock, pfl). The engine flips
/// it against its live state, so two quick presses can't both read a stale one.
pub fn toggle(inner: &mut Inner, group: &str, key: &str) -> Result<(), String> {
    let deck = deck_of(group).ok_or_else(|| format!("unknown group {group}"))?;
    let cmd = match key {
        "play" => DeckCmd::Play(Set::Toggle),
        "sync_enabled" => DeckCmd::Sync(Set::Toggle),
        "quantize" => DeckCmd::Quantize(Set::Toggle),
        "keylock" => DeckCmd::Keylock(Set::Toggle),
        "pfl" => DeckCmd::Pfl(Set::Toggle),
        _ => return Err(format!("{group} {key} isn't a toggle")),
    };
    inner.send(Command::Deck(deck, cmd))
}

/// Current value of a continuous control, in the units [`apply`] takes, so on-screen
/// knobs and faders show what the keyboard and MIDI last set.
pub fn value(snap: &EngineSnapshot, group: &str, key: &str) -> Option<f64> {
    if group == "[Master]" {
        let m = &snap.mixer;
        return Some(match key {
            "crossfader" => m.crossfader,
            "gain" => m.main_gain,
            "balance" => m.balance,
            "headGain" => m.head_gain,
            "headMix" => m.head_mix,
            _ => return None,
        });
    }
    if let Some(d) = deck_in(&EQ_GROUPS, group) {
        return Some(snap.deck(d).eq[eq_band(key)?]);
    }
    if let Some(d) = deck_in(&FILTER_GROUPS, group) {
        return (key == "super1").then(|| (snap.deck(d).filter + 1.0) / 2.0);
    }
    let d = snap.deck(deck_of(group)?);
    Some(match key {
        "volume" => d.volume,
        "pregain" => d.pregain,
        "rate" => d.fader,
        _ => return None,
    })
}
