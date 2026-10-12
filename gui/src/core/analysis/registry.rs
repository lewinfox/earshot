//! The analysers: what each works out, where it runs, and its version.
//!
//! Each result is saved per analyser in `analysis_results`, with the analyser's
//! version and the hash of the audio it ran on (`files.content_hash`). A result is
//! stale when the analyser has a newer version or the track's audio has changed, so
//! "re-analyse stale tracks" is one query per analyser (`Reader::stale_tracks`).

use crate::core::store::WAVEFORM_VERSION;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AnalyserId {
    /// The scrolling waveform and its thumbnail (`blobs`).
    Waveform,
    /// LUFS and peak (`track_features` lufs, peak): auto-gain.
    Loudness,
    /// The placeholder energy (`track_features` energy): suggestions.
    Energy,
    /// The beatgrid (a `beatgrids` row) and the tempo range (bpm_min, bpm_max, drifts).
    Grid,
}

/// Where an analyser runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunsIn {
    Rust,
    /// The Python worker (`worker/`), which reports the same version in its hello.
    Worker,
}

/// The audio an analyser wants. Every analyser of a track works from one decode of
/// the file; a worker analyser gets a mono WAV made from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioNeeds {
    /// `None`: the file's own sample rate.
    pub rate: Option<u32>,
    pub channels: u8,
}

#[derive(Clone, Copy, Debug)]
pub struct AnalyserSpec {
    pub id: AnalyserId,
    /// Bump when the results change, so older ones count as stale.
    pub version: u32,
    pub runs_in: RunsIn,
    // Every analyser so far takes the file's own rate and nothing reads another's
    // results; these are for the ones to come (key, CLAP embeddings at 48 kHz).
    #[allow(dead_code)]
    pub needs: AudioNeeds,
    /// Analysers whose results this one reads; [`REGISTRY`] lists them first.
    #[allow(dead_code)]
    pub depends_on: &'static [AnalyserId],
    /// The `track_features` it writes. Saving all of them, from anywhere (a deck load
    /// measures loudness too), counts as a result of this analyser.
    pub features: &'static [&'static str],
}

const NATIVE_STEREO: AudioNeeds = AudioNeeds {
    rate: None,
    channels: 2,
};

/// Every analyser, each after the ones it depends on.
pub const REGISTRY: &[AnalyserSpec] = &[
    AnalyserSpec {
        id: AnalyserId::Waveform,
        version: WAVEFORM_VERSION as u32,
        runs_in: RunsIn::Rust,
        needs: NATIVE_STEREO,
        depends_on: &[],
        features: &[],
    },
    AnalyserSpec {
        id: AnalyserId::Loudness,
        version: 1,
        runs_in: RunsIn::Rust,
        needs: NATIVE_STEREO,
        depends_on: &[],
        features: &["lufs", "peak"],
    },
    AnalyserSpec {
        id: AnalyserId::Energy,
        version: 1,
        runs_in: RunsIn::Rust,
        needs: NATIVE_STEREO,
        depends_on: &[],
        features: &["energy"],
    },
    AnalyserSpec {
        id: AnalyserId::Grid,
        // As `ANALYSERS["grid"]` in worker/earshot_worker/__main__.py.
        version: 2,
        runs_in: RunsIn::Worker,
        needs: AudioNeeds {
            rate: None,
            channels: 1,
        },
        depends_on: &[],
        features: &["bpm_min", "bpm_max", "drifts"],
    },
];

impl AnalyserId {
    pub const ALL: [AnalyserId; 4] = [
        AnalyserId::Waveform,
        AnalyserId::Loudness,
        AnalyserId::Energy,
        AnalyserId::Grid,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            AnalyserId::Waveform => "waveform",
            AnalyserId::Loudness => "loudness",
            AnalyserId::Energy => "energy",
            AnalyserId::Grid => "grid",
        }
    }

    pub fn parse(s: &str) -> Option<AnalyserId> {
        AnalyserId::ALL.into_iter().find(|a| a.as_str() == s)
    }

    pub fn spec(self) -> &'static AnalyserSpec {
        REGISTRY
            .iter()
            .find(|s| s.id == self)
            .expect("every analyser is registered")
    }
}

/// The analysers whose features are all among `names`.
pub fn measured_by(names: &[&str]) -> impl Iterator<Item = &'static AnalyserSpec> {
    REGISTRY
        .iter()
        .filter(move |s| !s.features.is_empty() && s.features.iter().all(|f| names.contains(f)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_complete_and_in_dependency_order() {
        let ids: Vec<AnalyserId> = REGISTRY.iter().map(|s| s.id).collect();
        assert_eq!(ids, AnalyserId::ALL);
        for (i, s) in REGISTRY.iter().enumerate() {
            for d in s.depends_on {
                assert!(ids[..i].contains(d), "{:?} needs {d:?} first", s.id);
            }
            assert_eq!(AnalyserId::parse(s.id.as_str()), Some(s.id));
        }
    }

    #[test]
    fn features_name_their_analyser() {
        let found: Vec<AnalyserId> = measured_by(&["energy", "lufs", "peak"])
            .map(|s| s.id)
            .collect();
        assert_eq!(found, [AnalyserId::Loudness, AnalyserId::Energy]);
        assert_eq!(measured_by(&["lufs"]).count(), 0); // peak missing
    }
}
