//! The control bus: Mixxx-style `[Group] key = value` controls mapped onto engine
//! commands. Keyboard, on-screen controls and MIDI all come through here.
//!
//! Buttons send 1 on press and 0 on release. Controls that the UI owns (library
//! navigation, loading the selected track) are handed back to the caller to forward.

use std::sync::atomic::Ordering::Relaxed;

use earshot_engine::{Command, EQ_HIGH, EQ_LOW, EQ_MID, Press, Shared, TempoRange};

use crate::core::player::Inner;

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

fn deck_of(group: &str) -> Option<usize> {
    CHANNELS.iter().position(|g| *g == group)
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
        let cmd = match key {
            "crossfader" => Command::Crossfader { value },
            "gain" => Command::MainGain { value },
            "balance" => Command::Balance { value },
            "headGain" => Command::HeadGain { value },
            "headMix" => Command::HeadMix { value },
            _ => return Err(format!("unknown control {group} {key}")),
        };
        inner.send(cmd)?;
        return Ok(Outcome::Done);
    }
    if let Some(deck) = EQ_GROUPS.iter().position(|g| *g == group) {
        let band = eq_band(key).ok_or_else(|| format!("unknown control {group} {key}"))?;
        inner.send(Command::Eq { deck, band, value })?;
        return Ok(Outcome::Done);
    }
    if let Some(deck) = FILTER_GROUPS.iter().position(|g| *g == group) {
        if key != "super1" {
            return Err(format!("unknown control {group} {key}"));
        }
        // Mixxx's super1 is 0..1 with the filter off at 0.5.
        inner.send(Command::Filter {
            deck,
            value: value * 2.0 - 1.0,
        })?;
        return Ok(Outcome::Done);
    }
    let deck = deck_of(group).ok_or_else(|| format!("unknown group {group}"))?;
    let pressed = value > 0.0;
    let press = if pressed { Press::Down } else { Press::Up };

    // Parametrised keys first: hotcue_N_activate, hotcue_N_clear, beatloop_X_toggle.
    if let Some(rest) = key.strip_prefix("hotcue_") {
        let (n, action) = rest.split_once('_').ok_or("bad hotcue control")?;
        let slot = n
            .parse::<usize>()
            .map_err(|e| e.to_string())?
            .checked_sub(1)
            .ok_or("hotcues start at 1")?;
        let cmd = match action {
            "activate" => Command::Hotcue { deck, slot, press },
            "clear" if pressed => Command::HotcueClear { deck, slot },
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
            inner.send(Command::BeatLoop { deck, beats })?;
        }
        return Ok(Outcome::Done);
    }

    let cmd = match key {
        "play" => Command::Play { deck, on: pressed },
        "cue_default" => Command::Cue { deck, press },
        "sync_enabled" => Command::Sync { deck, on: pressed },
        "quantize" => Command::Quantize { deck, on: pressed },
        // Put the cue point at the playhead (snapped when quantising).
        "cue_set" if pressed => Command::SetCue { deck, at: None },
        "cue_set" => return Ok(Outcome::Done),
        "keylock" => Command::Keylock { deck, on: pressed },
        "pfl" => Command::Pfl { deck, on: pressed },
        "volume" => Command::Volume { deck, value },
        "pregain" => Command::Pregain { deck, value },
        // Mixxx's `rate` is the fader, -1..1 across its range. The engine decides
        // whether it moves the tempo (see `Command::TempoFader`).
        "rate" => Command::TempoFader {
            deck,
            position: value,
        },
        // Step to the next range (±6, ±10, ±16, WIDE), like a CDJ's tempo range button.
        "rateRange" if pressed => Command::TempoRange {
            deck,
            range: range_of(inner, deck).next(),
        },
        "rateRange" => return Ok(Outcome::Done),
        "rate_temp_up" => Command::Nudge {
            deck,
            amount: if pressed { 0.04 } else { 0.0 },
        },
        "rate_temp_down" => Command::Nudge {
            deck,
            amount: if pressed { -0.04 } else { 0.0 },
        },
        // Mixxx's 0..1 through the track; below 0 is the lead-in before it.
        "playposition" => {
            let duration = inner.shared().map_or(0.0, |s| s.decks[deck].duration.get());
            Command::Seek {
                deck,
                seconds: value.min(1.0) * duration,
            }
        }
        "beatjump_size" => {
            inner.jump_size[deck] = value;
            return Ok(Outcome::Done);
        }
        "beatloop_size" => {
            inner.loop_size[deck] = value;
            return Ok(Outcome::Done);
        }
        "beatjump" if value != 0.0 => Command::BeatJump { deck, beats: value },
        "beatjump_forward" if pressed => Command::BeatJump {
            deck,
            beats: inner.jump_size[deck],
        },
        "beatjump_backward" if pressed => Command::BeatJump {
            deck,
            beats: -inner.jump_size[deck],
        },
        // The loop button: starts a loop of the chosen size, or ends the current one
        // (whatever its size by now).
        "beatloop_activate" if pressed => {
            let looping = inner
                .shared()
                .is_some_and(|s| s.decks[deck].loop_active.load(Relaxed));
            if looping {
                Command::ReloopToggle { deck }
            } else {
                Command::BeatLoop {
                    deck,
                    beats: inner.loop_size[deck],
                }
            }
        }
        "loop_in" if pressed => Command::LoopIn { deck },
        "loop_out" if pressed => Command::LoopOut { deck },
        "reloop_toggle" if pressed => Command::ReloopToggle { deck },
        "loop_halve" if pressed => Command::LoopScale { deck, factor: 0.5 },
        "loop_double" if pressed => Command::LoopScale { deck, factor: 2.0 },
        "beatjump" | "beatjump_forward" | "beatjump_backward" | "beatloop_activate" | "loop_in"
        | "loop_out" | "reloop_toggle" | "loop_halve" | "loop_double" => return Ok(Outcome::Done), // releases
        _ => return Err(format!("unknown control {group} {key}")),
    };
    inner.send(cmd)?;
    Ok(Outcome::Done)
}

/// The deck's tempo fader range, as the engine last published it.
fn range_of(inner: &Inner, deck: usize) -> TempoRange {
    inner
        .shared()
        .map(|s| TempoRange::from_index(s.decks[deck].rate_range.load(Relaxed)))
        .unwrap_or_default()
}

/// Current value of a toggle-style control, so button presses can flip it.
pub fn current(inner: &Inner, group: &str, key: &str) -> Option<bool> {
    let shared = inner.shared()?;
    let d = &shared.decks[deck_of(group)?];
    match key {
        "play" => Some(d.playing.load(Relaxed)),
        "sync_enabled" => Some(d.sync.load(Relaxed)),
        "quantize" => Some(d.quantize.load(Relaxed)),
        "keylock" => Some(d.keylock.load(Relaxed)),
        "pfl" => Some(d.pfl.load(Relaxed)),
        _ => None,
    }
}

/// Current value of a continuous control, in the units [`apply`] takes, so on-screen
/// knobs and faders show what the keyboard and MIDI last set.
pub fn value(shared: &Shared, group: &str, key: &str) -> Option<f64> {
    if group == "[Master]" {
        return Some(match key {
            "crossfader" => shared.crossfader.get(),
            "gain" => shared.main_gain.get(),
            "balance" => shared.balance.get(),
            "headGain" => shared.head_gain.get(),
            "headMix" => shared.head_mix.get(),
            _ => return None,
        });
    }
    if let Some(d) = EQ_GROUPS.iter().position(|g| *g == group) {
        return Some(shared.decks[d].eq[eq_band(key)?].get());
    }
    if let Some(d) = FILTER_GROUPS.iter().position(|g| *g == group) {
        return (key == "super1").then(|| (shared.decks[d].filter.get() + 1.0) / 2.0);
    }
    let d = &shared.decks[deck_of(group)?];
    Some(match key {
        "volume" => d.volume.get(),
        "pregain" => d.pregain.get(),
        "rate" => d.fader.get(),
        _ => return None,
    })
}
