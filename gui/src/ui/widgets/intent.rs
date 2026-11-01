//! What a control or message means, which picks its colour: one vocabulary for
//! buttons ([`super::Button`]), notes ([`super::Note`]) and pills ([`super::Pill`]).

use egui::Color32;

use crate::ui::theme::THEME;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Intent {
    /// Nothing in particular.
    #[default]
    Neutral,
    /// Worth knowing; something is going on (the accent).
    Info,
    /// It worked; all is well.
    Success,
    /// Look before going on.
    Warning,
    /// It failed, or can't be undone.
    Danger,
}

impl Intent {
    /// Its colour: a pill's border, a danger button's fill.
    pub fn colour(self) -> Color32 {
        match self {
            Intent::Neutral => THEME.line,
            Intent::Info => THEME.accent,
            Intent::Success => THEME.ok,
            Intent::Warning => THEME.warn,
            Intent::Danger => THEME.bad,
        }
    }

    /// Text in it: plain for neutral and info, else its colour.
    pub fn text_colour(self) -> Color32 {
        match self {
            Intent::Neutral | Intent::Info => THEME.text,
            i => i.colour(),
        }
    }
}
