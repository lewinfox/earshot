//! The headphones' preview player: start, pause, seek and stop, and where it is.

use super::*;

impl EarshotApp {
    /// Play a track in the headphones from `start` (0..1), decoding it first. A track
    /// with no beatgrid yet is queued for analysis too.
    pub(super) fn start_preview(&mut self, id: i64, start: f32) {
        if self.index.get(id).is_some_and(|t| !t.analysed()) && self.index.analysis(id).is_none() {
            self.lib
                .analyser
                .queue(id, None, false, AnalysisMode::Static);
        }
        let decoding = Arc::new(AtomicBool::new(true));
        self.preview = Some(Preview {
            id,
            decoding: decoding.clone(),
            start,
        });
        let (player, store) = (self.player.clone(), self.lib.store.clone());
        std::thread::spawn(move || {
            if let Err(e) = player::preview(&player, &store, id, start as f64) {
                eprintln!("preview: {e}");
            }
            decoding.store(false, Relaxed);
        });
    }

    /// Play a track in the headphones, or pause / resume it if it's the one loaded.
    pub(super) fn toggle_preview(&mut self, id: i64) {
        match self.preview_state() {
            Some(p) if p.id == id => {
                if self
                    .preview
                    .as_ref()
                    .is_some_and(|p| p.decoding.load(Relaxed))
                {
                    return;
                }
                if let Err(e) = player::play_preview(&self.player, !p.playing) {
                    eprintln!("preview: {e}");
                }
            }
            _ => self.start_preview(id, 0.0),
        }
    }

    /// Move the preview of `id` to `at` (0..1), starting it there if it isn't loaded.
    pub(super) fn seek_preview(&mut self, id: i64, at: f32) {
        let loaded = self
            .preview
            .as_ref()
            .is_some_and(|p| p.id == id && !p.decoding.load(Relaxed));
        if !loaded {
            return self.start_preview(id, at);
        }
        if let Err(e) = player::seek_preview(&self.player, at as f64) {
            eprintln!("preview: {e}");
        }
    }

    pub(super) fn stop_preview(&mut self, id: i64) {
        if self.preview.as_ref().is_some_and(|p| p.id == id) {
            self.preview = None;
            if let Err(e) = player::stop_preview(&self.player) {
                eprintln!("preview: {e}");
            }
        }
    }

    /// The track in the preview player, how far through it is, and whether it's playing.
    pub(super) fn preview_state(&self) -> Option<PreviewState> {
        let p = self.preview.as_ref()?;
        if p.decoding.load(Relaxed) {
            return Some(PreviewState {
                id: p.id,
                at: p.start,
                playing: true,
            });
        }
        let sh = &self.frame.snap.as_ref()?.preview;
        let at = if sh.duration > 0.0 {
            sh.position / sh.duration
        } else {
            0.0
        };
        Some(PreviewState {
            id: p.id,
            at: at as f32,
            playing: sh.playing,
        })
    }
}
