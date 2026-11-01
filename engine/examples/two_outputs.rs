//! Do two outputs on different devices stay in step under PipeWire?
//!
//! The first stream ("master") makes the sound and hands a copy to the second
//! ("phones") through a queue, as earshot would. If PipeWire keeps the two devices'
//! clocks together, the queue stays level; if not, it fills or empties steadily.
//!
//!   cargo run -p earshot-engine --example two_outputs -- list
//!   cargo run ... -- <master device> <phones device> [seconds] [--silent]
//!
//! Devices are matched by part of their name. A quiet tick plays once a second on
//! both, so you can also listen for them drifting apart.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, HostId, StreamConfig};

const RATE: u32 = 48_000;
const CHUNK: u32 = 256;
/// Frames kept in the queue at the start: its level to hold.
const PREFILL: usize = 1024;

#[derive(Default)]
struct Counts {
    made: AtomicU64,
    played: AtomicU64,
    /// Frames the phones stream wanted and the queue didn't have.
    short: AtomicU64,
    /// Frames the queue had no room for.
    over: AtomicU64,
    errors: AtomicU64,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let host = cpal::host_from_id(HostId::PipeWire).expect("PipeWire host");
    let outputs: Vec<_> = host
        .output_devices()
        .expect("devices")
        .filter_map(|d| Some((d.description().ok()?.name().to_string(), d)))
        .collect();
    if args.first().is_none_or(|a| a == "list") {
        for (name, d) in &outputs {
            let id = d.id().map(|i| i.to_string()).unwrap_or_default();
            let ch = d
                .supported_output_configs()
                .map(|c| c.map(|c| c.channels()).max().unwrap_or(0))
                .unwrap_or(0);
            println!("{name}  [{id}]  {ch} ch");
        }
        return;
    }
    let find = |part: &str| {
        let part = part.to_lowercase();
        outputs
            .iter()
            .find(|(n, _)| n.to_lowercase().contains(&part))
            .unwrap_or_else(|| panic!("no output device matching {part:?}"))
            .clone()
    };
    let (master_name, master) = find(&args[0]);
    let (phones_name, phones) = find(&args[1]);
    let seconds: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(600);
    let tick = !args.iter().any(|a| a == "--silent");
    println!("master: {master_name}\nphones: {phones_name}\n{seconds} s\n");

    let config = StreamConfig {
        channels: 2,
        sample_rate: RATE,
        buffer_size: BufferSize::Fixed(CHUNK),
    };
    let (mut tx, mut rx) = rtrb::RingBuffer::<f32>::new(RATE as usize * 2);
    for _ in 0..PREFILL * 2 {
        tx.push(0.0).unwrap();
    }
    let counts = Arc::new(Counts::default());

    let c = counts.clone();
    let mut frame: u64 = 0;
    let master_stream = master
        .build_output_stream(
            config,
            move |out: &mut [f32], _| {
                for f in out.chunks_mut(2) {
                    // 20 ms of 1 kHz at the top of each second.
                    let t = frame % RATE as u64;
                    let s = if tick && t < RATE as u64 / 50 {
                        0.05 * (t as f32 * 1000.0 * std::f32::consts::TAU / RATE as f32).sin()
                    } else {
                        0.0
                    };
                    f[0] = s;
                    f[1] = s;
                    for _ in 0..2 {
                        if tx.push(s).is_err() {
                            c.over.fetch_add(1, Relaxed);
                        }
                    }
                    frame += 1;
                }
                c.made.fetch_add(out.len() as u64 / 2, Relaxed);
            },
            {
                let c = counts.clone();
                move |e| {
                    c.errors.fetch_add(1, Relaxed);
                    eprintln!("master: {e}");
                }
            },
            None,
        )
        .expect("master stream");

    let c = counts.clone();
    let phones_stream = phones
        .build_output_stream(
            config,
            move |out: &mut [f32], _| {
                for s in out.iter_mut() {
                    *s = rx.pop().unwrap_or_else(|_| {
                        c.short.fetch_add(1, Relaxed);
                        0.0
                    });
                }
                c.played.fetch_add(out.len() as u64 / 2, Relaxed);
            },
            {
                let c = counts.clone();
                move |e| {
                    c.errors.fetch_add(1, Relaxed);
                    eprintln!("phones: {e}");
                }
            },
            None,
        )
        .expect("phones stream");

    master_stream.play().unwrap();
    phones_stream.play().unwrap();
    let start = Instant::now();
    // Level samples once both streams run, for the fit.
    let mut levels: Vec<(f64, f64)> = vec![];
    println!(
        "{:>6} {:>10} {:>10} {:>7} {:>6} {:>6} {:>4}",
        "s", "made", "played", "queue", "short", "over", "err"
    );
    while start.elapsed() < Duration::from_secs(seconds) {
        std::thread::sleep(Duration::from_secs(10));
        let t = start.elapsed().as_secs_f64();
        let made = counts.made.load(Relaxed);
        let played = counts.played.load(Relaxed);
        // Frames in the queue: the prefill, plus made minus played (short frames were
        // played as silence without taking from it).
        let short = counts.short.load(Relaxed) / 2;
        let over = counts.over.load(Relaxed) / 2;
        let queue = PREFILL as i64 + made as i64 - over as i64 - (played as i64 - short as i64);
        levels.push((t, queue as f64));
        println!(
            "{t:>6.0} {made:>10} {played:>10} {queue:>7} {short:>6} {over:>6} {:>4}",
            counts.errors.load(Relaxed)
        );
    }

    // The queue's trend, by least squares, after the first 30 s settle.
    let pts: Vec<_> = levels.iter().filter(|p| p.0 > 30.0).copied().collect();
    if pts.len() >= 3 {
        let n = pts.len() as f64;
        let (mx, my) = (
            pts.iter().map(|p| p.0).sum::<f64>() / n,
            pts.iter().map(|p| p.1).sum::<f64>() / n,
        );
        let slope = pts.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum::<f64>()
            / pts.iter().map(|p| (p.0 - mx).powi(2)).sum::<f64>();
        let (lo, hi) = pts.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| {
            (lo.min(p.1), hi.max(p.1))
        });
        println!(
            "\nqueue trend: {slope:+.3} frames/s ({:+.1} ppm); level {lo:.0}..{hi:.0} frames",
            slope / RATE as f64 * 1e6
        );
        println!(
            "uncorrected, a 20 ppm difference would be {:.1} frames/s",
            20e-6 * RATE as f64
        );
    }
}
