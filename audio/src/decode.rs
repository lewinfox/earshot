//! Whole-file decoding to stereo f32 with symphonia, from wherever the audio is
//! ([`Location`]).
//!
//! The spike decodes a whole track into memory (~10 MB per stereo minute at 44.1 kHz).
//! The real engine will stream with a cache; see docs/design.md.

use std::io::{Read, Seek, SeekFrom};

use crate::media::{Location, Media};

use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
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

/// [`Media`] as symphonia reads it.
struct Source(Box<dyn Media>);

impl Read for Source {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

impl Seek for Source {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.0.seek(pos)
    }
}

impl MediaSource for Source {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        self.0.len()
    }
}

fn open(at: &Location) -> Result<Box<dyn FormatReader>, String> {
    let media = at.open()?;
    let mss = MediaSourceStream::new(Box::new(Source(media)), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = at.extension() {
        hint.with_extension(&ext);
    }
    // Gapless trimming (encoder delay and padding) is on by default in the decoder options.
    let fmt_opts = FormatOptions::default();
    symphonia::default::get_probe()
        .probe(&hint, mss, fmt_opts, MetadataOptions::default())
        .map_err(|e| format!("{at}: {e}"))
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

/// Whether the track's audio is stored losslessly (PCM, FLAC, ALAC, WavPack…), by its
/// codec, not its file name: an `.m4a` can be either.
pub fn is_lossless(at: &Location) -> Result<bool, String> {
    let format = open(at)?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or("no audio track")?;
    let codec = track
        .codec_params
        .as_ref()
        .and_then(|c| c.audio())
        .ok_or("no audio codec parameters")?
        .codec;
    use symphonia::core::codecs::audio::well_known::*;
    // symphonia numbers uncompressed PCM below the lossy codecs (Vorbis first).
    let pcm = (CODEC_ID_PCM_S32LE..CODEC_ID_VORBIS).contains(&codec);
    let compressed = [
        CODEC_ID_FLAC,
        CODEC_ID_ALAC,
        CODEC_ID_WAVPACK,
        CODEC_ID_MONKEYS_AUDIO,
        CODEC_ID_TTA,
    ];
    Ok(pcm || compressed.contains(&codec))
}

/// Read tags without decoding audio.
pub fn read_tags(at: &Location) -> Result<TrackTags, String> {
    let mut format = open(at)?;
    Ok(tags_of(&mut format))
}

/// The embedded cover picture (front cover if marked, else the first one), still
/// encoded (usually JPEG or PNG). Doesn't decode audio.
pub fn read_cover(at: &Location) -> Result<Option<Vec<u8>>, String> {
    use symphonia::core::meta::StandardVisualKey;
    let mut format = open(at)?;
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

/// Decode a whole track to stereo f32. Mono is duplicated, extra channels are dropped.
pub fn decode(at: &Location) -> Result<(TrackAudio, TrackTags), String> {
    let mut format = open(at)?;
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

#[cfg(test)]
mod lossless_tests {
    use super::*;

    #[test]
    fn lossless_by_codec() {
        let dir = std::env::temp_dir().join(format!("earshot-lossless-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A tiny 16-bit PCM WAV.
        let wav = dir.join("a.wav");
        let mut b = Vec::new();
        let data: Vec<u8> = vec![0; 400];
        b.extend(b"RIFF");
        b.extend((36 + data.len() as u32).to_le_bytes());
        b.extend(b"WAVEfmt ");
        b.extend(16u32.to_le_bytes());
        b.extend(1u16.to_le_bytes());
        b.extend(2u16.to_le_bytes());
        b.extend(44_100u32.to_le_bytes());
        b.extend((44_100u32 * 4).to_le_bytes());
        b.extend(4u16.to_le_bytes());
        b.extend(16u16.to_le_bytes());
        b.extend(b"data");
        b.extend((data.len() as u32).to_le_bytes());
        b.extend(&data);
        std::fs::write(&wav, b).unwrap();
        assert_eq!(is_lossless(&Location::File(wav)), Ok(true));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A 16-bit stereo AIFF at 44.1 kHz holding `frames` frames; the left channel ramps
    /// up from zero and the right is its negative.
    fn aiff(frames: u16) -> Vec<u8> {
        let mut data = Vec::new();
        for i in 0..frames as i16 {
            data.extend((i * 100).to_be_bytes());
            data.extend((-i * 100).to_be_bytes());
        }
        let mut b = Vec::new();
        b.extend(b"FORM");
        b.extend((4 + 8 + 18 + 8 + 8 + data.len() as u32).to_be_bytes());
        b.extend(b"AIFFCOMM");
        b.extend(18u32.to_be_bytes());
        b.extend(2u16.to_be_bytes());
        b.extend((frames as u32).to_be_bytes());
        b.extend(16u16.to_be_bytes());
        // 44100 as an 80-bit extended float.
        b.extend([0x40, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0]);
        b.extend(b"SSND");
        b.extend((8 + data.len() as u32).to_be_bytes());
        b.extend(0u32.to_be_bytes()); // offset
        b.extend(0u32.to_be_bytes()); // block size
        b.extend(&data);
        b
    }

    #[test]
    fn decodes_aiff() {
        let dir = std::env::temp_dir().join(format!("earshot-aiff-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Both spellings the library scan accepts.
        for name in ["a.aiff", "a.aif"] {
            let path = dir.join(name);
            std::fs::write(&path, aiff(100)).unwrap();
            let at = Location::File(path);
            assert_eq!(is_lossless(&at), Ok(true));
            let tags = read_tags(&at).unwrap();
            assert_eq!(tags.duration, Some(100.0 / 44_100.0));
            let (audio, _) = decode(&at).unwrap();
            assert_eq!(audio.sample_rate, 44_100);
            assert_eq!(audio.frames(), 100);
            let expect = 300.0 / 32_768.0;
            assert!((audio.samples[6] - expect).abs() < 1e-6);
            assert!((audio.samples[7] + expect).abs() < 1e-6);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
