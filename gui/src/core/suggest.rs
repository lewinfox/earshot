//! "What could I play next?" Score every candidate against a reference (the selected
//! track, or the playing deck at its actual pitch) and keep the breakdown for display.
//!
//! A [`Scorer`] is a list of weighted [`Feature`]s. A feature that can't tell (a key
//! or BPM is unknown) returns `None`, and its weight is shared among the others, so a
//! track isn't scored on a made-up middling value. Vibe (sound-alike embeddings) will
//! be another feature; until then it simply isn't in the list.

use std::collections::HashSet;

use super::keys::{Camelot, transpose, wheel_distance};

/// A tempo change bigger than this (6 %) scores zero.
pub const TEMPO_TOLERANCE: f64 = 0.06;

/// Multiplies the total when the tempo is too far to mix without a big pitch change.
const UNMIXABLE: f64 = 0.3;
/// Multiplies the total for a track already played this session.
const PLAYED: f64 = 0.2;

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
    /// Effective key: shifted by the tempo fader unless the deck has keylock on.
    pub key: Option<Camelot>,
    pub energy: f32,
    pub direction: Direction,
    /// Tracks already played this session score lower.
    pub played: &'a HashSet<i64>,
    /// Tracks never to suggest (say, the ones already in the Flow being built).
    pub exclude: &'a HashSet<i64>,
}

/// One part of a score.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub name: &'static str,
    /// 0..1, or `None` when this feature couldn't tell.
    pub score: Option<f64>,
    /// Its share of the total: its weight over the weights of the parts that scored.
    pub share: f64,
}

#[derive(Clone, Debug)]
pub struct Scored {
    pub id: i64,
    pub total: f64,
    /// Every feature, in the scorer's order.
    pub parts: Vec<Part>,
    /// Candidate tempo after half/double-time matching, as a change from the reference.
    pub tempo_change: Option<f64>,
}

impl Scored {
    /// One part's score, by feature name.
    pub fn part(&self, name: &str) -> Option<f64> {
        self.parts.iter().find(|p| p.name == name)?.score
    }
}

/// One thing a good next track has in common with the reference.
pub trait Feature: Send + Sync {
    fn name(&self) -> &'static str;
    /// 0..1, or `None` when either track lacks what this needs.
    fn score(&self, r: &Reference, c: &Candidate) -> Option<f64>;
}

/// Harmonic mixing on the Camelot wheel.
pub struct Key;
/// Within [`TEMPO_TOLERANCE`], allowing half and double time.
pub struct Tempo;
/// Energy moving the way the [`Direction`] asks.
pub struct Energy;

impl Feature for Key {
    fn name(&self) -> &'static str {
        "key"
    }
    fn score(&self, r: &Reference, c: &Candidate) -> Option<f64> {
        Some(key_score(r.key?, c.key?))
    }
}

impl Feature for Tempo {
    fn name(&self) -> &'static str {
        "tempo"
    }
    fn score(&self, r: &Reference, c: &Candidate) -> Option<f64> {
        let ch = change(r, c)?;
        Some((1.0 - ch.abs() / TEMPO_TOLERANCE).max(0.0))
    }
}

impl Feature for Energy {
    fn name(&self) -> &'static str {
        "energy"
    }
    fn score(&self, r: &Reference, c: &Candidate) -> Option<f64> {
        let off = (c.energy - r.energy - r.direction.delta()).abs() as f64;
        Some((1.0 - off * 3.0).max(0.0))
    }
}

fn change(r: &Reference, c: &Candidate) -> Option<f64> {
    match (r.bpm, c.bpm) {
        (Some(rb), Some(cb)) if rb > 0.0 && cb > 0.0 => Some(tempo_change(rb, cb)),
        _ => None,
    }
}

/// Weighted features, then two penalties: a tempo too far to mix, and a track already
/// played this session.
pub struct Scorer {
    pub features: Vec<(Box<dyn Feature>, f64)>,
}

impl Default for Scorer {
    fn default() -> Self {
        Scorer {
            features: vec![
                (Box::new(Key), 0.35),
                (Box::new(Tempo), 0.3),
                (Box::new(Energy), 0.1),
            ],
        }
    }
}

impl Scorer {
    pub fn score(&self, r: &Reference, c: &Candidate) -> Scored {
        let scores: Vec<Option<f64>> = self.features.iter().map(|(f, _)| f.score(r, c)).collect();
        let known: f64 = self
            .features
            .iter()
            .zip(&scores)
            .filter(|(_, s)| s.is_some())
            .map(|((_, w), _)| w)
            .sum();
        let parts: Vec<Part> = self
            .features
            .iter()
            .zip(scores)
            .map(|((f, w), score)| Part {
                name: f.name(),
                score,
                share: if score.is_some() && known > 0.0 { w / known } else { 0.0 },
            })
            .collect();
        let mut total: f64 = parts.iter().map(|p| p.share * p.score.unwrap_or(0.0)).sum();
        if parts.iter().any(|p| p.name == "tempo" && p.score == Some(0.0)) {
            total *= UNMIXABLE;
        }
        if r.played.contains(&c.id) {
            total *= PLAYED;
        }
        Scored {
            id: c.id,
            total,
            parts,
            tempo_change: change(r, c),
        }
    }

    /// The best `n` candidates, best first, leaving out the reference itself and
    /// anything it excludes.
    pub fn suggest(&self, r: &Reference, cands: &[Candidate], n: usize) -> Vec<Scored> {
        let mut out: Vec<Scored> = cands
            .iter()
            .filter(|c| c.id != r.id && !r.exclude.contains(&c.id))
            .map(|c| self.score(r, c))
            .collect();
        out.sort_by(|a, b| b.total.total_cmp(&a.total));
        out.truncate(n);
        out
    }
}

/// The best `n` candidates by the default [`Scorer`].
pub fn suggest(r: &Reference, cands: &[Candidate], n: usize) -> Vec<Scored> {
    Scorer::default().suggest(r, cands, n)
}

pub fn key_score(a: Camelot, b: Camelot) -> f64 {
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

    fn reference<'a>(played: &'a HashSet<i64>, exclude: &'a HashSet<i64>) -> Reference<'a> {
        Reference {
            id: 0,
            bpm: Some(125.0),
            key: parse_key("8A"),
            energy: 0.5,
            direction: Direction::Build,
            played,
            exclude,
        }
    }

    #[test]
    fn keys() {
        let k = |s| parse_key(s).unwrap();
        assert_eq!(key_score(k("8A"), k("8A")), 1.0);
        assert_eq!(key_score(k("8A"), k("9A")), 0.9);
        assert_eq!(key_score(k("8A"), k("8B")), 0.85);
        assert_eq!(key_score(k("8A"), k("10A")), 0.45);
        assert_eq!(key_score(k("8A"), k("3A")), 0.35); // a semitone up
        assert_eq!(key_score(k("8A"), k("2A")), 0.0);
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
        let (played, none) = (HashSet::from([4]), HashSet::new());
        let r = reference(&played, &none);
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

    #[test]
    fn a_missing_feature_shares_its_weight() {
        let none = HashSet::new();
        let r = reference(&none, &none);
        let mut c = cand(1, 125.0, "8A", 0.65);
        c.key = None;
        let s = Scorer::default().score(&r, &c);
        // No key: tempo and energy (both perfect) carry the whole score, rather than a
        // fixed middling key score dragging it down.
        assert_eq!(s.part("key"), None);
        assert!((s.total - 1.0).abs() < 1e-6, "{}", s.total);
        let shares: Vec<f64> = s.parts.iter().map(|p| p.share).collect();
        assert!((shares[1] - 0.3 / 0.4).abs() < 1e-9 && (shares[2] - 0.1 / 0.4).abs() < 1e-9);
        assert_eq!(shares[0], 0.0);
        // With every feature known, the shares are the weights over their sum.
        let full = Scorer::default().score(&r, &cand(2, 125.0, "8A", 0.65));
        assert!((full.parts[0].share - 0.35 / 0.75).abs() < 1e-9);
        assert!((full.total - 1.0).abs() < 1e-6);
    }

    #[test]
    fn excluded_tracks_are_left_out_not_penalised() {
        let (none, exclude) = (HashSet::new(), HashSet::from([1]));
        let r = reference(&none, &exclude);
        let cands = [cand(1, 125.0, "8A", 0.65), cand(2, 125.0, "8A", 0.65)];
        let ids: Vec<i64> = suggest(&r, &cands, 1).iter().map(|s| s.id).collect();
        // Track 1 would have tied for first; it's gone, so it can't crowd out track 2.
        assert_eq!(ids, vec![2]);
    }
}
