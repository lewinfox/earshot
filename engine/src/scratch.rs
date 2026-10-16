//! Scratching: a hand on the record. The mouse on a deck's waveform and a controller's
//! jog wheel both say the same two things: the hand is on (or off), and it moved this
//! far (seconds of track, forwards or back) at this time. While the hand is on, the
//! record follows it and plays what passes, like vinyl.
//!
//! The hand's reports come in steps (a mouse about 60 times a second, unevenly; a jog
//! wheel every few milliseconds), so the record follows the hand a moment late, along
//! straight lines between the reports: smooth speed, and it ends up exactly where the
//! hand did, so rocking back and forth comes back to the same spot. The moment is a
//! little more than the time between reports (see [`Scratch::delay`]).
//!
//! Distances are the hand's own travel since it went down, not places in the track, so
//! a hot cue or a loop moving the playhead mid-scratch doesn't drag it back.
//!
//! Let go, and the record goes back to the deck's speed (the play speed, or still):
//! at once from a slow hand, or coasting down from a flick ([`THROW_SPEED`]).

/// Reports kept, oldest first: enough for [`MAX_DELAY`] of a fast jog wheel.
const POINTS: usize = 64;
/// The record follows the hand this long after it, at least and at most: a little
/// more than the time between reports, within these.
const MIN_DELAY: f64 = 0.004;
const MAX_DELAY: f64 = 0.035;
/// How quickly the record's lag behind the hand follows changes in the reports'
/// timing, and how long a late report is remembered (seconds).
const LAG_SECS: f64 = 0.5;
const LATE_SECS: f64 = 2.0;
/// How hard the record is pulled back onto the hand's path when off it (seconds to
/// close a gap).
const PULL_SECS: f64 = 0.01;
/// How quickly the record's speed changes (seconds): fast, but not a click.
const SPEED_SECS: f64 = 0.002;
/// The fastest a hand turns the record, times normal speed.
const MAX_SPEED: f64 = 16.0;
/// Let go faster than this (times normal speed, away from the deck's speed) and the
/// record coasts down to the deck's speed over about [`THROW_SECS`].
pub const THROW_SPEED: f64 = 2.0;
const THROW_SECS: f64 = 0.3;
/// Below a throw, letting go eases back this quickly: no click.
const RELEASE_SECS: f64 = 0.005;
/// A nudge lets go this long after the record has caught up with the wheel's last
/// report.
const NUDGE_IDLE: f64 = 0.05;

#[derive(Clone, Copy, Debug, PartialEq)]
enum State {
    Off,
    /// A hand on the record; `nudge`: no hand really, just a jog wheel turned on a
    /// stopped deck, so it lets go by itself once the wheel stops ([`NUDGE_IDLE`]).
    Held {
        nudge: bool,
    },
    /// Let go: easing to this speed with this time constant.
    Released {
        to: f64,
        secs: f64,
    },
}

pub struct Scratch {
    state: State,
    /// The hand's reports: (time, its travel since it went down), in time order.
    points: [(f64, f64); POINTS],
    len: usize,
    /// The hand's travel so far, and the record's.
    hand: f64,
    record: f64,
    /// The record's speed, track seconds per second (negative: backwards).
    speed: f64,
    /// The time between the hand's reports, smoothed.
    interval: f64,
    /// How late its reports reach the engine (it renders a block at a time), the
    /// latest lately.
    late: f64,
    /// How long after the hand the record follows, easing towards [`Scratch::delay`]:
    /// a sudden change would speed the record up or slow it down. Kept from one
    /// scratch to the next (it's usually the same hand on the same thing).
    lag: f64,
}

impl Default for Scratch {
    fn default() -> Self {
        Scratch {
            state: State::Off,
            points: [(0.0, 0.0); POINTS],
            len: 0,
            hand: 0.0,
            record: 0.0,
            speed: 0.0,
            interval: MAX_DELAY,
            late: 0.0,
            lag: 0.02,
        }
    }
}

impl Scratch {
    /// The hand is on the record, or coasting after it let go: [`Scratch::step`] says
    /// how the record moves, not the deck.
    pub fn active(&self) -> bool {
        self.state != State::Off
    }

    /// The hand is on the record.
    pub fn held(&self) -> bool {
        self.state == State::Held { nudge: false }
    }

    /// The record's speed now (times normal).
    pub fn speed(&self) -> f64 {
        self.speed
    }

    /// The hand goes down at `now`, on a record moving at `speed` (0 if stopped).
    pub fn touch(&mut self, now: f64, speed: f64) {
        if self.state != State::Off {
            // Back on a coasting record: carry on from its speed.
            self.speed = self.speed.clamp(-MAX_SPEED, MAX_SPEED);
        } else {
            self.speed = speed;
        }
        self.state = State::Held { nudge: false };
        self.hand = 0.0;
        self.record = 0.0;
        self.points[0] = (now, 0.0);
        self.len = 1;
        self.interval = MAX_DELAY;
        self.late = 0.0;
    }

    /// The hand moved `by` seconds of track at `at`; the engine heard of it at `now`.
    pub fn moved(&mut self, by: f64, at: f64, now: f64) {
        if !matches!(self.state, State::Held { .. }) {
            return;
        }
        self.late = (self.late * (-(at - self.points[self.len - 1].0).max(0.0) / LATE_SECS).exp())
            .max(now - at);
        let last = self.points[self.len - 1].0;
        let at = at.max(last);
        let gap = at - last;
        if gap < 0.1 {
            self.interval += (gap - self.interval) * 0.3;
        }
        self.hand += by;
        if self.len == POINTS {
            self.points.copy_within(1.., 0);
            self.len -= 1;
        }
        self.points[self.len] = (at, self.hand);
        self.len += 1;
    }

    /// Move a stopped record `by` seconds at `at` (heard of at `now`), as if by hand,
    /// letting go by itself once the moves stop. Nothing if a hand is on it already.
    pub fn nudge(&mut self, by: f64, at: f64, now: f64) {
        if self.held() {
            return;
        }
        if !matches!(self.state, State::Held { nudge: true }) {
            self.touch(at, 0.0);
            self.state = State::Held { nudge: true };
        }
        self.moved(by, at, now);
    }

    /// The hand comes off; the deck's own speed is `to` (0 if it isn't playing).
    pub fn release(&mut self, to: f64) {
        if !matches!(self.state, State::Held { .. }) {
            return;
        }
        let secs = if (self.speed - to).abs() > THROW_SPEED {
            THROW_SECS
        } else {
            RELEASE_SECS
        };
        self.state = State::Released { to, secs };
    }

    /// Stop at once (a track loaded under the hand).
    pub fn cancel(&mut self) {
        self.state = State::Off;
        self.speed = 0.0;
    }

    /// How long after the hand the record should follow: long enough that the next
    /// report is usually in before the record gets to the last.
    fn delay(&self) -> f64 {
        (self.interval * 1.5 + self.late).clamp(MIN_DELAY, MAX_DELAY)
    }

    /// Where the hand was at `t` along the line through its reports, and how fast it
    /// was going (0 before the first and after the last).
    fn hand_at(&mut self, t: f64) -> (f64, f64) {
        // Reports the record has passed are dropped, but the one just before `t` stays.
        let mut drop = 0;
        while drop + 1 < self.len && self.points[drop + 1].0 <= t {
            drop += 1;
        }
        if drop > 0 {
            self.points.copy_within(drop..self.len, 0);
            self.len -= drop;
        }
        let (t0, x0) = self.points[0];
        if self.len < 2 || t < t0 {
            return (x0, 0.0);
        }
        let (t1, x1) = self.points[1];
        if t1 <= t0 {
            return (x1, 0.0);
        }
        let v = (x1 - x0) / (t1 - t0);
        (x0 + v * (t - t0), v)
    }

    /// The record's speed for the sample at `t` (seconds, the engine's clock), `sr`
    /// samples a second. Ends the scratch once a released record is back at the
    /// deck's speed.
    pub fn step(&mut self, t: f64, sr: f64) -> f64 {
        match self.state {
            State::Off => {}
            State::Held { nudge } => {
                let last = self.points[self.len - 1].0;
                if nudge && t - self.lag > last + NUDGE_IDLE {
                    self.release(0.0);
                }
                self.lag += (self.delay() - self.lag) * ease(LAG_SECS, sr);
                let (x, v) = self.hand_at(t - self.lag);
                let want = (v + (x - self.record) / PULL_SECS).clamp(-MAX_SPEED, MAX_SPEED);
                self.speed += (want - self.speed) * ease(SPEED_SECS, sr);
                self.record += self.speed / sr;
            }
            State::Released { to, secs } => {
                self.speed += (to - self.speed) * ease(secs, sr);
                if (self.speed - to).abs() < 0.01 {
                    self.speed = to;
                    self.state = State::Off;
                }
            }
        }
        self.speed
    }
}

/// The share of the way to a target one sample covers, easing over `secs`.
fn ease(secs: f64, sr: f64) -> f64 {
    1.0 - (-1.0 / (secs * sr)).exp()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    /// Run `secs` from `t`, returning where the record got to (its travel).
    fn run(s: &mut Scratch, t: &mut f64, secs: f64) -> f64 {
        let mut x = 0.0;
        for _ in 0..(secs * SR) as usize {
            x += s.step(*t, SR) / SR;
            *t += 1.0 / SR;
        }
        x
    }

    #[test]
    fn a_steady_drag_plays_at_a_steady_speed() {
        // A mouse at 60 reports a second, a little uneven, moving at normal speed.
        let mut s = Scratch::default();
        let mut t = 0.0;
        s.touch(t, 0.0);
        let mut speeds = vec![];
        let mut since = 0.0;
        for i in 0..60 {
            // Each report says how far the hand went since the last one.
            s.moved(since, t, t);
            let dt = if i % 2 == 0 { 0.014 } else { 0.019 };
            since = dt;
            for _ in 0..(dt * SR) as usize {
                speeds.push(s.step(t, SR));
                t += 1.0 / SR;
            }
        }
        // Once going, the speed hardly wavers.
        let settled = &speeds[speeds.len() / 2..];
        let (lo, hi) = settled
            .iter()
            .fold((f64::MAX, f64::MIN), |(l, h), &v| (l.min(v), h.max(v)));
        assert!(lo > 0.8 && hi < 1.2, "{lo}..{hi}");
    }

    /// The speeds while a hand moves at normal speed, reporting every `every` seconds,
    /// the reports heard a block (256 samples) at a time, as the engine hears them.
    fn steady(every: f64) -> (f64, f64) {
        let mut s = Scratch::default();
        s.touch(0.0, 0.0);
        let block = 256;
        let (mut sample, mut sent) = (0usize, 0.0);
        let mut speeds = vec![];
        while sample < (SR * 1.0) as usize {
            let now = sample as f64 / SR;
            while sent + every <= now {
                sent += every;
                s.moved(every, sent, now);
            }
            for i in 0..block {
                speeds.push(s.step(now + i as f64 / SR, SR));
            }
            sample += block;
        }
        speeds[speeds.len() / 2..]
            .iter()
            .fold((f64::MAX, f64::MIN), |(l, h), &v| (l.min(v), h.max(v)))
    }

    #[test]
    fn reports_heard_a_block_late_still_play_smoothly() {
        // A mouse, and a jog wheel ticking every millisecond.
        for every in [1.0 / 60.0, 0.001] {
            let (lo, hi) = steady(every);
            assert!(lo > 0.97 && hi < 1.03, "every {every}: {lo}..{hi}");
        }
    }

    #[test]
    fn a_nudge_moves_its_distance_and_lets_go() {
        let mut s = Scratch::default();
        let mut t = 0.0;
        let mut x = 0.0;
        for _ in 0..20 {
            s.nudge(0.001, t, t);
            x += run(&mut s, &mut t, 0.005);
        }
        x += run(&mut s, &mut t, 0.2);
        assert!((x - 0.02).abs() < 1e-4, "{x}");
        assert!(!s.active());
    }

    #[test]
    fn back_and_forth_returns_to_the_same_spot() {
        let mut s = Scratch::default();
        let mut t = 0.0;
        s.touch(t, 0.0);
        let mut x = 0.0;
        for i in 0..40 {
            // 10 reports forward, 10 back, twice: a jog wheel's ticks.
            let dir = if (i / 10) % 2 == 0 { 1.0 } else { -1.0 };
            s.moved(dir * 0.01, t, t);
            x += run(&mut s, &mut t, 0.005);
        }
        x += run(&mut s, &mut t, 0.1);
        assert!(x.abs() < 1e-4, "{x}");
    }

    #[test]
    fn a_still_hand_stops_a_playing_record() {
        let mut s = Scratch::default();
        let mut t = 0.0;
        s.touch(t, 1.0);
        run(&mut s, &mut t, 0.05);
        assert!(s.speed().abs() < 0.01, "{}", s.speed());
    }

    #[test]
    fn letting_go_eases_back_or_coasts() {
        // Slow: back to the play speed almost at once.
        let mut s = Scratch::default();
        let mut t = 0.0;
        s.touch(t, 1.0);
        run(&mut s, &mut t, 0.05);
        s.release(1.0);
        run(&mut s, &mut t, 0.05);
        assert!(!s.active());
        assert_eq!(s.speed(), 1.0);

        // A flick backwards: it coasts, still going a while later.
        let mut s = Scratch::default();
        let mut t = 0.0;
        s.touch(t, 0.0);
        for _ in 0..10 {
            s.moved(-0.08, t, t);
            run(&mut s, &mut t, 0.01);
        }
        assert!(s.speed() < -THROW_SPEED, "{}", s.speed());
        s.release(1.0);
        run(&mut s, &mut t, 0.1);
        assert!(s.active() && s.speed() < 0.0, "{}", s.speed());
        run(&mut s, &mut t, 3.0);
        assert!(!s.active());
    }
}
