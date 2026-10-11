//! The tempo fader's range, as on a CDJ: ±6, ±10, ±16 % or WIDE.
//!
//! The fader runs -1..1 (Mixxx's `rate` control). Its position maps to a tempo change
//! (`rate`, a fraction: 0.1 = 10 % faster) through the range. WIDE goes from a
//! standstill (0 % speed) to 256 % speed: down to -100 % on one side of the centre
//! and up to +156 % on the other, so the middle is still no change.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TempoRange {
    Six,
    #[default]
    Ten,
    Sixteen,
    Wide,
}

/// WIDE's fastest: 256 % speed.
const WIDE_UP: f64 = 1.56;

impl TempoRange {
    pub const ALL: [TempoRange; 4] = [Self::Six, Self::Ten, Self::Sixteen, Self::Wide];

    /// The next range, wrapping round (for a button that steps through them).
    pub fn next(self) -> TempoRange {
        let i = Self::ALL.iter().position(|r| *r == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Six => "±6",
            Self::Ten => "±10",
            Self::Sixteen => "±16",
            Self::Wide => "WIDE",
        }
    }

    /// Slowest and fastest tempo change.
    pub fn bounds(self) -> (f64, f64) {
        match self {
            Self::Six => (-0.06, 0.06),
            Self::Ten => (-0.10, 0.10),
            Self::Sixteen => (-0.16, 0.16),
            Self::Wide => (-1.0, WIDE_UP),
        }
    }

    /// Fader position (-1..1, minus slower) to tempo change.
    pub fn rate(self, fader: f64) -> f64 {
        let (lo, hi) = self.bounds();
        let f = fader.clamp(-1.0, 1.0);
        if f < 0.0 { -f * lo } else { f * hi }
    }

    /// Tempo change to fader position, clamped to the range.
    pub fn fader(self, rate: f64) -> f64 {
        let (lo, hi) = self.bounds();
        if rate < 0.0 {
            (rate / -lo).max(-1.0)
        } else {
            (rate / hi).min(1.0)
        }
    }

    /// As a number, for publishing through an atomic.
    pub fn index(self) -> u32 {
        Self::ALL.iter().position(|r| *r == self).unwrap_or(0) as u32
    }

    pub fn from_index(i: u32) -> TempoRange {
        Self::ALL.get(i as usize).copied().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fader_and_rate_round_trip() {
        for r in TempoRange::ALL {
            for f in [-1.0, -0.5, 0.0, 0.25, 1.0] {
                assert!((r.fader(r.rate(f)) - f).abs() < 1e-12, "{r:?} {f}");
            }
        }
        assert_eq!(TempoRange::Ten.rate(1.0), 0.10);
        assert_eq!(TempoRange::Wide.rate(-1.0), -1.0); // a standstill
        assert!((TempoRange::Wide.rate(1.0) - 1.56).abs() < 1e-12); // 256 % speed
        // Out of range: the fader stops at its end.
        assert_eq!(TempoRange::Six.fader(0.10), 1.0);
    }
}
