//! The keyboard, through the keymap.

use super::*;
use crate::core::bus::Control;

impl EarshotApp {
    /// Keys do what the keymap says. Deck keys work unless a text field has focus;
    /// Esc leaves the field.
    pub(super) fn keyboard(&mut self, ctx: &egui::Context) {
        let typing = ctx.wants_keyboard_input();
        let events: Vec<_> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match *e {
                    egui::Event::Key {
                        key,
                        physical_key,
                        pressed,
                        repeat,
                        modifiers,
                    } => Some((physical_key.unwrap_or(key), pressed, repeat, modifiers)),
                    _ => None,
                })
                .collect()
        });
        for (key, pressed, repeat, modifiers) in events {
            if key == Key::Escape {
                ctx.memory_mut(|m| m.stop_text_input());
                continue;
            }
            if !pressed {
                // Let go of a held button, whatever the modifiers are now.
                if let Some(cmd) = self.held_keys.remove(&key) {
                    self.dispatch(cmd);
                }
                continue;
            }
            if typing || modifiers.alt {
                continue;
            }
            let Some(b) = self.keymap.find_with_ctrl(
                self.prefs.mode,
                key.name(),
                modifiers.shift,
                modifiers.command,
            ) else {
                continue;
            };
            if repeat && !b.repeats() {
                continue;
            }
            // Deck 1's and 2's keys go to the decks the hands play (1 or 3, 2 or 4);
            // `target` keys to Prepare's target deck.
            let mut b = b.clone();
            b.control = match b.control {
                Control::Deck(_, c) if b.on_target => Control::Deck(self.prefs.target(), c),
                c => self.prefs.layered(c),
            };
            let (press, release) = (b.press(), b.release());
            if let Some(release) = release {
                self.held_keys.insert(key, release);
            }
            if let Some(cmd) = press {
                self.dispatch(cmd);
            }
        }
    }
}

impl EarshotApp {
    /// The text size Ctrl +/- or Ctrl 0 asks for: the next of View > Text size's.
    pub(super) fn text_size_keys(&self, ctx: &egui::Context) -> Option<f32> {
        use egui::gui_zoom::kb_shortcuts as k;
        let (bigger, smaller, reset) = ctx.input_mut(|i| {
            (
                i.consume_shortcut(&k::ZOOM_IN) || i.consume_shortcut(&k::ZOOM_IN_SECONDARY),
                i.consume_shortcut(&k::ZOOM_OUT),
                i.consume_shortcut(&k::ZOOM_RESET),
            )
        });
        let now = self.prefs.ui_scale;
        let steps = crate::ui::prefs::TEXT_SIZES.map(|(z, _)| z);
        if reset {
            Some(1.0)
        } else if bigger {
            steps.iter().copied().find(|&z| z > now + 0.01)
        } else if smaller {
            steps.iter().rev().copied().find(|&z| z < now - 0.01)
        } else {
            None
        }
    }
}

/// F11 puts the window in or out of full screen.
pub(super) fn full_screen_key(ctx: &egui::Context) {
    let pressed = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F11));
    if pressed {
        let full = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!full));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn f11_toggles_full_screen() {
        let ctx = egui::Context::default();
        let press = |full: bool| {
            let mut input = egui::RawInput::default();
            input
                .viewports
                .entry(egui::ViewportId::ROOT)
                .or_default()
                .fullscreen = Some(full);
            input.events.push(egui::Event::Key {
                key: egui::Key::F11,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
            let out = ctx.run(input, |ctx| super::full_screen_key(ctx));
            out.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .clone()
        };
        assert_eq!(press(false), [egui::ViewportCommand::Fullscreen(true)]);
        assert_eq!(press(true), [egui::ViewportCommand::Fullscreen(false)]);
    }
}
