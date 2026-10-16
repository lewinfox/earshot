//! Playback: a [`Transport`] (where the playhead is, whether it's moving, the loop) and
//! a [`Player`] that reads audio from it at a speed, with optional keylock. Decks and
//! the preview player are both a `Player`.

use crate::command::LoopState;
use crate::decode::TrackAudio;
use crate::keylock::{self, Keylock};
use crate::scratch::Scratch;

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
        self.pos += step;
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
    pub fn render(&mut self, audio: &TrackAudio, buf: &mut [f32], sr: f64, now: f64) -> bool {
        if self.scratch.active() {
            self.render_scratch(audio, buf, sr, now);
            return false;
        }
        if !self.transport.playing {
            self.idle();
            return false;
        }
        let src_rate = audio.sample_rate as f64;
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
            k.restart(|i| sample_at(audio, source(start + i as f64 * step) * src_rate));
        }
        let mut played = buf.len() / 2;
        let mut ended = false;
        for (f, frame) in buf.chunks_exact_mut(2).enumerate() {
            let (l, r) = sample_at(audio, source(t.pos + lead) * src_rate);
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
    fn render_scratch(&mut self, audio: &TrackAudio, buf: &mut [f32], sr: f64, now: f64) {
        if let Some(k) = &mut self.shifter {
            k.idle();
        }
        let src_rate = audio.sample_rate as f64;
        let t = &mut self.transport;
        for (i, frame) in buf.chunks_exact_mut(2).enumerate() {
            let v = self.scratch.step(now + i as f64 / sr, sr);
            t.pos = (t.pos + v / sr).clamp(-t.lead_in, t.duration);
            (frame[0], frame[1]) = sample_at(audio, t.pos * src_rate);
        }
    }
}

/// Cubic Hermite interpolation at fractional source frame `x`.
#[inline]
fn sample_at(audio: &TrackAudio, x: f64) -> (f32, f32) {
    let frames = audio.frames() as i64;
    let i = x.floor() as i64;
    let t = (x - i as f64) as f32;
    let get = |k: i64, c: usize| -> f32 {
        if k < 0 || k >= frames {
            0.0
        } else {
            audio.samples[(k as usize) * 2 + c]
        }
    };
    let mut out = [0f32; 2];
    for (c, o) in out.iter_mut().enumerate() {
        let (y0, y1, y2, y3) = (get(i - 1, c), get(i, c), get(i + 1, c), get(i + 2, c));
        let c1 = 0.5 * (y2 - y0);
        let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
        let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
        *o = ((c3 * t + c2) * t + c1) * t + y1;
    }
    (out[0], out[1])
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
