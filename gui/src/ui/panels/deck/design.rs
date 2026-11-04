//! The deck's controls row at its design size: every size in the row (CUE / play,
//! the jog ring, the pages and their tabs) is one of these times the row's scale
//! (see [`super::Dims`]), so the row grows and shrinks as one piece.
//!
//! The row is `ROW_H` tall at scale 1: two rows of buttons (`2 * BTN_H + GAP_Y`),
//! with the jog ring (`JOG_D`) and CUE over play (`CUE_D` each) beside them.
//!
//! Mocked up in HTML (`deck-mockup.html`, "Copy sizes"): tweak it there and paste the
//! block it copies over this one.

// ---- the row ----
/// Between buttons side by side, and between the two rows.
pub const GAP_X: f32 = 3.0;
pub const GAP_Y: f32 = 4.0;
/// Between groups: CUE / play and the page, pads and loops, loops and jumps.
pub const GROUP_GAP: f32 = 6.0;

// ---- buttons ----
/// A hot cue pad (and the grid page's anchor button, as wide).
pub const BTN_W: f32 = 34.0;
pub const BTN_H: f32 = 40.0;
/// Loop buttons: ½, ⟲ n, ×2 over IN, OUT, RELOOP.
pub const LOOP_W: f32 = 37.33;
/// Beat jump: ◀ n, n ▶ (BEAT JUMP n under them spans both).
pub const JUMP_W: f32 = 40.0;
/// The grid page: the BPM row's buttons (its BPM box is two wide) and the nudges.
pub const GRID_W: f32 = 38.0;
pub const NUDGE_W: f32 = 57.0;
/// A stem's button (two by two).
pub const STEM_W: f32 = 44.0;

// ---- CUE / play and the jog ring ----
/// CUE and play: circles, one above the other.
pub const CUE_D: f32 = 40.0;
/// Between CUE / play and the jog ring.
pub const TRANSPORT_GAP: f32 = 4.0;
/// The jog ring's box (the ring itself is `JOG_ROOM` in from each side, for its
/// arrows), and extra room on its left, towards CUE / play.
pub const JOG_D: f32 = 84.0;
pub const JOG_ROOM: f32 = 5.0;
pub const JOG_LEFT: f32 = 6.0;
/// Q, by the transport on Prepare's player.
pub const Q_W: f32 = 24.0;
pub const Q_H: f32 = 18.0;

// ---- page tabs ----
pub const TABS_W: f32 = 22.0;
pub const TAB_GAP: f32 = 4.0;

// ---- fonts ----
pub const FONT_PAD: f32 = 12.0;
/// A hot cue's name, under its number.
pub const FONT_CAPTION: f32 = 8.5;
pub const FONT_LOOP: f32 = 10.0;
/// ½ and ×2.
pub const FONT_LOOP_STEP: f32 = 11.0;
pub const FONT_JUMP: f32 = 10.0;
pub const FONT_GRID: f32 = 10.0;
/// "1 at 0:00.000" and where the grid came from, beside the nudges.
pub const FONT_GRID_NOTE: f32 = 10.0;
pub const FONT_STEM: f32 = 9.0;
pub const FONT_CUE: f32 = 11.0;
pub const FONT_PLAY: f32 = 12.0;
pub const FONT_Q: f32 = 10.0;
/// In the jog ring: the BPM ("124" and ".00"), the tempo change and range, VINYL.
pub const FONT_BPM: f32 = 18.5;
pub const FONT_BPM_FRAC: f32 = 11.0;
pub const FONT_TEMPO: f32 = 8.8;
pub const FONT_VINYL: f32 = 7.4;
/// In the jog ring: how far the BPM's baseline sits below the centre, the tempo
/// line's top below the centre, and VINYL's gap above the BPM.
pub const BPM_DROP: f32 = 1.85;
pub const TEMPO_DROP: f32 = 3.7;
pub const VINYL_GAP: f32 = 1.85;
