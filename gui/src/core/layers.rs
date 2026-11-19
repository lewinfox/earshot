//! Which deck each hand plays. With four decks showing, the left hand (the keyboard's
//! left-hand keys, a controller's left deck) plays deck 1 or deck 3, and the right
//! hand deck 2 or deck 4: its "layer". The top bar's `[1] | [2]` switch, the
//! `deck_N_layer_toggle` commands (G and H) or a controller's own button change it.
//!
//! The UI owns the choice (`ui::prefs::UiPrefs::layers`) and writes it here each
//! frame; the MIDI thread and the lights thread read it from here, without the UI,
//! so a controller's messages and lights follow the switch at once.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use earshot_engine::DeckId;

use super::bus::{Control, DeckControl};

/// The hands' layers, shared between threads.
#[derive(Clone, Default)]
pub struct Layers(Arc<Inner>);

#[derive(Default)]
struct Inner {
    /// The left and right hands are on their second deck (3, 4).
    on: [AtomicBool; 2],
    /// Four decks show: the layers only count then.
    four: AtomicBool,
}

impl Layers {
    /// Set both hands' layers, and whether four decks show.
    pub fn set(&self, on: [bool; 2], four: bool) {
        self.0.on[0].store(on[0], Relaxed);
        self.0.on[1].store(on[1], Relaxed);
        self.0.four.store(four, Relaxed);
    }

    /// The left (`side` 0) or right (1) hand is on its second deck.
    pub fn on(&self, side: usize) -> bool {
        self.0.four.load(Relaxed) && self.0.on[side.min(1)].load(Relaxed)
    }

    /// The deck the left (`side` 0) or right (1) hand plays: 1 or 3, 2 or 4.
    pub fn deck(&self, side: usize) -> DeckId {
        let side = side.min(1);
        DeckId::new(side + if self.on(side) { 2 } else { 0 }).expect("a deck")
    }

    /// The deck a control for `deck` goes to: deck 1's and 2's (a hand's) go to the
    /// deck that hand plays now; decks 3 and 4 (a four-deck controller's own) stay.
    pub fn deck_for(&self, deck: DeckId) -> DeckId {
        if deck.index() < 2 {
            self.deck(deck.index())
        } else {
            deck
        }
    }

    /// `control`, sent to the deck its hand plays now. The switch itself stays with
    /// its hand.
    pub fn layered(&self, control: Control) -> Control {
        match control {
            Control::Deck(_, DeckControl::LayerToggle) => control,
            Control::Deck(d, c) => Control::Deck(self.deck_for(d), c),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deck(i: usize) -> DeckId {
        DeckId::new(i).unwrap()
    }

    #[test]
    fn a_hands_controls_go_to_the_deck_it_plays() {
        let l = Layers::default();
        let play = |d| Control::Deck(deck(d), DeckControl::Play);
        // Two decks showing: the layers don't count.
        l.set([true, true], false);
        assert_eq!(l.layered(play(0)), play(0));
        l.set([true, false], true);
        assert_eq!(l.layered(play(0)), play(2));
        assert_eq!(l.layered(play(1)), play(1));
        // A four-deck controller's own decks 3 and 4 are left alone.
        assert_eq!(l.layered(play(2)), play(2));
        assert_eq!(l.layered(play(3)), play(3));
        // The switch stays with its hand, whichever deck that plays.
        let switch = Control::Deck(deck(0), DeckControl::LayerToggle);
        assert_eq!(l.layered(switch), switch);
    }
}
