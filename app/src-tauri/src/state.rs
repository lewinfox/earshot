use std::sync::{Arc, Mutex};

use earshot_engine::beatgrid::Beatgrid;
use earshot_engine::decode::TrackAudio;
use earshot_engine::{Command, DECKS, Engine, Shared};

use crate::midi::Midi;

#[derive(Default, Clone)]
pub struct DeckSlot {
    pub audio: Option<Arc<TrackAudio>>,
    pub grid: Option<Arc<Beatgrid>>,
    pub grid_source: String,
    pub waveform: Arc<Vec<u8>>,
}

pub struct Inner {
    pub engine: Option<Engine>,
    pub decks: [DeckSlot; DECKS],
    pub generation: u64,
    pub jump_size: [f64; DECKS],
    pub loop_size: [f64; DECKS],
}

impl Inner {
    pub fn send(&mut self, cmd: Command) -> Result<(), String> {
        self.engine
            .as_mut()
            .ok_or("audio is not running")?
            .send(cmd)
    }

    pub fn shared(&self) -> Option<Arc<Shared>> {
        self.engine.as_ref().map(|e| e.shared().clone())
    }
}

pub struct AppState {
    pub inner: Mutex<Inner>,
    pub midi: Mutex<Midi>,
}

impl Default for AppState {
    fn default() -> Self {
        AppState {
            inner: Mutex::new(Inner {
                engine: None,
                decks: Default::default(),
                generation: 0,
                jump_size: [4.0; DECKS],
                loop_size: [4.0; DECKS],
            }),
            midi: Mutex::new(Midi::default()),
        }
    }
}
