//! earshot's real-time audio engine: decks, mixer and output.
//!
//! [`Engine`] is the control-side handle: it owns the output stream, sends
//! [`Command`]s to the audio thread, collects [`EngineEvent`]s from it, and hands out
//! a [`Monitor`] for reading the latest [`EngineSnapshot`].

pub mod command;
pub mod dsp;
pub mod engine;
pub mod keylock;
pub mod output;
pub mod processor;
pub mod scratch;
pub mod snapshot;
pub mod tempo;
pub mod transport;

// The offline code moved to `earshot-audio`. Re-exported so existing imports keep
// working; new code should use `earshot_audio` directly.
pub use earshot_audio::{beatgrid, decode, demo, energy, loudness, media, waveform};

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::thread;
use std::time::Duration;

pub use command::{
    ChannelParam, ChannelState, Command, CueSet, DeckCmd, DeckId, DeckState, EQ_HIGH, EQ_LOW,
    EQ_MID, EngineState, HOTCUES, HotcueMode, LoadedTrack, LoopState, MAX_DECKS, MasterParam,
    MixerState, ParamId, Press, PreviewCmd, Set, XfSide,
};
pub use engine::{Out, Routing};
pub use output::{OutputInfo, OutputOptions};
pub use snapshot::{DeckSnap, EngineEvent, EngineSnapshot, MixerSnap, Monitor, PreviewSnap, Stats};
pub use tempo::TempoRange;
pub use transport::LEAD_IN;

use engine::Renderer;

pub struct Engine {
    commands: rtrb::Producer<Command>,
    spare: Vec<rtrb::Producer<Command>>,
    events: rtrb::Consumer<EngineEvent>,
    monitor: Monitor,
    decks: usize,
    info: OutputInfo,
    gc_stop: Arc<AtomicBool>,
    // Dropped last: stops the audio thread.
    _output: output::OutputHandle,
}

impl Engine {
    /// Start audio with `decks` live decks (at most [`MAX_DECKS`]).
    pub fn start(opts: &OutputOptions, decks: usize) -> Result<Engine, String> {
        let decks = decks.min(MAX_DECKS);
        let (app, ends) = engine::wire();
        let mut garbage_rx = app.garbage;

        let gc_stop = Arc::new(AtomicBool::new(false));
        let stop = gc_stop.clone();
        thread::Builder::new()
            .name("earshot-gc".into())
            .spawn(move || {
                while !stop.load(Relaxed) {
                    while let Ok(g) = garbage_rx.pop() {
                        drop(g);
                    }
                    thread::sleep(Duration::from_millis(50));
                }
            })
            .map_err(|e| e.to_string())?;

        let stats = ends.stats().clone();
        let make = move |rate| Renderer::new(ends, rate, decks);
        let (output, info) = output::start(opts, stats, make)?;
        Ok(Engine {
            commands: app.commands,
            spare: app.spare,
            events: app.events,
            monitor: app.monitor,
            decks,
            info,
            gc_stop,
            _output: output,
        })
    }

    /// Queue a command for the audio thread. Fails only if the queue is full.
    pub fn send(&mut self, cmd: Command) -> Result<(), String> {
        self.commands
            .push(cmd)
            .map_err(|_| "engine command queue is full".to_string())
    }

    /// A command ring of its own for another thread (e.g. MIDI), so it can send
    /// without sharing [`Engine::send`]'s. There are a few; `None` once they're gone.
    pub fn sender(&mut self) -> Option<CommandSender> {
        self.spare.pop().map(CommandSender)
    }

    /// How many decks are live.
    pub fn decks(&self) -> usize {
        self.decks
    }

    /// Deck `index`, if it's live.
    pub fn deck(&self, index: usize) -> Option<DeckId> {
        DeckId::new(index).filter(|_| index < self.decks)
    }

    /// The next thing that happened on the audio thread, oldest first.
    pub fn next_event(&mut self) -> Option<EngineEvent> {
        self.events.pop().ok()
    }

    pub fn monitor(&self) -> &Monitor {
        &self.monitor
    }

    /// The state as of the most recent callback.
    pub fn snapshot(&self) -> EngineSnapshot {
        self.monitor.snapshot()
    }

    pub fn info(&self) -> &OutputInfo {
        &self.info
    }
}

/// One thread's own ring into the engine (see [`Engine::sender`]). Commands from one
/// ring arrive in order; rings are drained one after another each block.
pub struct CommandSender(rtrb::Producer<Command>);

impl CommandSender {
    /// Queue a command. Fails if the queue is full or the engine has stopped.
    pub fn send(&mut self, cmd: Command) -> Result<(), String> {
        if self.0.is_abandoned() {
            return Err("audio is not running".into());
        }
        self.0
            .push(cmd)
            .map_err(|_| "engine command queue is full".to_string())
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.gc_stop.store(true, Relaxed);
    }
}
