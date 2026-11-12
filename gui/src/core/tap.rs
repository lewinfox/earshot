//! A tempo from taps (an effect unit's BPM tap mode): the average gap between the
//! last few taps, counted afresh after a pause.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// A gap longer than this starts a new count.
const PAUSE: Duration = Duration::from_secs(2);
/// The tempo is the average over this many gaps (or as many as there are).
const GAPS: usize = 4;

#[derive(Default)]
pub struct Taps {
    at: VecDeque<Instant>,
}

impl Taps {
    /// A tap at `at`: the tempo (BPM) from the taps so far, once there are two.
    pub fn tap(&mut self, at: Instant) -> Option<f64> {
        if self
            .at
            .back()
            .is_some_and(|last| at.saturating_duration_since(*last) > PAUSE)
        {
            self.at.clear();
        }
        self.at.push_back(at);
        while self.at.len() > GAPS + 1 {
            self.at.pop_front();
        }
        let (first, last) = (self.at.front()?, self.at.back()?);
        let gaps = self.at.len() - 1;
        let span = last.saturating_duration_since(*first).as_secs_f64();
        (gaps > 0 && span > 0.0).then(|| 60.0 * gaps as f64 / span)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taps_make_a_tempo() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut t = Taps::default();
        assert_eq!(t.tap(ms(0)), None, "one tap is no tempo");
        assert_eq!(t.tap(ms(500)), Some(120.0));
        // Uneven taps average out: 500, 480, 520 ms.
        t.tap(ms(980));
        assert_eq!(t.tap(ms(1500)), Some(120.0));
        // Only the last four gaps count: four at 400 ms forget the 500s.
        for n in 1..=4 {
            t.tap(ms(1500 + 400 * n));
        }
        assert_eq!(t.tap(ms(3500)), Some(150.0));
        // After a pause, a new count.
        assert_eq!(t.tap(ms(9000)), None);
        assert_eq!(t.tap(ms(9600)), Some(100.0));
    }
}
