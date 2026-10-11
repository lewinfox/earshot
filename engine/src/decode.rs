//! Whole-file decoding to stereo f32 with symphonia.
//!
//! The spike decodes a whole track into memory (~10 MB per stereo minute at 44.1 kHz).
//! The real engine will stream with a cache; see docs/design.md.

use std::fs::File;
use std::path::Path;

use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, StandardTag};

/// Decoded audio, stereo interleaved.
pub struct TrackAudio {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

impl TrackAudio {
    pub fn frames(&self) -> usize {
        self.samples.len() / 2
    }

    pub fn duration(&self) -> f64 {
        self.frames() as f64 / self.sample_rate as f64
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct TrackTags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub bpm: Option<f64>,
    pub key: Option<String>,
    pub duration: Option<f64>,
}

fn open(path: &Path) -> Result<Box<dyn FormatReader>, String> {
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    // Gapless trimming (encoder delay and padding) is on by default in the decoder options.
    let fmt_opts = FormatOptions::default();
    symphonia::default::get_probe()
        .probe(&hint, mss, fmt_opts, MetadataOptions::default())
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn tags_of(format: &mut Box<dyn FormatReader>) -> TrackTags {
    let mut tags = TrackTags::default();
    if let Some(track) = format.default_track(TrackType::Audio) {
        let rate = track
            .codec_params
            .as_ref()
            .and_then(|c| c.audio())
            .and_then(|a| a.sample_rate);
        if let (Some(n), Some(rate)) = (track.num_frames, rate) {
            tags.duration = Some(n as f64 / rate as f64);
        }
    }
    let mut meta = format.metadata();
    if let Some(rev) = meta.skip_to_latest() {
        for tag in &rev.media.tags {
            match &tag.std {
                Some(StandardTag::TrackTitle(s)) => tags.title = Some(s.to_string()),
                Some(StandardTag::Artist(s)) => tags.artist = Some(s.to_string()),
                Some(StandardTag::Bpm(b)) => tags.bpm = Some(*b as f64),
                Some(StandardTag::InitialKey(s)) => tags.key = Some(s.to_string()),
                _ => {}
            }
        }
    }
    tags
}

/// Read tags without decoding audio.
pub fn read_tags(path: &Path) -> Result<TrackTags, String> {
    let mut format = open(path)?;
    Ok(tags_of(&mut format))
}

/// The embedded cover picture (front cover if marked, else the first one), still
/// encoded (usually JPEG or PNG). Doesn't decode audio.
pub fn read_cover(path: &Path) -> Result<Option<Vec<u8>>, String> {
    use symphonia::core::meta::StandardVisualKey;
    let mut format = open(path)?;
    let mut meta = format.metadata();
    let Some(rev) = meta.skip_to_latest() else {
        return Ok(None);
    };
    let visuals = &rev.media.visuals;
    Ok(visuals
        .iter()
        .find(|v| v.usage == Some(StandardVisualKey::FrontCover))
        .or(visuals.first())
        .map(|v| v.data.to_vec()))
}

/// Decode a whole file to stereo f32. Mono is duplicated, extra channels are dropped.
pub fn decode_file(path: &Path) -> Result<(TrackAudio, TrackTags), String> {
    let mut format = open(path)?;
    let mut tags = tags_of(&mut format);
    let track = format
        .default_track(TrackType::Audio)
        .ok_or("no audio track")?;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|c| c.audio())
        .ok_or("no audio codec parameters")?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|e| e.to_string())?;
    let track_id = track.id;

    let mut out: Vec<f32> = Vec::new();
    let mut scratch: Vec<f32> = Vec::new();
    let mut sample_rate = params.sample_rate.unwrap_or(44_100);
    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(Error::ResetRequired) => break,
            Err(e) => return Err(e.to_string()),
        };
        if packet.track_id != track_id {
            continue;
        }
        let buf = match decoder.decode(&packet) {
            Ok(b) => b,
            Err(Error::DecodeError(_)) => continue,
            Err(e) => return Err(e.to_string()),
        };
        let channels = buf.spec().channels().count().max(1);
        sample_rate = buf.spec().rate();
        scratch.resize(buf.samples_interleaved(), 0.0);
        buf.copy_to_slice_interleaved(&mut scratch);
        out.reserve(scratch.len() / channels * 2);
        for frame in scratch.chunks_exact(channels) {
            let l = frame[0];
            let r = if channels > 1 { frame[1] } else { l };
            out.push(l);
            out.push(r);
        }
    }
    let audio = TrackAudio {
        samples: out,
        sample_rate,
    };
    tags.duration = Some(audio.duration());
    Ok((audio, tags))
}
