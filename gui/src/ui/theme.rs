//! earshot's look in one place: the palette, the deck, hot cue and memory cue
//! colours, the controls' colours, spacing, and the frames everything is drawn in.
//! No other UI file names a colour by its numbers; they read [`THEME`] (panels get it
//! as `cx.theme`).

use std::sync::atomic::{AtomicU32, Ordering};

use earshot_engine::{DeckId, MAX_DECKS};
use egui::{Color32, CornerRadius, Margin, RichText, Stroke, Ui, Vec2, vec2};

use crate::core::store::Memory;

/// The text size, as a multiple of normal (View > Text size, Ctrl +/-). Only the
/// lists, bars, menus and windows' text grows, with what sits in a line of it (table
/// rows, their waveforms); the decks and mixer keep their hand-fitted sizes
/// ([`normal_text`]). An `f32`'s bits.
static TEXT_SCALE: AtomicU32 = AtomicU32::new(0x3f80_0000);

/// The text size, as a multiple of normal.
pub fn text_scale() -> f32 {
    f32::from_bits(TEXT_SCALE.load(Ordering::Relaxed))
}

/// A font size (or a text line's height) at the chosen text size.
pub fn fs(size: f32) -> f32 {
    size * text_scale()
}

/// Set the text size: earshot's own fonts read [`fs`], egui's widgets its text styles.
pub fn set_text_scale(ctx: &egui::Context, k: f32) {
    TEXT_SCALE.store(k.to_bits(), Ordering::Relaxed);
    ctx.all_styles_mut(|s| s.text_styles = text_styles(k));
}

/// egui's text styles at `k` times their normal size.
fn text_styles(k: f32) -> std::collections::BTreeMap<egui::TextStyle, egui::FontId> {
    let mut styles = egui::Style::default().text_styles;
    for font in styles.values_mut() {
        font.size *= k;
    }
    styles
}

/// A resizable side panel's id, new for each text size: egui keeps a panel's width
/// under its id, so a new id opens it at its saved width at this size.
pub fn sized_panel(name: impl std::hash::Hash) -> egui::Id {
    egui::Id::new((name, text_scale().to_bits()))
}

/// This part of the UI (a deck, the mixer) keeps normal-sized text whatever the
/// text size: its layout is fitted by hand.
pub fn normal_text(ui: &mut Ui) {
    if text_scale() != 1.0 {
        ui.style_mut().text_styles = text_styles(1.0);
    }
}

const fn rgb8(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// The colours a memory cue can be given, as Rekordbox offers them. Saved as 0xRRGGBB,
/// so a colour from elsewhere (an import) still shows.
pub const CUE_PALETTE: [(u32, &str); 8] = [
    (0xde_44_cf, "Pink"),
    (0xe6_28_28, "Red"),
    (0xff_8c_00, "Orange"),
    (0xe0_d2_20, "Yellow"),
    (0x28_e2_14, "Green"),
    (0x10_b1_76, "Aqua"),
    (0x30_5a_ff, "Blue"),
    (0x8c_32_ff, "Purple"),
];

/// Gaps and margins.
pub struct Spacing {
    /// Between widgets.
    pub item: Vec2,
    pub button_padding: Vec2,
    /// Inside a panel's frame (decks, the mixer, the cue pane).
    pub panel_margin: Margin,
    pub panel_radius: u8,
    /// Inside the top and bottom bars.
    pub bar_margin: Margin,
}

pub struct Theme {
    // ---- the palette ------------------------------------------------------------------
    /// The window behind everything.
    pub bg: Color32,
    /// Panels, bars and windows.
    pub panel: Color32,
    /// Raised parts of a panel: pills, inactive widgets.
    pub panel_2: Color32,
    /// Borders and separators.
    pub line: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub bad: Color32,
    /// Behind waveforms.
    pub wave_bg: Color32,
    /// Selected text and rows.
    pub selection: Color32,
    /// Slider rails and other inactive fills.
    pub rail: Color32,
    /// A hovered widget's border.
    pub hover_line: Color32,
    /// Dark text on a bright chip or lit button.
    pub ink: Color32,
    /// Drawn over anything: playheads, outlines on dark.
    pub marker: Color32,

    // ---- decks and cues ---------------------------------------------------------------
    /// Decks A to D. Prepare's player has the accent.
    pub deck: [Color32; MAX_DECKS],
    pub hotcue: [Color32; 8],
    /// Memory cues and loops with no colour of their own.
    pub memory_cue: Color32,
    pub memory_loop: Color32,
    /// The deck's CUE, play and loop buttons.
    pub cue_button: Color32,
    pub play_button: Color32,
    pub loop_button: Color32,

    // ---- waveforms ----------------------------------------------------------------------
    /// The downbeat line on the scrolling waveform, and the red half of "Anchor 1 here".
    pub downbeat: Color32,
    /// Other beats' ticks.
    pub beat_tick: Color32,
    /// The loop's shading: active, set but not active, and on the overview.
    pub loop_shade: Color32,
    pub loop_shade_idle: Color32,
    pub loop_shade_overview: Color32,
    /// Over what's been played on the overview.
    pub played_shade: Color32,

    // ---- controls -----------------------------------------------------------------------
    /// Knob bodies and fader caps.
    pub face: Color32,
    pub face_hover: Color32,
    /// Fader grooves and meter backs.
    pub groove: Color32,
    /// The unlit part of a knob's arc.
    pub arc_track: Color32,
    /// Unlit buttons.
    pub button: Color32,
    /// Unlit meter segments.
    pub meter_off: Color32,
    /// The jog ring's face, and the dot that turns with the track.
    pub jog_face: Color32,
    pub jog_dot: Color32,
    /// Library tree icons.
    pub icon: Color32,

    // ---- the Flow canvas ----------------------------------------------------------------
    pub card: Color32,
    pub card_line: Color32,
    pub card_hover: Color32,
    pub card_shadow: Color32,
    pub chips_bg: Color32,
    pub energy_chip: Color32,
    /// Cards on the minimap, and the minimap's back.
    pub minimap_card: Color32,
    pub minimap_bg: Color32,
    /// A card's waveform before it's previewed, and over what's been previewed.
    pub thumb_idle: Color32,
    pub thumb_played: Color32,

    // ---- suggestions -------------------------------------------------------------------
    /// The score bars: key, tempo, energy.
    pub score: [Color32; 3],

    // ---- the tempo lock tab -----------------------------------------------------------
    /// The original's and the straightened version's waveforms, curves and histograms.
    pub versions: [Color32; 2],
    /// Sections at one tempo, in turn (they repeat).
    pub sections: [Color32; 5],

    pub spacing: Spacing,
}

const WARN: Color32 = rgb8(0xff, 0xcc, 0x4d);
const ACCENT: Color32 = rgb8(0xff, 0x8c, 0x28);
const DECKS: [Color32; MAX_DECKS] = [
    rgb8(0x3f, 0xa7, 0xff),
    rgb8(0xff, 0x5d, 0x8f),
    rgb8(0x4f, 0xd4, 0x7a),
    rgb8(0xc2, 0x8c, 0xff),
];
const HOTCUES: [Color32; 8] = [
    rgb8(0x3c, 0xd3, 0x6b),
    rgb8(0xff, 0x4f, 0x4f),
    rgb8(0xff, 0xb0, 0x2e),
    rgb8(0x2f, 0x9b, 0xff),
    rgb8(0xc2, 0x5b, 0xff),
    rgb8(0x18, 0xd2, 0xc7),
    rgb8(0xff, 0x74, 0xc8),
    rgb8(0xe6, 0xe0, 0x4a),
];

/// The theme (dark; the only one so far).
pub const THEME: Theme = Theme {
    bg: rgb8(0x0f, 0x10, 0x13),
    panel: rgb8(0x17, 0x18, 0x1d),
    panel_2: rgb8(0x1f, 0x20, 0x27),
    line: rgb8(0x2b, 0x2d, 0x36),
    text: rgb8(0xe6, 0xe6, 0xea),
    muted: rgb8(0x8a, 0x8c, 0x99),
    accent: ACCENT,
    ok: rgb8(0x45, 0xd4, 0x83),
    warn: WARN,
    bad: rgb8(0xff, 0x5b, 0x5b),
    wave_bg: rgb8(0x08, 0x09, 0x0b),
    selection: rgb8(0x2b, 0x5f, 0xb8),
    rail: rgb8(0x34, 0x37, 0x44),
    hover_line: rgb8(0x44, 0x47, 0x56),
    ink: rgb8(0x11, 0x11, 0x11),
    marker: Color32::WHITE,

    deck: DECKS,
    hotcue: HOTCUES,
    memory_cue: WARN,
    memory_loop: HOTCUES[0],
    cue_button: rgb8(0xf0, 0xa0, 0x30),
    play_button: DECKS[2],
    loop_button: HOTCUES[0],

    downbeat: Color32::from_rgba_unmultiplied_const(255, 70, 70, 230),
    beat_tick: Color32::from_rgba_premultiplied(90, 90, 90, 90),
    loop_shade: Color32::from_rgba_unmultiplied_const(255, 180, 40, 50),
    loop_shade_idle: Color32::from_rgba_premultiplied(18, 18, 18, 18),
    loop_shade_overview: Color32::from_rgba_unmultiplied_const(255, 180, 40, 80),
    played_shade: Color32::from_black_alpha(115),

    face: rgb8(0x2a, 0x2c, 0x34),
    face_hover: rgb8(0x36, 0x39, 0x44),
    groove: rgb8(0x0b, 0x0c, 0x0f),
    arc_track: rgb8(0x3a, 0x3d, 0x4a),
    button: rgb8(0x22, 0x24, 0x2b),
    meter_off: rgb8(0x1a, 0x1b, 0x20),
    jog_face: rgb8(0x0c, 0x0d, 0x10),
    jog_dot: rgb8(0xe8, 0x3a, 0x3a),
    icon: Color32::from_gray(140),

    card: rgb8(0x24, 0x26, 0x2d),
    card_line: rgb8(0x35, 0x38, 0x45),
    card_hover: rgb8(0x5a, 0x5f, 0x73),
    card_shadow: Color32::from_black_alpha(70),
    chips_bg: rgb8(0x1a, 0x1b, 0x21),
    energy_chip: rgb8(0x6f, 0xcf, 0x8e),
    minimap_card: Color32::from_gray(110),
    minimap_bg: Color32::from_black_alpha(200),
    thumb_idle: Color32::from_gray(150),
    thumb_played: Color32::from_black_alpha(130),

    score: [HOTCUES[4], DECKS[0], WARN],

    versions: [DECKS[0], DECKS[2]],
    sections: [DECKS[0], DECKS[2], DECKS[1], WARN, DECKS[3]],

    spacing: Spacing {
        item: vec2(6.0, 4.0),
        button_padding: vec2(7.0, 3.0),
        panel_margin: Margin::symmetric(8, 6),
        panel_radius: 8,
        bar_margin: Margin::symmetric(10, 5),
    },
};

impl Theme {
    /// Give egui's own widgets this look.
    pub fn install(&self, ctx: &egui::Context) {
        let mut v = egui::Visuals::dark();
        v.panel_fill = self.bg;
        v.window_fill = self.panel;
        v.extreme_bg_color = self.wave_bg;
        v.faint_bg_color = self.panel_2;
        v.selection.bg_fill = self.selection;
        v.widgets.inactive.weak_bg_fill = self.panel_2;
        // Slider rails and other inactive fills need to stand out from the panels.
        v.widgets.inactive.bg_fill = self.rail;
        v.slider_trailing_fill = true;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, self.line);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, self.hover_line);
        v.widgets.noninteractive.fg_stroke.color = self.text;
        v.widgets.inactive.fg_stroke.color = self.text;
        ctx.set_visuals(v);
        let (item, padding) = (self.spacing.item, self.spacing.button_padding);
        ctx.style_mut(|s| {
            s.spacing.item_spacing = item;
            s.spacing.button_padding = padding;
        });
    }

    /// A deck's colour; Prepare's player has the accent.
    pub fn deck_colour(&self, deck: DeckId) -> Color32 {
        self.deck.get(deck.index()).copied().unwrap_or(self.accent)
    }

    /// A hot cue's colour (they repeat past 8).
    pub fn hotcue_colour(&self, slot: usize) -> Color32 {
        self.hotcue[slot % self.hotcue.len()]
    }

    /// A memory cue's or loop's colour: its own, or the default for its kind.
    pub fn memory_colour(&self, m: &Memory) -> Color32 {
        match (m.color, m.end) {
            (Some(c), _) => rgb(c),
            (None, Some(_)) => self.memory_loop,
            (None, None) => self.memory_cue,
        }
    }

    /// The rounded frame round a deck, the mixer and other panels.
    pub fn panel_frame(&self) -> egui::Frame {
        egui::Frame::new()
            .fill(self.panel)
            .stroke(Stroke::new(1.0_f32, self.line))
            .corner_radius(self.spacing.panel_radius)
            .inner_margin(self.spacing.panel_margin)
    }

    /// The top and bottom bars.
    pub fn bar_frame(&self) -> egui::Frame {
        egui::Frame::new()
            .fill(self.panel)
            .inner_margin(self.spacing.bar_margin)
    }

    /// A small rounded status label with a coloured border; clickable.
    pub fn pill(&self, ui: &mut Ui, text: &str, border: Color32) -> egui::Response {
        egui::Frame::new()
            .fill(self.panel_2)
            .stroke(Stroke::new(1.0_f32, border))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::symmetric(8, 2))
            .show(ui, |ui| ui.label(RichText::new(text).size(fs(11.0))))
            .response
            .interact(egui::Sense::click())
    }
}

/// A saved 0xRRGGBB colour.
pub fn rgb(c: u32) -> Color32 {
    rgb8((c >> 16) as u8, (c >> 8) as u8, c as u8)
}
