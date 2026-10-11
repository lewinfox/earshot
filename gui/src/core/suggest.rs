//! "What could I play next?" Score every candidate against a reference (the selected
//! track, or the playing deck at its actual pitch) and keep the breakdown for display.

use std::collections::HashSet;

use super::keys::{Camelot, transpose, wheel_distance};

/// Weights of the parts of a score. Vibe (sound-alike embeddings) isn't built yet, so
/// every pair scores a neutral 0.5 there; it keeps its weight so scores won't shift
/// scale once it is.
const W_KEY: f64 = 0.35;
const W_TEMPO: f64 = 0.3;
const W_VIBE: f64 = 0.25;
const W_ENERGY: f64 = 0.1;
const VIBE: f64 = 0.5;

/// A tempo change bigger than this (6 %) scores zero.
pub const TEMPO_TOLERANCE: f64 = 0.06;

/// Desired energy change after the reference.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Direction {
    Drop,
    #[default]
    Hold,
    Build,
}

impl Direction {
    pub fn delta(self) -> f32 {
        match self {
            Direction::Drop => -0.15,
            Direction::Hold => 0.0,
            Direction::Build => 0.15,
        }
    }
}

/// What a scored track needs to know about itself.
#[derive(Clone, Copy, Debug)]
pub struct Candidate {
    pub id: i64,
    pub bpm: Option<f64>,
    pub key: Option<Camelot>,
    /// Ranked across the library, 0..1.
    pub energy: f32,
}

pub struct Reference<'a> {
    pub id: i64,
    /// Effective BPM: what the deck is actually playing at.
    pub bpm: Option<f64>,
    /// Effective key (shifted by pitch, as there is no key lock).
    pub key: Option<Camelot>,
    pub energy: f32,
    pub direction: Direction,
    /// Tracks already played this session score lower.
    pub played: &'a HashSet<i64>,
}

#[derive(Clone, Copy, Debug)]
pub struct Scored {
    pub id: i64,
    pub total: f64,
    pub key: f64,
    pub tempo: f64,
    pub energy: f64,
    /// Candidate tempo after half/double-time matching, as a change from the reference.
    pub tempo_change: Option<f64>,
}

pub fn key_score(a: Option<Camelot>, b: Option<Camelot>) -> f64 {
    let (Some(a), Some(b)) = (a, b) else {
        return 0.3;
    };
    let d = wheel_distance(a, b);
    if a.minor == b.minor {
        match d {
            0 => 1.0,
            1 => 0.9,
            2 => 0.45, // energy boost (+2)
            5 => 0.35, // up or down a semitone (7 steps round the wheel)
            _ => 0.0,
        }
    } else {
        // Relative major/minor shares the number.
        match d {
            0 => 0.85,
            1 => 0.4, // diagonal move
            _ => 0.0,
        }
    }
}

/// Fractional tempo change to reach `cand` from `reference`, allowing half/double time.
pub fn tempo_change(reference: f64, cand: f64) -> f64 {
    [cand, cand * 2.0, cand / 2.0]
        .into_iter()
        .map(|c| c / reference - 1.0)
        .min_by(|a, b| a.abs().total_cmp(&b.abs()))
        .unwrap()
}

/// Effective key of a deck: pitching by `speed` without key lock moves the key.
pub fn effective_key(key: Option<Camelot>, speed: f64) -> Option<Camelot> {
    let semis = if speed > 0.0 {
        (12.0 * speed.log2()).round() as i32
    } else {
        0
    };
    key.map(|k| transpose(k, semis))
}

pub fn score(r: &Reference, c: &Candidate) -> Scored {
    let key = key_score(r.key, c.key);
    let (tempo, tempo_change) = match (r.bpm, c.bpm) {
        (Some(rb), Some(cb)) if rb > 0.0 && cb > 0.0 => {
            let ch = tempo_change(rb, cb);
            ((1.0 - ch.abs() / TEMPO_TOLERANCE).max(0.0), Some(ch))
        }
        _ => (0.3, None),
    };
    let energy = (1.0 - ((c.energy - r.energy - r.direction.delta()).abs() * 3.0) as f64).max(0.0);
    let mut total = W_KEY * key + W_TEMPO * tempo + W_VIBE * VIBE + W_ENERGY * energy;
    if tempo == 0.0 {
        total *= 0.3; // unmixable without a big pitch change
    }
    if r.played.contains(&c.id) {
        total *= 0.2;
    }
    Scored {
        id: c.id,
        total,
        key,
        tempo,
        energy,
        tempo_change,
    }
}

/// The best `n` candidates, best first, leaving out the reference itself.
pub fn suggest(r: &Reference, cands: &[Candidate], n: usize) -> Vec<Scored> {
    let mut out: Vec<Scored> = cands
        .iter()
        .filter(|c| c.id != r.id)
        .map(|c| score(r, c))
        .collect();
    out.sort_by(|a, b| b.total.total_cmp(&a.total));
    out.truncate(n);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::keys::parse_key;

    fn cand(id: i64, bpm: f64, key: &str, energy: f32) -> Candidate {
        Candidate {
            id,
            bpm: Some(bpm),
            key: parse_key(key),
            energy,
        }
    }

    #[test]
    fn keys() {
        let k = |s| parse_key(s);
        assert_eq!(key_score(k("8A"), k("8A")), 1.0);
        assert_eq!(key_score(k("8A"), k("9A")), 0.9);
        assert_eq!(key_score(k("8A"), k("8B")), 0.85);
        assert_eq!(key_score(k("8A"), k("10A")), 0.45);
        assert_eq!(key_score(k("8A"), k("3A")), 0.35); // a semitone up
        assert_eq!(key_score(k("8A"), k("2A")), 0.0);
        assert_eq!(key_score(None, k("2A")), 0.3);
    }

    #[test]
    fn half_and_double_time() {
        assert!((tempo_change(128.0, 64.0)).abs() < 1e-9);
        assert!((tempo_change(87.0, 174.0)).abs() < 1e-9);
        assert!((tempo_change(125.0, 126.25) - 0.01).abs() < 1e-9);
    }

    #[test]
    fn pitch_moves_the_key() {
        let am = parse_key("8A");
        assert_eq!(effective_key(am, 1.0), am);
        assert_eq!(effective_key(am, 2f64.powf(1.0 / 12.0)), parse_key("3A"));
        assert_eq!(effective_key(am, 1.02), am); // +2 % rounds to no change
    }

    #[test]
    fn ranking() {
        let played = HashSet::from([4]);
        let r = Reference {
            id: 0,
            bpm: Some(125.0),
            key: parse_key("8A"),
            energy: 0.5,
            direction: Direction::Build,
            played: &played,
        };
        let cands = [
            cand(0, 125.0, "8A", 0.5),  // the reference itself
            cand(1, 125.0, "8A", 0.65), // same key, a bit more energy
            cand(2, 125.0, "8A", 0.35), // same key, less energy
            cand(3, 140.0, "8A", 0.65), // too far in tempo
            cand(4, 125.0, "8A", 0.65), // already played
            cand(5, 126.0, "2B", 0.65), // clashing key
        ];
        let ids: Vec<i64> = suggest(&r, &cands, 10).iter().map(|s| s.id).collect();
        assert_eq!(ids, vec![1, 2, 5, 4, 3]);
    }
}
