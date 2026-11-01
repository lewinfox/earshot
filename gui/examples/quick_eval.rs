//! Score the quick analysis (earshot-audio's `quick`: a steady grid and a key from
//! signal processing) against what's in an earshot library, and time it.
//!
//! ```text
//! cargo run --release -p earshot-gui --example quick_eval -- [options] [files or folders]
//!
//!   --db PATH     library.db to read (default: earshot's data folder, or
//!                 $EARSHOT_DATA_DIR). Opened read-only.
//!   --limit N     only the first N tracks
//!   --csv PATH    also write one row per track and reference grid
//!   files…        just analyse these (folders are searched) and print what it finds
//! ```
//!
//! With the library, every track that has a file and at least one grid is decoded
//! with the app's own decoder (so times line up with the stored grids) and compared
//! with each grid it has, by source: `analysis` (Beat This!, static), `analysis-dynamic`,
//! `import` (another DJ program's, e.g. rekordbox's), `user` (fixed by hand) and
//! `sidecar`. The quick grid is worked out twice: on its own, and with the track's BPM
//! tag as a hint (what the app does). Keys are compared with the library's key field
//! (from tags, or typed in). See docs/quick-analysis.md.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use earshot_engine::beatgrid::{Beatgrid, Constant, GridModel};
use earshot_engine::decode::{self, TrackAudio};
use earshot_engine::media::Location;
use earshot_engine::quick::{self, Key, QuickGrid};
use rusqlite::{Connection, OpenFlags};

/// The confidence the app needs before it uses a quick grid (gui/src/core/player.rs).
const APP_CONFIDENCE: f32 = 1.0;

struct Args {
    db: PathBuf,
    limit: Option<usize>,
    csv: Option<PathBuf>,
    files: Vec<PathBuf>,
}

fn args() -> Result<Args, String> {
    let mut a = Args {
        db: data_dir().join("library.db"),
        limit: None,
        csv: None,
        files: vec![],
    };
    let mut it = std::env::args().skip(1);
    while let Some(x) = it.next() {
        let mut val = || it.next().ok_or(format!("{x} needs a value"));
        match x.as_str() {
            "--db" => a.db = PathBuf::from(val()?),
            "--limit" => a.limit = Some(val()?.parse().map_err(|_| "--limit N")?),
            "--csv" => a.csv = Some(PathBuf::from(val()?)),
            "-h" | "--help" => {
                return Err(
                    "usage: quick_eval [--db PATH] [--limit N] [--csv PATH] [files…]".into(),
                );
            }
            _ => a.files.push(PathBuf::from(x)),
        }
    }
    Ok(a)
}

/// As the app works it out (gui/src/core/services.rs).
fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("EARSHOT_DATA_DIR") {
        return PathBuf::from(d);
    }
    directories::BaseDirs::new()
        .map(|d| d.data_dir().join("io.github.lewinfox.earshot"))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn main() {
    let a = match args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let r = if a.files.is_empty() {
        library(&a)
    } else {
        files(&a.files)
    };
    if let Err(e) = r {
        eprintln!("quick_eval: {e}");
        std::process::exit(1);
    }
}

// ---- just files -------------------------------------------------------------------------

const AUDIO: [&str; 9] = [
    "mp3", "m4a", "mp4", "aac", "flac", "wav", "aif", "aiff", "ogg",
];

fn collect(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        let mut entries: Vec<_> = std::fs::read_dir(p)
            .map(|d| d.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        entries.sort();
        for e in entries {
            collect(&e, out);
        }
    } else if p
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO.contains(&e.to_ascii_lowercase().as_str()))
    {
        out.push(p.to_path_buf());
    }
}

fn files(paths: &[PathBuf]) -> Result<(), String> {
    let mut all = vec![];
    for p in paths {
        collect(p, &mut all);
    }
    println!("{}", cores());
    let (mut n, mut total) = (0, Duration::ZERO);
    for p in all {
        let t = Instant::now();
        let (audio, tags) = match decode::decode(&Location::File(p.clone())) {
            Ok(x) => x,
            Err(e) => {
                println!("{}: can't decode: {e}", p.display());
                continue;
            }
        };
        let dec = t.elapsed();
        let q = quick::analyse(&audio, tags.bpm);
        n += 1;
        total += q.took;
        println!(
            "{}\n  {:.0} s · decode {} · quick {} · grid {} · key {}{}",
            p.display(),
            audio.duration(),
            ms(dec),
            ms(q.took),
            q.grid.map_or("none".into(), |g| grid_text(&g)),
            q.key.as_ref().map_or("none".into(), |k| format!(
                "{} ({})",
                k.key.camelot(),
                k.key.name()
            )),
            tags.bpm
                .map_or(String::new(), |b| format!(" · tag {b} BPM")),
        );
    }
    if n > 0 {
        println!("\n{n} tracks, quick analysis {} on average", ms(total / n));
    }
    Ok(())
}

// ---- against the library ------------------------------------------------------------------

/// A library track with a file to read and grids to compare with.
struct Track {
    id: i64,
    name: String,
    at: Location,
    tag_bpm: Option<f64>,
    key: Option<String>,
    /// The latest grid of each source.
    grids: Vec<(String, GridModel)>,
}

fn read_library(db: &Path, limit: Option<usize>) -> Result<Vec<Track>, String> {
    let c = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("{}: {e}", db.display()))?;
    let e = |e: rusqlite::Error| e.to_string();
    let mut q = c
        .prepare(
            "SELECT t.id,
               (SELECT f.path FROM files f WHERE f.track_id = t.id AND f.missing_since IS NULL
                ORDER BY f.id LIMIT 1),
               (SELECT value FROM track_meta_resolved WHERE track_id = t.id AND field = 'title'),
               (SELECT value FROM track_meta_resolved WHERE track_id = t.id AND field = 'artist'),
               (SELECT value FROM track_meta_resolved WHERE track_id = t.id AND field = 'bpm'),
               (SELECT value FROM track_meta_resolved WHERE track_id = t.id AND field = 'key')
             FROM tracks t
             WHERE t.deleted_at IS NULL AND EXISTS (SELECT 1 FROM beatgrids g WHERE g.track_id = t.id)
             ORDER BY t.id",
        )
        .map_err(e)?;
    let rows = q
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<Vec<u8>>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })
        .map_err(e)?;
    let mut grids = c
        .prepare(
            "SELECT source, kind, bpm, anchor_s, anchor_pos, meter, beats FROM beatgrids
             WHERE id IN (SELECT max(id) FROM beatgrids WHERE track_id = ?1 GROUP BY source)
             ORDER BY source",
        )
        .map_err(e)?;
    let mut out = vec![];
    for row in rows {
        let (id, path, title, artist, bpm, key) = row.map_err(e)?;
        let Some(path) = path else { continue };
        let mut gs = vec![];
        let mut q = grids.query([id]).map_err(e)?;
        while let Some(r) = q.next().map_err(e)? {
            let source: String = r.get(0).map_err(e)?;
            let kind: String = r.get(1).map_err(e)?;
            let model = if kind == "constant" {
                GridModel::Constant(Constant {
                    bpm: r.get(2).map_err(e)?,
                    anchor: r.get(3).map_err(e)?,
                    anchor_pos: r.get::<_, i64>(4).map_err(e)? as u8,
                    meter: r.get::<_, i64>(5).map_err(e)? as u8,
                })
            } else {
                let b: Vec<u8> = r.get(6).map_err(e)?;
                match Beatgrid::from_bytes(&b) {
                    Ok(g) => GridModel::Markers {
                        times: g.times().to_vec(),
                        positions: g.positions().to_vec(),
                    },
                    Err(_) => continue,
                }
            };
            gs.push((source, model));
        }
        let at = Location::from_bytes(path);
        let name = match (artist.filter(|a| !a.is_empty()), title) {
            (Some(a), Some(t)) => format!("{a} - {t}"),
            (None, Some(t)) => t,
            _ => at.to_string(),
        };
        out.push(Track {
            id,
            name,
            at,
            tag_bpm: bpm
                .and_then(|b| b.trim().parse().ok())
                .filter(|b: &f64| *b > 40.0),
            key,
            grids: gs,
        });
        if limit.is_some_and(|l| out.len() >= l) {
            break;
        }
    }
    Ok(out)
}

/// How the quick grid's tempo compares with a reference grid's.
fn tempo_class(q: f64, r: f64) -> &'static str {
    let near = |x: f64| (q / r / x - 1.0).abs() < 0.02;
    if (q - r).abs() <= 0.05 {
        "exact"
    } else if (q - r).abs() <= 0.5 {
        "close"
    } else if near(2.0) {
        "double"
    } else if near(0.5) {
        "half"
    } else if near(2.0 / 3.0) {
        "2/3"
    } else if near(1.5) {
        "3/2"
    } else {
        "wrong"
    }
}

/// The quick grid against one reference grid.
#[derive(Default)]
struct Cmp {
    ref_bpm: f64,
    class: &'static str,
    /// Median and 90th percentile of |quick beat - nearest reference beat| (ms), over
    /// the middle 80% of the track; only when the tempo is exact or close.
    phase_ms: Option<(f64, f64)>,
    /// Signed median (ms): positive, the quick grid is late.
    offset_ms: Option<f64>,
    /// Share of the quick grid's 1s that land on the reference's 1s (where it knows them).
    downbeats: Option<f64>,
}

fn compare(q: &Constant, r: &Beatgrid, duration: f64) -> Cmp {
    let mut c = Cmp {
        ref_bpm: r.mean_bpm(),
        ..Default::default()
    };
    c.class = tempo_class(q.bpm, c.ref_bpm);
    if !matches!(c.class, "exact" | "close") {
        return c;
    }
    let period = 60.0 / q.bpm;
    let (lo, hi) = (0.1 * duration, 0.9 * duration);
    let k0 = ((lo - q.anchor) / period).ceil() as i64;
    let k1 = ((hi - q.anchor) / period).floor() as i64;
    let (mut offs, mut agree, mut known) = (vec![], 0, 0);
    for k in k0..=k1 {
        let t = q.anchor + k as f64 * period;
        let i = r.beat_at(t).round();
        let d = t - r.time_at(i);
        offs.push(d);
        // A quick 1 (the anchor is a 1, in 4/4) close to a reference beat.
        if k.rem_euclid(q.meter as i64) == 0 && d.abs() < 0.25 * period {
            let p = r.position_of(i as i64);
            if p != 0 {
                known += 1;
                agree += (p == 1) as usize;
            }
        }
    }
    if offs.is_empty() {
        return c;
    }
    let mut abs: Vec<f64> = offs.iter().map(|d| d.abs() * 1000.0).collect();
    abs.sort_by(f64::total_cmp);
    offs.sort_by(f64::total_cmp);
    let pct = |v: &[f64], p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
    c.phase_ms = Some((pct(&abs, 0.5), pct(&abs, 0.9)));
    c.offset_ms = Some(pct(&offs, 0.5) * 1000.0);
    c.downbeats = (known > 0).then(|| agree as f64 / known as f64);
    c
}

/// "8A", "Am", "F# minor" … as a Camelot (number, minor).
fn parse_key(raw: &str) -> Option<(u8, bool)> {
    const MAJOR: [u8; 12] = [8, 3, 10, 5, 12, 7, 2, 9, 4, 11, 6, 1];
    let s = raw.trim();
    if let Some(l) = s.chars().last().filter(|c| "AaBb".contains(*c))
        && let Ok(n) = s[..s.len() - 1].trim().parse::<u8>()
    {
        return (1..=12)
            .contains(&n)
            .then_some((n, l.eq_ignore_ascii_case(&'a')));
    }
    let mut ch = s.chars();
    let mut pc: i32 = match ch.next()?.to_ascii_uppercase() {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let rest: String = ch.collect();
    let mut rest = rest.as_str();
    if let Some(r) = rest.strip_prefix(['#', '♯']) {
        pc += 1;
        rest = r;
    } else if let Some(r) = rest.strip_prefix(['b', '♭']) {
        pc -= 1;
        rest = r;
    }
    let minor = match rest.trim().to_lowercase().as_str() {
        "" | "maj" | "major" => false,
        "m" | "min" | "minor" => true,
        _ => return None,
    };
    Some((
        MAJOR[(pc + if minor { 3 } else { 0 }).rem_euclid(12) as usize],
        minor,
    ))
}

/// The quick key against the library's.
fn key_class(q: Key, lib: Option<&str>) -> &'static str {
    let Some(l) = lib.and_then(parse_key) else {
        return "-";
    };
    let (n, minor) = (q.camelot_number(), q.minor);
    let steps = (n as i32 - l.0 as i32)
        .rem_euclid(12)
        .min((l.0 as i32 - n as i32).rem_euclid(12));
    match (steps, minor == l.1) {
        (0, true) => "exact",
        (0, false) => "relative",
        (1, true) => "fifth",
        _ => "other",
    }
}

fn ms(d: Duration) -> String {
    format!("{:.0} ms", d.as_secs_f64() * 1000.0)
}

fn grid_text(g: &QuickGrid) -> String {
    format!(
        "{:.2} BPM, 1 at {:.3} s (confidence {:.2})",
        g.grid.bpm, g.grid.anchor, g.confidence
    )
}

fn cores() -> String {
    format!(
        "{} cores, {}",
        std::thread::available_parallelism().map_or(1, |n| n.get()),
        std::env::consts::OS
    )
}

/// Running totals for one reference source.
#[derive(Default)]
struct Tally {
    n: usize,
    classes: BTreeMap<&'static str, usize>,
    /// The same, without the BPM tag hint.
    classes_nohint: BTreeMap<&'static str, usize>,
    phase_10: usize,
    phase_20: usize,
    phased: usize,
    offsets: Vec<f64>,
    down_agree: usize,
    down_known: usize,
    /// Tracks the app would use the quick grid for, and how many of those are exact.
    confident: usize,
    confident_exact: usize,
}

fn library(a: &Args) -> Result<(), String> {
    let tracks = read_library(&a.db, a.limit)?;
    println!(
        "{} · {} tracks with grids in {}\n",
        cores(),
        tracks.len(),
        a.db.display()
    );
    let mut csv = match &a.csv {
        Some(p) => {
            let mut f = std::fs::File::create(p).map_err(|e| format!("{}: {e}", p.display()))?;
            writeln!(
                f,
                "track_id,name,source,ref_bpm,quick_bpm,confidence,tempo,tempo_without_hint,\
                 phase_median_ms,phase_p90_ms,offset_ms,downbeats,tag_bpm,library_key,quick_key,key,\
                 decode_ms,quick_ms"
            )
            .map_err(|e| e.to_string())?;
            Some(f)
        }
        None => None,
    };
    let mut tallies: BTreeMap<String, Tally> = BTreeMap::new();
    let (mut keys, mut quick_time, mut decode_time, mut done, mut worst) = (
        BTreeMap::<&str, usize>::new(),
        Duration::ZERO,
        Duration::ZERO,
        0u32,
        Duration::ZERO,
    );
    let mut failed = 0;
    for (i, t) in tracks.iter().enumerate() {
        let start = Instant::now();
        let audio: TrackAudio = match decode::decode(&t.at) {
            Ok((audio, _)) => audio,
            Err(e) => {
                println!("[{}/{}] {}: can't decode: {e}", i + 1, tracks.len(), t.name);
                failed += 1;
                continue;
            }
        };
        let dec = start.elapsed();
        let q = quick::analyse(&audio, t.tag_bpm);
        let plain = if t.tag_bpm.is_some() {
            quick::grid(&audio, None)
        } else {
            q.grid
        };
        done += 1;
        quick_time += q.took;
        decode_time += dec;
        worst = worst.max(q.took);
        let kc = q
            .key
            .as_ref()
            .map_or("none", |k| key_class(k.key, t.key.as_deref()));
        *keys.entry(kc).or_default() += 1;
        println!(
            "[{}/{}] {}\n  quick: {} · key {} (library {}: {kc}) · decode {} · quick {}",
            i + 1,
            tracks.len(),
            t.name,
            q.grid.as_ref().map_or("no grid".into(), grid_text),
            q.key.as_ref().map_or("none".into(), |k| k.key.camelot()),
            t.key.as_deref().unwrap_or("-"),
            ms(dec),
            ms(q.took),
        );
        for (source, model) in &t.grids {
            let Ok(r) = Beatgrid::from_model(model.clone(), audio.duration()) else {
                continue;
            };
            let tally = tallies.entry(source.clone()).or_default();
            tally.n += 1;
            let nohint = plain.map_or("none", |g| tempo_class(g.grid.bpm, r.mean_bpm()));
            *tally.classes_nohint.entry(nohint).or_default() += 1;
            let Some(g) = q.grid else {
                *tally.classes.entry("none").or_default() += 1;
                println!("  vs {source}: {:.2} BPM; quick found none", r.mean_bpm());
                continue;
            };
            let c = compare(&g.grid, &r, audio.duration());
            *tally.classes.entry(c.class).or_default() += 1;
            if g.confidence >= APP_CONFIDENCE {
                tally.confident += 1;
                tally.confident_exact += (c.class == "exact") as usize;
            }
            if let Some((med, _)) = c.phase_ms {
                tally.phased += 1;
                tally.phase_10 += (med <= 10.0) as usize;
                tally.phase_20 += (med <= 20.0) as usize;
            }
            tally.offsets.extend(c.offset_ms);
            if let Some(d) = c.downbeats {
                tally.down_known += 1;
                tally.down_agree += (d >= 0.75) as usize;
            }
            println!(
                "  vs {source}: {:.2} BPM, tempo {} (without the tag hint: {nohint}){}{}",
                c.ref_bpm,
                c.class,
                c.phase_ms.map_or(String::new(), |(m, p)| format!(
                    ", beats off by {m:.0} ms (90%: {p:.0} ms), offset {:+.0} ms",
                    c.offset_ms.unwrap_or(0.0)
                )),
                c.downbeats
                    .map_or(String::new(), |d| format!(", 1s agree {:.0}%", d * 100.0)),
            );
            if let Some(f) = csv.as_mut() {
                let o = |x: Option<f64>| x.map_or(String::new(), |v| format!("{v:.1}"));
                writeln!(
                    f,
                    "{},\"{}\",{source},{:.3},{:.3},{:.2},{},{nohint},{},{},{},{},{},{},{},{kc},{:.0},{:.0}",
                    t.id,
                    t.name.replace('"', "'"),
                    c.ref_bpm,
                    g.grid.bpm,
                    g.confidence,
                    c.class,
                    o(c.phase_ms.map(|p| p.0)),
                    o(c.phase_ms.map(|p| p.1)),
                    o(c.offset_ms),
                    c.downbeats.map_or(String::new(), |d| format!("{d:.2}")),
                    t.tag_bpm.map_or(String::new(), |b| format!("{b}")),
                    t.key.as_deref().unwrap_or("").replace(',', " "),
                    q.key.as_ref().map_or(String::new(), |k| k.key.camelot()),
                    dec.as_secs_f64() * 1000.0,
                    q.took.as_secs_f64() * 1000.0,
                )
                .map_err(|e| e.to_string())?;
            }
        }
    }

    println!("\n==== summary ====\n{}", cores());
    if done == 0 {
        println!("no tracks decoded ({failed} failed)");
        return Ok(());
    }
    println!(
        "{done} tracks ({failed} couldn't be decoded). Quick analysis {} on average, {} at worst; \
         decoding {} on average.",
        ms(quick_time / done),
        ms(worst),
        ms(decode_time / done),
    );
    let pct = |a: usize, b: usize| {
        if b == 0 {
            "-".to_string()
        } else {
            format!("{:.0}% ({a}/{b})", 100.0 * a as f64 / b as f64)
        }
    };
    for (source, t) in &tallies {
        let list = |m: &BTreeMap<&str, usize>| {
            m.iter()
                .map(|(k, v)| format!("{k} {v}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut offs = t.offsets.clone();
        offs.sort_by(f64::total_cmp);
        let median_offset = offs.get(offs.len() / 2).copied();
        println!("\nagainst {source} grids ({} tracks)", t.n);
        println!(
            "  tempo exact (±0.05 BPM): {}   [{}]",
            pct(*t.classes.get("exact").unwrap_or(&0), t.n),
            list(&t.classes)
        );
        println!(
            "  without the tag hint:    {}   [{}]",
            pct(*t.classes_nohint.get("exact").unwrap_or(&0), t.n),
            list(&t.classes_nohint)
        );
        println!(
            "  beats within 10 ms: {}, within 20 ms: {} (of tracks with the tempo right)",
            pct(t.phase_10, t.phased),
            pct(t.phase_20, t.phased)
        );
        if let Some(o) = median_offset {
            println!("  typical offset: {o:+.1} ms (positive: the quick grid is later)");
        }
        println!(
            "  1s on the reference's 1s (75% or more of the track): {}",
            pct(t.down_agree, t.down_known)
        );
        println!(
            "  confident enough for the app (≥ {APP_CONFIDENCE}): {} tracks, exact on {}",
            t.confident,
            pct(t.confident_exact, t.confident)
        );
    }
    let total: usize = keys
        .iter()
        .filter(|(k, _)| **k != "-")
        .map(|(_, v)| *v)
        .sum();
    let get = |k| *keys.get(k).unwrap_or(&0);
    println!(
        "\nkeys against the library's ({} tracks have one): exact {}, exact or relative {}, \
         within a fifth {}   [{}]",
        total,
        pct(get("exact"), total),
        pct(get("exact") + get("relative"), total),
        pct(get("exact") + get("relative") + get("fifth"), total),
        keys.iter()
            .map(|(k, v)| format!("{k} {v}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    if let Some(p) = &a.csv {
        println!("\nwrote {}", p.display());
    }
    Ok(())
}
