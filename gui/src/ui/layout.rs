//! How the window is divided: the one place sizes are decided.
//!
//! egui lays out in a single pass and nothing in it shares space out by itself, so the
//! sharing is done here, from the top down, before anything is drawn:
//!
//! 1. [`split`] shares a length (a row's width, a column's height) between [`Track`]s,
//!    much as CSS flexbox does: each has a size it wants, a least size, and may take
//!    spare room ([`Track::fill`]) or be the first to give way ([`Track::yields`]).
//!    It never hands out more than there is.
//! 2. [`shell`] uses it to divide the window under the top bar into the waveforms, the
//!    decks' row, a tab bar and the body, and picks the scale the decks and mixer are
//!    drawn at. [`library_row`], [`beside_row`] and [`flow_row`] divide the body.
//! 3. A panel is given its size and fills it. It never chooses its own outer size, and
//!    never needs to know the window's.
//!
//! Everything in here is plain arithmetic on numbers (no drawing), so it is tested
//! directly, at every window size, in this file's tests.
//!
//! Sizes are in points. Shared ones (margins, bar heights, text sizes) are named in
//! `ui::theme`; a panel's own design sizes live with the panel (`deck::design`,
//! `mixer`). `docs/ui-layout.md` says how to add or change a part of the UI.

use egui::{Rect, Sense, Ui, Vec2};

use super::panels::{deck, mixer};
use super::theme::THEME;

// ---- sharing a length ------------------------------------------------------------------

/// One part of a row or a column.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Track {
    /// The size it has when there's room.
    pub want: f32,
    /// The least it can be squeezed to.
    pub min: f32,
    /// The most it takes of spare room.
    pub max: f32,
    /// Its share of spare room, against the other tracks' (0: none).
    pub grow: f32,
    /// When there isn't room for everyone: tracks with a higher number shrink first.
    pub yields: u8,
}

impl Track {
    /// Always `size`.
    pub const fn fixed(size: f32) -> Track {
        Track {
            want: size,
            min: size,
            max: size,
            grow: 0.0,
            yields: 0,
        }
    }

    /// `want` when there's room, squeezed down to `min` when there isn't.
    pub const fn sized(want: f32, min: f32) -> Track {
        let want = if want < min { min } else { want };
        Track {
            want,
            min,
            max: want,
            grow: 0.0,
            yields: 0,
        }
    }

    /// Takes the room the others leave, and is at least `min`.
    pub const fn fill(min: f32) -> Track {
        Track {
            want: min,
            min,
            max: f32::INFINITY,
            grow: 1.0,
            yields: 0,
        }
    }

    /// Shrinks before tracks with a lower `order` do (the default is 0: last).
    pub const fn yields(mut self, order: u8) -> Track {
        self.yields = order;
        self
    }

    /// No part of the row at all (a pane that's closed), keeping the tracks' order.
    pub const fn none() -> Track {
        Track::fixed(0.0)
    }
}

/// Share `avail` between `tracks`: each its size in order. They never add up to more
/// than `avail`: short of room, the tracks that yield first shrink towards their least
/// size, and only if every track is at its least are they all scaled down together.
pub fn split<const N: usize>(avail: f32, tracks: [Track; N]) -> [f32; N] {
    let avail = avail.max(0.0);
    let mut sizes = tracks.map(|t| t.want.max(t.min));
    let total = |s: &[f32; N]| s.iter().sum::<f32>();
    let mut over = total(&sizes) - avail;
    if over > 0.0 {
        let mut orders: Vec<u8> = tracks.iter().map(|t| t.yields).collect();
        orders.sort_unstable_by(|a, b| b.cmp(a));
        orders.dedup();
        for order in orders {
            if over <= 0.0 {
                break;
            }
            // What this group can give up, each in proportion to its own slack.
            let slack: f32 = (0..N)
                .filter(|&i| tracks[i].yields == order)
                .map(|i| sizes[i] - tracks[i].min)
                .sum();
            if slack <= 0.0 {
                continue;
            }
            let give = over.min(slack);
            for i in (0..N).filter(|&i| tracks[i].yields == order) {
                sizes[i] -= give * (sizes[i] - tracks[i].min) / slack;
            }
            over -= give;
        }
        if over > 0.0 {
            let total = total(&sizes);
            if total > 0.0 {
                let k = avail / total;
                sizes = sizes.map(|s| s * k);
            }
        }
        return sizes;
    }
    // Room to spare: to the tracks that grow, by their shares, up to their most.
    let mut spare = -over;
    for _ in 0..N {
        let growing: f32 = (0..N)
            .filter(|&i| sizes[i] < tracks[i].max)
            .map(|i| tracks[i].grow)
            .sum();
        if spare <= 0.01 || growing <= 0.0 {
            break;
        }
        let mut given = 0.0;
        for i in 0..N {
            if sizes[i] < tracks[i].max && tracks[i].grow > 0.0 {
                let add = (spare * tracks[i].grow / growing).min(tracks[i].max - sizes[i]);
                sizes[i] += add;
                given += add;
            }
        }
        spare -= given;
    }
    sizes
}

// ---- the window ------------------------------------------------------------------------

/// The body (the library, a tool) is never squeezed below this height by the decks or
/// the waveforms.
pub const BODY_MIN_H: f32 = 150.0;
/// The most of the window's height the decks' row takes (unless the smallest decks
/// need more).
const DECKS_SHARE: f32 = 0.45;
/// A scrolling waveform's least height, and the gap between two.
const WAVE_MIN_H: f32 = 16.0;
pub const WAVE_GAP: f32 = 3.0;
/// The most of the window's height the waveforms can be dragged to.
const WAVES_MAX_SHARE: f32 = 0.5;
/// Between the decks stacked on one side of the mixer.
pub const STACK_GAP: f32 = 4.0;
/// Below this scale the decks' buttons are too small to read: with a mixer wanted and
/// no room for both, the mixer isn't drawn and the decks take its room.
const DECK_SCALE_FLOOR: f32 = 0.5;
/// The mixer's smallest scale: its knobs' captions stop fitting below it.
const MIXER_SCALE_MIN: f32 = 0.75;

/// What the window has to show, from the preferences.
#[derive(Clone, Copy, Debug)]
pub struct ShellInput {
    /// Decks showing: 1, 2 or 4.
    pub decks: usize,
    /// The mixer is wanted between the decks.
    pub mixer: bool,
    /// The scrolling waveforms' row shows.
    pub waves: bool,
    /// The decks' row shows (not in the Flow editor's full screen).
    pub decks_row: bool,
    /// A bar of tabs shows over the body (Prepare's tools, Controller's tabs).
    pub tab_bar: bool,
    /// The waveforms' row's share of the height, as last dragged.
    pub waves_share: f32,
}

/// The decks' row: the decks either side of the mixer.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DeckRow {
    /// The row's height inside its margins, which the mixer fills.
    pub height: f32,
    /// Each deck's height (a side's decks share the row).
    pub deck_h: f32,
    /// The scale the decks' controls are drawn at (`deck::design` sizes times this).
    pub scale: f32,
    /// The mixer's scale and its width with its frame, if it's drawn.
    pub mixer: Option<(f32, f32)>,
    /// The mixer was wanted but there's no room for it beside decks that can be read.
    pub mixer_dropped: bool,
}

/// The window under the top bar and over the status bar, divided top to bottom. The
/// heights are whole panels', margins and all, and add up to no more than there is.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Shell {
    /// What was divided.
    pub size: Vec2,
    /// The waveforms' row (0 if hidden), and one waveform's height in it.
    pub waves_h: f32,
    pub wave_h: f32,
    /// The least and most the waveforms' row can be dragged to here.
    pub waves_range: (f32, f32),
    /// The decks' row (0 if hidden) and what's in it.
    pub decks_h: f32,
    pub decks: DeckRow,
    /// The tab bar (0 without one).
    pub tabs_h: f32,
    /// The body: what's left.
    pub body_h: f32,
}

/// The margins round a row of the shell, as (across, down) totals.
fn row_margins() -> Vec2 {
    THEME.spacing.row_margin.sum()
}

fn waves_margins() -> Vec2 {
    THEME.spacing.waves_margin.sum()
}

/// Divide a window of `size` (what the bars leave) for `input`.
pub fn shell(size: Vec2, input: ShellInput) -> Shell {
    let tabs_h = if input.tab_bar {
        THEME.spacing.tab_bar_h
    } else {
        0.0
    };
    let n = input.decks.max(1) as f32;
    let waves_min = if input.waves {
        n * WAVE_MIN_H + (n - 1.0) * WAVE_GAP + waves_margins().y
    } else {
        0.0
    };
    // The decks first: their row's height follows from the scale that fits.
    let (decks, decks_h) = if input.decks_row {
        // What the row may take: its share of the height, or all the body and the
        // waveforms can spare if that's less.
        let spare = size.y - tabs_h - waves_min - BODY_MIN_H;
        // Its share, or what decks just big enough to read need if that's more.
        let per_side = input.decks.div_ceil(2).max(1) as f32;
        let readable = per_side * deck::height(DECK_SCALE_FLOOR)
            + STACK_GAP * (per_side - 1.0)
            + row_margins().y;
        let most = (size.y * DECKS_SHARE).max(readable).min(spare) - row_margins().y;
        let row = deck_row(size.x - row_margins().x, most, input.decks, input.mixer);
        (row, row.height + row_margins().y)
    } else {
        (DeckRow::default(), 0.0)
    };
    let waves_max = (size.y * WAVES_MAX_SHARE)
        .min(size.y - tabs_h - decks_h - BODY_MIN_H)
        .max(waves_min);
    let waves = if input.waves {
        Track::sized(
            (size.y * input.waves_share).clamp(waves_min, waves_max),
            waves_min,
        )
        .yields(1)
    } else {
        Track::none()
    };
    let [waves_h, decks_h, tabs_h, body_h] = split(
        size.y,
        [
            waves,
            Track::fixed(decks_h),
            Track::fixed(tabs_h),
            Track::fill(BODY_MIN_H),
        ],
    );
    let wave_h = ((waves_h - waves_margins().y - (n - 1.0) * WAVE_GAP) / n).max(0.0);
    Shell {
        size,
        waves_h,
        wave_h,
        waves_range: (waves_min, waves_max),
        decks_h,
        decks,
        tabs_h,
        body_h,
    }
}

/// The decks' row in `width`, no taller than `most` if the smallest decks allow: the
/// biggest scale at which the decks fit both, and the mixer scaled to the row.
fn deck_row(width: f32, most: f32, decks: usize, mixer_wanted: bool) -> DeckRow {
    let per_side = decks.div_ceil(2).max(1) as f32;
    let gaps = STACK_GAP * (per_side - 1.0);
    let gap_x = THEME.spacing.item.x;
    let solve = |with_mixer: bool| {
        // The mixer is as tall as the row, and the row as tall as the decks: a few
        // rounds settle it (each only ever shrinks the mixer).
        let mut m = 1.0_f32;
        let mut row = DeckRow::default();
        let mut k_wide = deck::SCALE_MAX;
        for _ in 0..4 {
            let mixer_w = if with_mixer {
                mixer::outer_size(decks, m).x
            } else {
                0.0
            };
            let cells = if with_mixer { 3.0 } else { 2.0 };
            let side = ((width - mixer_w - gap_x * (cells - 1.0)) / 2.0).max(0.0);
            k_wide = deck::scale_for_width(side, with_mixer);
            let k_tall = deck::scale_for_height((most - gaps) / per_side);
            let k = k_wide.min(k_tall).clamp(deck::SCALE_MIN, deck::SCALE_MAX);
            let mut height = per_side * deck::height(k) + gaps;
            if with_mixer {
                m = (height / mixer::outer_size(decks, 1.0).y).clamp(MIXER_SCALE_MIN, 1.0);
                height = height.max(mixer::outer_size(decks, m).y);
            }
            row = DeckRow {
                height,
                deck_h: (height - gaps) / per_side,
                scale: k,
                mixer: with_mixer.then(|| (m, mixer::outer_size(decks, m).x)),
                mixer_dropped: false,
            };
        }
        (row, k_wide)
    };
    let (row, k_wide) = solve(mixer_wanted);
    if mixer_wanted && k_wide < DECK_SCALE_FLOOR {
        let (mut row, _) = solve(false);
        row.mixer_dropped = true;
        return row;
    }
    row
}

// ---- the body --------------------------------------------------------------------------

/// The library's panes' least widths, and the track list's.
const SOURCE_MIN_W: f32 = 140.0;
const ASIDE_MIN_W: f32 = 220.0;
const TABLE_MIN_W: f32 = 300.0;
/// A Prepare tool's, beside the library; and the library's beside it.
const TOOL_MIN_W: f32 = 320.0;
const BESIDE_MIN_W: f32 = 320.0;
/// The most of a row a dragged pane can take.
const PANE_MAX_SHARE: f32 = 0.6;

/// The library's row: `| stack | source | track list | aside | stack |`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LibraryRow {
    /// The icon stacks (the right one 0 without asides).
    pub stack: f32,
    pub aside_stack: f32,
    /// The source pane and the aside (0 if closed).
    pub source: f32,
    pub aside: f32,
    /// The track list: what's left.
    pub table: f32,
}

/// Divide the library's `width`. `source` and `aside`: each pane's share of the width
/// as last dragged, if it's open. `asides`: the right icon stack shows.
pub fn library_row(
    width: f32,
    source: Option<f32>,
    aside: Option<f32>,
    asides: bool,
) -> LibraryRow {
    let stack = THEME.spacing.stack_w;
    let pane = |share: Option<f32>, min: f32, order: u8| match share {
        Some(s) => Track::sized(width * s.min(PANE_MAX_SHARE), min).yields(order),
        None => Track::none(),
    };
    let aside_stack = if asides { stack } else { 0.0 };
    let [stack, source, table, aside, aside_stack] = split(
        width,
        [
            Track::fixed(stack),
            // The aside gives way first, then the source, and the list last.
            pane(source, SOURCE_MIN_W, 1),
            Track::fill(TABLE_MIN_W),
            pane(aside.filter(|_| asides), ASIDE_MIN_W, 2),
            Track::fixed(aside_stack),
        ],
    );
    LibraryRow {
        stack,
        aside_stack,
        source,
        aside,
        table,
    }
}

/// Prepare's `| library | tool |`: the library's width, of `width`, at its `share`.
pub fn beside_row(width: f32, share: f32) -> f32 {
    let [library, _tool] = split(
        width,
        [
            Track::sized(width * share.min(PANE_MAX_SHARE), BESIDE_MIN_W).yields(1),
            Track::fill(TOOL_MIN_W),
        ],
    );
    library
}

/// The Flow editor's `| library | canvas | suggestions | stack |`: the suggestions'
/// width (0 if closed), given their `share` of `width`. The library's is
/// [`beside_row`]'s of what's left.
pub fn flow_aside(width: f32, share: Option<f32>) -> f32 {
    let Some(share) = share else { return 0.0 };
    let [_rest, aside, _stack] = split(
        width,
        [
            Track::fill(BESIDE_MIN_W + TOOL_MIN_W),
            Track::sized(width * share.min(PANE_MAX_SHARE), ASIDE_MIN_W).yields(1),
            Track::fixed(THEME.spacing.stack_w),
        ],
    );
    aside
}

// ---- dragging a pane's edge ------------------------------------------------------------

/// Which edge of a pane is dragged to resize it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    /// A pane on the left: its right edge.
    Right,
    /// A pane on the right: its left edge.
    Left,
    /// A row at the top: its bottom edge.
    Bottom,
}

/// How far in from the pane's edge the handle reaches.
const HANDLE: f32 = 5.0;

/// A handle along `edge` of the pane `ui` draws (call it inside the pane, last). While
/// it's dragged: the size, in points, the pane is being dragged to. The pane's size
/// comes from this module, so the caller turns that into a share of its row and saves
/// it (`ViewCmd::Resize`); the next frame's layout does the rest.
pub fn splitter(ui: &mut Ui, edge: Edge) -> Option<f32> {
    // The pane's whole rectangle, its frame's margins included.
    let pane = ui.clip_rect();
    let handle = match edge {
        Edge::Right => Rect::from_min_max(egui::pos2(pane.right() - HANDLE, pane.top()), pane.max),
        Edge::Left => Rect::from_min_max(pane.min, egui::pos2(pane.left() + HANDLE, pane.bottom())),
        Edge::Bottom => {
            Rect::from_min_max(egui::pos2(pane.left(), pane.bottom() - HANDLE), pane.max)
        }
    };
    let r = ui.interact(handle, ui.id().with("splitter"), Sense::drag());
    let cursor = match edge {
        Edge::Bottom => egui::CursorIcon::ResizeVertical,
        _ => egui::CursorIcon::ResizeHorizontal,
    };
    let r = r.on_hover_and_drag_cursor(cursor);
    if r.hovered() || r.dragged() {
        let stroke = if r.dragged() {
            ui.visuals().widgets.active.fg_stroke
        } else {
            ui.visuals().widgets.hovered.fg_stroke
        };
        let p = ui.painter();
        match edge {
            Edge::Right => p.vline(pane.right() - 1.0, pane.y_range(), stroke),
            Edge::Left => p.vline(pane.left() + 1.0, pane.y_range(), stroke),
            Edge::Bottom => p.hline(pane.x_range(), pane.bottom() - 1.0, stroke),
        };
    }
    let at = r.dragged().then(|| r.interact_pointer_pos()).flatten()?;
    Some(match edge {
        Edge::Right => at.x - pane.left(),
        Edge::Left => pane.right() - at.x,
        Edge::Bottom => at.y - pane.top(),
    })
}

#[cfg(test)]
mod tests {
    use egui::vec2;

    use super::*;
    use crate::ui::window::MIN_SIZE;

    fn sum<const N: usize>(s: [f32; N]) -> f32 {
        s.iter().sum()
    }

    #[test]
    fn split_gives_spare_room_to_the_tracks_that_fill() {
        let s = split(
            1000.0,
            [
                Track::fixed(100.0),
                Track::fill(50.0),
                Track::sized(200.0, 80.0),
            ],
        );
        assert_eq!(s, [100.0, 700.0, 200.0]);
    }

    #[test]
    fn split_shrinks_the_first_to_yield_first() {
        let tracks = [
            Track::sized(300.0, 100.0).yields(1),
            Track::fill(200.0),
            Track::sized(300.0, 100.0).yields(2),
        ];
        // 100 short: all from the one that yields first.
        assert_eq!(split(700.0, tracks), [300.0, 200.0, 200.0]);
        // 300 short: it's at its least, and the next gives the rest.
        assert_eq!(split(500.0, tracks), [200.0, 200.0, 100.0]);
        // Less than everyone's least: all scaled down, nothing past the edge.
        let s = split(200.0, tracks);
        assert!((sum(s) - 200.0).abs() < 0.01, "{s:?}");
        assert!((s[1] - 100.0).abs() < 0.01, "{s:?}");
    }

    #[test]
    fn split_never_hands_out_more_than_there_is() {
        let tracks = [
            Track::fixed(36.0),
            Track::sized(250.0, 140.0).yields(1),
            Track::fill(300.0),
            Track::sized(380.0, 220.0).yields(2),
        ];
        for w in (0..2600).step_by(7) {
            let s = split(w as f32, tracks);
            assert!(sum(s) <= w as f32 + 0.01, "{w}: {s:?}");
            assert!(s.iter().all(|v| *v >= 0.0), "{w}: {s:?}");
        }
    }

    /// Window sizes from the smallest to a big desktop, as the bars leave them.
    fn windows() -> impl Iterator<Item = Vec2> {
        let widths = [MIN_SIZE.x, 1280.0, 1440.0, 1680.0, 1920.0, 2560.0, 3840.0];
        let heights = [MIN_SIZE.y - 60.0, 700.0, 840.0, 1020.0, 1380.0, 2100.0];
        widths
            .into_iter()
            .flat_map(move |w| heights.into_iter().map(move |h| vec2(w, h)))
    }

    fn inputs() -> impl Iterator<Item = ShellInput> {
        let flags = [false, true];
        [1, 2, 4].into_iter().flat_map(move |decks| {
            flags.into_iter().flat_map(move |mixer| {
                flags.into_iter().flat_map(move |waves| {
                    flags.into_iter().flat_map(move |tab_bar| {
                        [0.0, 0.16, 0.9]
                            .into_iter()
                            .map(move |waves_share| ShellInput {
                                decks,
                                mixer,
                                waves,
                                decks_row: true,
                                tab_bar,
                                waves_share,
                            })
                    })
                })
            })
        })
    }

    #[test]
    fn the_shell_fits_every_window() {
        for size in windows() {
            for input in inputs() {
                let s = shell(size, input);
                let what = format!("{size:?} {input:?}: {s:?}");
                let total = s.waves_h + s.decks_h + s.tabs_h + s.body_h;
                assert!(total <= size.y + 0.01, "too tall: {what}");
                assert!(
                    s.body_h >= BODY_MIN_H - 0.01,
                    "no room for the body: {what}"
                );
                assert!(s.decks.scale >= deck::SCALE_MIN, "{what}");
                assert!(s.decks.scale <= deck::SCALE_MAX + 0.001, "{what}");
                // The decks and the mixer fit across.
                let side = deck::width(s.decks.scale, s.decks.mixer.is_some());
                let mixer = s.decks.mixer.map_or(0.0, |(_, w)| w + THEME.spacing.item.x);
                let across = 2.0 * side + mixer + THEME.spacing.item.x + row_margins().x;
                assert!(across <= size.x + 0.5, "too wide ({across}): {what}");
                if input.waves {
                    assert!(s.wave_h >= WAVE_MIN_H - 0.01, "waveforms too thin: {what}");
                    assert!(s.waves_h <= s.waves_range.1 + 0.01, "{what}");
                } else {
                    assert_eq!(s.waves_h, 0.0, "{what}");
                }
            }
        }
    }

    #[test]
    fn the_decks_are_readable_in_the_smallest_window() {
        let size = vec2(MIN_SIZE.x, MIN_SIZE.y - 60.0);
        for decks in [2, 4] {
            for mixer in [false, true] {
                let s = shell(
                    size,
                    ShellInput {
                        decks,
                        mixer,
                        waves: true,
                        decks_row: true,
                        tab_bar: false,
                        waves_share: 0.16,
                    },
                );
                assert!(
                    s.decks.scale >= DECK_SCALE_FLOOR,
                    "{decks} decks, mixer {mixer}: {s:?}"
                );
            }
        }
    }

    #[test]
    fn a_big_window_shows_the_decks_and_mixer_at_full_size() {
        let s = shell(
            vec2(1920.0, 1020.0),
            ShellInput {
                decks: 2,
                mixer: true,
                waves: true,
                decks_row: true,
                tab_bar: false,
                waves_share: 0.16,
            },
        );
        assert!((s.decks.scale - deck::SCALE_MAX).abs() < 0.001, "{s:?}");
        assert_eq!(s.decks.mixer.map(|(m, _)| m), Some(1.0), "{s:?}");
        assert!(!s.decks.mixer_dropped);
    }

    #[test]
    fn the_mixer_makes_way_when_four_decks_would_be_too_small() {
        let input = ShellInput {
            decks: 4,
            mixer: true,
            waves: true,
            decks_row: true,
            tab_bar: false,
            waves_share: 0.16,
        };
        let small = shell(vec2(MIN_SIZE.x, MIN_SIZE.y - 60.0), input);
        assert!(
            small.decks.mixer_dropped && small.decks.mixer.is_none(),
            "{small:?}"
        );
        let big = shell(vec2(1680.0, 990.0), input);
        assert!(
            !big.decks.mixer_dropped && big.decks.mixer.is_some(),
            "{big:?}"
        );
    }

    #[test]
    fn the_library_row_fits_every_width() {
        for w in (300..3000).step_by(13) {
            let w = w as f32;
            for source in [None, Some(0.05), Some(0.17), Some(0.9)] {
                for aside in [None, Some(0.24), Some(0.9)] {
                    for asides in [false, true] {
                        let r = library_row(w, source, aside, asides);
                        let total = r.stack + r.source + r.table + r.aside + r.aside_stack;
                        assert!(total <= w + 0.01, "{w} {source:?} {aside:?}: {r:?}");
                        if w >= MIN_SIZE.x {
                            assert!(r.table >= TABLE_MIN_W - 0.01, "{w}: {r:?}");
                            if source.is_some() {
                                assert!(r.source >= SOURCE_MIN_W - 0.01, "{w}: {r:?}");
                            }
                        }
                        assert_eq!(r.source > 0.0, source.is_some());
                        assert_eq!(r.aside > 0.0, aside.is_some() && asides);
                    }
                }
            }
        }
    }
}
