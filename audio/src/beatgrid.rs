//! Beatgrids: what a grid is meant to be ([`GridModel`]), and the beat list worked
//! out from it ([`Beatgrid`]).
//!
//! - A steady track's grid is [`GridModel::Constant`]: "a 1 here, then a beat every
//!   60 / bpm seconds", both ways. That is what's saved and what edits change; the
//!   beats are worked out again from it, so edits never drift.
//! - A drifting track's grid is [`GridModel::Markers`]: every detected beat, each with
//!   its position in the bar. Tempo is whatever the gaps between beats say.
//!
//! [`Beatgrid`] keeps the beat times (seconds from the start of the decoded audio)
//! plus a bar position per beat (1, 2, 3, 4 …; 0 = unknown), so its lookups don't
//! allocate and are safe on the audio thread. The lookups carry the grid on past both
//! ends, before 0 s too, at the edge tempo. See `docs/design.md` for the byte format
//! of a markers grid.
//!
//! Times are relative to our own decoder's first sample. Exporting grids to Rekordbox
//! or Mixxx will need a `Timebase` saved beside the model (which decoder made these
//! seconds, and its offset from the file's own first sample). It isn't built yet.

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
    /// A steady grid with an unusable tempo, anchor or bar position.
    BadConstant(&'static str),
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
            GridError::BadConstant(what) => write!(f, "steady grid has an unusable {what}"),
        }
    }
}

impl std::error::Error for GridError {}

/// What a grid is meant to be. This, not the beat list, is what's saved.
#[derive(Debug, Clone, PartialEq)]
pub enum GridModel {
    /// One tempo: see [`Constant`].
    Constant(Constant),
    /// Every beat, with its bar position (0 = unknown), as [`Beatgrid::new`] takes them.
    Markers { times: Vec<f64>, positions: Vec<u8> },
}

/// A steady grid: a beat at `anchor`, then one every `60 / bpm` seconds either way,
/// for ever. Beat n is at `anchor + n · period`, for every n, negative too.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Constant {
    pub bpm: f64,
    /// A beat's time (s). Usually a 1; it may be before 0 or past the end.
    pub anchor: f64,
    /// Where the anchor beat is in its bar: 1 (the downbeat) to `meter`.
    pub anchor_pos: u8,
    /// Beats in a bar.
    pub meter: u8,
}

impl Constant {
    /// `bpm` with a 1 at `anchor`, in 4/4.
    pub fn new(bpm: f64, anchor: f64) -> Self {
        Self {
            bpm,
            anchor,
            anchor_pos: 1,
            meter: BEATS_PER_BAR,
        }
    }

    /// Seconds per beat.
    pub fn period(&self) -> f64 {
        60.0 / self.bpm
    }

    fn check(&self) -> Result<(), GridError> {
        if !(self.bpm.is_finite() && self.bpm > 0.0) {
            return Err(GridError::BadConstant("tempo"));
        }
        if !self.anchor.is_finite() {
            return Err(GridError::BadConstant("anchor"));
        }
        if self.meter == 0 || self.anchor_pos == 0 || self.anchor_pos > self.meter {
            return Err(GridError::BadConstant("bar position"));
        }
        Ok(())
    }

    /// Bar position of the beat `offset` beats after the anchor (negative: before).
    fn position(&self, offset: i64) -> u8 {
        ((self.anchor_pos as i64 - 1 + offset).rem_euclid(self.meter as i64) + 1) as u8
    }
}

/// A grid as the decks use it: the beat times worked out from a [`GridModel`], kept
/// so lookups don't allocate.
#[derive(Debug, Clone, PartialEq)]
pub struct Beatgrid {
    times: Vec<f64>,
    positions: Vec<u8>,
    /// The model, for a steady grid. `None`: markers, which are `times` and
    /// `positions` themselves.
    constant: Option<Constant>,
}

impl Beatgrid {
    /// A markers grid: these beats, with these bar positions.
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
        Ok(Self {
            times,
            positions,
            constant: None,
        })
    }

    /// The grid a model describes. A steady one keeps its beats in `0..=duration`
    /// (the lookups carry on past both ends); markers are taken as they are.
    pub fn from_model(model: GridModel, duration: f64) -> Result<Self, GridError> {
        let c = match model {
            GridModel::Markers { times, positions } => return Self::new(times, positions),
            GridModel::Constant(c) => c,
        };
        c.check()?;
        let period = c.period();
        // Beats `lo..=hi` after the anchor: the first at or after 0 s to the last
        // before the end.
        let lo = (-c.anchor / period).ceil() as i64;
        let hi = ((duration - c.anchor) / period).floor() as i64;
        if hi <= lo {
            return Err(GridError::TooFewBeats);
        }
        Ok(Self {
            times: (lo..=hi).map(|k| c.anchor + k as f64 * period).collect(),
            positions: (lo..=hi).map(|k| c.position(k)).collect(),
            constant: Some(c),
        })
    }

    /// A steady grid covering `0..=duration`, with the beat at `anchor` as a 1-beat.
    pub fn constant(bpm: f64, anchor: f64, duration: f64) -> Result<Self, GridError> {
        Self::from_model(GridModel::Constant(Constant::new(bpm, anchor)), duration)
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

    /// What this grid is meant to be (a markers grid's beats are copied).
    pub fn model(&self) -> GridModel {
        match self.constant {
            Some(c) => GridModel::Constant(c),
            None => GridModel::Markers {
                times: self.times.clone(),
                positions: self.positions.clone(),
            },
        }
    }

    /// The model, if this is a steady grid.
    pub fn as_constant(&self) -> Option<&Constant> {
        self.constant.as_ref()
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

    /// Beats in a bar, where bar positions have to be invented.
    fn meter(&self) -> u8 {
        self.constant.map_or(BEATS_PER_BAR, |c| c.meter)
    }

    // ---- storage (markers) -------------------------------------------------------

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

    /// Fractional beat index at time `t` (beat 0 is the first kept beat). Extrapolates
    /// at the edge tempo outside the kept beats, so it is defined for any `t`, before
    /// 0 s too.
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

    /// Time of the nearest beat (kept or extrapolated) to `t`.
    pub fn quantize(&self, t: f64) -> f64 {
        self.time_at(self.beat_at(t).round())
    }

    /// Local tempo at `t`, averaged over `half_window` beats either side. A steady
    /// grid's own tempo.
    pub fn bpm_at(&self, t: f64, half_window: usize) -> f64 {
        if let Some(c) = &self.constant {
            return c.bpm;
        }
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

    /// The average tempo; exactly a steady grid's own.
    pub fn mean_bpm(&self) -> f64 {
        if let Some(c) = &self.constant {
            return c.bpm;
        }
        let n = self.times.len();
        60.0 * (n - 1) as f64 / (self.times[n - 1] - self.times[0])
    }

    /// Bar position of beat `i`, which may be outside the kept range (negative too:
    /// counted on in whole bars from the nearest kept beat). 0 if unknown.
    pub fn position_of(&self, i: i64) -> u8 {
        let n = self.times.len() as i64;
        let edge = i.clamp(0, n - 1);
        let p = self.positions[edge as usize];
        if edge == i || p == 0 {
            return p;
        }
        let m = self.meter() as i64;
        let p = (p as i64).min(m);
        ((p - 1 + (i - edge)).rem_euclid(m) + 1) as u8
    }

    /// Index of the first kept 1-beat, if any.
    pub fn first_downbeat(&self) -> Option<usize> {
        self.positions.iter().position(|&p| p == 1)
    }

    /// The bar and the beat in it at time `t` (the last beat at or before it), counting
    /// bar 1 from the first kept 1-beat. Bars before it, in the lead-in before 0 s too,
    /// count down: 0, -1, …. `None` when the grid has no bar positions. Not for the
    /// audio thread: it counts the kept downbeats.
    pub fn bar_beat(&self, t: f64) -> Option<(i64, u8)> {
        let d = self.first_downbeat()? as i64;
        let i = (self.beat_at(t) + 1e-6).floor() as i64;
        let pos = self.position_of(i);
        let m = self.meter() as i64;
        let n = self.times.len() as i64;
        // Beat i's bar starts at this beat.
        let start = i - (pos.max(1) as i64 - 1);
        let bar = if start < d {
            1 - (d - start + m - 1) / m
        } else {
            let last = i.min(n - 1);
            let kept = self.positions[d as usize..=last as usize]
                .iter()
                .filter(|&&p| p == 1)
                .count() as i64;
            // Bars started past the last kept beat.
            let after = if i > last {
                let p_last = (self.positions[last as usize].max(1) as i64).min(m);
                (p_last - 1 + i - last) / m
            } else {
                0
            };
            kept + after
        };
        Some((bar, pos))
    }

    // ---- whole-grid edits ------------------------------------------------------
    // Each returns a new grid re-fitted to cover `0..=duration`. A steady grid's edits
    // change its model and work the beats out again; markers are edited beat by beat.

    /// Set the tempo. A steady grid keeps its anchor. Markers have every interval
    /// scaled about beat `pivot` so the mean tempo becomes `bpm`, which keeps the shape
    /// of a drifting grid.
    pub fn with_bpm(&self, bpm: f64, pivot: usize, duration: f64) -> Result<Self, GridError> {
        if let Some(c) = self.constant {
            return Self::from_model(GridModel::Constant(Constant { bpm, ..c }), duration);
        }
        let ratio = self.mean_bpm() / bpm;
        let ta = self.times[pivot.min(self.len() - 1)];
        let times = self.times.iter().map(|&t| ta + (t - ta) * ratio).collect();
        Self::new(times, self.positions.clone())?.refit(duration)
    }

    /// Shift every beat by `dt` seconds.
    pub fn nudged(&self, dt: f64, duration: f64) -> Result<Self, GridError> {
        if let Some(c) = self.constant {
            let anchor = c.anchor + dt;
            return Self::from_model(GridModel::Constant(Constant { anchor, ..c }), duration);
        }
        let times = self.times.iter().map(|&t| t + dt).collect();
        Self::new(times, self.positions.clone())?.refit(duration)
    }

    /// Put a 1 exactly at `t`. A steady grid is anchored there; markers are shifted so
    /// their beat nearest `t` lands on it, and renumbered in 4/4 from it.
    pub fn anchored_at(&self, t: f64, duration: f64) -> Result<Self, GridError> {
        if let Some(c) = self.constant {
            let c = Constant {
                anchor: t,
                anchor_pos: 1,
                ..c
            };
            return Self::from_model(GridModel::Constant(c), duration);
        }
        let nearest = self.nearest_kept(t);
        let g = self.nudged(t - self.times[nearest], duration)?;
        // Re-fitting may add beats at the start, so find the beat again.
        Ok(g.with_downbeat(g.nearest_kept(t)))
    }

    /// Index of the kept beat nearest `t`.
    fn nearest_kept(&self, t: f64) -> usize {
        self.beat_at(t).round().clamp(0.0, (self.len() - 1) as f64) as usize
    }

    /// Make beat `i` a 1-beat and renumber everything from it (in the steady grid's
    /// meter, else 4/4). A steady grid keeps its anchor and changes where that is in
    /// the bar.
    pub fn with_downbeat(&self, i: usize) -> Self {
        if let Some(c) = self.constant {
            // Beat i is `k` beats after the anchor.
            let k = ((self.times[i] - c.anchor) / c.period()).round() as i64;
            let c = Constant {
                anchor_pos: ((-k).rem_euclid(c.meter as i64) + 1) as u8,
                ..c
            };
            let positions = (0..self.len() as i64)
                .map(|j| c.position(k + j - i as i64))
                .collect();
            return Self {
                times: self.times.clone(),
                positions,
                constant: Some(c),
            };
        }
        let positions = (0..self.len() as i64)
            .map(|j| bar_position(j - i as i64))
            .collect();
        Self {
            times: self.times.clone(),
            positions,
            constant: None,
        }
    }

    /// Half the tempo: keep every other beat, choosing the parity that keeps the 1s.
    pub fn halved(&self, duration: f64) -> Result<Self, GridError> {
        if let Some(c) = self.constant {
            // Beats an even number after the anchor's 1 stay. If the anchor isn't one
            // of them, the beat before it is.
            let p = c.anchor_pos as i64 - 1;
            let (anchor, p) = if p % 2 == 0 {
                (c.anchor, p / 2)
            } else {
                (c.anchor - c.period(), (p - 1) / 2)
            };
            let c = Constant {
                bpm: c.bpm / 2.0,
                anchor,
                anchor_pos: (p.rem_euclid(c.meter as i64) + 1) as u8,
                meter: c.meter,
            };
            return Self::from_model(GridModel::Constant(c), duration);
        }
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
        if let Some(c) = self.constant {
            let p = 2 * (c.anchor_pos as i64 - 1);
            let c = Constant {
                bpm: c.bpm * 2.0,
                anchor_pos: (p.rem_euclid(c.meter as i64) + 1) as u8,
                ..c
            };
            return Self::from_model(GridModel::Constant(c), duration);
        }
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

    /// Keep the beats in `0..=duration`, extending at the edge tempo to cover it.
    pub fn refit(&self, duration: f64) -> Result<Self, GridError> {
        if let Some(c) = self.constant {
            return Self::from_model(GridModel::Constant(c), duration);
        }
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
    fn model_and_beats_round_trip() {
        // A steady grid: the beats come from the model, and give it back unchanged.
        let c = Constant {
            bpm: 127.3,
            anchor: 33.7,
            anchor_pos: 3,
            meter: 4,
        };
        let g = Beatgrid::from_model(GridModel::Constant(c), 120.0).unwrap();
        assert_eq!(g.model(), GridModel::Constant(c));
        assert_eq!(g.mean_bpm(), 127.3, "exactly the model's tempo");
        let at = g.times().iter().position(|&t| close(t, 33.7)).unwrap();
        assert_eq!(g.positions()[at], 3);
        assert_eq!(g.positions()[at + 2], 1);
        assert!(g.times()[0] >= 0.0 && g.times()[0] < c.period());
        assert_eq!(Beatgrid::from_model(g.model(), 120.0).unwrap(), g);
        // Markers: the beats are the model.
        let m = Beatgrid::new(vec![0.1, 0.6, 1.2, 1.7], vec![3, 4, 1, 2]).unwrap();
        assert!(m.as_constant().is_none());
        assert_eq!(Beatgrid::from_model(m.model(), 2.0).unwrap(), m);
        // A model that can't make a grid.
        let bad = Constant { anchor_pos: 5, ..c };
        assert_eq!(
            Beatgrid::from_model(GridModel::Constant(bad), 60.0),
            Err(GridError::BadConstant("bar position"))
        );
    }

    #[test]
    fn bytes_round_trip() {
        let g = Beatgrid::new(vec![0.1, 0.6, 1.2, 1.7], vec![3, 4, 1, 2]).unwrap();
        let b = g.to_bytes();
        assert_eq!(b.len(), 12 + 9 * g.len());
        assert_eq!(&b[0..4], b"EBGR");
        assert_eq!(Beatgrid::from_bytes(&b).unwrap(), g);
        assert_eq!(Beatgrid::from_bytes(&b[..20]), Err(GridError::Truncated));
    }

    #[test]
    fn beat_at_and_time_at_are_inverse_including_extrapolation() {
        let g = Beatgrid::new(vec![1.0, 1.5, 2.1, 2.6], vec![1, 2, 3, 4]).unwrap();
        for &t in &[-3.0, 0.0, 1.0, 1.2, 2.1, 2.35, 3.9] {
            assert!(close(g.time_at(g.beat_at(t)), t), "t={t}");
        }
        assert!(close(g.beat_at(0.5), -1.0));
        assert!(close(g.beat_at(3.1), 4.0));
        assert!(close(g.quantize(1.7), 1.5));
        assert!(close(g.quantize(1.9), 2.1));
    }

    #[test]
    fn markers_extrapolate_at_their_edge_tempos() {
        // 0.5 s apart at the start, 0.6 s at the end.
        let g = Beatgrid::new(vec![2.0, 2.5, 3.0, 3.6, 4.2], vec![1, 2, 3, 4, 1]).unwrap();
        assert!(close(g.time_at(-4.0), 0.0));
        assert!(close(g.time_at(-6.0), -1.0), "before 0 s");
        assert!(close(g.beat_at(-1.0), -6.0));
        assert!(close(g.quantize(-0.76), -1.0));
        assert!(close(g.time_at(6.0), 4.2 + 2.0 * 0.6));
        assert!(close(g.quantize(5.3), 5.4));
        // Bar positions carry on in 4/4 from each end.
        assert_eq!(
            (-6..0).map(|i| g.position_of(i)).collect::<Vec<_>>(),
            [3, 4, 1, 2, 3, 4]
        );
        assert_eq!(
            (5..10).map(|i| g.position_of(i)).collect::<Vec<_>>(),
            [2, 3, 4, 1, 2]
        );
    }

    #[test]
    fn steady_grids_carry_on_before_zero_and_past_the_end() {
        // A 1 marked mid-song at 61.0 s, 120 BPM: beat n is at 61 + n / 2 for every n.
        let g =
            Beatgrid::from_model(GridModel::Constant(Constant::new(120.0, 61.0)), 90.0).unwrap();
        assert!(close(g.times()[0], 0.0));
        for t in [-10.0, -2.0, -0.5, 0.0, 61.0, 89.5, 95.0] {
            assert!(close(g.quantize(t), t), "a beat at {t}");
            assert!(close(g.time_at(g.beat_at(t + 0.1)), t + 0.1));
        }
        assert!(close(g.quantize(-0.26), -0.5));
        // 0 s is 122 beats before the anchor, a 3 (122 = 30 bars and 2 beats).
        assert_eq!(g.positions()[0], 3);
        // Before 0 s, Euclidean: -0.5 s is a 2, -1.0 s a 1, -3.0 s a 1 again.
        let pos = |t: f64| g.position_of(g.beat_at(t).round() as i64);
        assert_eq!([pos(-0.5), pos(-1.0), pos(-1.5), pos(-3.0)], [2, 1, 4, 1]);
        assert_eq!([pos(91.0), pos(93.0)], [1, 1]);
    }

    #[test]
    fn bar_numbers_count_down_into_the_lead_in() {
        let g = Beatgrid::constant(120.0, 1.0, 30.0).unwrap(); // 1s at 1, 3, 5 … s
        assert_eq!(g.bar_beat(1.0), Some((1, 1)));
        assert_eq!(g.bar_beat(1.6), Some((1, 2)));
        assert_eq!(g.bar_beat(3.0), Some((2, 1)));
        assert_eq!(g.bar_beat(0.5), Some((0, 4)));
        assert_eq!(g.bar_beat(-0.5), Some((0, 2)));
        assert_eq!(g.bar_beat(-1.0), Some((0, 1)));
        assert_eq!(g.bar_beat(-1.2), Some((-1, 4)));
        // Past the end (the last kept beat is at 30 s, the 3 of bar 15).
        assert_eq!(g.bar_beat(29.0), Some((15, 1)));
        assert_eq!(g.bar_beat(30.0), Some((15, 3)));
        assert_eq!(g.bar_beat(31.0), Some((16, 1)));
        assert_eq!(g.bar_beat(32.4), Some((16, 3)));
        assert_eq!(g.bar_beat(33.0), Some((17, 1)));
        let unknown = Beatgrid::new(vec![0.0, 1.0], vec![0, 0]).unwrap();
        assert_eq!(unknown.bar_beat(0.5), None);
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

    /// A drifting grid: 120 BPM speeding up by 1 ms a beat.
    fn markers() -> Beatgrid {
        let mut times = vec![0.0];
        for i in 0..100 {
            times.push(times[i] + 0.5 - 0.001 * i as f64);
        }
        Beatgrid::new(times, (0..101).map(|i| bar_position(i)).collect()).unwrap()
    }

    #[test]
    fn markers_set_bpm_scales_about_the_pivot() {
        let g = markers();
        let h = g.with_bpm(125.0, 8, 60.0).unwrap();
        let i = h.times().iter().position(|&t| close(t, g.times()[8]));
        let i = i.expect("pivot beat stays put");
        let gap = |g: &Beatgrid, i: usize| g.times()[i + 1] - g.times()[i];
        assert!(close(gap(&h, i), gap(&g, 8) * g.mean_bpm() / 125.0));
        assert!(*h.times().last().unwrap() > 59.0);
    }

    #[test]
    fn markers_nudge_refits_to_track() {
        let g = markers();
        let h = g.nudged(0.1, 60.0).unwrap();
        assert!(close(h.times()[0], 0.1));
        let h = g.nudged(-0.1, 60.0).unwrap();
        assert!(close(h.times()[0], 0.4));
        assert_eq!(h.positions()[0], 2, "bar positions survive the shift");
        let a = g.anchored_at(10.02, 60.0).unwrap();
        let i = a.times().iter().position(|&t| close(t, 10.02)).unwrap();
        assert_eq!(a.positions()[i], 1);
    }

    #[test]
    fn markers_halve_keeps_downbeats_and_double_inverts_it() {
        let g = Beatgrid::constant(128.0, 60.0 / 128.0, 30.0).unwrap(); // 1-beat at index 1
        let g = Beatgrid::new(g.times().to_vec(), g.positions().to_vec()).unwrap();
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
    fn markers_set_downbeat_renumbers() {
        let g = markers().with_downbeat(2);
        assert_eq!(&g.positions()[..6], &[3, 4, 1, 2, 3, 4]);
        assert_eq!(g.position_of(-1), 2);
    }

    fn constant_of(g: &Beatgrid) -> Constant {
        *g.as_constant().expect("still a steady grid")
    }

    #[test]
    fn steady_edits_change_the_model() {
        let g = Beatgrid::constant(120.0, 12.25, 60.0).unwrap();
        // BPM: just the number; the anchor stays where it is.
        let h = g.with_bpm(125.5, 40, 60.0).unwrap();
        assert_eq!(constant_of(&h), Constant::new(125.5, 12.25));
        assert!(h.times().iter().any(|&t| t == 12.25));
        // Nudge: moves the anchor.
        let n = g.nudged(-0.01, 60.0).unwrap();
        assert_eq!(constant_of(&n).anchor, 12.25 - 0.01);
        // Anchor here: a 1 exactly at the playhead, the tempo kept.
        let a = h.anchored_at(30.123, 60.0).unwrap();
        assert_eq!(constant_of(&a), Constant::new(125.5, 30.123));
        let i = a.times().iter().position(|&t| t == 30.123).unwrap();
        assert_eq!(a.positions()[i], 1);
        // Setting a downbeat keeps the anchor and says where it is in the bar.
        let i = g.times().iter().position(|&t| close(t, 13.25)).unwrap();
        let d = g.with_downbeat(i);
        assert_eq!(constant_of(&d).anchor, 12.25);
        assert_eq!(constant_of(&d).anchor_pos, 3);
        assert_eq!(d.positions()[i], 1);
        assert_eq!(d, Beatgrid::from_model(d.model(), 60.0).unwrap());
    }

    #[test]
    fn steady_halve_and_double_keep_the_ones() {
        let g = Beatgrid::constant(128.0, 10.0, 60.0).unwrap();
        let h = g.halved(60.0).unwrap();
        assert_eq!(constant_of(&h), Constant::new(64.0, 10.0));
        assert_eq!(h.doubled(60.0).unwrap(), g);
        // The anchor a 2: the beat before it (the 1) is kept as the new anchor.
        let c = Constant {
            anchor_pos: 2,
            ..Constant::new(120.0, 10.5)
        };
        let g = Beatgrid::from_model(GridModel::Constant(c), 60.0).unwrap();
        let h = constant_of(&g.halved(60.0).unwrap());
        assert_eq!((h.bpm, h.anchor, h.anchor_pos), (60.0, 10.0, 1));
        // A 3 doubled is a 1 (two bars on): 2 · 2 beats after the 1.
        let c = Constant { anchor_pos: 3, ..c };
        let g = Beatgrid::from_model(GridModel::Constant(c), 60.0).unwrap();
        assert_eq!(constant_of(&g.doubled(60.0).unwrap()).anchor_pos, 1);
    }

    #[test]
    fn steady_edits_dont_drift() {
        let start = Beatgrid::constant(124.0, 7.5, 300.0).unwrap();
        let mut g = start.clone();
        // Hundreds of edits that cancel out: BPM up and down, nudges both ways.
        for i in 0..500 {
            g = g.with_bpm(124.0 + (i % 7) as f64 * 0.01, 0, 300.0).unwrap();
            g = g.nudged(0.001, 300.0).unwrap();
        }
        for _ in 0..500 {
            g = g.nudged(-0.001, 300.0).unwrap();
        }
        g = g.with_bpm(124.0, 0, 300.0).unwrap();
        let c = constant_of(&g);
        assert_eq!(c.bpm, 124.0, "the tempo is exactly what was typed");
        assert!((c.anchor - 7.5).abs() < 1e-10, "anchor {}", c.anchor);
        assert_eq!(g.len(), start.len());
        // The last beat is where it should be to well under a microsecond.
        let last = *g.times().last().unwrap();
        assert!((last - start.times().last().unwrap()).abs() < 1e-9);
        // Halving and doubling 100 times comes back to the very same grid.
        let mut h = start.clone();
        for _ in 0..100 {
            h = h.halved(300.0).unwrap().doubled(300.0).unwrap();
        }
        assert_eq!(h, start);
    }
}
