//! Colour waveform summary: per bin, the peak level and the energy in three bands,
//! like Rekordbox's RGB waveform. Computed once per track; the UI draws from it.

use crate::decode::TrackAudio;

pub const BINS_PER_SECOND: f64 = 150.0;

/// 4 bytes per bin: `[peak, low, mid, high]`, each 0..=255, normalised per track.
pub fn summarise(audio: &TrackAudio) -> Vec<u8> {
    let sr = audio.sample_rate as f64;
    let frames_per_bin = (sr / BINS_PER_SECOND).max(1.0);
    let bins = (audio.frames() as f64 / frames_per_bin).ceil() as usize;

    // One-pole low-passes at 200 Hz and 2 kHz split the signal into three bands.
    let a_lo = one_pole(200.0, sr);
    let a_hi = one_pole(2000.0, sr);
    let (mut lp_lo, mut lp_hi) = (0.0f32, 0.0f32);

    let mut raw = vec![[0f32; 4]; bins];
    let mut sums = [0f32; 3];
    let mut peak = 0f32;
    let mut count = 0usize;
    let mut bin = 0usize;
    let mut next_edge = frames_per_bin;
    for (i, frame) in audio.samples.chunks_exact(2).enumerate() {
        let x = 0.5 * (frame[0] + frame[1]);
        lp_lo += a_lo * (x - lp_lo);
        lp_hi += a_hi * (x - lp_hi);
        let (low, mid, high) = (lp_lo, lp_hi - lp_lo, x - lp_hi);
        sums[0] += low * low;
        sums[1] += mid * mid;
        sums[2] += high * high;
        peak = peak.max(frame[0].abs()).max(frame[1].abs());
        count += 1;
        if (i + 1) as f64 >= next_edge || i + 1 == audio.frames() {
            let n = count.max(1) as f32;
            raw[bin.min(bins - 1)] = [
                peak,
                (sums[0] / n).sqrt(),
                (sums[1] / n).sqrt(),
                (sums[2] / n).sqrt(),
            ];
            bin += 1;
            next_edge += frames_per_bin;
            sums = [0.0; 3];
            peak = 0.0;
            count = 0;
        }
    }

    let mut max = [1e-9f32; 4];
    for b in &raw {
        for k in 0..4 {
            max[k] = max[k].max(b[k]);
        }
    }
    let band_max = max[1].max(max[2]).max(max[3]);
    let mut out = Vec::with_capacity(bins * 4);
    for b in &raw {
        out.push(to_u8(b[0] / max[0]));
        for k in 1..4 {
            // Square root keeps quiet bands visible next to the kick.
            out.push(to_u8((b[k] / band_max).sqrt()));
        }
    }
    out
}

fn one_pole(cutoff: f64, sr: f64) -> f32 {
    (1.0 - (-2.0 * std::f64::consts::PI * cutoff / sr).exp()) as f32
}

fn to_u8(x: f32) -> u8 {
    (x.clamp(0.0, 1.0) * 255.0).round() as u8
}
