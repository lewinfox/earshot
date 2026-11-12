//! Playback: a [`Transport`] (where the playhead is, whether it's moving, the loop) and
//! a [`Player`] that reads audio from it at a speed, with optional keylock. Decks and
//! the preview player are both a `Player`.

use crate::command::LoopState;
use crate::decode::TrackAudio;
use crate::keylock::{self, Keylock};
use crate::scratch::Scratch;
use crate::stems::{self, StemAudio};

/// How far before the start of a track a deck's playhead may go (seconds).
pub const LEAD_IN: f64 = 30.0;

/// Why the playhead moved. Every move other than plain playback goes through
/// [`Transport::jump`] with one of these, so features that care (slip, censor, roll
/// loops) have one place to hook in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JumpReason {
    Seek,
    Cue,
    Hotcue,
    BeatJump,
    /// Sync lining the beats up.
    Sync,
    /// Playback reached a loop's end and went back to its start.
    LoopWrap,
    /// A loop was shortened past the playhead, or turned back on behind it.
    Loop,
    Load,
    Restore,
    /// Playback reached the end of the track.
    End,
}

pub struct Transport {
    pos: f64,
    pub playing: bool,
    pub loop_: LoopState,
    /// The track's active loop (`start..end`, seconds): playing forward onto its
    /// start, with no loop on, turns it on.
    pub active_loop: Option<(f64, f64)>,
    duration: f64,
    /// How far before 0 the playhead may go.
    lead_in: f64,
}

impl Transport {
    pub fn new(lead_in: f64) -> Transport {
        Transport {
            pos: 0.0,
            playing: false,
            loop_: LoopState::default(),
            active_loop: None,
            duration: 0.0,
            lead_in,
        }
    }

    /// Playhead, seconds.
    pub fn pos(&self) -> f64 {
        self.pos
    }

    pub fn duration(&self) -> f64 {
        self.duration
    }

    /// For a newly loaded track (the playhead is then moved with `jump`).
    pub fn set_duration(&mut self, d: f64) {
        self.duration = d;
    }

    /// Move the playhead, kept between the lead-in and the end of the track.
    pub fn jump(&mut self, to: f64, _why: JumpReason) {
        self.pos = to.clamp(-self.lead_in, self.duration);
    }

    /// Play on by `step` seconds, wrapping round an active loop. Returns true when
    /// this reached the end of the track (the transport stops there).
    #[inline]
    fn advance(&mut self, step: f64) -> bool {
        let from = self.pos;
        self.pos += step;
        if let Some((start, end)) = self.active_loop
            && !self.loop_.active
            && from <= start
            && self.pos > start
            && end > start
        {
            self.loop_ = LoopState {
                start: Some(start),
                end: Some(end),
                active: true,
            };
        }
        if let Some((start, end)) = self.loop_.bounds()
            && self.pos >= end
        {
            self.jump(self.pos - (end - start), JumpReason::LoopWrap);
        }
        if self.pos >= self.duration {
            self.jump(self.duration, JumpReason::End);
            self.playing = false;
            return true;
        }
        false
    }
}

/// A transport with a speed and (optionally) keylock: what plays a track.
pub struct Player {
    pub transport: Transport,
    /// Track seconds per real second.
    pub speed: f64,
    /// Keep the key when the speed changes (needs a shifter, see [`Player::new`]).
    pub keylock: bool,
    shifter: Option<Keylock>,
    /// A hand on the record (the mouse on the waveform, a jog wheel): see
    /// [`crate::scratch`].
    pub scratch: Scratch,
}

impl Player {
    /// `keylock`: allocate a pitch shifter (`sample_rate`, up to `max_block` frames)
    /// so the player can keep its key.
    pub fn new(lead_in: f64, keylock: Option<(u32, usize)>) -> Player {
        Player {
            transport: Transport::new(lead_in),
            speed: 1.0,
            keylock: false,
            shifter: keylock.map(|(sr, block)| Keylock::new(sr, block)),
            scratch: Scratch::default(),
        }
    }

    /// The hand goes on the record (`true`) or comes off, at `now` (the engine's
    /// clock). Let go, a playing deck carries on at its speed.
    pub fn hand(&mut self, on: bool, now: f64) {
        let speed = if self.transport.playing {
            self.speed
        } else {
            0.0
        };
        if on {
            self.scratch.touch(now, speed);
        } else {
            self.scratch.release(speed);
        }
    }

    /// Not playing this block.
    pub fn idle(&mut self) {
        if let Some(k) = &mut self.shifter {
            k.idle();
        }
    }

    /// Play `audio` into `buf` (interleaved stereo, already zeroed) at an output rate of
    /// `sr`, the block starting at `now` (the engine's clock, seconds). Returns true if
    /// the track ended during this block.
    pub fn render(&mut self, mix: &Mix, buf: &mut [f32], sr: f64, now: f64) -> bool {
        if self.scratch.active() {
            self.render_scratch(mix, buf, sr, now);
            return false;
        }
        if !self.transport.playing {
            self.idle();
            return false;
        }
        let src_rate = mix.audio.sample_rate as f64;
        let step = self.speed / sr;
        let loop_ = self.transport.loop_.bounds();
        let speed = self.speed;
        // Where the audio for a playhead position comes from: with keylock the player
        // reads ahead by the shifter's delay, wrapping round a loop.
        let mut shifter = match &mut self.shifter {
            Some(k) if self.keylock && keylock::SPEEDS.contains(&speed) => Some(k),
            other => {
                if let Some(k) = other {
                    k.idle();
                }
                None
            }
        };
        let lead = shifter.as_ref().map_or(0.0, |k| k.lead(step));
        let source = |t: f64| match loop_ {
            Some((lin, lout)) if t >= lout => lin + (t - lin) % (lout - lin),
            _ => t,
        };
        let t = &mut self.transport;
        if let Some(k) = shifter.as_deref_mut()
            && k.needs_restart(t.pos)
        {
            let len = k.preroll_frames();
            let start = t.pos + lead - len as f64 * step;
            k.restart(|i| mix.sample(source(start + i as f64 * step) * src_rate, 0.0));
        }
        let mut played = buf.len() / 2;
        let frames = played as f32;
        let mut ended = false;
        for (f, frame) in buf.chunks_exact_mut(2).enumerate() {
            let (l, r) = mix.sample(source(t.pos + lead) * src_rate, f as f32 / frames);
            frame[0] = l;
            frame[1] = r;
            if t.advance(step) {
                played = f + 1;
                ended = true;
                break;
            }
        }
        if let Some(k) = shifter {
            k.process(&mut buf[..played * 2], speed, t.pos);
        }
        ended
    }
}

impl Player {
    /// Play a block at the speed the hand gives, forwards or back, like a turntable
    /// (so without keylock). Loops don't apply: the hand goes where it likes, between
    /// the lead-in and the end.
    fn render_scratch(&mut self, mix: &Mix, buf: &mut [f32], sr: f64, now: f64) {
        if let Some(k) = &mut self.shifter {
            k.idle();
        }
        let src_rate = mix.audio.sample_rate as f64;
        let frames = (buf.len() / 2) as f32;
        let t = &mut self.transport;
        for (i, frame) in buf.chunks_exact_mut(2).enumerate() {
            let v = self.scratch.step(now + i as f64 / sr, sr);
            t.pos = (t.pos + v / sr).clamp(-t.lead_in, t.duration);
            (frame[0], frame[1]) = mix.sample(t.pos * src_rate, i as f32 / frames);
        }
    }
}

/// What a player plays: a track, and its stems at levels that move from `from` to
/// `to` across the block (so a change is a short ramp, not a click).
pub struct Mix<'a> {
    pub audio: &'a TrackAudio,
    /// `None`: the track as it is (no stems, or every stem at 1).
    pub stems: Option<StemMix<'a>>,
}

pub struct StemMix<'a> {
    pub audio: &'a StemAudio,
    pub from: [f32; stems::COUNT],
    pub to: [f32; stems::COUNT],
}

impl<'a> Mix<'a> {
    pub fn plain(audio: &'a TrackAudio) -> Mix<'a> {
        Mix { audio, stems: None }
    }

    /// The stereo frame at fractional source frame `x`, `w` (0..1) of the way through
    /// the block. With stems: `g_other·track + Σ (g_s − g_other)·stem_s`, "other"
    /// being the track minus the stored stems.
    #[inline]
    fn sample(&self, x: f64, w: f32) -> (f32, f32) {
        let audio = self.audio;
        let frames = audio.frames() as i64;
        let (l, r) = cubic(x, |k| {
            if k < 0 || k >= frames {
                (0.0, 0.0)
            } else {
                let i = k as usize * 2;
                (audio.samples[i], audio.samples[i + 1])
            }
        });
        let Some(m) = &self.stems else {
            return (l, r);
        };
        let g = |s: usize| m.from[s] + (m.to[s] - m.from[s]) * w;
        let go = g(stems::OTHER);
        let (mut ol, mut or) = (go * l, go * r);
        for s in 0..stems::STORED {
            let c = g(s) - go;
            if c != 0.0 {
                let (sl, sr) = cubic(x, |k| m.audio.get(k, s));
                ol += c * sl;
                or += c * sr;
            }
        }
        (ol, or)
    }
}

/// Cubic Hermite interpolation at fractional source frame `x`, of the frames `get`
/// gives.
#[inline]
fn cubic(x: f64, get: impl Fn(i64) -> (f32, f32)) -> (f32, f32) {
    let i = x.floor() as i64;
    let t = (x - i as f64) as f32;
    let y = [get(i - 1), get(i), get(i + 1), get(i + 2)];
    let one = |y0: f32, y1: f32, y2: f32, y3: f32| {
        let c1 = 0.5 * (y2 - y0);
        let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
        let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
        ((c3 * t + c2) * t + c1) * t + y1
    };
    (
        one(y[0].0, y[1].0, y[2].0, y[3].0),
        one(y[0].1, y[1].1, y[2].1, y[3].1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jumps_stay_inside_the_track_and_lead_in() {
        let mut t = Transport::new(LEAD_IN);
        t.set_duration(60.0);
        t.jump(-100.0, JumpReason::Seek);
        assert_eq!(t.pos(), -LEAD_IN);
        t.jump(100.0, JumpReason::Seek);
        assert_eq!(t.pos(), 60.0);
        let mut p = Transport::new(0.0);
        p.set_duration(10.0);
        p.jump(-1.0, JumpReason::Seek);
        assert_eq!(p.pos(), 0.0);
    }

    #[test]
    fn advance_wraps_loops_and_stops_at_the_end() {
        let mut t = Transport::new(LEAD_IN);
        t.set_duration(10.0);
        t.playing = true;
        t.loop_ = LoopState {
            start: Some(1.0),
            end: Some(2.0),
            active: true,
        };
        t.jump(1.9, JumpReason::Seek);
        assert!(!t.advance(0.15));
        assert!((t.pos() - 1.05).abs() < 1e-12);
        t.loop_.active = false;
        t.jump(9.95, JumpReason::Seek);
        assert!(t.advance(0.1));
        assert_eq!(t.pos(), 10.0);
        assert!(!t.playing);
    }
}

#[cfg(test)]
mod stem_tests {
    use super::*;

    /// A track made of three tones (drums, vocals, other) with its stems: drums and
    /// vocals stored, bass silent.
    fn parts() -> (TrackAudio, StemAudio) {
        let n = 4410;
        let tone = |f: f64, i: usize| {
            (0.2 * (i as f64 * f / 44_100.0 * std::f64::consts::TAU).sin()) as f32
        };
        let stereo = |g: &dyn Fn(usize) -> f32| TrackAudio {
            samples: (0..n).flat_map(|i| [g(i), g(i)]).collect(),
            sample_rate: 44_100,
        };
        let track = stereo(&|i| tone(440.0, i) + tone(660.0, i) + tone(880.0, i));
        let drums = stereo(&|i| tone(440.0, i));
        let bass = stereo(&|_| 0.0);
        let vocals = stereo(&|i| tone(660.0, i));
        let stems = StemAudio::new([&drums, &bass, &vocals], n);
        (track, stems)
    }

    #[test]
    fn stems_at_full_level_are_the_track_exactly() {
        let (track, stems) = parts();
        let plain = Mix::plain(&track);
        let mix = Mix {
            audio: &track,
            stems: Some(StemMix {
                audio: &stems,
                from: [1.0; stems::COUNT],
                to: [1.0; stems::COUNT],
            }),
        };
        for x in [10.0, 100.5, 2000.25] {
            assert_eq!(plain.sample(x, 0.5), mix.sample(x, 0.5));
        }
    }

    #[test]
    fn a_stem_alone_is_that_stem() {
        let (track, stems) = parts();
        // Vocals only.
        let g = [0.0, 0.0, 1.0, 0.0];
        let mix = Mix {
            audio: &track,
            stems: Some(StemMix {
                audio: &stems,
                from: g,
                to: g,
            }),
        };
        for k in [10usize, 1000, 3000] {
            let want = (0.2 * (k as f64 * 660.0 / 44_100.0 * std::f64::consts::TAU).sin()) as f32;
            let (l, _) = mix.sample(k as f64, 0.0);
            // Half precision: about 1e-4 at this level.
            assert!((l - want).abs() < 1e-3, "{l} vs {want}");
        }
        // Other alone: the 880 Hz tone, worked out from the track.
        let g = [0.0, 0.0, 0.0, 1.0];
        let mix = Mix {
            audio: &track,
            stems: Some(StemMix {
                audio: &stems,
                from: g,
                to: g,
            }),
        };
        let k = 1234usize;
        let want = (0.2 * (k as f64 * 880.0 / 44_100.0 * std::f64::consts::TAU).sin()) as f32;
        assert!((mix.sample(k as f64, 0.0).0 - want).abs() < 1e-3);
    }
}
