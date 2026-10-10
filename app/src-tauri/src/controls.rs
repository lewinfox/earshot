//! The control bus: Mixxx-style `[Group] key = value` controls mapped onto engine
//! commands. Keyboard, on-screen controls and MIDI all come through here.
//!
//! Buttons send 1 on press and 0 on release. Controls that the UI owns (library
//! navigation, loading the selected track) are handed back to the caller to forward.

use earshot_engine::{Command, Press, RATE_RANGE};

use crate::state::Inner;

pub enum Outcome {
    Done,
    /// A control for the UI (e.g. `[Library] MoveVertical`); forward it as an event.
    Ui,
}

fn deck_of(group: &str) -> Option<usize> {
    match group {
        "[Channel1]" => Some(0),
        "[Channel2]" => Some(1),
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
            "quantize" => Command::Quantize { on: value > 0.0 },
            _ => return Err(format!("unknown control {group} {key}")),
        };
        inner.send(cmd)?;
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
        "pfl" => Command::Pfl { deck, on: pressed },
        "volume" => Command::Volume { deck, value },
        // Mixxx's `rate` is -1..1 across the fader range.
        "rate" => Command::Rate {
            deck,
            value: value * RATE_RANGE,
        },
        "rate_temp_up" => Command::Nudge {
            deck,
            amount: if pressed { 0.04 } else { 0.0 },
        },
        "rate_temp_down" => Command::Nudge {
            deck,
            amount: if pressed { -0.04 } else { 0.0 },
        },
        "playposition" => {
            let duration = inner.shared().map_or(0.0, |s| s.decks[deck].duration.get());
            Command::Seek {
                deck,
                seconds: value.clamp(0.0, 1.0) * duration,
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
        "beatloop_activate" if pressed => Command::BeatLoop {
            deck,
            beats: inner.loop_size[deck],
        },
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

/// Current value of a toggle-style control, so button presses can flip it.
pub fn current(inner: &Inner, group: &str, key: &str) -> Option<bool> {
    use std::sync::atomic::Ordering::Relaxed;
    let shared = inner.shared()?;
    let d = &shared.decks[deck_of(group)?];
    match key {
        "play" => Some(d.playing.load(Relaxed)),
        "sync_enabled" => Some(d.sync.load(Relaxed)),
        "pfl" => Some(d.pfl.load(Relaxed)),
        _ => None,
    }
}
