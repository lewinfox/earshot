//! Notices when the sound card stops asking for audio. The audio callback normally
//! runs hundreds of times a second; if the sound system stops calling it (a PipeWire
//! stream that stalled, a USB interface that hiccupped), the decks freeze while still
//! "playing", and nothing in earshot is wrong to show it. A thread here watches the
//! callback count; when it hasn't moved for [`STALL`], it reports it, and the UI
//! reopens the audio device (`AudioCmd::Stalled`), which is what fixes it by hand.

use std::time::{Duration, Instant};

use earshot_engine::EngineSnapshot;

use super::player::Player;

/// How long without a callback counts as stalled.
pub const STALL: Duration = Duration::from_millis(500);
/// How often the callback count is checked.
const TICK: Duration = Duration::from_millis(100);
/// While it stays stalled (the restart didn't help), how often it's reported again.
const AGAIN: Duration = Duration::from_secs(5);

/// The decision, apart from the thread and the clock.
#[derive(Default)]
pub struct Watch {
    /// The callback count last seen, and when it was last seen to change.
    last: Option<(u64, Instant)>,
    reported: Option<Instant>,
}

impl Watch {
    /// With the callback count now (`None`: no sound card open), whether to report a
    /// stall: once on stalling, then every [`AGAIN`] while it lasts.
    pub fn tick(&mut self, now: Instant, callbacks: Option<u64>) -> Option<Duration> {
        let Some(n) = callbacks else {
            *self = Watch::default();
            return None;
        };
        match self.last {
            Some((seen, since)) if seen == n => {
                let quiet = now.duration_since(since);
                let due = self.reported.is_none_or(|r| now.duration_since(r) >= AGAIN);
                if quiet >= STALL && due {
                    self.reported = Some(now);
                    return Some(quiet);
                }
            }
            // Moving (or a new engine, whose count started again).
            _ => {
                *self = Watch {
                    last: Some((n, now)),
                    reported: None,
                }
            }
        }
        None
    }
}

/// What the decks were doing when it stalled, for the log.
pub fn describe(snap: &EngineSnapshot) -> String {
    let decks: Vec<String> = snap
        .live()
        .iter()
        .chain(std::iter::once(&snap.prepare))
        .enumerate()
        .filter(|(_, d)| d.track_id.is_some())
        .map(|(i, d)| {
            format!(
                "{} track {} at {:.2} s{}",
                if i < snap.decks_live {
                    ((b'A' + i as u8) as char).to_string()
                } else {
                    "prepare".into()
                },
                d.track_id.unwrap_or_default(),
                d.position,
                if d.playing { " playing" } else { "" }
            )
        })
        .collect();
    format!(
        "{} callbacks, {} frames a block; {}",
        snap.callbacks,
        snap.last_block_frames,
        if decks.is_empty() {
            "no tracks loaded".into()
        } else {
            decks.join(", ")
        }
    )
}

/// Start watching. `report` gets how long it's been quiet and what the decks were
/// doing, on its own thread.
pub fn start(player: Player, report: impl Fn(Duration, String) + Send + 'static) {
    std::thread::Builder::new()
        .name("earshot-audio-watch".into())
        .spawn(move || {
            let mut watch = Watch::default();
            loop {
                std::thread::sleep(TICK);
                let snap = player.lock().unwrap().device_snapshot();
                if let Some(quiet) = watch.tick(Instant::now(), snap.as_ref().map(|s| s.callbacks))
                    && let Some(s) = &snap
                {
                    report(quiet, describe(s));
                }
            }
        })
        .expect("spawn audio watch");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stall_is_reported_once_then_again_while_it_lasts() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut w = Watch::default();
        assert_eq!(w.tick(at(0), Some(10)), None);
        assert_eq!(w.tick(at(400), Some(10)), None, "not long enough");
        assert_eq!(w.tick(at(600), Some(10)), Some(Duration::from_millis(600)));
        assert_eq!(w.tick(at(700), Some(10)), None, "reported already");
        assert_eq!(
            w.tick(at(5600), Some(10)),
            Some(Duration::from_millis(5600)),
            "still stalled"
        );
        // Moving again (a new engine counts from 0): nothing, and the clock restarts.
        assert_eq!(w.tick(at(5700), Some(3)), None);
        assert_eq!(w.tick(at(6100), Some(3)), None);
        assert_eq!(w.tick(at(6300), Some(3)), Some(Duration::from_millis(600)));
        // No sound card (the null output, or none): never.
        assert_eq!(w.tick(at(9000), None), None);
        assert_eq!(w.tick(at(20000), None), None);
    }

    #[test]
    fn a_running_callback_is_never_reported() {
        let t0 = Instant::now();
        let mut w = Watch::default();
        for i in 0..100u64 {
            assert_eq!(
                w.tick(t0 + Duration::from_millis(i * 100), Some(i * 40)),
                None
            );
        }
    }
}
