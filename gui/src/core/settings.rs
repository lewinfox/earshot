//! Deck and audio settings: engine and app state, kept in the Store's `settings`
//! table as JSON. (The UI's own preferences, like the layout, are kept by the UI.)

use earshot_engine::TempoRange;
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::player::AudioSettings;
use super::store::{Reader, StoreCmd};

/// [`AudioSettings`]: the output device, buffer size and routing.
pub const AUDIO: &str = "audio.output";
/// Each deck's tempo fader range, deck 1 first.
pub const TEMPO_RANGES: &str = "decks.tempo_ranges";

/// What was saved, read once at startup.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    pub audio: AudioSettings,
    pub tempo_ranges: Vec<TempoRange>,
}

impl Settings {
    pub fn load(reader: &Reader) -> Settings {
        Settings {
            audio: read(reader, AUDIO).unwrap_or_default(),
            tempo_ranges: read(reader, TEMPO_RANGES).unwrap_or_default(),
        }
    }
}

fn read<T: DeserializeOwned>(reader: &Reader, key: &str) -> Option<T> {
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
