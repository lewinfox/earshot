//! Stems Phase 0 measurements (issue #2), engine side.
//!
//! 1. Keylock's delay: how late a stem mute would be heard if stems are mixed
//!    before the pitch shifter.
//! 2. The cost of one shifter per deck against one per stem (4), as a share of one
//!    CPU core.
//! 3. With `--wav OUT`: write the engine's own decode of the track as a float WAV,
//!    for `research/stems/separate_bench.py`.
//!
//! cargo run --release -p earshot-engine --example stems_bench -- TRACK [--wav OUT]

use std::io::Write;
use std::time::Instant;

use earshot_engine::decode;
use earshot_engine::keylock::Keylock;
use earshot_engine::media::Location;

const BLOCK: usize = 256;
const SPEED: f64 = 1.06;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let track = args.first().expect("usage: stems_bench TRACK [--wav OUT]");
    let wav_out = args
        .iter()
        .position(|a| a == "--wav")
        .map(|i| args[i + 1].clone());

    let t = Instant::now();
    let (audio, _) = decode::decode(&Location::File(track.into())).expect("decode");
    let sr = audio.sample_rate;
    println!(
        "decoded {:.1} s at {} Hz in {:.2} s",
        audio.duration(),
        sr,
        t.elapsed().as_secs_f64()
    );

    // 1. Delay.
    let k = Keylock::new(sr, BLOCK);
    let delay_s = k.lead(1.0 / sr as f64);
    println!(
        "keylock delay: {} frames = {:.0} ms",
        (delay_s * sr as f64).round(),
        delay_s * 1000.0
    );

    // 2. Cost. 20 s of real audio from a minute in (or the start of a short track).
    let start = (60 * sr as usize).min(audio.frames().saturating_sub(20 * sr as usize));
    let n = (20 * sr as usize).min(audio.frames() - start);
    let clip = &audio.samples[start * 2..(start + n) * 2];
    for shifters in [1usize, 4] {
        let mut ks: Vec<Keylock> = (0..shifters).map(|_| Keylock::new(sr, BLOCK)).collect();
        let mut buf = vec![0.0f32; BLOCK * 2];
        let t = Instant::now();
        let mut blocks = 0usize;
        for chunk in clip.chunks_exact(BLOCK * 2) {
            for k in ks.iter_mut() {
                buf.copy_from_slice(chunk);
                k.process(&mut buf, SPEED, 0.0);
            }
            blocks += 1;
        }
        let used = t.elapsed().as_secs_f64();
        let audio_s = (blocks * BLOCK) as f64 / sr as f64;
        println!(
            "{shifters} shifter(s): {:.1}% of one core per deck ({:.1} µs per {BLOCK}-frame block)",
            100.0 * used / audio_s,
            1e6 * used / blocks as f64
        );
    }

    // 3. The engine's decode, for the separation test.
    if let Some(out) = wav_out {
        write_f32_wav(&out, &audio.samples, sr).expect("write wav");
        println!("wrote {out}");
    }
}

/// Stereo IEEE-float WAV.
fn write_f32_wav(path: &str, samples: &[f32], sr: u32) -> std::io::Result<()> {
    let data_len = (samples.len() * 4) as u32;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&3u16.to_le_bytes())?; // IEEE float
    f.write_all(&2u16.to_le_bytes())?;
    f.write_all(&sr.to_le_bytes())?;
    f.write_all(&(sr * 8).to_le_bytes())?;
    f.write_all(&8u16.to_le_bytes())?;
    f.write_all(&32u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    for s in samples {
        f.write_all(&s.to_le_bytes())?;
    }
    Ok(())
}
