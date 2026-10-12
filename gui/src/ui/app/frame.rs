//! Once a frame: reading the engine, deciding whether to draw again, and the
//! frame-time readout.

use super::*;

impl EarshotApp {
    pub(super) fn deck_info(&self, deck: DeckId) -> DeckInfo {
        let inner = self.player.lock().unwrap();
        let s = inner.session(deck);
        let t = s.track_id.and_then(|id| self.index.get(id));
        DeckInfo {
            title: s.title.clone(),
            artist: s.artist.clone(),
            grid: s.grid.clone(),
            grid_source: s.grid_source.clone(),
            generation: s.generation,
            waveform: s.waveform.clone(),
            detail: s.detail.clone(),
            tag_bpm: t.and_then(|t| t.tag_bpm),
            key: t.and_then(|t| t.key),
            analysing: s.track_id.and_then(|id| match self.index.analysis(id)? {
                AnalysisState::Running { progress, stage } => Some((*progress, stage.to_string())),
                AnalysisState::Queued => Some((0.0, "waiting to analyse".into())),
                AnalysisState::Failed(_) => None,
            }),
            jump_size: s.jump_size,
            loop_size: s.loop_size,
            leader: inner.sync_leader == Some(deck),
            cues: s
                .track_id
                .and_then(|id| self.cues.get(&id).cloned())
                .unwrap_or_default(),
        }
    }

    /// Read the engine for this frame: its snapshot (catching the decks' sessions up,
    /// and saving changed hot cues), each player's details, and the preview.
    pub(super) fn read_engine(&mut self, ctx: &egui::Context) {
        self.frame.snap = player::poll(&self.player, &self.lib.store);
        for id in session::players(DECKS) {
            let info = self.deck_info(id);
            let sh = self.frame.snap.as_ref().map(|s| s.deck(id));
            self.panels.decks[id.index()].tick(ctx, sh, &info);
            self.frame.decks[id.index()] = Some(info);
        }
        self.frame.preview = self.preview_state();
    }

    /// Whether anything on screen moves by itself: a deck or the preview playing (or
    /// decoding), a waveform being scrubbed, a track loading or being analysed.
    pub(super) fn moving(&self) -> bool {
        let playing = self.frame.snap.as_ref().is_some_and(|s| {
            s.live().iter().any(|d| d.playing) || s.prepare.playing || s.preview.playing
        });
        playing
            || self
                .preview
                .as_ref()
                .is_some_and(|p| p.decoding.load(Relaxed))
            || self.panels.decks.iter().any(|d| d.busy())
            || !self.analysis.busy.is_empty()
    }

    /// Ask for the next frame only while something moves, and a moment after.
    /// Otherwise the next frame comes with input, or an event from another thread
    /// (the sink asks for a repaint with each).
    pub(super) fn schedule_repaint(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        if self.moving() {
            self.settle_until = Some(now + SETTLE);
        }
        match self.settle_until {
            Some(t) if now < t => ctx.request_repaint(),
            _ => self.settle_until = None,
        }
    }

    pub(super) fn tick_hud(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        let dt = ctx.input(|i| i.unstable_dt);
        self.hud.frames.push_back((now, dt));
        while self
            .hud
            .frames
            .front()
            .is_some_and(|(t, _)| now.duration_since(*t).as_secs_f32() > 2.0)
        {
            self.hud.frames.pop_front();
        }
        if self
            .hud
            .last
            .is_some_and(|l| now.duration_since(l).as_secs_f32() < 0.5)
        {
            return;
        }
        self.hud.last = Some(now);
        let mut dts: Vec<f32> = self.hud.frames.iter().map(|f| f.1).collect();
        if dts.is_empty() {
            return;
        }
        dts.sort_by(|a, b| b.total_cmp(a));
        let avg = dts.len() as f32 / dts.iter().sum::<f32>();
        let low = 1.0 / dts[(dts.len() / 100).min(dts.len() - 1)];
        self.hud.text = format!(
            "{avg:.0} fps · 1% {low:.0} · worst {:.0} ms",
            dts[0] * 1000.0
        );
        if let Some(m) = &self.monitor {
            let (snap, s) = (m.snapshot(), m.stats());
            let block = snap.last_block_frames as f64;
            let rate = snap.sample_rate as f64;
            let budget = if rate > 0.0 {
                block / rate * 1000.0
            } else {
                0.0
            };
            let worst = s.take_max_callback_ns() as f64 / 1e6;
            self.hud.audio = format!(
                "cb {worst:.2}/{budget:.1} ms · slow {} · xrun {}",
                s.slow_callbacks.load(Relaxed),
                s.xruns.load(Relaxed)
            );
        }
    }
}
