//! Small shared pieces: colour maths, waveform and Camelot key colours, time
//! formatting. The palette itself is in [`super::theme`].

use egui::Color32;

pub fn dim(c: Color32, k: f32) -> Color32 {
    Color32::from_rgb(
        (c.r() as f32 * k) as u8,
        (c.g() as f32 * k) as u8,
        (c.b() as f32 * k) as u8,
    )
}

pub fn hsl(h: f32, s: f32, l: f32) -> Color32 {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = (h.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let f = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(f(r), f(g), f(b))
}

// ---- waveform colours ---------------------------------------------------------------

/// Bass carries most of the energy in dance music; these lift mids and highs so they
/// still show through.
const MID_GAIN: f32 = 2.0;
const HIGH_GAIN: f32 = 4.0;

/// Band levels (as stored: square-rooted, 0..255) to a colour: bass red, mids green,
/// highs blue, like Rekordbox's RGB waveform. Squaring and cubing the ratios to the
/// strongest band keeps them saturated.
pub fn band_colour(low: u8, mid: u8, high: u8) -> Color32 {
    let sq = |v: u8, g: f32| {
        let x = v as f32 / 255.0;
        x * x * g
    };
    let b = [sq(low, 1.0), sq(mid, MID_GAIN), sq(high, HIGH_GAIN)];
    let m = b[0].max(b[1]).max(b[2]).max(1e-4);
    let ch = |x: f32| ((0.12 + 0.88 * (x / m).powi(3)) * 255.0) as u8;
    Color32::from_rgb(ch(b[0]), ch(b[1]), ch(b[2]))
}

// ---- Camelot keys ---------------------------------------------------------------------

pub use crate::core::keys::{Camelot, key_name};

/// Key colour round the Camelot wheel; B (major) lighter.
pub fn key_colour(k: Camelot) -> Color32 {
    hsl(
        ((k.n as f32 - 1.0) * 30.0 + 150.0) % 360.0,
        0.7,
        if k.minor { 0.58 } else { 0.72 },
    )
}

// ---- formatting ------------------------------------------------------------------------

/// Minutes, seconds and tenths, like 02:44.7.
pub fn fmt_tenths(s: f64) -> String {
    if !s.is_finite() {
        return "--:--.-".into();
    }
    let tenths = (s.abs() * 10.0) as u64;
    format!(
        "{}{:02}:{:02}.{}",
        if s < 0.0 { "-" } else { "" },
        tenths / 600,
        (tenths / 10) % 60,
        tenths % 10
    )
}

pub fn mmss(s: f64) -> String {
    format!("{}:{:02}", (s / 60.0) as u64, (s % 60.0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenths() {
        assert_eq!(fmt_tenths(164.7), "02:44.7");
        assert_eq!(fmt_tenths(-3.25), "-00:03.2");
    }
}
