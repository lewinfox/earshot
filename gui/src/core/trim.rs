//! Prepare's Trim tool: cut a track down to the part between two points and save it as
//! a new file. ffmpeg does the cut (sample-accurate, re-encoding to the format the new
//! file's name asks for), with a few milliseconds' fade at each cut so it doesn't click,
//! and keeps the original's tags.
//!
//! 1. [`TrimCmd::SaveAs`] asks where to save (a save dialog, on its own thread).
//! 2. [`TrimCmd::Save`] cuts the file there, then [`TrimEvent::Saved`] (or `Failed`).
//!    A file saved in a library folder is scanned in.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::track::TrackId;

/// Each cut's fade, in seconds: short enough not to be heard as a fade.
const FADE: f64 = 0.005;

#[derive(Debug, Clone)]
pub enum TrimCmd {
    /// Ask where to save `track` cut to `start..end` s.
    SaveAs {
        track: TrackId,
        start: f64,
        end: f64,
    },
    /// Cut `track` to `start..end` s and save it at `to`.
    Save {
        track: TrackId,
        start: f64,
        end: f64,
        to: PathBuf,
    },
}

#[derive(Debug, Clone)]
pub enum TrimEvent {
    Saved {
        track: TrackId,
        path: PathBuf,
        /// It went into a library folder, and has been scanned in.
        in_library: bool,
    },
    Failed {
        track: TrackId,
        error: String,
    },
}

/// What a cut needs to know about the original.
#[derive(Debug, Clone)]
pub struct Source {
    pub path: PathBuf,
    pub title: String,
    /// Its length in seconds (no fade at a cut at either end).
    pub duration: f64,
}

/// The name the save dialog offers: "Name (trimmed).ext".
pub fn default_name(from: &Path) -> String {
    let stem = from.file_stem().unwrap_or_default().to_string_lossy();
    match from.extension() {
        Some(e) => format!("{stem} (trimmed).{}", e.to_string_lossy()),
        None => format!("{stem} (trimmed)"),
    }
}

/// Cut `src` to `start..end` s into `to`, written beside it under another name first
/// and then renamed, so a half-written file is never seen there.
pub fn cut(src: &Source, start: f64, end: f64, to: &Path) -> Result<(), String> {
    if end - start < 0.1 {
        return Err("the part to keep is too short".into());
    }
    if same_file(&src.path, to) {
        return Err("choose another name: this is the original".into());
    }
    let part = partial(to);
    let out = Command::new("ffmpeg")
        .args(args(src, start, end, &part))
        .output()
        .map_err(|e| format!("ffmpeg: {e} (is it installed?)"))?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&part);
        let err = String::from_utf8_lossy(&out.stderr);
        let last = err.lines().rev().find(|l| !l.trim().is_empty());
        return Err(format!("ffmpeg: {}", last.unwrap_or("failed")));
    }
    std::fs::rename(&part, to).map_err(|e| {
        let _ = std::fs::remove_file(&part);
        format!("{}: {e}", to.display())
    })
}

/// Where the cut is written before it's renamed to `to`: hidden, with `to`'s extension
/// (ffmpeg picks the format by it).
fn partial(to: &Path) -> PathBuf {
    let name = to.file_name().unwrap_or_default().to_string_lossy();
    let ext = to
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    to.with_file_name(format!(".{name}.trimming{ext}"))
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// ffmpeg's arguments for the cut.
fn args(src: &Source, start: f64, end: f64, to: &Path) -> Vec<String> {
    let ext = to
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let len = end - start;
    let mut filter = format!("atrim=start={start:.6}:end={end:.6},asetpts=PTS-STARTPTS");
    if start > 0.0 {
        filter += &format!(",afade=t=in:d={FADE}");
    }
    if end < src.duration {
        filter += &format!(",afade=t=out:st={:.6}:d={FADE}", (len - FADE).max(0.0));
    }
    let mut a: Vec<String> = ["-hide_banner", "-nostdin", "-y", "-i"]
        .map(String::from)
        .to_vec();
    a.push(src.path.to_string_lossy().into_owned());
    a.extend(["-map".into(), "0:a:0".into()]);
    // Cover art rides along where the format holds it.
    let cover = matches!(ext.as_str(), "mp3" | "m4a" | "mp4" | "aac");
    if cover {
        a.extend(
            [
                "-map",
                "0:v?",
                "-c:v",
                "copy",
                "-disposition:v",
                "attached_pic",
            ]
            .map(String::from),
        );
    }
    a.extend(["-af".into(), filter]);
    let codec: &[&str] = match ext.as_str() {
        "mp3" => &["-c:a", "libmp3lame", "-b:a", "320k", "-id3v2_version", "3"],
        "m4a" | "mp4" | "aac" => &["-c:a", "aac", "-b:a", "256k"],
        "wav" => &["-c:a", "pcm_s24le"],
        "aif" | "aiff" => &["-c:a", "pcm_s24be"],
        _ => &[],
    };
    a.extend(codec.iter().map(|s| s.to_string()));
    // The original's tags, but not its audio hash (this is different audio), and a
    // title that tells the two apart.
    a.extend(["-map_metadata", "0", "-metadata", "AUDIO_SHA256="].map(String::from));
    if !src.title.is_empty() {
        a.extend(["-metadata".into(), format!("title={} (trimmed)", src.title)]);
    }
    a.push(to.to_string_lossy().into_owned());
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src() -> Source {
        Source {
            path: "/music/Song.mp3".into(),
            title: "Song".into(),
            duration: 200.0,
        }
    }

    #[test]
    fn default_name_keeps_the_extension() {
        assert_eq!(default_name(Path::new("/m/Song.mp3")), "Song (trimmed).mp3");
        assert_eq!(default_name(Path::new("/m/Song")), "Song (trimmed)");
    }

    #[test]
    fn partial_is_hidden_beside_with_the_same_extension() {
        assert_eq!(
            partial(Path::new("/m/Song (trimmed).flac")),
            Path::new("/m/.Song (trimmed).flac.trimming.flac")
        );
    }

    #[test]
    fn fades_only_at_cuts_inside_the_track() {
        let a = args(&src(), 10.0, 70.0, Path::new("/o/x.mp3"));
        let f = &a[a.iter().position(|s| s == "-af").unwrap() + 1];
        assert!(f.starts_with("atrim=start=10.000000:end=70.000000"), "{f}");
        assert!(f.contains("afade=t=in"), "{f}");
        assert!(f.contains("afade=t=out:st=59.995000"), "{f}");
        assert!(a.contains(&"libmp3lame".to_string()));
        assert!(a.contains(&"title=Song (trimmed)".to_string()));
        assert!(a.contains(&"AUDIO_SHA256=".to_string()));

        let a = args(&src(), 0.0, 200.0, Path::new("/o/x.wav"));
        let f = &a[a.iter().position(|s| s == "-af").unwrap() + 1];
        assert!(!f.contains("afade"), "{f}");
        assert!(a.contains(&"pcm_s24le".to_string()));
        assert!(!a.contains(&"0:v?".to_string()));
    }
}
