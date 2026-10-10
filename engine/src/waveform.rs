//! Waveform data for the UI, computed once per track:
//!
//! - [`summarise`]: per 1/150 s, the peak level and the energy in three bands (low,
//!   mid, high), which give the colour, like Rekordbox's RGB waveform.
//! - [`detail`]: per 1/2000 s, the lowest and highest sample, so that zoomed in the UI
//!   draws the actual wave, not just its outline.

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

pub const PREVIEW_BINS: usize = 128;

/// The [`summarise`] output squeezed to [`PREVIEW_BINS`] bins of the same 4 bytes
/// (peak: the highest; bands: the average), for a thumbnail of the whole track.
pub fn preview(summary: &[u8]) -> Vec<u8> {
    let bins = summary.len() / 4;
    let mut out = vec![0u8; PREVIEW_BINS * 4];
    if bins == 0 {
        return out;
    }
    for j in 0..PREVIEW_BINS {
        let a = j * bins / PREVIEW_BINS;
        let b = ((j + 1) * bins / PREVIEW_BINS).max(a + 1).min(bins);
        let mut sums = [0u32; 3];
        let mut peak = 0u8;
        for i in a..b {
            peak = peak.max(summary[i * 4]);
            for k in 0..3 {
                sums[k] += summary[i * 4 + 1 + k] as u32;
            }
        }
        let n = (b - a) as u32;
        out[j * 4] = peak;
        for k in 0..3 {
            out[j * 4 + 1 + k] = (sums[k] / n) as u8;
        }
    }
    out
}

pub const DETAIL_PER_SECOND: f64 = 2000.0;

/// 2 bytes per bin: `[min, max]` of the mono signal, as `128 + 127 * x`, where `x` is
/// scaled so the track's loudest sample is ±1 (the same scaling as the summary's peak).
pub fn detail(audio: &TrackAudio) -> Vec<u8> {
    let frames_per_bin = (audio.sample_rate as f64 / DETAIL_PER_SECOND).max(1.0);
    let bins = (audio.frames() as f64 / frames_per_bin).ceil() as usize;
    let peak = audio
        .samples
        .chunks_exact(2)
        .fold(1e-9f32, |m, f| m.max((0.5 * (f[0] + f[1])).abs()));
    let mut out = Vec::with_capacity(bins * 2);
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    let mut next_edge = frames_per_bin;
    for (i, f) in audio.samples.chunks_exact(2).enumerate() {
        let x = 0.5 * (f[0] + f[1]) / peak;
        lo = lo.min(x);
        hi = hi.max(x);
        if (i + 1) as f64 >= next_edge || i + 1 == audio.frames() {
            out.push(signed_u8(lo));
            out.push(signed_u8(hi));
            next_edge += frames_per_bin;
            (lo, hi) = (f32::MAX, f32::MIN);
        }
    }
    out.truncate(bins * 2);
    out
}

fn signed_u8(x: f32) -> u8 {
    (128.0 + 127.0 * x.clamp(-1.0, 1.0)).round() as u8
}

fn one_pole(cutoff: f64, sr: f64) -> f32 {
    (1.0 - (-2.0 * std::f64::consts::PI * cutoff / sr).exp()) as f32
}

fn to_u8(x: f32) -> u8 {
    (x.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_follows_a_slow_sine() {
        // 50 Hz at 44.1 kHz: each 1/2000 s bin is a short slice of the cycle, so its
        // min and max are close together and trace the sine.
        let sr = 44_100u32;
        let samples: Vec<f32> = (0..sr as usize)
            .flat_map(|i| {
                let x = (2.0 * std::f32::consts::PI * 50.0 * i as f32 / sr as f32).sin() * 0.5;
                [x, x]
            })
            .collect();
        let d = detail(&TrackAudio { samples, sample_rate: sr });
        assert_eq!(d.len(), 2 * 2000);
        // Bin 10 is at 5 ms = a quarter cycle: the top of the wave, scaled to ±1.
        assert!(d[20] > 250 && d[21] >= d[20], "{:?}", &d[18..24]);
        // Bin 30 is at 15 ms = three quarters: the bottom.
        assert!(d[61] < 6, "{:?}", &d[58..64]);
        for b in d.chunks_exact(2) {
            assert!(b[0] <= b[1] && b[1] - b[0] < 25);
        }
    }
}
