//! The Tempo lock tab's half in the shell: the tool's events, and its A/B player.
//!
//! Listening uses the headphones' preview player: the tab's audio replaces a library
//! preview (and the other way round). Each version is decoded once, on a thread, and
//! kept while the tool is on that track. The click track is mixed into a copy of the
//! version playing (the engine has no metronome), made when it's switched on.

use std::path::PathBuf;

use earshot_engine::decode::{self, TrackAudio};
use earshot_engine::loudness;

use super::*;
use crate::core::tempo_lock::{ListenCmd, TempoLockEvent, Version, version_beats, with_clicks};
use crate::core::track::TrackId;
use crate::ui::panels::tempo_lock::ListenStatus;

/// What's playing: a version, of which file, with clicks or not.
#[derive(Clone, Debug, PartialEq)]
struct Key {
    version: Version,
    file: PathBuf,
    clicks: bool,
}

#[derive(Clone, Default)]
pub(super) struct Listener {
    st: Arc<Mutex<ListenState>>,
}

#[derive(Default)]
struct ListenState {
    /// Clean decodes, by file (the original and the straightened MP3).
    decoded: Vec<(PathBuf, Arc<TrackAudio>)>,
    /// The version with clicks, made when last asked for.
    clicked: Option<(Key, Arc<TrackAudio>)>,
    /// The track's auto-gain, used for both versions.
    gain: Option<(TrackId, f64)>,
    /// In the preview player now, and its length (s).
    loaded: Option<(Key, f64)>,
    decoding: bool,
    error: Option<String>,
}

impl EarshotApp {
    /// News from the tool, for the tab. A new track or a new straightened file stops
    /// the tab's playback of the old one.
    pub(super) fn tempo_lock_event(&mut self, e: TempoLockEvent) {
        let stale = |k: &Key| match &e {
            TempoLockEvent::Analysed { .. } => true,
            TempoLockEvent::Progress {
                stage: crate::core::tempo_lock::Stage::Analyse,
                fraction,
                ..
            } => *fraction == 0.0,
            TempoLockEvent::Straightened { .. } => k.version == Version::Straightened,
            _ => false,
        };
        let stop = {
            let mut st = self.listener.st.lock().unwrap();
            let stop = st.loaded.as_ref().is_some_and(|(k, _)| stale(k));
            if matches!(e, TempoLockEvent::Analysed { .. }) {
                // Keep only the decodes of this session's files.
                st.decoded.clear();
                st.clicked = None;
            }
            if stop {
                st.loaded = None;
            }
            stop
        };
        if stop && let Err(e) = player::stop_preview(&self.player) {
            eprintln!("tempo lock: {e}");
        }
        self.panels.tempo_lock.on_event(e);
    }

    /// Once a frame: tell the tab what the preview player has of its own. A library
    /// preview takes the player over.
    pub(super) fn sync_tempo_lock(&mut self) {
        let mut st = self.listener.st.lock().unwrap();
        if self.preview.is_some() {
            st.loaded = None;
        }
        self.panels.tempo_lock.listen = ListenStatus {
            loaded: st.loaded.as_ref().map(|(k, _)| (k.version, k.clicks)),
            decoding: st.decoding,
            error: st.error.clone(),
        };
    }

    pub(super) fn listen(&mut self, cmd: ListenCmd) {
        let ours = self.preview.is_none() && self.listener.st.lock().unwrap().loaded.is_some();
        let result = match cmd {
            ListenCmd::Pause if ours => player::play_preview(&self.player, false),
            ListenCmd::Stop if ours => {
                self.listener.st.lock().unwrap().loaded = None;
                player::stop_preview(&self.player)
            }
            ListenCmd::Pause | ListenCmd::Stop => Ok(()),
            ListenCmd::Play {
                version,
                at,
                clicks,
            } => self.listen_play(version, at, clicks),
        };
        if let Err(e) = result {
            self.listener.st.lock().unwrap().error = Some(e);
        }
    }

    fn listen_play(&mut self, version: Version, at: f64, clicks: bool) -> Result<(), String> {
        let now = self.tempo_lock.current();
        let (Some(track), Some(chart)) = (now.track, now.chart) else {
            return Ok(());
        };
        let file = match (version, &now.straightened) {
            (Version::Original, _) => self
                .lib
                .store
                .reader
                .track_path(track)
                .map_err(|e| e.to_string())?,
            (Version::Straightened, Some(s)) => s.output.clone(),
            (Version::Straightened, None) => return Ok(()),
        };
        let key = Key {
            version,
            file,
            clicks,
        };
        {
            let mut st = self.listener.st.lock().unwrap();
            st.error = None;
            // Already in the player: just move there.
            if self.preview.is_none()
                && let Some((k, duration)) = &st.loaded
                && *k == key
            {
                let at = (at / duration.max(1e-6)).clamp(0.0, 1.0);
                drop(st);
                player::seek_preview(&self.player, at)?;
                return player::play_preview(&self.player, true);
            }
            st.decoding = true;
        }
        // A library preview gives way.
        self.preview = None;
        let (beats, downbeats) = {
            let (b, d) = version_beats(version, &chart, now.straightened.as_deref());
            (b.to_vec(), d.to_vec())
        };
        let seq = player::next_preview(&self.player);
        let (listener, player, store, ctx) = (
            self.listener.clone(),
            self.player.clone(),
            self.lib.store.clone(),
            self.ctx.clone(),
        );
        std::thread::Builder::new()
            .name("earshot-tempo-lock-listen".into())
            .spawn(move || {
                let r = listener.start(&player, &store, seq, track, key, at, &beats, &downbeats);
                let mut st = listener.st.lock().unwrap();
                st.decoding = false;
                if let Err(e) = r {
                    st.error = Some(e);
                }
                drop(st);
                ctx.request_repaint();
            })
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

impl Listener {
    /// Decode (or reuse) the audio for `key`, add the clicks if asked, and start it in
    /// the preview player at `at` s, unless another preview has been asked for since.
    #[allow(clippy::too_many_arguments)]
    fn start(
        &self,
        player: &Player,
        store: &Store,
        seq: u64,
        track: TrackId,
        key: Key,
        at: f64,
        beats: &[f64],
        downbeats: &[bool],
    ) -> Result<(), String> {
        let cached = {
            let st = self.st.lock().unwrap();
            st.decoded
                .iter()
                .find(|(f, _)| *f == key.file)
                .map(|(_, a)| a.clone())
        };
        let clean = match cached {
            Some(a) => a,
            None => {
                let a = Arc::new(decode::decode_file(&key.file)?.0);
                let mut st = self.st.lock().unwrap();
                st.decoded.retain(|(f, _)| *f != key.file);
                // The original and one straightened file at most.
                if st.decoded.len() >= 2 {
                    st.decoded.remove(0);
                }
                st.decoded.push((key.file.clone(), a.clone()));
                a
            }
        };
        let audio = if key.clicks {
            let cached = {
                let st = self.st.lock().unwrap();
                st.clicked
                    .as_ref()
                    .filter(|(k, _)| *k == key)
                    .map(|(_, a)| a.clone())
            };
            match cached {
                Some(a) => a,
                None => {
                    let a = Arc::new(with_clicks(&clean, beats, downbeats));
                    self.st.lock().unwrap().clicked = Some((key.clone(), a.clone()));
                    a
                }
            }
        } else {
            clean.clone()
        };
        let known = self
            .st
            .lock()
            .unwrap()
            .gain
            .filter(|(t, _)| *t == track)
            .map(|(_, g)| g);
        let gain = match known {
            Some(g) => g,
            None => {
                let g = match store.reader.loudness(track).map_err(|e| e.to_string())? {
                    Some(l) => l.auto_gain(),
                    None => loudness::measure(&clean).auto_gain(),
                };
                self.st.lock().unwrap().gain = Some((track, g));
                g
            }
        };
        let duration = audio.duration();
        let at = at.clamp(0.0, (duration - 0.01).max(0.0));
        if player::preview_audio(player, seq, audio, at, gain)? {
            self.st.lock().unwrap().loaded = Some((key, duration));
        }
        Ok(())
    }
}
