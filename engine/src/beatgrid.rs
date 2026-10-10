//! Beatgrids: every beat stored individually, each with its position in the bar.
//!
//! A grid is a strictly increasing list of beat times (seconds from the start of the
//! decoded audio) plus a bar position per beat (1, 2, 3, 4 …; 0 = unknown). Tempo is
//! never stored; it is whatever the gaps between beats say, so a drifting live
//! recording is represented exactly. See `docs/design.md` for the byte format.

use std::fmt;

/// Bar length assumed when bar positions have to be invented (edits, extrapolation).
pub const BEATS_PER_BAR: u8 = 4;

const MAGIC: &[u8; 4] = b"EBGR";
const VERSION: u16 = 1;
const HEADER_LEN: usize = 12;

#[derive(Debug, Clone, PartialEq)]
pub enum GridError {
    TooFewBeats,
    NotIncreasing(usize),
    LengthMismatch,
    BadMagic,
    UnsupportedVersion(u16),
    Truncated,
}

impl fmt::Display for GridError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GridError::TooFewBeats => write!(f, "a beatgrid needs at least two beats"),
            GridError::NotIncreasing(i) => write!(f, "beat {i} is not after the beat before it"),
            GridError::LengthMismatch => write!(f, "times and positions differ in length"),
            GridError::BadMagic => write!(f, "not a beatgrid blob"),
            GridError::UnsupportedVersion(v) => write!(f, "unsupported beatgrid version {v}"),
            GridError::Truncated => write!(f, "beatgrid blob is truncated"),
        }
    }
}

impl std::error::Error for GridError {}

#[derive(Debug, Clone, PartialEq)]
pub struct Beatgrid {
    times: Vec<f64>,
    positions: Vec<u8>,
}

impl Beatgrid {
    pub fn new(times: Vec<f64>, positions: Vec<u8>) -> Result<Self, GridError> {
        if times.len() != positions.len() {
            return Err(GridError::LengthMismatch);
        }
        if times.len() < 2 {
            return Err(GridError::TooFewBeats);
        }
        for i in 1..times.len() {
            if !(times[i] > times[i - 1]) {
                return Err(GridError::NotIncreasing(i));
            }
        }
        Ok(Self { times, positions })
    }

    /// A steady grid covering `0..=duration`, with the beat at `anchor` as a 1-beat.
    pub fn constant(bpm: f64, anchor: f64, duration: f64) -> Result<Self, GridError> {
        let period = 60.0 / bpm;
        let first = anchor - (anchor / period).floor() * period;
        let n = ((duration - first) / period).floor() as usize + 1;
        let anchor_index = ((anchor - first) / period).round() as i64;
        let times = (0..n).map(|i| first + i as f64 * period).collect();
        let positions = (0..n as i64)
            .map(|i| bar_position(i - anchor_index))
            .collect();
        Self::new(times, positions)
    }

    /// Build from beat-tracker output that may have gaps: `index` is the beat count for
    /// each detected beat (it jumps by 2 or more where beats were missed), and
    /// `is_downbeat` marks bar starts. This is tempo-lock's `analyse` output.
    pub fn from_indexed(
        times: &[f64],
        index: &[i64],
        is_downbeat: &[bool],
    ) -> Result<Self, GridError> {
        if times.len() != index.len() || times.len() != is_downbeat.len() {
            return Err(GridError::LengthMismatch);
        }
        let mut out_t = Vec::with_capacity(times.len());
        let mut down = Vec::with_capacity(times.len());
        for i in 0..times.len() {
            if i > 0 {
                let gap = index[i] - index[i - 1];
                for k in 1..gap {
                    let f = k as f64 / gap as f64;
                    out_t.push(times[i - 1] + f * (times[i] - times[i - 1]));
                    down.push(false);
                }
            }
            out_t.push(times[i]);
            down.push(is_downbeat[i]);
        }
        let positions = positions_from_downbeats(&down);
        Self::new(out_t, positions)
    }

    pub fn len(&self) -> usize {
        self.times.len()
    }

    pub fn is_empty(&self) -> bool {
        self.times.is_empty()
    }

    pub fn times(&self) -> &[f64] {
        &self.times
    }

    pub fn positions(&self) -> &[u8] {
        &self.positions
    }

    // ---- storage ---------------------------------------------------------------

    pub fn to_bytes(&self) -> Vec<u8> {
        let n = self.times.len();
        let mut out = Vec::with_capacity(HEADER_LEN + 9 * n);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(n as u32).to_le_bytes());
        for t in &self.times {
            out.extend_from_slice(&t.to_le_bytes());
        }
        out.extend_from_slice(&self.positions);
        out
    }

    pub fn from_bytes(b: &[u8]) -> Result<Self, GridError> {
        if b.len() < HEADER_LEN {
            return Err(GridError::Truncated);
        }
        if &b[0..4] != MAGIC {
            return Err(GridError::BadMagic);
        }
        let version = u16::from_le_bytes([b[4], b[5]]);
        if version != VERSION {
            return Err(GridError::UnsupportedVersion(version));
        }
        let n = u32::from_le_bytes([b[8], b[9], b[10], b[11]]) as usize;
        if b.len() < HEADER_LEN + 9 * n {
            return Err(GridError::Truncated);
        }
        let times = b[HEADER_LEN..HEADER_LEN + 8 * n]
            .chunks_exact(8)
            .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
            .collect();
        let positions = b[HEADER_LEN + 8 * n..HEADER_LEN + 9 * n].to_vec();
        Self::new(times, positions)
    }

    // ---- lookups (allocation-free, safe on the audio thread) -------------------

    /// Fractional beat index at time `t`. Extrapolates at the edge tempo outside the
    /// stored beats, so it is defined for any `t`.
    pub fn beat_at(&self, t: f64) -> f64 {
        let n = self.times.len();
        let idx = self.times.partition_point(|&x| x <= t);
        let i = idx.clamp(1, n - 1) - 1;
        let (t0, t1) = (self.times[i], self.times[i + 1]);
        i as f64 + (t - t0) / (t1 - t0)
    }

    /// Inverse of [`beat_at`](Self::beat_at).
    pub fn time_at(&self, beat: f64) -> f64 {
        let n = self.times.len();
        let i = (beat.floor().max(0.0) as usize).min(n - 2);
        let (t0, t1) = (self.times[i], self.times[i + 1]);
        t0 + (beat - i as f64) * (t1 - t0)
    }

    /// Time of the nearest beat (stored or extrapolated) to `t`.
    pub fn quantize(&self, t: f64) -> f64 {
        self.time_at(self.beat_at(t).round())
    }

    /// Local tempo at `t`, averaged over `half_window` beats either side.
    pub fn bpm_at(&self, t: f64, half_window: usize) -> f64 {
        let n = self.times.len();
        let centre = self.beat_at(t).round().clamp(0.0, (n - 1) as f64) as usize;
        let lo = centre.saturating_sub(half_window);
        let hi = (centre + half_window).min(n - 1);
        let (lo, hi) = if hi == lo {
            (lo.saturating_sub(1), (hi + 1).min(n - 1))
        } else {
            (lo, hi)
        };
        60.0 * (hi - lo) as f64 / (self.times[hi] - self.times[lo])
    }

    pub fn mean_bpm(&self) -> f64 {
        let n = self.times.len();
        60.0 * (n - 1) as f64 / (self.times[n - 1] - self.times[0])
    }

    /// Bar position of beat `i`, which may be outside the stored range (extrapolated in
    /// 4/4 from the nearest stored beat). 0 if unknown.
    pub fn position_of(&self, i: i64) -> u8 {
        let n = self.times.len() as i64;
        let edge = i.clamp(0, n - 1);
        let p = self.positions[edge as usize];
        if edge == i || p == 0 {
            return p;
        }
        let p = p.min(BEATS_PER_BAR) as i64;
        (((p - 1) + (i - edge)).rem_euclid(BEATS_PER_BAR as i64) + 1) as u8
    }

    /// Index of the first stored 1-beat, if any.
    pub fn first_downbeat(&self) -> Option<usize> {
        self.positions.iter().position(|&p| p == 1)
    }

    // ---- whole-grid edits ------------------------------------------------------
    // Each returns a new grid re-fitted to cover `0..=duration`.

    /// Scale every interval about beat `anchor` so the mean tempo becomes `bpm`.
    /// Exact for a steady grid, and keeps the shape of a drifting one.
    pub fn with_bpm(&self, bpm: f64, anchor: usize, duration: f64) -> Result<Self, GridError> {
        let ratio = self.mean_bpm() / bpm;
        let ta = self.times[anchor.min(self.len() - 1)];
        let times = self.times.iter().map(|&t| ta + (t - ta) * ratio).collect();
        Self::new(times, self.positions.clone())?.refit(duration)
    }

    /// Shift every beat by `dt` seconds.
    pub fn nudged(&self, dt: f64, duration: f64) -> Result<Self, GridError> {
        let times = self.times.iter().map(|&t| t + dt).collect();
        Self::new(times, self.positions.clone())?.refit(duration)
    }

    /// Make beat `i` a 1-beat and renumber everything in 4/4 from it.
    pub fn with_downbeat(&self, i: usize) -> Self {
        let positions = (0..self.len() as i64)
            .map(|j| bar_position(j - i as i64))
            .collect();
        Self {
            times: self.times.clone(),
            positions,
        }
    }

    /// Half the tempo: keep every other beat, choosing the parity that keeps the 1s.
    pub fn halved(&self, duration: f64) -> Result<Self, GridError> {
        let d = self.first_downbeat().unwrap_or(0);
        let times: Vec<f64> = self
            .times
            .iter()
            .enumerate()
            .filter(|(j, _)| (*j as i64 - d as i64) % 2 == 0)
            .map(|(_, &t)| t)
            .collect();
        let new_d = (d / 2) as i64;
        let positions = (0..times.len() as i64)
            .map(|j| bar_position(j - new_d))
            .collect();
        Self::new(times, positions)?.refit(duration)
    }

    /// Double the tempo: insert a beat halfway between each pair.
    pub fn doubled(&self, duration: f64) -> Result<Self, GridError> {
        let mut times = Vec::with_capacity(self.len() * 2);
        for w in self.times.windows(2) {
            times.push(w[0]);
            times.push((w[0] + w[1]) / 2.0);
        }
        times.push(*self.times.last().unwrap());
        let new_d = (self.first_downbeat().unwrap_or(0) * 2) as i64;
        let positions = (0..times.len() as i64)
            .map(|j| bar_position(j - new_d))
            .collect();
        Self::new(times, positions)?.refit(duration)
    }

    /// Drop beats outside `0..=duration` and extend at the edge tempo to cover it.
    pub fn refit(&self, duration: f64) -> Result<Self, GridError> {
        let keep: Vec<usize> = (0..self.len())
            .filter(|&i| self.times[i] >= 0.0 && self.times[i] <= duration)
            .collect();
        let (lo, hi) = match (keep.first(), keep.last()) {
            (Some(&lo), Some(&hi)) if hi > lo => (lo, hi),
            _ => return Err(GridError::TooFewBeats),
        };
        let mut start = lo as i64;
        while self.time_at((start - 1) as f64) >= 0.0 {
            start -= 1;
        }
        let mut end = hi as i64;
        while self.time_at((end + 1) as f64) <= duration {
            end += 1;
        }
        let times = (start..=end).map(|i| self.time_at(i as f64)).collect();
        let positions = (start..=end).map(|i| self.position_of(i)).collect();
        Self::new(times, positions)
    }
}

/// Position in a 4/4 bar of the beat `offset` beats after a 1-beat (negative = before).
fn bar_position(offset: i64) -> u8 {
    (offset.rem_euclid(BEATS_PER_BAR as i64) + 1) as u8
}

/// Count up from each downbeat; before the first one, count backwards in 4/4.
fn positions_from_downbeats(is_down: &[bool]) -> Vec<u8> {
    let Some(first) = is_down.iter().position(|&d| d) else {
        return vec![0; is_down.len()];
    };
    let mut out = Vec::with_capacity(is_down.len());
    let mut count: u8 = 0;
    for (i, &d) in is_down.iter().enumerate() {
        if i < first {
            out.push(bar_position(i as i64 - first as i64));
            continue;
        }
        count = if d { 1 } else { count.saturating_add(1) };
        out.push(count);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn constant_grid_covers_track_and_marks_anchor() {
        let g = Beatgrid::constant(120.0, 1.25, 10.0).unwrap();
        assert!(close(g.times()[0], 0.25));
        assert!(*g.times().last().unwrap() <= 10.0);
        let anchor = g.times().iter().position(|&t| close(t, 1.25)).unwrap();
        assert_eq!(g.positions()[anchor], 1);
        assert_eq!(g.positions()[anchor + 1], 2);
        assert_eq!(g.positions()[anchor - 1], 4);
        assert!(close(g.mean_bpm(), 120.0));
    }

    #[test]
    fn bytes_round_trip() {
        let g = Beatgrid::constant(127.3, 0.1, 30.0).unwrap();
        let b = g.to_bytes();
        assert_eq!(b.len(), 12 + 9 * g.len());
        assert_eq!(&b[0..4], b"EBGR");
        assert_eq!(Beatgrid::from_bytes(&b).unwrap(), g);
        assert_eq!(Beatgrid::from_bytes(&b[..20]), Err(GridError::Truncated));
    }

    #[test]
    fn beat_at_and_time_at_are_inverse_including_extrapolation() {
        let g = Beatgrid::new(vec![1.0, 1.5, 2.1, 2.6], vec![1, 2, 3, 4]).unwrap();
        for &t in &[0.0, 1.0, 1.2, 2.1, 2.35, 3.9] {
            assert!(close(g.time_at(g.beat_at(t)), t), "t={t}");
        }
        assert!(close(g.beat_at(0.5), -1.0));
        assert!(close(g.beat_at(3.1), 4.0));
        assert!(close(g.quantize(1.7), 1.5));
        assert!(close(g.quantize(1.9), 2.1));
    }

    #[test]
    fn local_tempo_follows_drift() {
        // 120 BPM speeding up to ~130.
        let mut t = 0.0;
        let mut times = vec![];
        for i in 0..200 {
            times.push(t);
            t += 60.0 / (120.0 + 10.0 * i as f64 / 199.0);
        }
        let g = Beatgrid::new(times, vec![0; 200]).unwrap();
        assert!((g.bpm_at(1.0, 4) - 120.2).abs() < 0.5);
        assert!((g.bpm_at(g.times()[195], 4) - 129.8).abs() < 0.5);
    }

    #[test]
    fn from_indexed_fills_missed_beats_and_counts_bars() {
        // Beat 3 was missed (index jumps 2 -> 4); a 3-beat bar starts at index 5.
        let times = [0.0, 0.5, 1.0, 2.0, 2.5, 3.0, 3.5, 4.0];
        let index = [0, 1, 2, 4, 5, 6, 7, 8];
        let down = [false, true, false, false, true, false, false, true];
        let g = Beatgrid::from_indexed(&times, &index, &down).unwrap();
        assert_eq!(g.len(), 9);
        assert!(close(g.times()[3], 1.5));
        assert_eq!(g.positions(), &[4, 1, 2, 3, 4, 1, 2, 3, 1]);
    }

    #[test]
    fn set_bpm_scales_about_anchor() {
        let g = Beatgrid::constant(120.0, 0.0, 60.0).unwrap();
        let h = g.with_bpm(125.0, 8, 60.0).unwrap();
        assert!(close(h.mean_bpm(), 125.0));
        assert!(
            h.times().iter().any(|&t| close(t, 4.0)),
            "anchor beat stays put"
        );
        assert!(*h.times().last().unwrap() > 59.0);
    }

    #[test]
    fn nudge_refits_to_track() {
        let g = Beatgrid::constant(120.0, 0.0, 10.0).unwrap();
        let h = g.nudged(0.1, 10.0).unwrap();
        assert!(close(h.times()[0], 0.1));
        let h = g.nudged(-0.1, 10.0).unwrap();
        assert!(close(h.times()[0], 0.4));
        assert_eq!(h.positions()[0], 2, "bar positions survive the shift");
    }

    #[test]
    fn halve_keeps_downbeats_and_double_inverts_it() {
        let g = Beatgrid::constant(128.0, 60.0 / 128.0, 30.0).unwrap(); // 1-beat at index 1
        let h = g.halved(30.0).unwrap();
        assert!(close(h.mean_bpm(), 64.0));
        assert!(h.times().iter().any(|&t| close(t, 60.0 / 128.0)));
        let d = h.doubled(30.0).unwrap();
        assert!(close(d.mean_bpm(), 128.0));
        assert_eq!(
            d.first_downbeat().map(|i| d.times()[i]),
            h.first_downbeat().map(|i| h.times()[i])
        );
    }

    #[test]
    fn set_downbeat_renumbers() {
        let g = Beatgrid::constant(120.0, 0.0, 5.0)
            .unwrap()
            .with_downbeat(2);
        assert_eq!(&g.positions()[..6], &[3, 4, 1, 2, 3, 4]);
        assert_eq!(g.position_of(-1), 2);
    }
}
