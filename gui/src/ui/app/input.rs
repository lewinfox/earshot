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
