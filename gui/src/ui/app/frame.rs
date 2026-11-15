//! Once a frame: reading the engine, deciding whether to draw again, and the
//! frame-time readout.

use super::*;

/// Real time lost: how long after, with no deck playing, the audio device is opened
/// again to get it back; and how long before trying that again.
const REOPEN_AFTER: std::time::Duration = std::time::Duration::from_secs(5);
const REOPEN_EVERY: std::time::Duration = std::time::Duration::from_secs(60);

impl EarshotApp {
    pub(super) fn deck_info(&self, deck: DeckId) -> DeckInfo {
        let inner = self.player.lock().unwrap();
        let s = inner.session(deck);
        let t = s.track_id.and_then(|id| self.index.get(id));
        DeckInfo {
            title: s.title.clone(),
            artist: s.artist.clone(),
            grid: s.grid.clone(),
            generation: s.generation,
            waveform: s.waveform.clone(),
            detail: s.detail.clone(),
            stem_waves: s.stem_waves.clone(),
            stems_job: s.track_id.and_then(|t| self.stems.status(t)),
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
            self.panels.decks[id.index()].stem_mode = self.prefs.stem_mode;
            self.panels.decks[id.index()].controller = self.prefs.controller();
            self.panels.decks[id.index()].active =
                self.prefs.four_decks() && self.prefs.active_decks().contains(&id);
            // Only the pads page in sampler mode shows them.
            if self.panels.decks[id.index()].pad_mode == crate::core::view::PadMode::Sampler {
                self.panels.decks[id.index()].sample_pads = self.sample_pads(id);
            }
            self.panels.decks[id.index()].tick(ctx, sh, &info);
            self.frame.decks[id.index()] = Some(info);
        }
        self.frame.preview = self.preview_state();
    }

    /// Whether anything on screen moves by itself: a deck or the preview playing (or
    /// decoding), a waveform being scrubbed, a track loading or being analysed.
    pub(super) fn moving(&self) -> bool {
        let playing = self.frame.snap.as_ref().is_some_and(|s| {
            s.live().iter().any(|d| d.playing)
                || s.preview.playing
                || s.sampler.iter().any(|v| v.playing)
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
        // By the frames' clock, as egui's own animations are.
        let now = ctx.input(|i| i.time);
        if self.moving() {
            self.settle_until = Some(now + SETTLE.as_secs_f64());
        }
        match self.settle_until {
            Some(t) if now < t => ctx.request_repaint(),
            _ => self.settle_until = None,
        }
    }

    /// What's being done about a machine that can't keep up (the audio comes first:
    /// analysis and stems wait, then the effects go, and keylock's quality last), for the log and the top
    /// bar. The audio thread can't write logs or show anything: it sets flags, and
    /// this notices when they change.
    pub(super) fn watch_strain(&mut self) {
        use crate::ui::panels::status::Alert;
        let (lost, cheap, fx_off) = self.monitor.as_ref().map_or((false, false, false), |m| {
            let s = m.stats();
            (
                s.realtime_lost.load(Relaxed),
                s.keylock_cheap.load(Relaxed),
                s.effects_off.load(Relaxed),
            )
        });
        if fx_off != self.hud.effects_off {
            self.hud.effects_off = fx_off;
            if fx_off {
                eprintln!(
                    "audio: making the sound is using most of the time there is: the \
                     effects are off (keylock is kept)"
                );
                self.notify(
                    notice::Level::Warning,
                    "The computer can't keep up with the audio: the effects are off for now",
                );
            } else {
                eprintln!("audio: there's room again: the effects can be used");
            }
        }
        // The audio itself is behind: whatever stems are being worked out stop.
        if (fx_off || lost) && self.stems.cancel_all() > 0 {
            eprintln!("stems: stopped, as the audio can't keep up");
        }
        if lost != self.hud.realtime_lost {
            self.hud.realtime_lost = lost;
            if lost {
                eprintln!(
                    "audio: ran past the real-time limit (SIGXCPU): the audio thread has \
                     dropped to normal priority, so the system doesn't end earshot. The \
                     sound may drop out; once no deck is playing the audio device is \
                     opened again, which goes back to real time"
                );
                self.hud.lost_at = Some(Instant::now());
                self.notify(
                    notice::Level::Warning,
                    "The computer can't keep up with the audio: it has dropped out of real \
                     time, and the sound may drop out",
                );
            } else {
                // A new engine: its streams run on new threads, real-time as usual.
                eprintln!("audio: the sound card was reopened after real time was lost");
                self.hud.lost_at = None;
            }
        }
        // Real time comes back only with a new audio stream (the system gives it to
        // the stream's thread when it's opened). Opening one is a moment's silence,
        // so: once no deck is playing, a little after the trouble, and not over and
        // over if it's lost again straight away.
        let playing = self
            .frame
            .snap
            .as_ref()
            .is_some_and(|sh| sh.live().iter().any(|d| d.playing));
        let now = Instant::now();
        if lost
            && !playing
            && self.hud.lost_at.is_some_and(|t| now.duration_since(t) >= REOPEN_AFTER)
            && self.hud.reopened_at.is_none_or(|t| now.duration_since(t) >= REOPEN_EVERY)
        {
            self.hud.reopened_at = Some(now);
            eprintln!("audio: no deck is playing: opening the audio device again for real time");
            let settings = self.settings.audio.clone();
            self.dispatch(Command::Audio(AudioCmd::Restart(settings)));
            return self.watch_strain();
        }
        if cheap != self.hud.keylock_cheap {
            self.hud.keylock_cheap = cheap;
            if cheap {
                eprintln!(
                    "audio: making the sound is using most of the time there is: keylock \
                     is on its lighter setting"
                );
            } else {
                eprintln!("audio: there's room again: keylock is back on its full setting");
            }
        }
        if let Some(power) = crate::core::power::latest() {
            self.hud.power = power;
            self.hud.memory_short = crate::core::power::memory_short();
        }
        // Short of resources: full analysis (the worker and its model) waits in its
        // queue, and tracks get the quick grid finder's grid meanwhile.
        let short =
            lost || cheap || fx_off || self.hud.power.is_some() || self.hud.memory_short;
        if short != self.hud.low_resource {
            self.hud.low_resource = short;
            self.lib.analyser.set_held(short);
            if short {
                eprintln!(
                    "analysis: the machine is short of resources: full analysis waits, \
                     and tracks get a quick grid meanwhile"
                );
            } else {
                eprintln!("analysis: there's room again: full analysis carries on");
            }
        }
        let mut alerts = vec![];
        if lost {
            alerts.push(Alert {
                text: "⚠ audio not real-time".into(),
                tip: "Making the sound took longer than the sound lasts, for long enough \
                      that the system was about to end earshot. The audio now runs at \
                      normal priority and may drop out. It goes back to real time by \
                      itself once no deck is playing (a moment's silence)."
                    .into(),
                serious: true,
            });
        }
        if fx_off {
            alerts.push(Alert {
                text: "effects off".into(),
                tip: "Making the sound is using most of the time there is, so the effects \
                      are off: the tracks and their keylock come first. Once there's room \
                      they can be switched on again."
                    .into(),
                serious: false,
            });
        }
        if cheap {
            alerts.push(Alert {
                text: "keylock: lighter".into(),
                tip: "Even with the effects off, making the sound is using most of the \
                      time there is, so keylock is on its lighter setting (a little less \
                      clean) rather than dropping out. It goes back by itself once \
                      there's room."
                    .into(),
                serious: false,
            });
        }
        if let Some(p) = &self.hud.power {
            alerts.push(Alert {
                text: format!("⚠ {}", p.text),
                tip: p.detail.clone(),
                serious: false,
            });
        }
        if self.hud.memory_short {
            alerts.push(Alert {
                text: "⚠ low memory".into(),
                tip: "Less than 1.5 GB of memory is left: not enough to start the \
                      analysis worker and its model."
                    .into(),
                serious: false,
            });
        }
        if short {
            alerts.push(Alert {
                text: "analysis: quick only".into(),
                tip: "The computer is short of resources, so full analysis is waiting: \
                      tracks get a quick grid for now (amber in the library). What's \
                      queued runs by itself once there's room again."
                    .into(),
                serious: false,
            });
        }
        self.hud.alerts = alerts;
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
        self.watch_strain();
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
            let xruns = s.xruns.load(Relaxed);
            self.hud.audio = format!(
                "cb {worst:.2}/{budget:.1} ms · slow {} · xrun {xruns}",
                s.slow_callbacks.load(Relaxed),
            );
            if self.audio.as_ref().is_ok_and(|i| i.phones.is_some()) {
                let (short, realigns) = (
                    s.phones_short.load(Relaxed),
                    s.phones_realigns.load(Relaxed),
                );
                let queued = s.phones_level.load(Relaxed) as f64 / rate.max(1.0) * 1000.0;
                // The audio thread can't write logs: it counts, and this notices.
                if short > self.hud.phones_short {
                    eprintln!(
                        "audio: headphones ran dry: {} frames of silence (queue {queued:.1} ms)",
                        short - self.hud.phones_short
                    );
                }
                if realigns > self.hud.phones_realigns {
                    eprintln!("audio: headphones out of line with the master: realigning");
                }
                (self.hud.phones_short, self.hud.phones_realigns) = (short, realigns);
                self.hud.audio += &format!(
                    "\nheadphones queue {queued:.1} ms · realigned {realigns} · ran dry {short} frames"
                );
            }
            // A dropout since the last look shows as full, whatever the timing said.
            let dropped = xruns > self.hud.xruns;
            self.hud.xruns = xruns;
            self.hud.load = (budget > 0.0).then(|| {
                let load = (worst / budget) as f32;
                if dropped { load.max(1.0) } else { load }
            });
        }
    }
}
