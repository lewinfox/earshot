//! The pad modes that keep state of their own: keyboard and key shift, as rekordbox
//! runs them on the DDJ-400 (its hardware diagram, note *6). Both lay semitones out
//! over the 8 pads in pages; SHIFT + pad 7 / 8 turns to the page before / after, and
//! a deck starts on page 2, whose pad 5 is the track's own key.
//!
//! - Keyboard: the pads with a hot cue first flash, to pick one (an empty, dark pad
//!   sets one at the playhead). Then each pad plays that hot cue with the key shifted by its
//!   semitones, as the hot cue pads do (held while paused: a preview). SHIFT + pads
//!   1-6 drop the hot cue, to pick again. The key stays where the last pad left it.
//! - Key shift: each pad sets the key shift. Page 5 has steps instead: up / down a
//!   semitone, a fifth up (+7), a fourth down (-5), an octave either way, and the
//!   reset.
//! - Pad FX 1 and 2: each pad plays an effect on its deck for as long as it's held
//!   (the engine's pad effect slot). Each mode has a bank of 16: 8 pads, and 8 more
//!   with SHIFT ([`PAD_FX_BANKS`]).
//!
//! Pads are numbered 0-7 here: 0-3 the top row, 4-7 the bottom.

use earshot_engine::{DeckCmd as EngineCmd, DeckSnap, MAX_DECKS, PadFx, PadFxKind, Press};

use super::lights::Light;
use super::view::PadMode;

/// What a pad does on a page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PadAction {
    /// Shift the key to this many semitones.
    Semitones(i8),
    /// Move the key shift by this many semitones.
    Step(i8),
    /// Back to the track's key.
    Reset,
    /// Key sync: not in earshot yet.
    KeySync,
}

use PadAction::{Reset, Semitones as S, Step};

/// The pages, as the DDJ-400's diagram has them. Keyboard mode has the first four.
pub const PAGES: [[Option<PadAction>; 8]; 5] = [
    [
        Some(S(12)),
        None,
        None,
        None,
        Some(S(8)),
        Some(S(9)),
        Some(S(10)),
        Some(S(11)),
    ],
    [
        Some(S(4)),
        Some(S(5)),
        Some(S(6)),
        Some(S(7)),
        Some(S(0)),
        Some(S(1)),
        Some(S(2)),
        Some(S(3)),
    ],
    [
        Some(S(-4)),
        Some(S(-3)),
        Some(S(-2)),
        Some(S(-1)),
        Some(S(-8)),
        Some(S(-7)),
        Some(S(-6)),
        Some(S(-5)),
    ],
    [
        Some(S(-12)),
        Some(S(-11)),
        Some(S(-10)),
        Some(S(-9)),
        None,
        None,
        None,
        None,
    ],
    [
        Some(PadAction::KeySync),
        Some(Step(1)),
        Some(Step(7)),
        Some(Step(12)),
        Some(Reset),
        Some(Step(-1)),
        Some(Step(-5)),
        Some(Step(-12)),
    ],
];

/// The page a deck starts on (page 2).
pub const FIRST_PAGE: usize = 1;

const fn fx(kind: PadFxKind, beats: f64) -> PadFx {
    PadFx::new(kind, beats)
}

use PadFxKind::{Brake, Echo, HighPass, LowPass, Reverb, Roll, Spin, Trans};

/// The two pad FX banks: PAD FX 1's effects A-P, then PAD FX 2's Q-AF. In each, the
/// first 8 are the pads' and the next 8 the pads' with SHIFT.
///
/// Rekordbox's own defaults couldn't be found written down anywhere, only that they
/// are these kinds of effect at a spread of lengths: this is a spread in that
/// spirit, not a copy of its banks.
pub const PAD_FX_BANKS: [[PadFx; 16]; 2] = [
    [
        fx(Echo, 0.25),
        fx(Echo, 0.5),
        fx(Echo, 0.75),
        fx(Echo, 1.0),
        fx(Roll, 0.125),
        fx(Roll, 0.25),
        fx(Roll, 0.5),
        fx(Roll, 1.0),
        fx(Trans, 0.125),
        fx(Trans, 0.25),
        fx(Trans, 0.5),
        fx(Trans, 1.0),
        fx(LowPass, 2.0),
        fx(HighPass, 2.0),
        fx(Brake, 0.75),
        fx(Spin, 0.5),
    ],
    [
        fx(Reverb, 1.0),
        fx(Reverb, 2.0),
        fx(Reverb, 4.0),
        fx(Echo, 0.125),
        fx(LowPass, 1.0),
        fx(LowPass, 4.0),
        fx(HighPass, 1.0),
        fx(HighPass, 4.0),
        fx(Roll, 0.0625),
        fx(Roll, 2.0),
        fx(Trans, 0.0625),
        fx(Trans, 2.0),
        fx(Brake, 0.5),
        fx(Brake, 2.0),
        fx(Spin, 1.0),
        fx(Spin, 2.0),
    ],
];

/// Which pad FX bank `mode` plays, if it's a pad FX mode.
pub fn pad_fx_bank(mode: PadMode) -> Option<usize> {
    match mode {
        PadMode::PadFx1 => Some(0),
        PadMode::PadFx2 => Some(1),
        _ => None,
    }
}

/// The effect on pad `pad` in a pad FX mode, with SHIFT or not.
pub fn pad_fx(mode: PadMode, pad: usize, shift: bool) -> Option<PadFx> {
    let slot = pad + if shift { 8 } else { 0 };
    PAD_FX_BANKS
        .get(pad_fx_bank(mode)?)?
        .get(slot)
        .copied()
        .filter(|_| pad < 8)
}

/// A length in beats as a pad shows it: 1/4, 3/4, 2.
pub fn beats_label(beats: f64) -> String {
    if beats >= 1.0 && beats.fract() == 0.0 {
        return format!("{beats}");
    }
    let sixteenths = ((beats * 16.0).round() as u32).max(1);
    let common = 1 << sixteenths.trailing_zeros().min(4);
    format!("{}/{}", sixteenths / common, 16 / common)
}

/// The letter rekordbox gives that effect: A-P in bank 1, Q-AF in bank 2.
pub fn pad_fx_letter(mode: PadMode, pad: usize, shift: bool) -> Option<String> {
    let n = pad_fx_bank(mode)? * 16 + pad + if shift { 8 } else { 0 };
    let letter = |i: usize| (b'A' + i as u8) as char;
    Some(match n {
        0..26 => letter(n).to_string(),
        _ => format!("A{}", letter(n - 26)),
    })
}

/// A deck's keyboard and key shift pads.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeckPads {
    /// Keyboard mode's hot cue, and the load it was picked for (a new track picks
    /// again).
    pub keyboard_cue: Option<(u8, u64)>,
    /// The keyboard pad held down (it lets the hot cue go on release).
    held: Option<u8>,
    /// The pad FX pad held down: its mode, its pad and whether it was pressed with
    /// SHIFT. Letting another go (after this one took over from it) does nothing.
    pub fx_held: Option<(PadMode, u8, bool)>,
    pub keyboard_page: usize,
    pub key_shift_page: usize,
}

impl Default for DeckPads {
    fn default() -> DeckPads {
        DeckPads {
            keyboard_cue: None,
            held: None,
            fx_held: None,
            keyboard_page: FIRST_PAGE,
            key_shift_page: FIRST_PAGE,
        }
    }
}

/// Every deck's, for the lights.
pub type AllPads = [DeckPads; MAX_DECKS];

/// How many pages a mode has.
fn pages(mode: PadMode) -> usize {
    match mode {
        PadMode::Keyboard => 4,
        _ => PAGES.len(),
    }
}

impl DeckPads {
    /// The page `mode` is on.
    pub fn page(&self, mode: PadMode) -> usize {
        match mode {
            PadMode::Keyboard => self.keyboard_page,
            _ => self.key_shift_page,
        }
    }

    fn page_mut(&mut self, mode: PadMode) -> &mut usize {
        match mode {
            PadMode::Keyboard => &mut self.keyboard_page,
            _ => &mut self.key_shift_page,
        }
    }

    /// What pad `pad` does in `mode` (keyboard or key shift) on its page.
    pub fn action(&self, mode: PadMode, pad: usize) -> Option<PadAction> {
        PAGES.get(self.page(mode))?.get(pad).copied().flatten()
    }

    /// Keyboard mode's hot cue, if it was picked for what's loaded now.
    pub fn keyboard_cue(&self, d: &DeckSnap) -> Option<u8> {
        self.keyboard_cue
            .filter(|&(_, g)| g == d.generation && d.track_id.is_some())
            .map(|(slot, _)| slot)
    }

    /// Pad `pad` pressed (`down`) or let go in `mode`, with SHIFT held or not, on a
    /// deck in state `d`: what the deck is told.
    pub fn press(
        &mut self,
        mode: PadMode,
        pad: usize,
        shift: bool,
        down: bool,
        d: &DeckSnap,
    ) -> Vec<EngineCmd> {
        if pad_fx_bank(mode).is_some() {
            return self.pad_fx(mode, pad, shift, down);
        }
        if shift {
            if down {
                match pad {
                    6 => *self.page_mut(mode) = self.page(mode).saturating_sub(1),
                    7 => *self.page_mut(mode) = (self.page(mode) + 1).min(pages(mode) - 1),
                    _ if mode == PadMode::Keyboard => self.keyboard_cue = None,
                    _ => {}
                }
            }
            return vec![];
        }
        match mode {
            PadMode::Keyboard => self.keyboard(pad, down, d),
            PadMode::KeyShift if down => match self.action(mode, pad) {
                Some(S(n)) => vec![EngineCmd::KeyShift(n as f64)],
                Some(Step(n)) => vec![EngineCmd::KeyShiftBy(n as f64)],
                Some(Reset) => vec![EngineCmd::KeyShift(0.0)],
                Some(PadAction::KeySync) | None => vec![],
            },
            _ => vec![],
        }
    }

    /// A pad FX pad: its effect goes on when it's pressed, and off when it's let go
    /// (unless another pad has taken over since). The pad let go is the one pressed
    /// whether or not SHIFT is still as it was.
    fn pad_fx(&mut self, mode: PadMode, pad: usize, shift: bool, down: bool) -> Vec<EngineCmd> {
        if down {
            let Some(effect) = pad_fx(mode, pad, shift) else {
                return vec![];
            };
            self.fx_held = Some((mode, pad as u8, shift));
            vec![EngineCmd::PadFx(Some(effect))]
        } else if self
            .fx_held
            .is_some_and(|(m, p, _)| (m, p as usize) == (mode, pad))
        {
            self.release_fx()
        } else {
            vec![]
        }
    }

    /// Let the held pad FX pad go, if there is one (the pads changed mode under it).
    pub fn release_fx(&mut self) -> Vec<EngineCmd> {
        match self.fx_held.take() {
            Some(_) => vec![EngineCmd::PadFx(None)],
            None => vec![],
        }
    }

    fn keyboard(&mut self, pad: usize, down: bool, d: &DeckSnap) -> Vec<EngineCmd> {
        let hot = |slot: u8, press| EngineCmd::Hotcue { slot, press };
        let Some(slot) = self.keyboard_cue(d) else {
            // Picking: the pad's hot cue, set here first if it's empty.
            if !down || d.track_id.is_none() || pad >= d.hotcues.len() {
                return vec![];
            }
            self.keyboard_cue = Some((pad as u8, d.generation));
            self.held = None;
            return match d.hotcues[pad] {
                Some(_) => vec![],
                None => vec![hot(pad as u8, Press::Down), hot(pad as u8, Press::Up)],
            };
        };
        if down {
            let Some(S(n)) = self.action(PadMode::Keyboard, pad) else {
                return vec![];
            };
            self.held = Some(pad as u8);
            vec![EngineCmd::KeyShift(n as f64), hot(slot, Press::Down)]
        } else if self.held == Some(pad as u8) {
            self.held = None;
            vec![hot(slot, Press::Up)]
        } else {
            vec![]
        }
    }

    /// Whether SHIFT gives pad `pad` (0-based) a job in `mode`: turning the page (pads
    /// 7 and 8), or picking another hot cue (keyboard mode's pads 1-6, once one is
    /// picked). The others keep their unshifted light while SHIFT is held.
    pub fn shift_job(&self, mode: PadMode, pad: usize, d: &DeckSnap) -> bool {
        // Every pad FX pad has another effect with SHIFT.
        pad_fx_bank(mode).is_some()
            || pad >= 6
            || (mode == PadMode::Keyboard && self.keyboard_cue(d).is_some())
    }

    /// Pad `pad`'s light in `mode` (SHIFT's message: `shift`), with a track loaded.
    /// Keyboard: the pads with a hot cue flash while picking; then the track's own key (±0)
    /// flashes and the other notes are lit. Key shift: the pad at the key now
    /// flashes. With SHIFT: the page turns that can go, and keyboard's drop.
    /// Pad FX: every pad is lit, and the one held flashes.
    pub fn light(&self, mode: PadMode, pad: usize, shift: bool, d: &DeckSnap) -> Light {
        let on = |b: bool| if b { Light::On } else { Light::Off };
        if pad_fx_bank(mode).is_some() {
            return match pad_fx(mode, pad, shift) {
                Some(_) if self.fx_held == Some((mode, pad as u8, shift)) => Light::Flash,
                Some(_) => Light::On,
                None => Light::Off,
            };
        }
        let picked = mode == PadMode::Keyboard && self.keyboard_cue(d).is_some();
        if shift {
            return match pad {
                6 => on(self.page(mode) > 0),
                7 => on(self.page(mode) + 1 < pages(mode)),
                _ => on(picked),
            };
        }
        if mode == PadMode::Keyboard && !picked {
            let set = d.hotcues.get(pad).is_some_and(Option::is_some);
            return if set { Light::Flash } else { Light::Off };
        }
        match self.action(mode, pad) {
            Some(S(0)) if mode == PadMode::Keyboard => Light::Flash,
            Some(S(n)) if mode == PadMode::KeyShift && n as f64 == d.key_shift.round() => {
                Light::Flash
            }
            Some(PadAction::KeySync) | None => Light::Off,
            Some(_) => Light::On,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded(generation: u64) -> DeckSnap {
        DeckSnap {
            track_id: Some(1),
            generation,
            ..Default::default()
        }
    }

    fn shift_of(cmds: &[EngineCmd]) -> Option<f64> {
        cmds.iter().find_map(|c| match c {
            EngineCmd::KeyShift(n) => Some(*n),
            _ => None,
        })
    }

    #[test]
    fn pad_fx_banks_are_lettered_a_to_af() {
        let letter = |mode, pad, shift| pad_fx_letter(mode, pad, shift).unwrap();
        assert_eq!(letter(PadMode::PadFx1, 0, false), "A");
        assert_eq!(letter(PadMode::PadFx1, 7, false), "H");
        assert_eq!(letter(PadMode::PadFx1, 0, true), "I");
        assert_eq!(letter(PadMode::PadFx1, 7, true), "P");
        assert_eq!(letter(PadMode::PadFx2, 0, false), "Q");
        assert_eq!(letter(PadMode::PadFx2, 1, true), "Z");
        assert_eq!(letter(PadMode::PadFx2, 2, true), "AA");
        assert_eq!(letter(PadMode::PadFx2, 7, true), "AF");
        assert_eq!(pad_fx_letter(PadMode::Keyboard, 0, false), None);
        // 32 effects, no two the same, each of a length the engine takes as it is.
        let all: Vec<PadFx> = PAD_FX_BANKS.iter().flatten().copied().collect();
        for (i, a) in all.iter().enumerate() {
            assert!((1.0 / 16.0..=16.0).contains(&a.beats), "{a:?}");
            assert!(!all[..i].contains(a), "{a:?} twice");
        }
        assert_eq!(pad_fx(PadMode::PadFx1, 0, false), Some(PAD_FX_BANKS[0][0]));
        assert_eq!(pad_fx(PadMode::PadFx2, 7, true), Some(PAD_FX_BANKS[1][15]));
        assert_eq!(pad_fx(PadMode::PadFx1, 8, false), None, "8 pads");
        assert_eq!(pad_fx(PadMode::HotCue, 0, false), None);
        let labels = [0.0625, 0.25, 0.75, 1.0, 1.5, 4.0].map(beats_label);
        assert_eq!(labels, ["1/16", "1/4", "3/4", "1", "3/2", "4"]);
    }

    #[test]
    fn pad_fx_is_on_while_its_pad_is_held() {
        let mut p = DeckPads::default();
        let d = loaded(1);
        let a = PAD_FX_BANKS[0][0];
        let j = PAD_FX_BANKS[0][9];
        // Pad 1 down: effect A on, and its light flashes; the others are lit.
        assert_eq!(
            p.press(PadMode::PadFx1, 0, false, true, &d),
            vec![EngineCmd::PadFx(Some(a))]
        );
        assert_eq!(p.light(PadMode::PadFx1, 0, false, &d), Light::Flash);
        assert_eq!(p.light(PadMode::PadFx1, 1, false, &d), Light::On);
        assert_eq!(
            p.light(PadMode::PadFx1, 0, true, &d),
            Light::On,
            "SHIFT's is another"
        );
        assert_eq!(
            p.light(PadMode::PadFx2, 0, false, &d),
            Light::On,
            "and bank 2's"
        );
        // SHIFT + pad 2 takes over: letting pad 1 go then does nothing.
        assert_eq!(
            p.press(PadMode::PadFx1, 1, true, true, &d),
            vec![EngineCmd::PadFx(Some(j))]
        );
        assert!(p.press(PadMode::PadFx1, 0, false, false, &d).is_empty());
        assert_eq!(p.light(PadMode::PadFx1, 1, true, &d), Light::Flash);
        assert_eq!(
            p.press(PadMode::PadFx1, 1, true, false, &d),
            vec![EngineCmd::PadFx(None)]
        );
        assert_eq!(p.fx_held, None);
        assert_eq!(p.light(PadMode::PadFx1, 1, true, &d), Light::On);
        // SHIFT turns no page here, and every shifted pad has a light of its own.
        assert_eq!(p.keyboard_page, FIRST_PAGE);
        assert!(p.shift_job(PadMode::PadFx2, 0, &d));
        // Bank 2's first pad, pressed with SHIFT (effect Y) and let go after SHIFT.
        let y = PAD_FX_BANKS[1][8];
        assert_eq!(
            p.press(PadMode::PadFx2, 0, true, true, &d),
            vec![EngineCmd::PadFx(Some(y))]
        );
        assert_eq!(
            p.press(PadMode::PadFx2, 0, false, false, &d),
            vec![EngineCmd::PadFx(None)]
        );
        // Held when the pads change mode: let go then, and its own release is nothing.
        p.press(PadMode::PadFx2, 3, false, true, &d);
        assert_eq!(p.release_fx(), vec![EngineCmd::PadFx(None)]);
        assert!(p.release_fx().is_empty());
        assert!(p.press(PadMode::PadFx2, 3, false, false, &d).is_empty());
    }

    #[test]
    fn page_two_has_the_root_on_pad_5() {
        let p = DeckPads::default();
        let notes: Vec<_> = (0..8).map(|i| p.action(PadMode::Keyboard, i)).collect();
        assert_eq!(notes[4], Some(S(0)));
        assert_eq!(notes[5], Some(S(1)));
        assert_eq!(notes[0], Some(S(4)));
        assert_eq!(notes[3], Some(S(7)));
    }

    #[test]
    fn keyboard_picks_a_hot_cue_then_plays_it_shifted() {
        let mut p = DeckPads::default();
        let mut d = loaded(1);
        d.hotcues[2] = Some(10.0);
        // Picking: C (set) flashes, A (empty) is dark; pressing C picks it, and sends
        // nothing.
        assert_eq!(p.light(PadMode::Keyboard, 2, false, &d), Light::Flash);
        assert_eq!(p.light(PadMode::Keyboard, 0, false, &d), Light::Off);
        assert!(p.press(PadMode::Keyboard, 2, false, true, &d).is_empty());
        assert!(p.press(PadMode::Keyboard, 2, false, false, &d).is_empty());
        assert_eq!(p.keyboard_cue(&d), Some(2));
        assert_eq!(
            p.light(PadMode::Keyboard, 4, false, &d),
            Light::Flash,
            "the root"
        );
        assert_eq!(p.light(PadMode::Keyboard, 5, false, &d), Light::On);
        // Pad 2 (top row, second) is +5: shift, then the hot cue, down and up.
        let down = p.press(PadMode::Keyboard, 1, false, true, &d);
        assert_eq!(shift_of(&down), Some(5.0));
        assert!(matches!(
            down[1],
            EngineCmd::Hotcue {
                slot: 2,
                press: Press::Down
            }
        ));
        let up = p.press(PadMode::Keyboard, 1, false, false, &d);
        assert!(matches!(
            up[..],
            [EngineCmd::Hotcue {
                slot: 2,
                press: Press::Up
            }]
        ));
        // A new track: pick again.
        assert_eq!(p.keyboard_cue(&loaded(2)), None);
    }

    #[test]
    fn keyboard_sets_an_empty_hot_cue_when_picked() {
        let mut p = DeckPads::default();
        let d = loaded(1);
        let cmds = p.press(PadMode::Keyboard, 5, false, true, &d);
        assert!(matches!(
            cmds[..],
            [
                EngineCmd::Hotcue {
                    slot: 5,
                    press: Press::Down
                },
                EngineCmd::Hotcue {
                    slot: 5,
                    press: Press::Up
                }
            ]
        ));
        assert_eq!(p.keyboard_cue(&d), Some(5));
    }

    #[test]
    fn shift_turns_pages_and_drops_the_cue() {
        let mut p = DeckPads::default();
        let mut d = loaded(1);
        d.hotcues[0] = Some(1.0);
        p.press(PadMode::Keyboard, 0, false, true, &d);
        // SHIFT + pad 7: page 1 (up to +12); again: still page 1.
        p.press(PadMode::Keyboard, 6, true, true, &d);
        p.press(PadMode::Keyboard, 6, true, true, &d);
        assert_eq!(p.keyboard_page, 0);
        assert_eq!(
            p.light(PadMode::Keyboard, 6, true, &d),
            Light::Off,
            "no page before"
        );
        assert_eq!(
            shift_of(&p.press(PadMode::Keyboard, 0, false, true, &d)),
            Some(12.0)
        );
        // SHIFT + pad 8 to the last keyboard page (4), not key shift's page 5.
        for _ in 0..5 {
            p.press(PadMode::Keyboard, 7, true, true, &d);
        }
        assert_eq!(p.keyboard_page, 3);
        assert_eq!(
            shift_of(&p.press(PadMode::Keyboard, 3, false, true, &d)),
            Some(-9.0)
        );
        assert!(
            p.press(PadMode::Keyboard, 4, false, true, &d).is_empty(),
            "no note there"
        );
        // SHIFT + pad 3 drops the cue: picking again.
        p.press(PadMode::Keyboard, 2, true, true, &d);
        assert_eq!(p.keyboard_cue(&d), None);
        assert_eq!(p.key_shift_page, FIRST_PAGE, "key shift's page is its own");
    }

    #[test]
    fn key_shift_pads_set_and_step_the_key() {
        let mut p = DeckPads::default();
        let mut d = loaded(1);
        assert_eq!(
            shift_of(&p.press(PadMode::KeyShift, 7, false, true, &d)),
            Some(3.0)
        );
        assert!(
            p.press(PadMode::KeyShift, 7, false, false, &d).is_empty(),
            "nothing on release"
        );
        d.key_shift = 3.0;
        assert_eq!(
            p.light(PadMode::KeyShift, 7, false, &d),
            Light::Flash,
            "the key now"
        );
        assert_eq!(p.light(PadMode::KeyShift, 4, false, &d), Light::On);
        for _ in 0..3 {
            p.press(PadMode::KeyShift, 7, true, true, &d);
        }
        assert_eq!(p.key_shift_page, 4);
        assert!(matches!(
            p.press(PadMode::KeyShift, 2, false, true, &d)[..],
            [EngineCmd::KeyShiftBy(7.0)]
        ));
        assert!(matches!(
            p.press(PadMode::KeyShift, 5, false, true, &d)[..],
            [EngineCmd::KeyShiftBy(-1.0)]
        ));
        assert_eq!(
            shift_of(&p.press(PadMode::KeyShift, 4, false, true, &d)),
            Some(0.0)
        );
        assert!(
            p.press(PadMode::KeyShift, 0, false, true, &d).is_empty(),
            "key sync: not yet"
        );
        assert_eq!(p.light(PadMode::KeyShift, 0, false, &d), Light::Off);
    }
}
