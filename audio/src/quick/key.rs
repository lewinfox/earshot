//! Musical key from signal processing alone.
//!
//! * **Harmonic part**: each bin of the spectrogram is median-filtered over time (as in
//!   harmonic/percussive separation), which keeps sustained partials and drops drums.
//! * **Spectrum**: a 16384-point STFT (2.7 Hz bins, needed for the bass octave), hop 4096.
//! * **Tuning**: the magnitude-weighted circular mean of spectral peaks' offsets from the
//!   A440 equal-tempered grid.
//! * **Chroma**: bins mapped to tuning-corrected semitones (40 Hz-2.1 kHz) with a
//!   raised-cosine weight and folded into 12 pitch classes. Each frame is
//!   log-compressed and normalised, and weighted by its harmonic energy. The bass
//!   octave is left out (ClubCraft found kicks and sub-bass slides smear it).
//! * **Profiles**: the chroma is correlated with all 24 rotations of key profiles
//!   suited to electronic dance music: a blend of Ibrahim Sha'ath's KeyFinder profiles
//!   and Faraldo et al.'s EDMA (2016).
//!
//! Ported from ClubCraft (see the module above for the licence).

use super::dsp;

const FMIN: f64 = 40.0;
const FMAX: f64 = 2100.0;
/// MIDI note below which a partial counts as bass (C3, 130.8 Hz).
const BASS_SPLIT_MIDI: f64 = 48.0;
/// Weight of the bass chroma against the treble's.
const BASS_WEIGHT: f32 = 0.0;

/// A key: its tonic as a pitch class (0 = C … 11 = B) and its mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub tonic: u8,
    pub minor: bool,
}

/// Camelot numbers of the major keys, by tonic: C is 8B, G 9B, and so on.
const MAJOR_CAMELOT: [u8; 12] = [8, 3, 10, 5, 12, 7, 2, 9, 4, 11, 6, 1];
const NAMES: [&str; 12] = [
    "C", "Db", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B",
];

impl Key {
    /// Its Camelot number (1..=12); `minor` keys are the "A" side.
    pub fn camelot_number(self) -> u8 {
        // A minor key shares its number with its relative major, 3 semitones up.
        let pc = (self.tonic + if self.minor { 3 } else { 0 }) % 12;
        MAJOR_CAMELOT[pc as usize]
    }

    /// "8A", "11B".
    pub fn camelot(self) -> String {
        format!(
            "{}{}",
            self.camelot_number(),
            if self.minor { "A" } else { "B" }
        )
    }

    /// "Am", "F#".
    pub fn name(self) -> String {
        format!(
            "{}{}",
            NAMES[self.tonic as usize % 12],
            if self.minor { "m" } else { "" }
        )
    }
}

/// What [`detect`] found.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyEstimate {
    pub key: Key,
    /// The best key's correlation (-1..1), and its lead over the runner-up.
    pub score: f32,
    pub margin: f32,
    /// The tuning, in cents from A440.
    pub tuning_cents: f32,
}

/// Major and minor key profiles, starting at the tonic: the mean of the z-normalised
/// Sha'ath (KeyFinder) and EDMA profiles.
fn profiles() -> ([f32; 12], [f32; 12]) {
    const SHAATH: ([f32; 12], [f32; 12]) = (
        [
            7.24, 3.50, 3.58, 2.85, 5.82, 4.56, 2.45, 6.99, 3.39, 4.56, 4.07, 4.46,
        ],
        [
            7.00, 3.143, 4.36, 5.40, 3.67, 4.09, 3.91, 6.20, 3.63, 3.15, 3.81, 2.69,
        ],
    );
    const EDMA: ([f32; 12], [f32; 12]) = (
        [
            0.1652, 0.0475, 0.0829, 0.0669, 0.0999, 0.0927, 0.0529, 0.1316, 0.0522, 0.0744, 0.0694,
            0.0643,
        ],
        [
            0.1724, 0.0400, 0.0761, 0.1205, 0.0504, 0.0886, 0.0415, 0.1353, 0.0730, 0.0513, 0.0916,
            0.0509,
        ],
    );
    let z = |v: [f32; 12]| {
        let m = v.iter().sum::<f32>() / 12.0;
        let sd = (v.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / 12.0).sqrt();
        v.map(|x| (x - m) / sd)
    };
    let blend = |a: [f32; 12], b: [f32; 12]| {
        let (a, b) = (z(a), z(b));
        std::array::from_fn(|i| 0.5 * (a[i] + b[i]))
    };
    (blend(SHAATH.0, EDMA.0), blend(SHAATH.1, EDMA.1))
}

/// The key of a mono signal at `sr`, or `None` if it has no harmonic content.
pub fn detect(mono: &[f32], sr: f64) -> Option<KeyEstimate> {
    let n_fft = if sr > 50_000.0 { 32_768 } else { 16_384 };
    let hop = n_fft / 4;
    let bin_hz = sr / n_fft as f64;
    let nb = ((FMAX * 1.03) / bin_hz).ceil() as usize + 2;
    let spec: Vec<Vec<f32>> = dsp::stft_map(mono, n_fft, hop, |s| {
        s[..nb].iter().map(|c| c.norm()).collect()
    });
    if spec.len() < 8 {
        return None;
    }
    let frames = harmonic_median(&spec, nb, 4);
    let tuning = estimate_tuning(&frames, bin_hz);
    let chroma = chroma_frames(&frames, bin_hz, tuning);
    let emax = chroma.iter().map(|c| c.2).fold(0.0f32, f32::max);
    if emax <= 1e-6 {
        return None;
    }
    let mut acc = [0.0f32; 12];
    for (bass, treble, e) in &chroma {
        if *e < emax * 0.02 {
            continue;
        }
        let w = e.sqrt();
        let (b, t) = (normalize_log(bass), normalize_log(treble));
        for i in 0..12 {
            acc[i] += w * (t[i] + BASS_WEIGHT * b[i]);
        }
    }
    let s: f32 = acc.iter().sum();
    if s <= 0.0 {
        return None;
    }
    for v in &mut acc {
        *v /= s;
    }
    let (major, minor) = profiles();
    let mut all: Vec<(Key, f32)> = (0..12u8)
        .flat_map(|t| {
            [
                (
                    Key {
                        tonic: t,
                        minor: false,
                    },
                    correlate(&acc, &major, t as usize),
                ),
                (
                    Key {
                        tonic: t,
                        minor: true,
                    },
                    correlate(&acc, &minor, t as usize),
                ),
            ]
        })
        .collect();
    all.sort_by(|a, b| b.1.total_cmp(&a.1));
    Some(KeyEstimate {
        key: all[0].0,
        score: all[0].1,
        margin: all[0].1 - all[1].1,
        tuning_cents: tuning,
    })
}

/// Each bin median-filtered over `2 * half + 1` frames.
fn harmonic_median(spec: &[Vec<f32>], nb: usize, half: usize) -> Vec<Vec<f32>> {
    let n = spec.len();
    let mut buf = Vec::with_capacity(2 * half + 1);
    (0..n)
        .map(|i| {
            let (a, b) = (i.saturating_sub(half), (i + half + 1).min(n));
            (0..nb)
                .map(|k| {
                    buf.clear();
                    buf.extend((a..b).map(|j| spec[j][k]));
                    dsp::median_inplace(&mut buf)
                })
                .collect()
        })
        .collect()
}

fn estimate_tuning(frames: &[Vec<f32>], bin_hz: f64) -> f32 {
    let (mut sx, mut sy) = (0.0f64, 0.0f64);
    let lo = ((100.0 / bin_hz) as usize).max(1);
    let hi = (FMAX / bin_hz) as usize;
    for f in frames.iter().step_by(2) {
        let mx = f[lo..hi].iter().copied().fold(0.0f32, f32::max);
        if mx <= 1e-6 {
            continue;
        }
        let thr = mx * 0.1;
        for k in lo..hi.min(f.len() - 1) {
            let v = f[k];
            if v > thr && v >= f[k - 1] && v > f[k + 1] {
                let off = dsp::parabolic(f[k - 1] as f64, v as f64, f[k + 1] as f64);
                let cents = 1200.0 * ((k as f64 + off) * bin_hz / 440.0).log2();
                let dev = cents - 100.0 * (cents / 100.0).round();
                let ang = dev / 100.0 * std::f64::consts::TAU;
                sx += v as f64 * ang.cos();
                sy += v as f64 * ang.sin();
            }
        }
    }
    (sy.atan2(sx) / std::f64::consts::TAU * 100.0) as f32
}

/// Per frame: (bass chroma, treble chroma, harmonic energy).
fn chroma_frames(
    frames: &[Vec<f32>],
    bin_hz: f64,
    tuning_cents: f32,
) -> Vec<([f32; 12], [f32; 12], f32)> {
    let nb = frames.first().map_or(0, |f| f.len());
    // (bin, pitch class, weight, bass).
    let map: Vec<(usize, usize, f32, bool)> = (1..nb)
        .filter_map(|k| {
            let f = k as f64 * bin_hz;
            if !(FMIN..=FMAX).contains(&f) {
                return None;
            }
            let p = 69.0 + 12.0 * (f / 440.0).log2() - tuning_cents as f64 / 100.0;
            let r = p.round();
            let w = (std::f64::consts::PI * (p - r)).cos().powi(2);
            (w >= 0.05).then(|| {
                (
                    k,
                    (r as i64).rem_euclid(12) as usize,
                    w as f32,
                    r < BASS_SPLIT_MIDI,
                )
            })
        })
        .collect();
    frames
        .iter()
        .map(|f| {
            let (mut bass, mut treble, mut e) = ([0.0f32; 12], [0.0f32; 12], 0.0f32);
            for &(k, pc, w, is_bass) in &map {
                let v = f[k] * w;
                e += v;
                if is_bass {
                    bass[pc] += v;
                } else {
                    treble[pc] += v;
                }
            }
            (bass, treble, e)
        })
        .collect()
}

fn normalize_log(c: &[f32; 12]) -> [f32; 12] {
    let mx = c.iter().copied().fold(0.0f32, f32::max);
    if mx <= 1e-9 {
        return [0.0; 12];
    }
    let o = c.map(|v| (1.0 + 10.0 * v / mx).ln());
    let s: f32 = o.iter().sum();
    o.map(|v| v / s.max(1e-9))
}

/// Pearson correlation of chroma `c`, rotated so `rot` is the tonic, with template `t`.
fn correlate(c: &[f32; 12], t: &[f32; 12], rot: usize) -> f32 {
    let mc = c.iter().sum::<f32>() / 12.0;
    let mt = t.iter().sum::<f32>() / 12.0;
    let (mut num, mut dc, mut dt) = (0.0, 0.0, 0.0);
    for i in 0..12 {
        let a = c[(i + rot) % 12] - mc;
        let b = t[i] - mt;
        num += a * b;
        dc += a * a;
        dt += b * b;
    }
    num / (dc * dt).sqrt().max(1e-12)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camelot_names() {
        let k = |tonic, minor| Key { tonic, minor };
        assert_eq!(k(9, true).camelot(), "8A"); // A minor
        assert_eq!(k(0, false).camelot(), "8B"); // C major
        assert_eq!(k(7, false).camelot(), "9B"); // G major
        assert_eq!(k(6, true).camelot(), "11A"); // F# minor
        assert_eq!(k(9, true).name(), "Am");
    }
}
