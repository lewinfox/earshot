//! earshot's offline audio code: decoding, beatgrids, and the measurements made from a
//! whole track (waveform, loudness, energy). None of it runs on the audio thread,
//! except the beatgrid lookups (`beat_at`, `time_at`, `quantize`), which don't allocate.

pub mod beatgrid;
pub mod decode;
pub mod demo;
pub mod energy;
pub mod loudness;
pub mod waveform;
