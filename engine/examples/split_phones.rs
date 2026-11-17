//! The engine with the headphones on their own device, on real hardware: prints the
//! headphones' queue (its lowest lately), how often they were brought back in line,
//! and how much they ran dry.
//! Silent (nothing is loaded).
//!
//!   cargo run -p earshot-engine --example split_phones -- <master> <phones> [seconds]

use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

use earshot_engine::{Engine, OutputOptions};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opts = OutputOptions {
        device: args.first().cloned(),
        buffer_frames: Some(256),
        null: false,
        phones_device: args.get(1).cloned(),
        stepped: false,
    };
    let seconds: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(30);
    let engine = Engine::start(&opts, 2).expect("engine");
    let info = engine.info().clone();
    println!("{info:?}");
    let rate = info.sample_rate as f64;
    for i in 1..=seconds / 5 {
        std::thread::sleep(Duration::from_secs(5));
        let s = engine.monitor().stats();
        println!(
            "{:>4}s  headphones queue {:.1} ms · realigned {} · ran dry {} frames · xruns {}",
            i * 5,
            s.phones_level.load(Relaxed) as f64 / rate * 1000.0,
            s.phones_realigns.load(Relaxed),
            s.phones_short.load(Relaxed),
            s.xruns.load(Relaxed),
        );
    }
}
