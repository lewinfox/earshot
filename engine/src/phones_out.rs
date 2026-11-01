//! The headphones on their own device (laptop-only DJing: master on the laptop's
//! speakers, headphones on a USB dongle). The engine renders in the master stream's
//! callback and hands the headphone bus over through a queue; the headphone stream's
//! callback plays from it.
//!
//! PipeWire runs both streams on one clock and corrects the second device's drift, so
//! the queue holds its level. But a hiccup (one stream skipping a cycle) leaves it
//! higher or lower for good: the headphones late, or about to run dry. [`Aligner`]
//! notices and brings the level back by playing the headphones very slightly fast or
//! slow ([`NUDGE`]) for a few seconds: no clicks, and the master is never touched. A
//! gap too big to close that way ([`JUMP`]) is closed at once.

use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

use crate::engine::{PhonesRoute, route};
use crate::snapshot::Stats;

/// How much faster or slower the headphones play while catching up: 0.3 %, about 5
/// cents of pitch.
pub const NUDGE: f64 = 0.003;
/// Off by more than this many frames: start nudging.
const START: f64 = 96.0;
/// Within this many: stop (at the next whole frame).
const STOP: f64 = 8.0;
/// Off by more than this (about 40 ms at 48 kHz): jump instead.
pub const JUMP: f64 = 2048.0;
/// The level is the lowest the queue's been in the last [`WINDOW`] × [`BUCKET`]
/// callbacks (about 2 s). Within a cycle the two streams run in either order, so a
/// reading is a chunk higher when the master went first; the lowest is when the
/// headphones did, and that's what has to stay above empty.
const WINDOW: usize = 8;
const BUCKET: u32 = 48;

/// Plays the headphone bus from the queue, keeping the queue at `target` frames.
pub struct Aligner {
    queue: rtrb::Consumer<f32>,
    /// Frames to keep queued.
    target: f64,
    /// The lowest readings of the last [`WINDOW`] buckets (`bucket` the latest
    /// whole one), and of the one being filled.
    lows: [f64; WINDOW],
    bucket: usize,
    calls: u32,
    low_now: f64,
    /// Back in line: stop nudging at the next whole frame.
    stopping: bool,
    /// Frames of queue used per frame played: 1, or 1 ± [`NUDGE`].
    step: f64,
    /// Between `prev` and `next`, 0..1 (played one frame late: `next` is the frame
    /// being blended towards).
    pos: f64,
    prev: [f32; 2],
    next: [f32; 2],
    /// Filling up again after running dry or a jump: silence until the level's back.
    refill: bool,
    route: Arc<PhonesRoute>,
    stats: Arc<Stats>,
}

impl Aligner {
    pub fn new(
        queue: rtrb::Consumer<f32>,
        target: usize,
        route: Arc<PhonesRoute>,
        stats: Arc<Stats>,
    ) -> Aligner {
        Aligner {
            queue,
            target: target as f64,
            lows: [target as f64; WINDOW],
            bucket: 0,
            calls: 0,
            low_now: f64::INFINITY,
            stopping: false,
            step: 1.0,
            pos: 1.0,
            prev: [0.0; 2],
            next: [0.0; 2],
            refill: true,
            route,
            stats,
        }
    }

    /// Frames in the queue now.
    fn queued(&self) -> usize {
        self.queue.slots() / 2
    }

    fn pop(&mut self) -> Option<[f32; 2]> {
        if self.queued() == 0 {
            return None;
        }
        let l = self.queue.pop().ok()?;
        let r = self.queue.pop().unwrap_or(l);
        Some([l, r])
    }

    fn observe(&mut self, now: f64) {
        self.low_now = self.low_now.min(now);
        self.calls += 1;
        if self.calls >= BUCKET {
            self.bucket = (self.bucket + 1) % WINDOW;
            self.lows[self.bucket] = self.low_now;
            self.low_now = f64::INFINITY;
            self.calls = 0;
        }
    }

    /// The queue's level: its lowest lately.
    fn level(&self) -> f64 {
        self.lows.iter().fold(self.low_now, |a, &b| a.min(b))
    }

    /// The level just now (the last bucket and this one): follows a nudge closely.
    fn level_now(&self) -> f64 {
        self.lows[self.bucket].min(self.low_now)
    }

    fn reset_window(&mut self, level: f64) {
        self.lows = [level; WINDOW];
        self.low_now = f64::INFINITY;
        self.calls = 0;
    }

    /// Fill an interleaved device buffer with `channels` channels.
    pub fn process(&mut self, out: &mut [f32], channels: usize) {
        let channels = channels.max(1);
        out.fill(0.0);
        let now = self.queued() as f64;
        if self.refill {
            if now < self.target {
                return;
            }
            self.refill = false;
            // One reading says little (a chunk more if the master went first): take
            // it as in line, and let the next couple of seconds' lowest say.
            self.reset_window(self.target);
            // Start again from what's queued. One frame is held to blend towards, so
            // each callback takes exactly what it plays (the first is silent).
            self.prev = [0.0; 2];
            self.next = [0.0; 2];
            self.pos = 1.0;
        }
        self.observe(now);
        let off = self.level() - self.target;
        if off.abs() > JUMP {
            self.jump(now);
            if self.refill {
                return;
            }
        } else if self.step == 1.0 && off.abs() > START {
            // Too full: the headphones are late, play faster. Too empty: slower.
            self.step = 1.0 + NUDGE * off.signum();
            self.stats.phones_realigns.fetch_add(1, Relaxed);
        } else if self.step != 1.0 && !self.stopping {
            let now_off = self.level_now() - self.target;
            self.stopping = if self.step > 1.0 {
                now_off <= STOP
            } else {
                now_off >= -STOP
            };
        }
        let routing = self.route.get();
        let mut short = 0u64;
        for frame in out.chunks_exact_mut(channels) {
            while self.pos >= 1.0 {
                self.pos -= 1.0;
                self.prev = self.next;
                match self.pop() {
                    Some(f) => self.next = f,
                    None => short += 1,
                }
            }
            let t = self.pos as f32;
            let l = self.prev[0] + (self.next[0] - self.prev[0]) * t;
            let r = self.prev[1] + (self.next[1] - self.prev[1]) * t;
            route(frame, routing, l, r);
            self.pos += self.step;
            // Back in line: stop at a whole frame, so plain playback isn't blurred.
            if self.stopping && self.pos >= 1.0 {
                self.pos = 1.0;
                self.step = 1.0;
                self.stopping = false;
                // What was seen while nudging is out of date.
                let now = self.level_now();
                self.reset_window(now);
            }
        }
        if short > 0 {
            self.stats.phones_short.fetch_add(short, Relaxed);
            self.refill = true;
            self.step = 1.0;
            self.stopping = false;
        }
        self.stats
            .phones_level
            .store(self.level().max(0.0) as u64, Relaxed);
    }

    /// Way off: drop what's too much at once, or wait (silent) for the queue to fill.
    fn jump(&mut self, now: f64) {
        self.stats.phones_realigns.fetch_add(1, Relaxed);
        self.step = 1.0;
        if now > self.target {
            for _ in 0..(now - self.target) as usize {
                let _ = self.pop();
            }
            self.reset_window(self.target);
        } else {
            self.refill = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Out;

    const CHUNK: usize = 256;

    fn setup(target: usize) -> (rtrb::Producer<f32>, Aligner, Arc<Stats>) {
        let (tx, rx) = rtrb::RingBuffer::new(48_000 * 2);
        let stats = Arc::new(Stats::default());
        let route = Arc::new(PhonesRoute::default());
        route.set(Out::Stereo(0));
        (tx, Aligner::new(rx, target, route, stats.clone()), stats)
    }

    /// A ramp, so a played sample says which frame it came from.
    fn feed(tx: &mut rtrb::Producer<f32>, from: &mut u64, n: usize) {
        for _ in 0..n {
            // Wrapping, so f32 keeps its precision.
            let v = (*from % 4096) as f32;
            tx.push(v).unwrap();
            tx.push(v).unwrap();
            *from += 1;
        }
    }

    fn cycle(tx: &mut rtrb::Producer<f32>, a: &mut Aligner, made: &mut u64, out: &mut [f32]) {
        feed(tx, made, CHUNK);
        a.process(out, 2);
    }

    #[test]
    fn steady_queue_plays_every_frame_in_order() {
        let (mut tx, mut a, stats) = setup(512);
        let mut made = 0;
        let mut out = vec![0.0; CHUNK * 2];
        feed(&mut tx, &mut made, 512 - CHUNK);
        let mut last: Option<f32> = None;
        let mut first = true;
        for _ in 0..200 {
            cycle(&mut tx, &mut a, &mut made, &mut out);
            for f in out.chunks(2) {
                if std::mem::take(&mut first) {
                    continue; // silence while the first frame's held
                }
                if let Some(p) = last {
                    assert_eq!(f[0], (p + 1.0) % 4096.0);
                }
                last = Some(f[0]);
            }
        }
        assert_eq!(stats.phones_realigns.load(Relaxed), 0);
        assert_eq!(stats.phones_short.load(Relaxed), 0);
    }

    /// The headphone stream misses a few cycles: the queue's too full (the headphones
    /// late). It's brought back without a jump, playing slightly fast.
    #[test]
    fn a_late_queue_catches_up_gently() {
        let (mut tx, mut a, stats) = setup(512);
        let mut made = 0;
        let mut out = vec![0.0; CHUNK * 2];
        feed(&mut tx, &mut made, 512 - CHUNK);
        for _ in 0..50 {
            cycle(&mut tx, &mut a, &mut made, &mut out);
        }
        feed(&mut tx, &mut made, 3 * CHUNK); // three cycles the phones didn't take
        let mut steps = vec![];
        for _ in 0..3000 {
            let before = out[out.len() - 2];
            cycle(&mut tx, &mut a, &mut made, &mut out);
            // Not across the ramp's wrap (4095 to 0: in between is nonsense).
            if before < 4090.0 && out[0] > 1.0 {
                steps.push(out[0] - before);
            }
        }
        assert!((a.level() - 512.0).abs() <= START, "{}", a.level());
        assert_eq!(stats.phones_realigns.load(Relaxed), 1);
        assert_eq!(stats.phones_short.load(Relaxed), 0);
        // Never more than a hair faster than real time, sample to sample.
        let bad: Vec<_> = steps
            .iter()
            .enumerate()
            .filter(|(_, s)| !(0.99..=1.01).contains(*s))
            .collect();
        assert!(bad.is_empty(), "{bad:?}");
        assert_eq!(a.step, 1.0);
    }

    /// The master stream misses cycles: the queue's too low. It plays slightly slow
    /// until it's back.
    #[test]
    fn an_early_queue_slows_down() {
        let (mut tx, mut a, stats) = setup(1024);
        let mut made = 0;
        let mut out = vec![0.0; CHUNK * 2];
        feed(&mut tx, &mut made, 1024 - CHUNK);
        for _ in 0..50 {
            cycle(&mut tx, &mut a, &mut made, &mut out);
        }
        for _ in 0..2 {
            a.process(&mut out, 2); // two cycles the master didn't make
        }
        for _ in 0..3000 {
            cycle(&mut tx, &mut a, &mut made, &mut out);
        }
        assert!((a.level() - 1024.0).abs() <= START, "{}", a.level());
        assert_eq!(stats.phones_short.load(Relaxed), 0);
        assert!(stats.phones_realigns.load(Relaxed) >= 1);
    }

    #[test]
    fn far_off_jumps() {
        let (mut tx, mut a, stats) = setup(512);
        let mut made = 0;
        let mut out = vec![0.0; CHUNK * 2];
        feed(&mut tx, &mut made, 512 - CHUNK);
        cycle(&mut tx, &mut a, &mut made, &mut out);
        feed(&mut tx, &mut made, 20_000);
        for _ in 0..1000 {
            cycle(&mut tx, &mut a, &mut made, &mut out);
        }
        assert!((a.level() - 512.0).abs() <= START, "{}", a.level());
        assert!(stats.phones_realigns.load(Relaxed) >= 1);
    }

    /// The streams run in either order from cycle to cycle: a chunk queued is enough
    /// (the lowest reading is held there), and nothing runs dry.
    #[test]
    fn either_order_with_one_chunk_queued() {
        let (mut tx, mut a, stats) = setup(CHUNK);
        let mut made = 0;
        let mut out = vec![0.0; CHUNK * 2];
        feed(&mut tx, &mut made, CHUNK);
        // A fixed but irregular pattern of who goes first.
        let mut seed = 7u32;
        for _ in 0..5000 {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            if seed >> 16 & 1 == 0 {
                cycle(&mut tx, &mut a, &mut made, &mut out);
            } else {
                a.process(&mut out, 2);
                feed(&mut tx, &mut made, CHUNK);
            }
        }
        assert_eq!(stats.phones_short.load(Relaxed), 0);
        assert_eq!(stats.phones_realigns.load(Relaxed), 0);
        assert!(a.queued() <= 2 * CHUNK);
    }

    /// Running dry: silence, then it waits for the level before playing again.
    #[test]
    fn running_dry_refills_first() {
        let (mut tx, mut a, stats) = setup(512);
        let mut made = 0;
        let mut out = vec![0.0; CHUNK * 2];
        feed(&mut tx, &mut made, 512 - CHUNK);
        cycle(&mut tx, &mut a, &mut made, &mut out);
        for _ in 0..10 {
            a.process(&mut out, 2);
        }
        assert!(stats.phones_short.load(Relaxed) > 0);
        for _ in 0..200 {
            cycle(&mut tx, &mut a, &mut made, &mut out);
        }
        assert!((a.level() - 512.0).abs() <= START, "{}", a.level());
    }
}
