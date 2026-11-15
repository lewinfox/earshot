//! The sampler: [`SAMPLER_SLOTS`] slots, each a short sound held whole in memory
//! (decoded off the audio thread and handed over with [`SamplerCmd::Load`]), any
//! number of them playing at once, mixed into the master.
//!
//! A slot plays from its start each time it's triggered, once through or round and
//! round ([`SamplerCmd::Loop`]), at its own volume, at the sound's own speed and
//! pitch (resampled to the output's rate; it doesn't follow the decks' tempo).
//! Triggering a slot that's sounding starts it again: the pass under way fades out
//! over a few milliseconds while the new one starts, so neither a stop nor a
//! retrigger clicks.
//!
//! Everything here runs on the audio thread and allocates nothing; a sound that's
//! replaced or taken away is handed back to be freed elsewhere.

use std::sync::Arc;

use crate::command::Set;
use crate::decode::TrackAudio;
use crate::dsp::{self, Smoothed};
use crate::processor::KNOB_SECS;

/// How many slots the sampler has: rekordbox's 16, pads 1-8 on each of two decks.
pub const SAMPLER_SLOTS: usize = 16;

/// Seconds a stopped (or retriggered) pass takes to fade out.
const FADE_SECS: f64 = 0.005;

/// A sampler slot. Checked when it's made, so indexing with it can never go out of
/// bounds on the audio thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SampleSlot(u8);

impl SampleSlot {
    pub const fn new(index: usize) -> Option<SampleSlot> {
        if index < SAMPLER_SLOTS {
            Some(SampleSlot(index as u8))
        } else {
            None
        }
    }

    /// 0, 1, … below [`SAMPLER_SLOTS`].
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// Every slot, in order.
    pub fn all() -> impl Iterator<Item = SampleSlot> {
        (0..SAMPLER_SLOTS).map(|i| SampleSlot(i as u8))
    }
}

/// What the sampler is told.
pub enum SamplerCmd {
    /// Put a sound in the slot (`None`: empty it). Whatever was playing there stops.
    Load {
        slot: SampleSlot,
        audio: Option<Arc<TrackAudio>>,
    },
    /// Play the slot from its start (again, if it's playing).
    Play(SampleSlot),
    /// Stop the slot.
    Stop(SampleSlot),
    /// Stop every slot.
    StopAll,
    /// The slot's volume, 0..1 (a fader: the gain is its square).
    Volume(SampleSlot, f64),
    /// Play the slot round and round, or once through.
    Loop(SampleSlot, Set),
}

/// A slot's sound and settings, for [`crate::Command::Restore`].
#[derive(Clone)]
pub struct SampleState {
    pub audio: Option<Arc<TrackAudio>>,
    pub volume: f64,
    pub looping: bool,
}

impl Default for SampleState {
    fn default() -> SampleState {
        SampleState {
            audio: None,
            volume: 1.0,
            looping: false,
        }
    }
}

/// One slot, as of the end of a callback.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SampleSnap {
    /// There's a sound in the slot.
    pub loaded: bool,
    pub playing: bool,
    pub looping: bool,
    /// 0..1: see [`SamplerCmd::Volume`].
    pub volume: f64,
    /// Seconds into the sound, and its length.
    pub position: f64,
    pub duration: f64,
}

/// One pass through a sound: where it is (in the sound's own frames) and how loud
/// (1 while it plays, falling to 0 once it's stopped).
#[derive(Clone, Copy, Default)]
struct Pass {
    pos: f64,
    gain: f32,
    /// Fading out: it ends when the gain reaches 0.
    stopping: bool,
}

impl Pass {
    fn sounding(&self) -> bool {
        self.gain > 0.0
    }
}

struct Voice {
    audio: Option<Arc<TrackAudio>>,
    /// The pass playing, and the one before it fading out (a retrigger).
    now: Pass,
    tail: Pass,
    looping: bool,
    volume: Smoothed,
}

impl Voice {
    fn new() -> Voice {
        Voice {
            audio: None,
            now: Pass::default(),
            tail: Pass::default(),
            looping: false,
            volume: Smoothed::new(1.0),
        }
    }

    fn playing(&self) -> bool {
        self.now.sounding() && !self.now.stopping
    }

    /// Anything of it is still heard: playing, or fading out.
    fn sounding(&self) -> bool {
        self.now.sounding() || self.tail.sounding()
    }

    fn play(&mut self) {
        if self.audio.as_ref().is_none_or(|a| a.frames() == 0) {
            return;
        }
        if self.now.sounding() {
            // The louder of the two passes under way carries on fading; the other
            // is all but silent already.
            if !self.tail.sounding() || self.now.gain >= self.tail.gain {
                self.tail = self.now;
            }
            self.tail.stopping = true;
        }
        self.now = Pass {
            pos: 0.0,
            gain: 1.0,
            stopping: false,
        };
    }

    fn stop(&mut self) {
        self.now.stopping = true;
        self.tail.stopping = true;
    }

    /// Add `n` frames of this slot to `out` (interleaved stereo).
    fn render(&mut self, out: &mut [f32], n: usize, rate: f64, fade: f32, ease: f64) {
        let Some(audio) = self.audio.as_deref() else {
            return;
        };
        if !self.sounding() {
            return;
        }
        let frames = audio.frames();
        // The sound's frames per output frame (exactly 1 at the same rate).
        let step = audio.sample_rate as f64 / rate;
        let looping = self.looping;
        for f in 0..n {
            let v = self.volume.next(ease) as f32;
            let g = v * v;
            let (mut l, mut r) = (0.0f32, 0.0f32);
            for pass in [&mut self.now, &mut self.tail] {
                if !pass.sounding() {
                    continue;
                }
                let (sl, sr) = read(audio, pass.pos, looping);
                l += pass.gain * sl;
                r += pass.gain * sr;
                if pass.stopping {
                    pass.gain = (pass.gain - fade).max(0.0);
                }
                pass.pos += step;
                if pass.pos >= frames as f64 {
                    if looping && !pass.stopping {
                        pass.pos -= frames as f64;
                    } else {
                        // The end (the sound's own last frames are its fade).
                        pass.gain = 0.0;
                    }
                }
            }
            out[2 * f] += g * l;
            out[2 * f + 1] += g * r;
            if !self.sounding() {
                break;
            }
        }
    }

    /// Set the volume: eased there while the slot sounds, at once while it's silent
    /// (so the next press starts at it).
    fn set_volume(&mut self, v: f64) {
        self.volume.target = if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if !self.sounding() {
            self.volume.current = self.volume.target;
        }
    }
}

/// The sound at fractional frame `x` (cubic Hermite), silent outside it; round the
/// end to the start when looping.
#[inline]
fn read(audio: &TrackAudio, x: f64, looping: bool) -> (f32, f32) {
    let frames = audio.frames() as i64;
    let get = |k: i64| -> (f32, f32) {
        let k = if looping {
            k.rem_euclid(frames)
        } else if k < 0 || k >= frames {
            return (0.0, 0.0);
        } else {
            k
        };
        let i = 2 * k as usize;
        (audio.samples[i], audio.samples[i + 1])
    };
    let i = x.floor() as i64;
    let t = (x - i as f64) as f32;
    if t == 0.0 {
        return get(i);
    }
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

/// The sampler's slots, on the audio thread.
pub(crate) struct Sampler {
    voices: [Voice; SAMPLER_SLOTS],
    /// The output's sample rate.
    rate: f64,
    /// How much a fading pass's gain falls each frame.
    fade: f32,
    ease: f64,
}

impl Sampler {
    pub fn new(sample_rate: f64) -> Sampler {
        Sampler {
            voices: std::array::from_fn(|_| Voice::new()),
            rate: sample_rate,
            fade: (1.0 / (FADE_SECS * sample_rate)) as f32,
            ease: dsp::ease_per_sample(KNOB_SECS, sample_rate),
        }
    }

    /// Carry out a command. Returns the sound it replaced, to be freed off the
    /// audio thread.
    pub fn apply(&mut self, cmd: SamplerCmd) -> Option<Arc<TrackAudio>> {
        match cmd {
            SamplerCmd::Load { slot, audio } => return self.load(slot, audio),
            SamplerCmd::Play(slot) => self.voices[slot.index()].play(),
            SamplerCmd::Stop(slot) => self.voices[slot.index()].stop(),
            SamplerCmd::StopAll => self.voices.iter_mut().for_each(Voice::stop),
            SamplerCmd::Volume(slot, v) => self.voices[slot.index()].set_volume(v),
            SamplerCmd::Loop(slot, set) => {
                let voice = &mut self.voices[slot.index()];
                voice.looping = set.apply(voice.looping);
            }
        }
        None
    }

    /// Put `audio` in a slot, silencing it. Returns what was there.
    pub fn load(
        &mut self,
        slot: SampleSlot,
        audio: Option<Arc<TrackAudio>>,
    ) -> Option<Arc<TrackAudio>> {
        let voice = &mut self.voices[slot.index()];
        voice.now = Pass::default();
        voice.tail = Pass::default();
        std::mem::replace(&mut voice.audio, audio)
    }

    /// Put a slot back as `state` has it. Returns the sound it replaced.
    pub fn restore(
        &mut self,
        slot: SampleSlot,
        state: &mut SampleState,
    ) -> Option<Arc<TrackAudio>> {
        let old = self.load(slot, state.audio.take());
        let voice = &mut self.voices[slot.index()];
        voice.looping = state.looping;
        voice.volume = Smoothed::new(state.volume.clamp(0.0, 1.0));
        old
    }

    /// Add every sounding slot to `out` (interleaved stereo, `n` frames).
    pub fn render(&mut self, out: &mut [f32], n: usize) {
        for voice in &mut self.voices {
            voice.render(out, n, self.rate, self.fade, self.ease);
        }
    }

    pub fn snapshot(&self) -> [SampleSnap; SAMPLER_SLOTS] {
        std::array::from_fn(|i| {
            let v = &self.voices[i];
            let (rate, duration) = v
                .audio
                .as_ref()
                .map_or((1.0, 0.0), |a| (a.sample_rate as f64, a.duration()));
            SampleSnap {
                loaded: v.audio.is_some(),
                playing: v.playing(),
                looping: v.looping,
                volume: v.volume.target,
                position: if v.playing() { v.now.pos / rate } else { 0.0 },
                duration,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    fn slot(i: usize) -> SampleSlot {
        SampleSlot::new(i).unwrap()
    }

    /// `secs` of a constant `level` on both channels, at `rate`.
    fn flat(level: f32, secs: f64, rate: u32) -> Arc<TrackAudio> {
        Arc::new(TrackAudio {
            samples: vec![level; 2 * (secs * rate as f64) as usize],
            sample_rate: rate,
        })
    }

    fn loaded(sounds: &[(usize, Arc<TrackAudio>)]) -> Sampler {
        let mut s = Sampler::new(SR as f64);
        for (i, a) in sounds {
            assert!(s.load(slot(*i), Some(a.clone())).is_none());
        }
        s
    }

    fn render(s: &mut Sampler, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0; 2 * frames];
        for block in out.chunks_mut(2 * 256) {
            let n = block.len() / 2;
            s.render(block, n);
        }
        out
    }

    #[test]
    fn slots_are_checked() {
        assert!(SampleSlot::new(SAMPLER_SLOTS - 1).is_some());
        assert!(SampleSlot::new(SAMPLER_SLOTS).is_none());
        assert_eq!(SampleSlot::all().count(), SAMPLER_SLOTS);
    }

    #[test]
    fn a_slot_plays_once_through_and_stops_itself() {
        let mut s = loaded(&[(0, flat(0.5, 0.1, SR))]);
        assert!(
            render(&mut s, 256).iter().all(|x| *x == 0.0),
            "silent until played"
        );
        s.apply(SamplerCmd::Play(slot(0)));
        let out = render(&mut s, 9600);
        assert!(
            out[..2 * 4800].iter().all(|x| (*x - 0.5).abs() < 1e-6),
            "its 0.1 s"
        );
        assert!(out[2 * 4800..].iter().all(|x| *x == 0.0), "then nothing");
        let snap = s.snapshot()[0];
        assert!(snap.loaded && !snap.playing);
        assert!((snap.duration - 0.1).abs() < 1e-9);
    }

    #[test]
    fn slots_play_together() {
        let mut s = loaded(&[(0, flat(0.25, 1.0, SR)), (9, flat(0.5, 1.0, SR))]);
        s.apply(SamplerCmd::Play(slot(0)));
        s.apply(SamplerCmd::Play(slot(9)));
        let out = render(&mut s, 1024);
        assert!(out.iter().all(|x| (*x - 0.75).abs() < 1e-6));
        let snap = s.snapshot();
        assert!(snap[0].playing && snap[9].playing && !snap[1].playing);
    }

    #[test]
    fn a_looping_slot_goes_round_until_stopped() {
        let mut s = loaded(&[(3, flat(0.5, 0.01, SR))]);
        s.apply(SamplerCmd::Loop(slot(3), Set::On));
        s.apply(SamplerCmd::Play(slot(3)));
        // Far past its 480 frames, still sounding.
        let out = render(&mut s, 4800);
        assert!(out.iter().all(|x| (*x - 0.5).abs() < 1e-6));
        assert!(s.snapshot()[3].playing && s.snapshot()[3].looping);
        // A stop fades it out within a few milliseconds, with no jump.
        s.apply(SamplerCmd::Stop(slot(3)));
        assert!(!s.snapshot()[3].playing);
        let out = render(&mut s, 1024);
        let left: Vec<f32> = out.chunks(2).map(|f| f[0]).collect();
        assert!(
            left.windows(2).all(|w| w[1] <= w[0] && w[0] - w[1] < 0.01),
            "a ramp"
        );
        let fade = (FADE_SECS * SR as f64) as usize;
        assert!(
            left[fade + 1..].iter().all(|x| *x == 0.0),
            "silent after the fade"
        );
        // Once through again.
        s.apply(SamplerCmd::Loop(slot(3), Set::Toggle));
        assert!(!s.snapshot()[3].looping);
    }

    #[test]
    fn volume_is_a_fader() {
        let mut s = loaded(&[(0, flat(1.0, 1.0, SR))]);
        s.apply(SamplerCmd::Volume(slot(0), 0.5));
        s.apply(SamplerCmd::Play(slot(0)));
        let out = render(&mut s, 256);
        assert!(
            out.iter().all(|x| (*x - 0.25).abs() < 1e-6),
            "set while silent: no ramp"
        );
        // Moved while playing: eased there.
        s.apply(SamplerCmd::Volume(slot(0), 2.0));
        let out = render(&mut s, 9600);
        assert!(out[0] < 0.3);
        assert!((out[out.len() - 1] - 1.0).abs() < 1e-3, "clamped to full");
        assert_eq!(s.snapshot()[0].volume, 1.0);
        s.apply(SamplerCmd::Volume(slot(0), f64::NAN));
        assert_eq!(s.snapshot()[0].volume, 0.0);
    }

    #[test]
    fn a_retrigger_starts_again_without_a_click() {
        // A ramp up from 0 to 1 over a second: its start is quiet, so a restart from
        // near full would click if the old pass were cut.
        let n = SR as usize;
        let samples = (0..n).flat_map(|i| [i as f32 / n as f32; 2]).collect();
        let ramp = Arc::new(TrackAudio {
            samples,
            sample_rate: SR,
        });
        let mut s = loaded(&[(0, ramp)]);
        s.apply(SamplerCmd::Play(slot(0)));
        let before = render(&mut s, n / 2);
        let last = before[before.len() - 2];
        assert!((last - 0.5).abs() < 0.01);
        s.apply(SamplerCmd::Play(slot(0)));
        let after = render(&mut s, 1024);
        let left: Vec<f32> = after.chunks(2).map(|f| f[0]).collect();
        assert!((left[0] - last).abs() < 0.01, "starts where it was");
        assert!(
            left.windows(2).all(|w| (w[1] - w[0]).abs() < 0.01),
            "no jump"
        );
        assert!(left[1000] < 0.05, "the new pass, from its start");
        assert!((s.snapshot()[0].position - 1024.0 / SR as f64).abs() < 1e-6);
    }

    #[test]
    fn a_sound_at_another_rate_plays_at_its_own_speed() {
        // A 1 kHz sine recorded at 44.1 kHz, played out at 48 kHz, is still 1 kHz
        // and still half a second long.
        let rate = 44_100;
        let n = rate / 2;
        let samples = (0..n)
            .flat_map(|i| {
                let v = (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / rate as f64).sin();
                [v as f32; 2]
            })
            .collect();
        let sine = Arc::new(TrackAudio {
            samples,
            sample_rate: rate as u32,
        });
        let mut s = loaded(&[(0, sine)]);
        s.apply(SamplerCmd::Play(slot(0)));
        let out = render(&mut s, SR as usize);
        let left: Vec<f32> = out.chunks(2).map(|f| f[0]).collect();
        let sounding = left.iter().rposition(|x| x.abs() > 1e-4).unwrap();
        assert!(
            (sounding as f64 / SR as f64 - 0.5).abs() < 0.002,
            "{sounding}"
        );
        let crossings = left[..sounding]
            .windows(2)
            .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
            .count();
        assert!(
            (crossings as f64 - 500.0).abs() <= 1.0,
            "{crossings} cycles in 0.5 s"
        );
    }

    #[test]
    fn loading_silences_the_slot_and_hands_back_the_old_sound() {
        let first = flat(0.5, 1.0, SR);
        let mut s = loaded(&[(0, first.clone())]);
        s.apply(SamplerCmd::Play(slot(0)));
        render(&mut s, 256);
        let old = s.apply(SamplerCmd::Load {
            slot: slot(0),
            audio: Some(flat(0.1, 1.0, SR)),
        });
        assert!(old.is_some_and(|a| Arc::ptr_eq(&a, &first)));
        assert!(!s.snapshot()[0].playing);
        assert!(render(&mut s, 256).iter().all(|x| *x == 0.0));
        // Emptied: a press does nothing.
        assert!(
            s.apply(SamplerCmd::Load {
                slot: slot(0),
                audio: None
            })
            .is_some()
        );
        s.apply(SamplerCmd::Play(slot(0)));
        assert!(!s.snapshot()[0].loaded && !s.snapshot()[0].playing);
        assert!(render(&mut s, 256).iter().all(|x| *x == 0.0));
        // An empty sound never plays either.
        s.load(slot(1), Some(flat(0.5, 0.0, SR)));
        s.apply(SamplerCmd::Play(slot(1)));
        assert!(!s.snapshot()[1].playing);
    }

    #[test]
    fn stop_all_and_restore() {
        let mut s = loaded(&[(0, flat(0.5, 1.0, SR)), (1, flat(0.5, 1.0, SR))]);
        s.apply(SamplerCmd::Play(slot(0)));
        s.apply(SamplerCmd::Play(slot(1)));
        s.apply(SamplerCmd::StopAll);
        assert!(s.snapshot().iter().all(|v| !v.playing));
        let mut st = SampleState {
            audio: Some(flat(1.0, 1.0, SR)),
            volume: 0.5,
            looping: true,
        };
        assert!(s.restore(slot(5), &mut st).is_none());
        assert!(st.audio.is_none(), "taken");
        let snap = s.snapshot()[5];
        assert!(snap.loaded && snap.looping && snap.volume == 0.5 && !snap.playing);
    }
}
