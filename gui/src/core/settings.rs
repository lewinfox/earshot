//! Deck and audio settings (tempo ranges, keylock and quantise), and the Flow canvas's: engine and app state, kept in the Store's `settings`
//! table as JSON. (The UI's own preferences, like the layout, are kept by the UI.)

use earshot_engine::TempoRange;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::collections::CollectionId;
use super::player::AudioSettings;
use super::store::{Reader, StoreCmd};

/// [`AudioSettings`]: the output device, buffer size and routing.
pub const AUDIO: &str = "audio.output";
/// Each deck's tempo fader range, deck 1 first.
pub const TEMPO_RANGES: &str = "decks.tempo_ranges";
/// Each player's options ([`DeckOptions`]), by `DeckId::index()`: decks 1-4, then
/// Prepare's player.
pub const DECK_OPTIONS: &str = "decks.options";
/// The Flow that was on the canvas last. (Where its canvas was looking is kept with
/// the Flow: `StoreCmd::SaveFlowCamera`.)
pub const FLOW_OPEN: &str = "flow.open";

/// What was saved, read once at startup.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    pub audio: AudioSettings,
    pub tempo_ranges: Vec<TempoRange>,
    pub deck_options: Vec<DeckOptions>,
    pub flow_open: Option<CollectionId>,
}

/// A player's options that stay as they were set, from one run to the next.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeckOptions {
    pub keylock: bool,
    pub quantize: bool,
}

impl Settings {
    pub fn load(reader: &Reader) -> Settings {
        Settings {
            audio: read(reader, AUDIO).unwrap_or_default(),
            tempo_ranges: read(reader, TEMPO_RANGES).unwrap_or_default(),
            deck_options: read(reader, DECK_OPTIONS).unwrap_or_default(),
            flow_open: read(reader, FLOW_OPEN).unwrap_or_default(),
        }
    }
}

/// A setting, if it's there and reads as a `T`.
pub fn read<T: DeserializeOwned>(reader: &Reader, key: &str) -> Option<T> {
    let v = reader.setting(key).ok().flatten()?;
    serde_json::from_str(&v)
        .inspect_err(|e| eprintln!("setting {key}: {e}"))
        .ok()
}

/// The command that saves a setting.
pub fn save(key: &str, value: &impl Serialize) -> StoreCmd {
    StoreCmd::SetSetting {
        key: key.into(),
        value: serde_json::to_string(value).expect("settings serialise"),
    }
}
