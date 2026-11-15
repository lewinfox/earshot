//! Doing less when the machine can't keep up. Each audio callback is timed against
//! how long the audio it made lasts (its load: 1 means it only just made it).
//!
//! What goes first is what matters least to how the tracks sound ([`Shedding`]): when
//! the load stays high the effects go off (the effect units and the pad effects).
//! Keylock is part of the sound of the tracks themselves, so it's kept as it is for
//! as long as it can be: only if the load stays high with the effects already off
//! does it go over to its cheaper setting
//! ([`crate::keylock::Keylock::set_cheap`]). When there has been room for a while
//! they come back in the other order: keylock's full setting first, then the effects.
//!
//! One [`LoadWatch`] decides each step (called "cheap" below, after keylock's).
//!
//! Going back is the slow direction, so it doesn't flap: the cheaper setting costs
//! less, so "room" means well under the load that set it off, held for
//! [`RECOVER_SECS`]; and if the full setting is too much again soon after, the next
//! wait is twice as long. Every switch re-primes the decks' shifters, which can be
//! heard as a small bump.

/// A callback using this much of its time or more counts as behind.
pub const HIGH: f32 = 0.75;
/// On the cheaper setting, this much or less is room to go back.
pub const LOW: f32 = 0.35;
/// Behind for this long (seconds of audio): go cheaper.
pub const STRAIN_SECS: f64 = 0.2;
/// Room for this long: go back to the full setting.
pub const RECOVER_SECS: f64 = 10.0;
/// The longest wait before going back, after going back keeps not working.
pub const RECOVER_MAX_SECS: f64 = 320.0;
/// Back on the full setting for less than this before it's too much again: going
/// back was a mistake, and the next wait doubles.
const SETTLED_SECS: f64 = 60.0;

/// Decides between keylock's full and cheaper settings from each callback's load.
/// Plain arithmetic: nothing here allocates, locks or waits.
#[derive(Debug, Clone)]
pub struct LoadWatch {
    cheap: bool,
    /// Seconds behind (on the full setting) or with room (on the cheaper one), lately:
    /// counts up while it is so and down while it isn't, so one odd callback neither
    /// sets it off nor starts the count again.
    held: f64,
    /// Seconds since the last switch.
    since: f64,
    /// How long there must be room before going back.
    recover: f64,
}

impl Default for LoadWatch {
    fn default() -> Self {
        LoadWatch {
            cheap: false,
            held: 0.0,
            since: f64::INFINITY,
            recover: RECOVER_SECS,
        }
    }
}

impl LoadWatch {
    /// Whether keylock should be on its cheaper setting.
    pub fn cheap(&self) -> bool {
        self.cheap
    }

    /// A callback made `secs` of audio using `load` of that time. Returns
    /// [`LoadWatch::cheap`] as it is now.
    pub fn callback(&mut self, load: f32, secs: f64) -> bool {
        self.since += secs;
        let (counts, enough) = if self.cheap {
            (load <= LOW, self.recover)
        } else {
            (load >= HIGH, STRAIN_SECS)
        };
        self.held = if counts {
            self.held + secs
        } else {
            (self.held - secs).max(0.0)
        };
        if self.held >= enough {
            self.switch(!self.cheap);
        }
        self.cheap
    }

    /// The real-time limit was hit ([`crate::realtime`]): go cheaper now.
    pub fn overrun(&mut self) {
        if !self.cheap {
            self.switch(true);
        }
    }

    fn switch(&mut self, cheap: bool) {
        if cheap {
            self.recover = if self.since < SETTLED_SECS {
                (self.recover * 2.0).min(RECOVER_MAX_SECS)
            } else {
                RECOVER_SECS
            };
        }
        self.cheap = cheap;
        self.held = 0.0;
        self.since = 0.0;
    }
}

/// A load that counts as neither behind nor room: what a step's watch is given while
/// it's another step's turn.
const NEUTRAL: f32 = (HIGH + LOW) / 2.0;

/// What's being left out to keep up, in the order it goes: the effects, then (only
/// if that wasn't enough) keylock's full setting. Plain arithmetic, like its two
/// [`LoadWatch`]es.
#[derive(Debug, Clone, Default)]
pub struct Shedding {
    effects: LoadWatch,
    keylock: LoadWatch,
}

impl Shedding {
    /// Whether the effects are off.
    pub fn effects_off(&self) -> bool {
        self.effects.cheap()
    }

    /// Whether keylock is on its cheaper setting. Never without the effects off.
    pub fn keylock_cheap(&self) -> bool {
        self.keylock.cheap()
    }

    /// A callback made `secs` of audio using `load` of that time.
    pub fn callback(&mut self, load: f32, secs: f64) {
        // Keylock's turn only with the effects off; the effects come back only once
        // keylock is on its full setting again.
        let (effects, keylock) = match (self.effects.cheap(), self.keylock.cheap()) {
            (false, _) => (load, NEUTRAL),
            (true, false) => (load, load),
            (true, true) => (NEUTRAL, load),
        };
        self.effects.callback(effects, secs);
        self.keylock.callback(keylock, secs);
    }

    /// The real-time limit was hit ([`crate::realtime`]): the effects go off now.
    /// Keylock follows only if the load stays high after that.
    pub fn overrun(&mut self) {
        self.effects.overrun();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_effects_go_first_and_keylock_only_if_that_was_not_enough() {
        let mut s = Shedding::default();
        let run = |s: &mut Shedding, load: f32, secs: f64| {
            let mut t = 0.0;
            while t < secs {
                t += BLOCK;
                s.callback(load, BLOCK);
            }
        };
        run(&mut s, 0.3, 30.0);
        assert!(!s.effects_off() && !s.keylock_cheap());
        // Behind: the effects go, and that brings the load down enough.
        run(&mut s, 0.9, STRAIN_SECS + 0.05);
        assert!(s.effects_off() && !s.keylock_cheap());
        run(&mut s, 0.6, 120.0);
        assert!(s.effects_off() && !s.keylock_cheap(), "keylock is kept");
        // Still behind with them off: keylock's cheaper setting.
        run(&mut s, 0.9, STRAIN_SECS + 0.05);
        assert!(s.effects_off() && s.keylock_cheap());
        // Room: keylock's full setting first, the effects a while after.
        run(&mut s, 0.1, RECOVER_SECS + 0.1);
        assert!(s.effects_off() && !s.keylock_cheap());
        run(&mut s, 0.1, RECOVER_SECS + 0.1);
        assert!(!s.effects_off() && !s.keylock_cheap());
        // The real-time limit: the effects at once, keylock not.
        s.overrun();
        assert!(s.effects_off() && !s.keylock_cheap());
    }

    /// A 256-frame block at 48 kHz.
    const BLOCK: f64 = 256.0 / 48_000.0;

    /// Feed `secs` of callbacks at `load`; the time (s) of the first switch, if any.
    fn feed(w: &mut LoadWatch, load: f32, secs: f64) -> Option<f64> {
        let before = w.cheap();
        let mut t = 0.0;
        while t < secs {
            t += BLOCK;
            if w.callback(load, BLOCK) != before {
                return Some(t);
            }
        }
        None
    }

    #[test]
    fn sustained_load_goes_cheaper_and_a_spike_does_not() {
        let mut w = LoadWatch::default();
        assert_eq!(feed(&mut w, 0.3, 60.0), None, "plenty of room");
        // A few slow callbacks among fast ones: not sustained.
        for _ in 0..200 {
            assert!(!w.callback(0.95, BLOCK));
            assert!(!w.callback(0.2, BLOCK));
            assert!(!w.callback(0.2, BLOCK));
        }
        assert_eq!(feed(&mut w, 0.74, 60.0), None, "just under the mark");
        let at = feed(&mut w, 0.8, 5.0).expect("goes cheaper");
        assert!((at - STRAIN_SECS).abs() < 2.0 * BLOCK, "after {at:.3} s");
        assert!(w.cheap());
    }

    #[test]
    fn mostly_behind_still_counts() {
        // Three callbacks in four behind: it gets there, more slowly.
        let mut w = LoadWatch::default();
        let mut blocks = 0;
        while !w.cheap() {
            blocks += 1;
            assert!(blocks < 1000);
            w.callback(if blocks % 4 == 0 { 0.5 } else { 0.9 }, BLOCK);
        }
        let secs = blocks as f64 * BLOCK;
        assert!(
            secs > STRAIN_SECS && secs < 3.0 * STRAIN_SECS,
            "{secs:.3} s"
        );
    }

    #[test]
    fn goes_back_only_with_room_for_a_while() {
        let mut w = LoadWatch::default();
        feed(&mut w, 0.9, 1.0);
        assert!(w.cheap());
        // The cheaper setting brought the load down, but not to where the full one
        // would fit: stay.
        assert_eq!(feed(&mut w, 0.5, 120.0), None);
        assert_eq!(feed(&mut w, LOW + 0.01, 120.0), None);
        // Room, but not for long enough; then busy again, which eats into the count.
        assert_eq!(feed(&mut w, 0.2, RECOVER_SECS / 2.0), None);
        assert_eq!(feed(&mut w, 0.6, RECOVER_SECS), None);
        let at = feed(&mut w, 0.2, 60.0).expect("goes back");
        assert!((at - RECOVER_SECS).abs() < 2.0 * BLOCK, "after {at:.2} s");
        assert!(!w.cheap());
    }

    #[test]
    fn going_back_too_soon_doubles_the_next_wait() {
        let mut w = LoadWatch::default();
        let mut waits = vec![];
        for _ in 0..8 {
            feed(&mut w, 0.9, 1.0);
            assert!(w.cheap());
            waits.push(feed(&mut w, 0.1, 1000.0).expect("goes back"));
        }
        for (i, want) in [10.0, 20.0, 40.0, 80.0, 160.0, 320.0, 320.0, 320.0]
            .iter()
            .enumerate()
        {
            assert!((waits[i] - want).abs() < 0.05, "{waits:?}");
        }
        // The full setting held for a good while: the wait starts over.
        assert_eq!(feed(&mut w, 0.3, SETTLED_SECS + 1.0), None);
        feed(&mut w, 0.9, 1.0);
        let at = feed(&mut w, 0.1, 1000.0).expect("goes back");
        assert!((at - RECOVER_SECS).abs() < 0.05, "after {at:.2} s");
    }

    #[test]
    fn hitting_the_real_time_limit_goes_cheaper_at_once() {
        let mut w = LoadWatch::default();
        w.overrun();
        assert!(w.cheap());
        // Already there: nothing more to do, and the count isn't started again.
        feed(&mut w, 0.1, RECOVER_SECS / 2.0);
        w.overrun();
        let at = feed(&mut w, 0.1, 60.0).expect("goes back");
        assert!((at - RECOVER_SECS / 2.0).abs() < 2.0 * BLOCK, "{at:.2} s");
    }
}
