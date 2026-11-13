//! The pad modes that keep state of their own: keyboard and key shift, as rekordbox
//! runs them on the DDJ-400 (its hardware diagram, note *6). Both lay semitones out
//! over the 8 pads in pages; SHIFT + pad 7 / 8 turns to the page before / after, and
//! a deck starts on page 2, whose pad 5 is the track's own key.
//!
//! - Keyboard: the pads first flash, to pick a hot cue (an empty pad sets one at the
//!   playhead). Then each pad plays that hot cue with the key shifted by its
//!   semitones, as the hot cue pads do (held while paused: a preview). SHIFT + pads
//!   1-6 drop the hot cue, to pick again. The key stays where the last pad left it.
//! - Key shift: each pad sets the key shift. Page 5 has steps instead: up / down a
//!   semitone, a fifth up (+7), a fourth down (-5), an octave either way, and the
//!   reset.
//!
//! Pads are numbered 0-7 here: 0-3 the top row, 4-7 the bottom.

use earshot_engine::{DeckCmd as EngineCmd, DeckSnap, MAX_DECKS, Press};

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
    [Some(S(12)), None, None, None, Some(S(8)), Some(S(9)), Some(S(10)), Some(S(11))],
    [Some(S(4)), Some(S(5)), Some(S(6)), Some(S(7)), Some(S(0)), Some(S(1)), Some(S(2)), Some(S(3))],
    [Some(S(-4)), Some(S(-3)), Some(S(-2)), Some(S(-1)), Some(S(-8)), Some(S(-7)), Some(S(-6)), Some(S(-5))],
    [Some(S(-12)), Some(S(-11)), Some(S(-10)), Some(S(-9)), None, None, None, None],
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

/// A deck's keyboard and key shift pads.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeckPads {
    /// Keyboard mode's hot cue, and the load it was picked for (a new track picks
    /// again).
    pub keyboard_cue: Option<(u8, u64)>,
    /// The keyboard pad held down (it lets the hot cue go on release).
    held: Option<u8>,
    pub keyboard_page: usize,
    pub key_shift_page: usize,
}

impl Default for DeckPads {
    fn default() -> DeckPads {
        DeckPads {
            keyboard_cue: None,
            held: None,
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
    pub fn press(&mut self, mode: PadMode, pad: usize, shift: bool, down: bool, d: &DeckSnap) -> Vec<EngineCmd> {
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

    /// Pad `pad`'s light in `mode` (SHIFT's message: `shift`), with a track loaded.
    /// Keyboard: every pad flashes while picking; then the track's own key (±0)
    /// flashes and the other notes are lit. Key shift: the pad at the key now
    /// flashes. With SHIFT: the page turns that can go, and keyboard's drop.
    pub fn light(&self, mode: PadMode, pad: usize, shift: bool, d: &DeckSnap) -> Light {
        let on = |b: bool| if b { Light::On } else { Light::Off };
        let picked = mode == PadMode::Keyboard && self.keyboard_cue(d).is_some();
        if shift {
            return match pad {
                6 => on(self.page(mode) > 0),
                7 => on(self.page(mode) + 1 < pages(mode)),
                _ => on(picked),
            };
        }
        if mode == PadMode::Keyboard && !picked {
            return Light::Flash;
        }
        match self.action(mode, pad) {
            Some(S(0)) if mode == PadMode::Keyboard => Light::Flash,
            Some(S(n)) if mode == PadMode::KeyShift && n as f64 == d.key_shift.round() => Light::Flash,
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
        // Picking: every pad flashes; pressing C picks it, and sends nothing.
        assert_eq!(p.light(PadMode::Keyboard, 0, false, &d), Light::Flash);
        assert!(p.press(PadMode::Keyboard, 2, false, true, &d).is_empty());
        assert!(p.press(PadMode::Keyboard, 2, false, false, &d).is_empty());
        assert_eq!(p.keyboard_cue(&d), Some(2));
        assert_eq!(p.light(PadMode::Keyboard, 4, false, &d), Light::Flash, "the root");
        assert_eq!(p.light(PadMode::Keyboard, 5, false, &d), Light::On);
        // Pad 2 (top row, second) is +5: shift, then the hot cue, down and up.
        let down = p.press(PadMode::Keyboard, 1, false, true, &d);
        assert_eq!(shift_of(&down), Some(5.0));
        assert!(matches!(down[1], EngineCmd::Hotcue { slot: 2, press: Press::Down }));
        let up = p.press(PadMode::Keyboard, 1, false, false, &d);
        assert!(matches!(up[..], [EngineCmd::Hotcue { slot: 2, press: Press::Up }]));
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
            [EngineCmd::Hotcue { slot: 5, press: Press::Down }, EngineCmd::Hotcue { slot: 5, press: Press::Up }]
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
        assert_eq!(p.light(PadMode::Keyboard, 6, true, &d), Light::Off, "no page before");
        assert_eq!(shift_of(&p.press(PadMode::Keyboard, 0, false, true, &d)), Some(12.0));
        // SHIFT + pad 8 to the last keyboard page (4), not key shift's page 5.
        for _ in 0..5 {
            p.press(PadMode::Keyboard, 7, true, true, &d);
        }
        assert_eq!(p.keyboard_page, 3);
        assert_eq!(shift_of(&p.press(PadMode::Keyboard, 3, false, true, &d)), Some(-9.0));
        assert!(p.press(PadMode::Keyboard, 4, false, true, &d).is_empty(), "no note there");
        // SHIFT + pad 3 drops the cue: picking again.
        p.press(PadMode::Keyboard, 2, true, true, &d);
        assert_eq!(p.keyboard_cue(&d), None);
        assert_eq!(p.key_shift_page, FIRST_PAGE, "key shift's page is its own");
    }

    #[test]
    fn key_shift_pads_set_and_step_the_key() {
        let mut p = DeckPads::default();
        let mut d = loaded(1);
        assert_eq!(shift_of(&p.press(PadMode::KeyShift, 7, false, true, &d)), Some(3.0));
        assert!(p.press(PadMode::KeyShift, 7, false, false, &d).is_empty(), "nothing on release");
        d.key_shift = 3.0;
        assert_eq!(p.light(PadMode::KeyShift, 7, false, &d), Light::Flash, "the key now");
        assert_eq!(p.light(PadMode::KeyShift, 4, false, &d), Light::On);
        for _ in 0..3 {
            p.press(PadMode::KeyShift, 7, true, true, &d);
        }
        assert_eq!(p.key_shift_page, 4);
        assert!(matches!(p.press(PadMode::KeyShift, 2, false, true, &d)[..], [EngineCmd::KeyShiftBy(7.0)]));
        assert!(matches!(p.press(PadMode::KeyShift, 5, false, true, &d)[..], [EngineCmd::KeyShiftBy(-1.0)]));
        assert_eq!(shift_of(&p.press(PadMode::KeyShift, 4, false, true, &d)), Some(0.0));
        assert!(p.press(PadMode::KeyShift, 0, false, true, &d).is_empty(), "key sync: not yet");
        assert_eq!(p.light(PadMode::KeyShift, 0, false, &d), Light::Off);
    }
}
