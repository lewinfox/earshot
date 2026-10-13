//! Sound-alike search over audio embeddings (CLAP, to come): every track's vector in
//! one contiguous block of memory, so "what sounds like this?" is one scan over it
//! rather than a read of every vector from the database.
//!
//! - Vectors are normalised to length 1 when they go in, so similarity is a plain
//!   dot product (cosine similarity, -1..1).
//! - Row `i` of the block is `ids[i]`; `rows` finds a track's row. Removing a track
//!   moves the last row into its place, so the block never has holes.
//! - The Store loads the block when it opens (`embeddings` table, one model) and
//!   keeps it in step as it saves new vectors (`Store::embeddings`). A merged-away
//!   track can stay in it until the app restarts: callers only show tracks they know.

use std::collections::HashMap;

use super::track::TrackId;

/// The model whose vectors the app searches (`embeddings.model`).
pub const CLAP_MODEL: &str = "clap";
/// Its vectors' length.
pub const CLAP_DIM: usize = 512;

#[derive(Clone, Debug)]
pub struct Embeddings {
    dim: usize,
    /// `ids.len() * dim` floats, each row of length 1.
    data: Vec<f32>,
    ids: Vec<TrackId>,
    rows: HashMap<TrackId, usize>,
}

impl Default for Embeddings {
    fn default() -> Embeddings {
        Embeddings::new(CLAP_DIM)
    }
}

impl Embeddings {
    pub fn new(dim: usize) -> Embeddings {
        Embeddings {
            dim,
            data: vec![],
            ids: vec![],
            rows: HashMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// A track's vector (normalised), if it has one.
    pub fn get(&self, id: TrackId) -> Option<&[f32]> {
        let r = *self.rows.get(&id)?;
        Some(&self.data[r * self.dim..(r + 1) * self.dim])
    }

    /// Add or replace a track's vector. False (and nothing changes) if it's the wrong
    /// length, or all zeros or not finite, so it has no direction.
    pub fn insert(&mut self, id: TrackId, v: &[f32]) -> bool {
        if v.len() != self.dim {
            return false;
        }
        let Some(scale) = inverse_length(v) else {
            return false;
        };
        let row = match self.rows.get(&id) {
            Some(&r) => r,
            None => {
                self.ids.push(id);
                self.data.resize(self.data.len() + self.dim, 0.0);
                self.rows.insert(id, self.ids.len() - 1);
                self.ids.len() - 1
            }
        };
        for (to, from) in self.data[row * self.dim..(row + 1) * self.dim]
            .iter_mut()
            .zip(v)
        {
            *to = from * scale;
        }
        true
    }

    /// Take a track's vector out. The last row moves into its place.
    pub fn remove(&mut self, id: TrackId) {
        let Some(row) = self.rows.remove(&id) else {
            return;
        };
        let last = self.ids.len() - 1;
        if row != last {
            let moved = self.ids[last];
            self.data
                .copy_within(last * self.dim..(last + 1) * self.dim, row * self.dim);
            self.ids[row] = moved;
            self.rows.insert(moved, row);
        }
        self.ids.pop();
        self.data.truncate(last * self.dim);
    }

    /// The `k` tracks most like `id`'s sound, most alike first, as (track,
    /// similarity). Not `id` itself, nor any `skip` says no to. Empty if `id` has no
    /// vector.
    pub fn like_track(
        &self,
        id: TrackId,
        k: usize,
        skip: impl Fn(TrackId) -> bool,
    ) -> Vec<(TrackId, f32)> {
        match self.get(id) {
            Some(q) => self.nearest(q, k, |t| t == id || skip(t)),
            None => vec![],
        }
    }

    /// The `k` tracks nearest a normalised query vector, most alike first.
    pub fn nearest(
        &self,
        query: &[f32],
        k: usize,
        skip: impl Fn(TrackId) -> bool,
    ) -> Vec<(TrackId, f32)> {
        if query.len() != self.dim || k == 0 {
            return vec![];
        }
        let mut found: Vec<(TrackId, f32)> = self
            .similarities(query)
            .filter(|(id, _)| !skip(*id))
            .collect();
        if found.len() > k {
            found.select_nth_unstable_by(k - 1, |a, b| b.1.total_cmp(&a.1));
            found.truncate(k);
        }
        found.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        found
    }

    /// Every track's similarity to a normalised query vector, in block order: one
    /// pass over the block.
    pub fn similarities<'a>(
        &'a self,
        query: &'a [f32],
    ) -> impl Iterator<Item = (TrackId, f32)> + 'a {
        debug_assert_eq!(query.len(), self.dim);
        self.data
            .chunks_exact(self.dim)
            .zip(&self.ids)
            .map(move |(row, &id)| (id, dot(row, query)))
    }
}

/// 1 / the vector's length, if it has a direction.
fn inverse_length(v: &[f32]) -> Option<f32> {
    let len = dot(v, v).sqrt();
    (len.is_finite() && len > 0.0).then(|| 1.0 / len)
}

/// Normalise a vector to length 1 (for a query that isn't in the block). `None` if
/// it has no direction.
pub fn normalised(v: &[f32]) -> Option<Vec<f32>> {
    let s = inverse_length(v)?;
    Some(v.iter().map(|x| x * s).collect())
}

/// Eight running sums, so the compiler can keep them in vector registers.
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0.0f32; 8];
    let (ca, cb) = (a.chunks_exact(8), b.chunks_exact(8));
    let (ra, rb) = (ca.remainder(), cb.remainder());
    for (x, y) in ca.zip(cb) {
        for i in 0..8 {
            acc[i] += x[i] * y[i];
        }
    }
    let tail: f32 = ra.iter().zip(rb).map(|(x, y)| x * y).sum();
    acc.iter().sum::<f32>() + tail
}

/// A vector as the `embeddings.vector` blob holds it: little-endian f32s.
pub fn to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// A blob back as a vector; `None` if its length isn't whole floats.
pub fn from_blob(b: &[u8]) -> Option<Vec<f32>> {
    if b.len() % 4 != 0 {
        return None;
    }
    Some(
        b.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(rows: &[(TrackId, &[f32])]) -> Embeddings {
        let mut e = Embeddings::new(rows[0].1.len());
        for (id, v) in rows {
            assert!(e.insert(*id, v));
        }
        e
    }

    #[test]
    fn vectors_are_kept_normalised_and_found_by_track() {
        let mut e = Embeddings::new(3);
        assert!(e.insert(7, &[3.0, 0.0, 4.0]));
        assert_eq!(e.get(7), Some(&[0.6, 0.0, 0.8][..]));
        assert!(!e.insert(8, &[1.0, 2.0]), "wrong length");
        assert!(!e.insert(8, &[0.0; 3]), "no direction");
        assert!(!e.insert(8, &[f32::NAN, 1.0, 0.0]));
        assert_eq!(e.len(), 1);
        // Replacing keeps one row.
        assert!(e.insert(7, &[0.0, 2.0, 0.0]));
        assert_eq!((e.len(), e.get(7)), (1, Some(&[0.0, 1.0, 0.0][..])));
    }

    #[test]
    fn removing_moves_the_last_row_into_the_gap() {
        let mut e = block(&[(1, &[1.0, 0.0]), (2, &[0.0, 1.0]), (3, &[-1.0, 0.0])]);
        e.remove(1);
        e.remove(42); // not there: nothing happens
        assert_eq!(e.len(), 2);
        assert_eq!(e.get(1), None);
        assert_eq!(e.get(3), Some(&[-1.0, 0.0][..]));
        assert_eq!(e.get(2), Some(&[0.0, 1.0][..]));
        assert_eq!(e.data.len(), 4);
        e.remove(3);
        e.remove(2);
        assert!(e.is_empty() && e.data.is_empty());
    }

    #[test]
    fn the_nearest_come_first_and_skipped_ones_never() {
        let e = block(&[
            (1, &[1.0, 0.0, 0.0]),
            (2, &[0.9, 0.1, 0.0]),
            (3, &[0.0, 1.0, 0.0]),
            (4, &[0.7, 0.7, 0.0]),
            (5, &[-1.0, 0.0, 0.0]),
        ]);
        let ids = |v: Vec<(TrackId, f32)>| v.into_iter().map(|x| x.0).collect::<Vec<_>>();
        assert_eq!(ids(e.like_track(1, 3, |_| false)), [2, 4, 3]);
        assert_eq!(ids(e.like_track(1, 10, |t| t == 4)), [2, 3, 5]);
        assert!(e.like_track(99, 3, |_| false).is_empty());
        let near = e.like_track(1, 1, |_| false);
        assert!((near[0].1 - 0.9 / (0.82f32).sqrt()).abs() < 1e-6);
        // A query from outside the block.
        let q = normalised(&[0.0, 0.0, 0.0]);
        assert!(q.is_none());
        let q = normalised(&[0.0, 3.0, 0.0]).unwrap();
        assert_eq!(ids(e.nearest(&q, 2, |_| false)), [3, 4]);
    }

    #[test]
    fn dot_matches_the_plain_sum_for_any_length() {
        for n in [0, 1, 7, 8, 9, 512, 515] {
            let a: Vec<f32> = (0..n).map(|i| (i as f32 * 0.37).sin()).collect();
            let b: Vec<f32> = (0..n).map(|i| (i as f32 * 0.11).cos()).collect();
            let plain: f32 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
            assert!((dot(&a, &b) - plain).abs() < 1e-3, "{n}");
        }
    }

    #[test]
    fn blobs_round_trip() {
        let v = vec![1.5, -0.25, f32::MIN_POSITIVE, 3e7];
        assert_eq!(from_blob(&to_blob(&v)), Some(v));
        assert_eq!(from_blob(&[0, 1, 2]), None);
    }
}
