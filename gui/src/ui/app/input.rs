//! The keyboard, through the keymap.

use super::*;

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
