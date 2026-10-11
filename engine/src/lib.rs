//! earshot's audio engine: beatgrids, decoding and the real-time deck/mixer.
//!
//! [`Engine`] is the control-side handle: it owns the output stream, sends
//! [`Command`]s to the audio thread and exposes the [`Shared`] state it publishes.

pub mod beatgrid;
pub mod decode;
pub mod demo;
pub mod dsp;
pub mod energy;
pub mod engine;
pub mod output;
pub mod waveform;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::thread;
use std::time::Duration;

pub use engine::{
    Command, DECKS, EQ_HIGH, EQ_LOW, EQ_MID, HOTCUES, Out, Press, RATE_RANGE, Routing, Shared,
};
pub use output::{OutputInfo, OutputOptions};

use engine::{Garbage, Renderer};

pub struct Engine {
    commands: rtrb::Producer<Command>,
    shared: Arc<Shared>,
    info: OutputInfo,
    gc_stop: Arc<AtomicBool>,
    // Dropped last: stops the audio thread.
    _output: output::OutputHandle,
}

impl Engine {
    pub fn start(opts: &OutputOptions) -> Result<Engine, String> {
        let (commands, command_rx) = rtrb::RingBuffer::<Command>::new(1024);
        let (garbage_tx, mut garbage_rx) = rtrb::RingBuffer::<Garbage>::new(64);
        let shared = Arc::new(Shared::default());

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

        let s = shared.clone();
        let make = move |rate| Renderer::new(command_rx, garbage_tx, s, rate);
        let (output, info) = output::start(opts, shared.clone(), make)?;
        Ok(Engine {
            commands,
            shared,
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

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    pub fn info(&self) -> &OutputInfo {
        &self.info
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.gc_stop.store(true, Relaxed);
    }
}
