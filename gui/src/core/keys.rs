//! Camelot keys: parsing, naming and moving round the wheel.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Camelot {
    pub n: u8, // 1..12
    pub minor: bool,
}

const MAJOR_BY_PITCH: [u8; 12] = [8, 3, 10, 5, 12, 7, 2, 9, 4, 11, 6, 1]; // C, Db, D …

/// Parse "8A", "Am", "C#m", "Ebmin", "F#" or "F# major".
pub fn parse_key(raw: &str) -> Option<Camelot> {
    let s = raw.trim();
    if let Some(letter) = s
        .chars()
        .last()
        .filter(|c| matches!(c, 'A' | 'B' | 'a' | 'b'))
        && let Ok(n) = s[..s.len() - 1].trim().parse::<u8>()
    {
        return (1..=12).contains(&n).then_some(Camelot {
            n,
            minor: letter.eq_ignore_ascii_case(&'a'),
        });
    }
    let mut chars = s.chars();
    let note = chars.next()?.to_ascii_uppercase();
    let mut pitch: i32 = match note {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let rest: String = chars.collect();
    let mut rest = rest.as_str();
    if let Some(r) = rest.strip_prefix('#').or_else(|| rest.strip_prefix('♯')) {
        pitch += 1;
        rest = r;
    } else if let Some(r) = rest.strip_prefix('b').or_else(|| rest.strip_prefix('♭')) {
        pitch -= 1;
        rest = r;
    }
    let rest = rest.trim().to_lowercase();
    let minor = match rest.as_str() {
        "" | "maj" | "major" => false,
        "m" | "min" | "minor" => true,
        _ => return None,
    };
    // A minor key's Camelot number is that of its relative major (3 semitones up).
    let n = MAJOR_BY_PITCH[((pitch + if minor { 3 } else { 0 }).rem_euclid(12)) as usize];
    Some(Camelot { n, minor })
}

pub fn key_name(k: Camelot) -> String {
    format!("{}{}", k.n, if k.minor { "A" } else { "B" })
}

/// Shift a key by semitones: each semitone is 7 steps round the wheel.
pub fn transpose(k: Camelot, semitones: i32) -> Camelot {
    Camelot {
        n: ((k.n as i32 - 1 + semitones * 7).rem_euclid(12) + 1) as u8,
        minor: k.minor,
    }
}

/// Steps round the wheel between two keys' numbers, 0..6 (ignores A/B).
pub fn wheel_distance(a: Camelot, b: Camelot) -> u8 {
    let d = (a.n as i32 - b.n as i32).unsigned_abs() % 12;
    d.min(12 - d) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert_eq!(parse_key("8A"), Some(Camelot { n: 8, minor: true }));
        assert_eq!(parse_key("Am"), Some(Camelot { n: 8, minor: true }));
        assert_eq!(parse_key("C"), Some(Camelot { n: 8, minor: false }));
        assert_eq!(parse_key("F#m"), Some(Camelot { n: 11, minor: true }));
        assert_eq!(parse_key("Ebmin"), Some(Camelot { n: 2, minor: true }));
    }

    #[test]
    fn wheel() {
        let k = |s| parse_key(s).unwrap();
        assert_eq!(transpose(k("8A"), 1), k("3A")); // Am up a semitone is Bbm
        assert_eq!(transpose(k("8A"), -1), k("1A"));
        assert_eq!(wheel_distance(k("1A"), k("12B")), 1);
        assert_eq!(wheel_distance(k("2A"), k("8A")), 6);
    }
}
