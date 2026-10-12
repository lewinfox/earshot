//! Decode files exactly as the app does (engine::decode::decode_file) and write the
//! same 32-bit float mono WAV the analysis queue sends the worker
//! (gui/src/core/analysis.rs write_mono_wav).
//!
//! usage: decode <in> <out.wav> [<in> <out.wav> ...]   (skips outputs that exist)
use std::io::{BufWriter, Write};
use std::path::Path;

use earshot_engine::decode::{TrackAudio, decode_file};

fn write_mono_wav(path: &Path, audio: &TrackAudio) -> std::io::Result<()> {
    let mut w = BufWriter::new(std::fs::File::create(path)?);
    let n = audio.frames() as u32;
    let sr = audio.sample_rate;
    w.write_all(b"RIFF")?;
    w.write_all(&(36 + n * 4).to_le_bytes())?;
    w.write_all(b"WAVEfmt ")?;
    w.write_all(&16u32.to_le_bytes())?;
    w.write_all(&3u16.to_le_bytes())?;
    w.write_all(&1u16.to_le_bytes())?;
    w.write_all(&sr.to_le_bytes())?;
    w.write_all(&(sr * 4).to_le_bytes())?;
    w.write_all(&4u16.to_le_bytes())?;
    w.write_all(&32u16.to_le_bytes())?;
    w.write_all(b"data")?;
    w.write_all(&(n * 4).to_le_bytes())?;
    for f in audio.samples.chunks_exact(2) {
        w.write_all(&(0.5 * (f[0] + f[1])).to_le_bytes())?;
    }
    w.flush()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    for pair in args.chunks(2) {
        let (src, dst) = (Path::new(&pair[0]), Path::new(&pair[1]));
        if dst.exists() {
            continue;
        }
        match decode_file(src) {
            Ok((audio, _)) => {
                let tmp = dst.with_extension("part");
                write_mono_wav(&tmp, &audio).expect("write");
                std::fs::rename(&tmp, dst).expect("rename");
                println!(
                    "{}\t{}\t{:.3}",
                    dst.display(),
                    audio.sample_rate,
                    audio.duration()
                );
            }
            Err(e) => eprintln!("{}: {e}", src.display()),
        }
    }
}
