//! Fallback fonts from the system, for the characters egui's own fonts don't have:
//! CJK, Tibetan, Braille, maths-style letters (𝓢𝓽𝔂𝓵𝓮) and other symbols people put
//! in track names. Without them those show as boxes ("tofu").
//!
//! Found with fontdb (fontconfig on Linux) on a thread of its own, so start-up doesn't
//! wait; the fonts change once they're loaded. Missing families are skipped. (No
//! colour emoji: egui can't draw them, and its own font has the common ones.)

use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily};

/// Tried in order, after egui's own fonts.
const FALLBACKS: [&str; 9] = [
    "DejaVu Sans",
    "Noto Sans",
    "Noto Sans CJK JP",
    "Droid Sans Fallback",
    "Noto Sans Tibetan",
    "Noto Serif Tibetan",
    "Noto Sans Symbols2",
    "Noto Sans Symbols",
    "Noto Sans Math",
];

pub fn install_fallbacks(ctx: &egui::Context) {
    let ctx = ctx.clone();
    let spawned = std::thread::Builder::new()
        .name("earshot-fonts".into())
        .spawn(move || {
            let defs = with_fallbacks(&system_fonts());
            ctx.set_fonts(defs);
            ctx.request_repaint();
        });
    if let Err(e) = spawned {
        eprintln!("fallback fonts: {e}");
    }
}

/// Each fallback family the system has: (name, file bytes, face index).
fn system_fonts() -> Vec<(&'static str, Vec<u8>, u32)> {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    FALLBACKS
        .iter()
        .filter_map(|&name| {
            let id = db.query(&fontdb::Query {
                families: &[fontdb::Family::Name(name)],
                ..Default::default()
            })?;
            // A query falls back to any font; only take the family asked for.
            let face = db.face(id)?;
            if !face.families.iter().any(|(f, _)| f == name) {
                return None;
            }
            db.with_face_data(id, |data, index| (name, data.to_vec(), index))
        })
        .collect()
}

/// egui's fonts with `found` after them in both families.
fn with_fallbacks(found: &[(&'static str, Vec<u8>, u32)]) -> FontDefinitions {
    let mut defs = FontDefinitions::default();
    for (name, bytes, index) in found {
        let mut data = FontData::from_owned(bytes.clone());
        data.index = *index;
        defs.font_data.insert((*name).into(), Arc::new(data));
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            defs.families
                .entry(family)
                .or_default()
                .push((*name).into());
        }
    }
    defs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On a machine with the usual fonts (DejaVu, Noto CJK and Tibetan), names like
    /// these draw without boxes. Skipped where the system has none of them.
    /// The sort arrows are in egui's own fonts, so they show without fallbacks.
    #[test]
    fn sort_arrows_need_no_fallback() {
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |_| {});
        let font = egui::FontId::proportional(14.0);
        assert!(ctx.fonts_mut(|f| f.has_glyph(&font, '⏶') && f.has_glyph(&font, '⏷')));
        // And without fallbacks, the names above would be boxes.
        assert!(!ctx.fonts_mut(|f| f.has_glyph(&font, '新')));
    }

    #[test]
    fn track_names_from_other_scripts_have_glyphs() {
        let found = system_fonts();
        if found.is_empty() {
            return;
        }
        let ctx = egui::Context::default();
        ctx.set_fonts(with_fallbacks(&found));
        let _ = ctx.run(Default::default(), |_| {});
        let font = egui::FontId::proportional(14.0);
        let names: Vec<&str> = found.iter().map(|f| f.0).collect();
        let mut missing = vec![];
        for (text, needs) in [
            ("⠶ 𝕄𝕊 𝗔𝗗𝗘 ℕℝ √ Хдиность ΔΛΞ", "DejaVu Sans"),
            ("し新", "Noto Sans CJK JP"),
            ("༺HASA༻", "Noto Serif Tibetan"),
        ] {
            if !names.contains(&needs) {
                continue;
            }
            for ch in text.chars().filter(|c| !c.is_whitespace()) {
                if !ctx.fonts_mut(|f| f.has_glyph(&font, ch)) {
                    missing.push(ch);
                }
            }
        }
        assert!(
            missing.is_empty(),
            "no glyph for {missing:?} with {names:?}"
        );
    }
}
