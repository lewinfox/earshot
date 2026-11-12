//! The Tempo lock tab's half in the shell: the tool's events, and its A/B on Prepare's
//! player.
//!
//! A track charted by the tool goes into Prepare's player. The straightened file is
//! an outside track (a file played without being in the library, with its steady
//! grid), so the player can load it too; the A/B swaps one for the other at the same
//! place in the music.

use std::path::Path;

use super::*;
use crate::core::external::{ExtTrack, Marker};
use crate::core::tempo_lock::{Straightened, TempoLockEvent, Version};
use crate::core::track::TrackId;
use crate::ui::panels::deck::wave::Layer;
use crate::ui::panels::tempo_lock::DetectedBeats;

/// The outside-track "library" the straightened files are registered under (real
/// sources are database rows, numbered from 1).
const SOURCE: crate::core::sources::SourceId = 0;


#[derive(Default)]
pub(super) struct Listener {
    /// The straightened file, as an outside track.
    straight: Option<TrackId>,
    /// A version loading into the player: where to put it once it's in, and whether
    /// to play on.
    pending: Option<Pending>,
}

struct Pending {
    ticket: u64,
    at: f64,
    play: bool,
}

impl EarshotApp {
    /// News from the tool, for the tab. A newly charted track goes into the player;
    /// a new straightened file replaces the old one (and the original plays instead,
    /// if the old one was playing).
    pub(super) fn tempo_lock_event(&mut self, e: TempoLockEvent) {
        match &e {
            TempoLockEvent::Analysed { track, .. } => {
                self.listener.straight = None;
                if self.player_track() != Some(*track) {
                    self.listen_load(*track, 0.0, false);
                }
            }
            TempoLockEvent::Straightened { result, .. } => {
                let old = self.listener.straight;
                let id = self.register_straightened(result);
                self.listener.straight = id;
                if old.is_some() && self.player_track() == old {
                    let at = self.panels.tempo_lock.cursor();
                    self.listen(Version::Original, at);
                }
            }
            _ => {}
        }
        self.panels.tempo_lock.on_event(e);
    }

    /// Once a frame: tell the tab which version is in the player, if either.
    pub(super) fn sync_tempo_lock(&mut self) {
        let id = self.player_track();
        let tab = &mut self.panels.tempo_lock;
        tab.player = match id {
            Some(i) if Some(i) == tab.track => Some(Version::Original),
            Some(i) if Some(i) == self.listener.straight => Some(Version::Straightened),
            _ => None,
        };
        tab.loading = self.listener.pending.is_some();
        // Its beats over the player's waveform, while one of its versions is in.
        // Only on Prepare's Tempo lock tab: elsewhere the deck is just a deck.
        let shown = self.prefs.mode == crate::core::view::Mode::Prepare
            && self.prefs.prepare.tool == crate::core::view::Tool::TempoLock;
        let layer = tab.player.zip(tab.chart.clone()).filter(|_| shown).map(|(version, chart)| {
            Arc::new(DetectedBeats {
                version,
                chart,
                straightened: tab.straightened.clone(),
            }) as Arc<dyn Layer>
        });
        self.panels.decks[self.prefs.target().index()].set_overlay("tempo-lock", layer);
    }

    /// Put `version` in the player at `at` s (its own time), or just move there if
    /// it's in already.
    pub(super) fn listen(&mut self, version: Version, at: f64) {
        let id = match version {
            Version::Original => self.panels.tempo_lock.track,
            Version::Straightened => self.listener.straight,
        };
        let Some(id) = id else {
            return;
        };
        if self.player_track() == Some(id) {
            self.listener.pending = None;
            self.dispatch((self.prefs.target(), EngineCmd::Seek(at)).into());
        } else {
            let playing = self.player_snap().is_some_and(|s| s.playing);
            self.listen_load(id, at, playing);
        }
    }

    /// A load into the player is done: move to where it was asked for.
    pub(super) fn tempo_lock_loaded(&mut self, ticket: u64, ok: bool) {
        if self
            .listener
            .pending
            .as_ref()
            .is_none_or(|p| p.ticket != ticket)
        {
            return;
        }
        let p = self.listener.pending.take().unwrap();
        if ok {
            self.dispatch((self.prefs.target(), EngineCmd::Seek(p.at)).into());
            if p.play {
                self.dispatch((self.prefs.target(), EngineCmd::Play(Set::On)).into());
            }
        }
    }

    fn listen_load(&mut self, id: TrackId, at: f64, play: bool) {
        let ticket = self.load(Some(self.prefs.target()), id);
        // Not from a library list: there's no "next track" to it.
        self.loaded_from[self.prefs.target().index()] = None;
        self.listener.pending = Some(Pending { ticket, at, play });
    }

    fn player_snap(&self) -> Option<earshot_engine::DeckSnap> {
        self.frame.snap.as_ref().map(|s| *s.deck(self.prefs.target()))
    }

    fn player_track(&self) -> Option<TrackId> {
        self.player_snap().and_then(|s| s.track_id)
    }

    /// The straightened file as an outside track, with its steady grid.
    fn register_straightened(&mut self, s: &Straightened) -> Option<TrackId> {
        let chart = self.panels.tempo_lock.chart.clone();
        let first = s.grid.target_beats.first().copied()?;
        // Its place in the bar, from the downbeats found (when a detected beat is a beat).
        let beat = chart
            .as_ref()
            .filter(|_| s.level == 1.0)
            .and_then(|c| c.analysis.is_downbeat.iter().position(|d| *d))
            .map_or(1, |i| ((4 - i % 4) % 4) as u8 + 1);
        let title = Path::new(&s.output)
            .file_stem()
            .map(|t| t.to_string_lossy().into_owned())
            .unwrap_or_default();
        let t = ExtTrack {
            id: s.output.to_string_lossy().into_owned(),
            location: s.output.to_string_lossy().into_owned(),
            file: Some(s.output.clone()),
            title,
            artist: String::new(),
            album: String::new(),
            genre: String::new(),
            comment: String::new(),
            bpm: Some(s.bpm),
            key: None,
            rating: 0,
            colour: None,
            duration_s: Some(s.duration),
            cues: vec![],
            grid: vec![Marker {
                start_s: first,
                bpm: s.bpm,
                beat,
            }],
        };
        let outside = self.lib.store.outside.clone();
        let id = outside.register(SOURCE, &t, false)?;
        let row = outside.get(id)?.lib_track();
        self.index.put_outside(vec![row]);
        Some(id)
    }
}
