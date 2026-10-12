//! Deck and audio settings, and the Flow canvas's: engine and app state, kept in the Store's `settings`
//! table as JSON. (The UI's own preferences, like the layout, are kept by the UI.)

use earshot_engine::TempoRange;
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::collections::CollectionId;
use super::player::AudioSettings;
use super::store::{Reader, StoreCmd};

/// [`AudioSettings`]: the output device, buffer size and routing.
pub const AUDIO: &str = "audio.output";
/// Each deck's tempo fader range, deck 1 first.
pub const TEMPO_RANGES: &str = "decks.tempo_ranges";
/// The Flow that was on the canvas last.
pub const FLOW_OPEN: &str = "flow.open";

/// Where a Flow's canvas was looking: `[left, top, right, bottom]` in canvas points.
pub fn flow_camera(flow: CollectionId) -> String {
    format!("flow.camera.{flow}")
}

/// What was saved, read once at startup.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    pub audio: AudioSettings,
    pub tempo_ranges: Vec<TempoRange>,
    pub flow_open: Option<CollectionId>,
}

impl Settings {
    pub fn load(reader: &Reader) -> Settings {
        Settings {
            audio: read(reader, AUDIO).unwrap_or_default(),
            tempo_ranges: read(reader, TEMPO_RANGES).unwrap_or_default(),
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
