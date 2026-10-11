//! Run candidate A on decoded WAVs and print one JSON line per file.
//! usage: fastgrid <id.wav> [...]
//! The time excludes reading the WAV (the app already has the decoded audio).
use std::path::Path;
use std::time::Instant;

fn main() {
    for arg in std::env::args().skip(1) {
        let p = Path::new(&arg);
        let (y, sr) = beatgrid_research::read_wav(p).expect("read wav");
        let t0 = Instant::now();
        let g = beatgrid_research::analyse(&y, sr);
        let secs = t0.elapsed().as_secs_f64();
        let id = p.file_stem().unwrap().to_string_lossy();
        println!(
            "{{\"id\": {id}, \"bpm\": {}, \"phase\": {}, \"win_resid_p90_ms\": {}, \"win_resid_max_ms\": {}, \"good_frac\": {}, \"coarse_bpm\": {}, \"seconds\": {secs}}}",
            g.bpm, g.phase, g.win_resid_p90_ms, g.win_resid_max_ms, g.good_frac, g.coarse_bpm
        );
    }
}
