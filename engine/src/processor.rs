//! The processing chain: a [`Processor`] trait with indexed, smoothed parameters, the
//! channel strip built from processors (trim → EQ → filter), and the master's chain.
//!
//! Processors are built off the audio thread; `process` and `set_param` must not
//! allocate.

use crate::command::{ChannelParam, ChannelState, MasterParam, XfSide};
use crate::dsp::{self, DjFilter, Eq3, Smoothed};

/// Seconds for knobs and faders to settle, so moves don't click.
pub const KNOB_SECS: f64 = 0.01;

/// One parameter of a processor.
#[derive(Debug, Clone, Copy)]
pub struct ParamSpec {
    pub name: &'static str,
    pub min: f64,
    pub max: f64,
    pub default: f64,
}

pub trait Processor: Send {
    fn params(&self) -> &'static [ParamSpec];
    /// Set parameter `i`'s target, clamped to its range. The processor eases towards
    /// it. Unknown `i` is ignored.
    fn set_param(&mut self, i: usize, v: f64);
    /// Parameter `i`'s target (NaN for an unknown `i`).
    fn param(&self, i: usize) -> f64;
    /// Process interleaved stereo in place.
    fn process(&mut self, buf: &mut [f32]);
    /// Forget any internal state (filter memory) and jump parameters to their targets.
    fn reset(&mut self);
}

/// `N` parameters eased towards their targets sample by sample.
struct Params<const N: usize> {
    spec: &'static [ParamSpec; N],
    v: [Smoothed; N],
    ease: f64,
}

impl<const N: usize> Params<N> {
    fn new(spec: &'static [ParamSpec; N], sr: f64) -> Self {
        Params {
            spec,
            v: spec.map(|s| Smoothed::new(s.default)),
            ease: dsp::ease_per_sample(KNOB_SECS, sr),
        }
    }

    fn set(&mut self, i: usize, v: f64) {
        if let (Some(s), Some(p)) = (self.spec.get(i), self.v.get_mut(i)) {
            p.target = v.clamp(s.min, s.max);
        }
    }

    fn get(&self, i: usize) -> f64 {
        self.v.get(i).map_or(f64::NAN, |p| p.target)
    }

    #[inline]
    fn next(&mut self, i: usize) -> f64 {
        self.v[i].next(self.ease)
    }

    fn settle(&mut self) {
        for p in &mut self.v {
            p.current = p.target;
        }
    }
}

macro_rules! params_impl {
    () => {
        fn params(&self) -> &'static [ParamSpec] {
            self.p.spec
        }
        fn set_param(&mut self, i: usize, v: f64) {
            self.p.set(i, v)
        }
        fn param(&self, i: usize) -> f64 {
            self.p.get(i)
        }
    };
}

// ---- gain ----------------------------------------------------------------------------

/// A channel's gain: the knob (trim) times the loaded track's auto-gain.
pub struct Trim {
    p: Params<2>,
}

impl Trim {
    pub const GAIN: usize = 0;
    pub const AUTO: usize = 1;
    const SPEC: [ParamSpec; 2] = [
        ParamSpec {
            name: "gain",
            min: 0.0,
            max: 4.0,
            default: 1.0,
        },
        ParamSpec {
            name: "auto gain",
            min: 0.05,
            max: 8.0,
            default: 1.0,
        },
    ];

    pub fn new(sr: f64) -> Trim {
        Trim {
            p: Params::new(&Self::SPEC, sr),
        }
    }
}

impl Processor for Trim {
    params_impl!();

    fn process(&mut self, buf: &mut [f32]) {
        for frame in buf.chunks_exact_mut(2) {
            let g = (self.p.next(0) * self.p.next(1)) as f32;
            frame[0] *= g;
            frame[1] *= g;
        }
    }

    fn reset(&mut self) {
        self.p.settle();
    }
}

// ---- EQ ------------------------------------------------------------------------------

/// The 3-band isolator: low, mid and high gains, linear (0 kills a band, 1 is flat).
pub struct Eq {
    p: Params<3>,
    eq: Eq3,
    sr: f64,
}

impl Eq {
    const SPEC: [ParamSpec; 3] = [
        ParamSpec {
            name: "low",
            min: 0.0,
            max: 4.0,
            default: 1.0,
        },
        ParamSpec {
            name: "mid",
            min: 0.0,
            max: 4.0,
            default: 1.0,
        },
        ParamSpec {
            name: "high",
            min: 0.0,
            max: 4.0,
            default: 1.0,
        },
    ];

    pub fn new(sr: f64) -> Eq {
        Eq {
            p: Params::new(&Self::SPEC, sr),
            eq: Eq3::new(sr),
            sr,
        }
    }
}

impl Processor for Eq {
    params_impl!();

    fn process(&mut self, buf: &mut [f32]) {
        for frame in buf.chunks_exact_mut(2) {
            let g = [self.p.next(0), self.p.next(1), self.p.next(2)];
            for (ch, s) in frame.iter_mut().enumerate() {
                *s = self.eq.run(ch, *s as f64, g) as f32;
            }
        }
    }

    fn reset(&mut self) {
        self.p.settle();
        self.eq = Eq3::new(self.sr);
    }
}

// ---- filter --------------------------------------------------------------------------

/// The one-knob DJ filter: -1 low-pass .. 0 off .. 1 high-pass. The knob is eased and
/// the filter re-tuned every [`Filter::RETUNE`] frames while it moves.
pub struct Filter {
    p: Params<1>,
    filter: DjFilter,
    /// The knob position the filter is tuned to.
    tuned: f64,
    sr: f64,
}

impl Filter {
    const RETUNE: usize = 32;
    const SPEC: [ParamSpec; 1] = [ParamSpec {
        name: "filter",
        min: -1.0,
        max: 1.0,
        default: 0.0,
    }];

    pub fn new(sr: f64) -> Filter {
        Filter {
            p: Params::new(&Self::SPEC, sr),
            filter: DjFilter::new(sr),
            tuned: 0.0,
            sr,
        }
    }
}

impl Processor for Filter {
    params_impl!();

    fn process(&mut self, buf: &mut [f32]) {
        for chunk in buf.chunks_mut(2 * Self::RETUNE) {
            let mut v = 0.0;
            for _ in 0..chunk.len() / 2 {
                v = self.p.next(0);
            }
            if v != self.tuned {
                self.filter.set(v);
                self.tuned = v;
            }
            if !self.filter.active() {
                continue;
            }
            for frame in chunk.chunks_exact_mut(2) {
                for (ch, s) in frame.iter_mut().enumerate() {
                    *s = self.filter.run(ch, *s as f64) as f32;
                }
            }
        }
    }

    fn reset(&mut self) {
        self.p.settle();
        self.filter = DjFilter::new(self.sr);
        self.tuned = 0.0;
        self.filter.set(self.p.get(0));
        self.tuned = self.p.get(0);
    }
}

// ---- master --------------------------------------------------------------------------

/// The main output's level and balance.
pub struct MainOut {
    p: Params<2>,
}

impl MainOut {
    pub const GAIN: usize = 0;
    pub const BALANCE: usize = 1;
    const SPEC: [ParamSpec; 2] = [
        ParamSpec {
            name: "gain",
            min: 0.0,
            max: 5.0,
            default: 1.0,
        },
        ParamSpec {
            name: "balance",
            min: -1.0,
            max: 1.0,
            default: 0.0,
        },
    ];

    pub fn new(sr: f64) -> MainOut {
        MainOut {
            p: Params::new(&Self::SPEC, sr),
        }
    }
}

impl Processor for MainOut {
    params_impl!();

    fn process(&mut self, buf: &mut [f32]) {
        for frame in buf.chunks_exact_mut(2) {
            let g = self.p.next(0);
            let bal = self.p.next(1);
            frame[0] *= (g * (1.0 - bal).min(1.0)) as f32;
            frame[1] *= (g * (1.0 + bal).min(1.0)) as f32;
        }
    }

    fn reset(&mut self) {
        self.p.settle();
    }
}

// ---- channel -------------------------------------------------------------------------

/// One mixer channel: a chain of processors, then the fader, crossfader side and
/// headphone cue. Kept apart from the deck so loading a track leaves the mixer as it is.
pub struct Channel {
    /// [`Channel::TRIM`], [`Channel::EQ`], [`Channel::FILTER`], in that order.
    chain: Vec<Box<dyn Processor>>,
    pub fader: Smoothed,
    pub pfl: bool,
    pub crossfader: XfSide,
    /// Straight to the main mix: no fader, crossfader or headphone cue (Prepare
    /// mode's player).
    pub direct: bool,
    /// Level meter (peak after the chain, before the fader), linear.
    pub vu: f32,
}

impl Channel {
    const TRIM: usize = 0;
    const EQ: usize = 1;
    const FILTER: usize = 2;

    pub fn new(sr: f64, crossfader: XfSide) -> Channel {
        Channel {
            chain: vec![
                Box::new(Trim::new(sr)),
                Box::new(Eq::new(sr)),
                Box::new(Filter::new(sr)),
            ],
            fader: Smoothed::new(1.0),
            pfl: false,
            crossfader,
            direct: false,
            vu: 0.0,
        }
    }

    /// Which processor and parameter a control is.
    fn slot(p: ChannelParam) -> Option<(usize, usize)> {
        match p {
            ChannelParam::Volume => None,
            ChannelParam::Trim => Some((Self::TRIM, Trim::GAIN)),
            ChannelParam::Eq(band) if band < 3 => Some((Self::EQ, band)),
            ChannelParam::Eq(_) => None,
            ChannelParam::Filter => Some((Self::FILTER, 0)),
        }
    }

    pub fn set(&mut self, p: ChannelParam, v: f64) {
        match p {
            ChannelParam::Volume => self.fader.target = v.clamp(0.0, 1.0),
            _ => {
                if let Some((i, k)) = Self::slot(p) {
                    self.chain[i].set_param(k, v);
                }
            }
        }
    }

    pub fn get(&self, p: ChannelParam) -> f64 {
        match p {
            ChannelParam::Volume => self.fader.target,
            _ => Self::slot(p).map_or(f64::NAN, |(i, k)| self.chain[i].param(k)),
        }
    }

    pub fn set_auto_gain(&mut self, g: f64) {
        self.chain[Self::TRIM].set_param(Trim::AUTO, g);
    }

    pub fn auto_gain(&self) -> f64 {
        self.chain[Self::TRIM].param(Trim::AUTO)
    }

    pub fn state(&self) -> ChannelState {
        ChannelState {
            volume: self.fader.target,
            trim: self.get(ChannelParam::Trim),
            eq: [0, 1, 2].map(|b| self.get(ChannelParam::Eq(b))),
            filter: self.get(ChannelParam::Filter),
            pfl: self.pfl,
            crossfader: self.crossfader,
        }
    }

    pub fn restore(&mut self, s: &ChannelState) {
        self.set(ChannelParam::Volume, s.volume);
        self.set(ChannelParam::Trim, s.trim);
        for (b, v) in s.eq.iter().enumerate() {
            self.set(ChannelParam::Eq(b), *v);
        }
        self.set(ChannelParam::Filter, s.filter);
        self.pfl = s.pfl;
        self.crossfader = s.crossfader;
    }

    /// Run the chain on one block of interleaved stereo, in place. Returns its peak.
    pub fn process(&mut self, buf: &mut [f32]) -> f32 {
        for p in &mut self.chain {
            p.process(buf);
        }
        buf.iter().fold(0f32, |m, s| m.max(s.abs()))
    }
}

/// The master's settings that are processors (the rest are mixed by hand).
pub struct MasterChain {
    chain: Vec<Box<dyn Processor>>,
}

impl MasterChain {
    const MAIN: usize = 0;

    pub fn new(sr: f64) -> MasterChain {
        MasterChain {
            chain: vec![Box::new(MainOut::new(sr))],
        }
    }

    /// For [`MasterParam::Gain`] and [`MasterParam::Balance`]; false for the others.
    pub fn set(&mut self, p: MasterParam, v: f64) -> bool {
        match p {
            MasterParam::Gain => self.chain[Self::MAIN].set_param(MainOut::GAIN, v),
            MasterParam::Balance => self.chain[Self::MAIN].set_param(MainOut::BALANCE, v),
            _ => return false,
        }
        true
    }

    pub fn get(&self, p: MasterParam) -> f64 {
        match p {
            MasterParam::Gain => self.chain[Self::MAIN].param(MainOut::GAIN),
            MasterParam::Balance => self.chain[Self::MAIN].param(MainOut::BALANCE),
            _ => f64::NAN,
        }
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        for p in &mut self.chain {
            p.process(buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 44_100.0;

    #[test]
    fn params_are_clamped_and_eased() {
        let mut t = Trim::new(SR);
        assert_eq!(t.params().len(), 2);
        t.set_param(Trim::GAIN, 10.0);
        assert_eq!(t.param(Trim::GAIN), 4.0);
        t.set_param(7, 1.0); // ignored
        assert!(t.param(7).is_nan());
        // One sample in, the gain has only started to move.
        let mut buf = [1.0f32, 1.0];
        t.process(&mut buf);
        assert!(buf[0] > 1.0 && buf[0] < 1.1, "{}", buf[0]);
        // A few time constants later it's there.
        let mut buf = vec![1.0f32; 2 * (SR * 0.2) as usize];
        t.process(&mut buf);
        assert!((buf[buf.len() - 1] - 4.0).abs() < 1e-4);
    }

    #[test]
    fn channel_maps_controls_to_its_chain() {
        let mut ch = Channel::new(SR, XfSide::Left);
        ch.set(ChannelParam::Eq(1), 0.0);
        ch.set(ChannelParam::Eq(5), 0.0); // no such band
        ch.set(ChannelParam::Filter, -3.0);
        ch.set(ChannelParam::Volume, 0.5);
        let s = ch.state();
        assert_eq!(s.eq, [1.0, 0.0, 1.0]);
        assert_eq!(s.filter, -1.0);
        assert_eq!(s.volume, 0.5);
        let mut other = Channel::new(SR, XfSide::Right);
        other.restore(&s);
        assert_eq!(other.state(), s);
    }
}
