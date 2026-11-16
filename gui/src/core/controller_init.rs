//! The controller's start-up, after audio and MIDI are open: asking a DDJ-400 where
//! its knobs, faders and switches are, so earshot starts out matching it.
//!
//! The order matters. Asked within a moment of its sound card starting to play, a
//! DDJ-400 can leave its headphone mix chirping until it's unplugged; asked before
//! its audio starts (as Mixxx does) or once audio has run a while, it's fine. So
//! the question waits until audio has been running for [`SETTLE`], and any reopen of
//! audio meanwhile starts the wait again.

use std::time::{Duration, Instant};

/// How long audio must have been playing before the controller is asked anything.
pub const SETTLE: Duration = Duration::from_secs(2);

/// What [`ControllerInit::step`] says to do now.
#[derive(Debug, PartialEq)]
pub enum Step {
    /// Nothing to do.
    Idle,
    /// Look again after this long.
    Wait(Duration),
    /// Ask the controller now (once; then [`Step::Idle`] until MIDI connects again).
    Ask {
        /// How long audio had been running.
        audio_for: Duration,
    },
}

#[derive(Debug, Default)]
pub struct ControllerInit {
    /// When audio last started or reopened; `None` until it has.
    audio_started: Option<Instant>,
    /// MIDI is open and the controller hasn't been asked since.
    pending: bool,
}

impl ControllerInit {
    /// Audio started, or reopened, at `now`: the wait starts again.
    pub fn audio_started(&mut self, now: Instant) {
        self.audio_started = Some(now);
    }

    /// MIDI (re)connected: the controller is to be asked, once audio has settled.
    pub fn midi_connected(&mut self) {
        self.pending = true;
    }

    /// What to do at `now`.
    pub fn step(&mut self, now: Instant) -> Step {
        if !self.pending {
            return Step::Idle;
        }
        let Some(started) = self.audio_started else {
            return Step::Wait(SETTLE);
        };
        let audio_for = now.saturating_duration_since(started);
        if audio_for < SETTLE {
            return Step::Wait(SETTLE - audio_for);
        }
        self.pending = false;
        Step::Ask { audio_for }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Asked once audio has run for SETTLE, only after MIDI connects, once each
    /// connect; a reopen of audio meanwhile starts the wait again.
    #[test]
    fn asks_once_audio_has_settled() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut c = ControllerInit::default();
        c.audio_started(at(0));
        assert_eq!(c.step(at(5000)), Step::Idle, "MIDI isn't open");

        c.midi_connected();
        let mut c2 = ControllerInit::default();
        c2.midi_connected();
        assert_eq!(c2.step(at(0)), Step::Wait(SETTLE), "no audio yet");

        let mut c = ControllerInit::default();
        c.audio_started(at(0));
        c.midi_connected();
        assert_eq!(c.step(at(500)), Step::Wait(Duration::from_millis(1500)));
        // Audio reopens: the wait starts again from there.
        c.audio_started(at(1500));
        assert_eq!(c.step(at(3000)), Step::Wait(Duration::from_millis(500)));
        assert_eq!(c.step(at(3500)), Step::Ask { audio_for: Duration::from_millis(2000) });
        assert_eq!(c.step(at(9000)), Step::Idle, "once");

        // A reconnect asks again.
        c.midi_connected();
        assert_eq!(c.step(at(9000)), Step::Ask { audio_for: Duration::from_millis(7500) });
    }
}
