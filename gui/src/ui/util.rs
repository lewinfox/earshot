//! Small shared pieces: colours, Camelot keys, time formatting, library search.

use egui::Color32;

pub const BG: Color32 = Color32::from_rgb(0x0f, 0x10, 0x13);
pub const PANEL: Color32 = Color32::from_rgb(0x17, 0x18, 0x1d);
pub const PANEL_2: Color32 = Color32::from_rgb(0x1f, 0x20, 0x27);
pub const LINE: Color32 = Color32::from_rgb(0x2b, 0x2d, 0x36);
pub const TEXT: Color32 = Color32::from_rgb(0xe6, 0xe6, 0xea);
pub const MUTED: Color32 = Color32::from_rgb(0x8a, 0x8c, 0x99);
pub const ACCENT: Color32 = Color32::from_rgb(0xff, 0x8c, 0x28);
pub const OK: Color32 = Color32::from_rgb(0x45, 0xd4, 0x83);
pub const WARN: Color32 = Color32::from_rgb(0xff, 0xcc, 0x4d);
pub const BAD: Color32 = Color32::from_rgb(0xff, 0x5b, 0x5b);
pub const WAVE_BG: Color32 = Color32::from_rgb(0x08, 0x09, 0x0b);
/// Decks A to D.
pub const DECK: [Color32; earshot_engine::MAX_DECKS] = [
    Color32::from_rgb(0x3f, 0xa7, 0xff),
    Color32::from_rgb(0xff, 0x5d, 0x8f),
    Color32::from_rgb(0x4f, 0xd4, 0x7a),
    Color32::from_rgb(0xc2, 0x8c, 0xff),
];

/// A deck's colour; Prepare's player has the accent.
pub fn deck_colour(deck: earshot_engine::DeckId) -> Color32 {
    DECK.get(deck.index()).copied().unwrap_or(ACCENT)
}
pub const HOTCUE: [Color32; 8] = [
    Color32::from_rgb(0x3c, 0xd3, 0x6b),
    Color32::from_rgb(0xff, 0x4f, 0x4f),
    Color32::from_rgb(0xff, 0xb0, 0x2e),
    Color32::from_rgb(0x2f, 0x9b, 0xff),
    Color32::from_rgb(0xc2, 0x5b, 0xff),
    Color32::from_rgb(0x18, 0xd2, 0xc7),
    Color32::from_rgb(0xff, 0x74, 0xc8),
    Color32::from_rgb(0xe6, 0xe0, 0x4a),
];

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

pub use crate::core::keys::{Camelot, key_name, parse_key};

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

// ---- search ------------------------------------------------------------------------------

/// A parsed search box: free words, `bpm:120-126` / `bpm:124`, `key:8A` (or `key:Am`).
pub struct Query {
    words: Vec<String>,
    bpm: Option<(f64, f64)>,
    key: Option<Camelot>,
}

impl Query {
    pub fn parse(q: &str) -> Query {
        let mut out = Query {
            words: vec![],
            bpm: None,
            key: None,
        };
        for tok in q.split_whitespace() {
            let lower = tok.to_lowercase();
            if let Some(b) = lower.strip_prefix("bpm:") {
                let r = match b.split_once('-') {
                    Some((a, c)) => a.parse().ok().zip(c.parse().ok()),
                    None => b.parse::<f64>().ok().map(|v| (v - 0.5, v + 0.5)),
                };
                if r.is_some() {
                    out.bpm = r;
                    continue;
                }
            }
            if let Some(k) = lower.strip_prefix("key:").and_then(|k| parse_key(k)) {
                out.key = Some(k);
                continue;
            }
            out.words.push(lower);
        }
        out
    }

    pub fn matches(
        &self,
        title: &str,
        artist: &str,
        bpm: Option<f64>,
        key: Option<Camelot>,
    ) -> bool {
        if self.key.is_some() && key != self.key {
            return false;
        }
        if let Some((lo, hi)) = self.bpm
            && !bpm.is_some_and(|b| b >= lo && b <= hi)
        {
            return false;
        }
        if self.words.is_empty() {
            return true;
        }
        let hay = format!("{title} {artist}").to_lowercase();
        self.words.iter().all(|w| hay.contains(w))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query() {
        let q = Query::parse("deep bpm:120-126 key:Am");
        assert!(q.matches("Deep Space", "x", Some(124.0), parse_key("8A")));
        assert!(!q.matches("Deep Space", "x", Some(128.0), parse_key("8A")));
        assert!(!q.matches("Shallow", "x", Some(124.0), parse_key("8A")));
    }

    #[test]
    fn tenths() {
        assert_eq!(fmt_tenths(164.7), "02:44.7");
        assert_eq!(fmt_tenths(-3.25), "-00:03.2");
    }
}
