# UI layout: how it works and how to change it

earshot's UI is egui, laid out with flexbox: [taffy](https://docs.rs/taffy), through
[`egui_taffy`](https://github.com/PPakalns/egui_taffy). A view says what its parts are
and how they share a row or a column, the way CSS does, and taffy works out the
sizes. Nothing is sized by hand from the window's size.

## The rules

1. **Parts share space with flexbox** (`gui/src/ui/flex.rs`). A row or a column
   holds parts; each part says what it wants (a fixed size, a share, "the rest"), and
   its least size.
2. **A part's box is the layout's.** A pane draws in the box it's given
   (`ui.max_rect()`) and never picks its own outer size.
3. **Shared sizes have names**: least widths and shares in `gui/src/ui/layout.rs`,
   margins and bar heights in `gui/src/ui/theme.rs` (`THEME.spacing`). A panel's own
   design sizes live with the panel (`panels/deck/design.rs`, the constants at the top
   of `panels/mixer.rs`).
4. **Anything that can hold more than fits scrolls** (`egui::ScrollArea`), and every
   pane is cut off at its edges, so nothing paints over its neighbours or the status
   bar.
5. **The UI size is egui's zoom** (`gui/src/ui/window.rs`). It scales text, controls
   and spacing together, so no code multiplies sizes by a text scale.

## The pieces

```
window (ui/window.rs: smallest 1024 x 640, kept on the screen, UI size)
├─ top bar            panels/status.rs    its effect units wrap to a second line
├─ shell              views/mod.rs        one column, everything between the bars:
│  ├─ waveforms         views/perform.rs    its share of the height, draggable
│  ├─ decks + mixer     views/perform.rs    | decks | mixer | decks |, every deck the
│  │                                        size of one of four, the row as tall as
│  │                                        they are, at most 45% of the window
│  ├─ tab bar           views/mod.rs        Prepare's tools, Controller's tabs
│  └─ body              views/mod.rs        the rest: the library, a tool beside it,
│                                           the Flow canvas
└─ status bar
```

## The flexbox helpers

`ui/flex.rs` wraps `egui_taffy` in a few names:

| Helper | What it is |
|---|---|
| `flex::root(ui, id, style, \|t\| ..)` | a layout in all the room `ui` has |
| `t.style(row()).add(\|t\| ..)` | a container: `row()` left to right, `col()` top to bottom |
| `flex::pane(t, style, \|ui\| ..)` | a part whose size is the layout's; draw in `ui.max_rect()` |
| `flex::fit(t, style, \|ui\| ..)` | a part as big as what's drawn in it (as last drawn) |
| `flex::framed(t, style, frame, \|t, rect\| ..)` | a container drawn as a panel, told its box |
| `flex::boxed(ui, frame, \|ui\| ..)` | fill a pane with a frame and draw inside its margins |

Styles start from `leaf()`, `row()` or `col()` and read like their CSS:

| Here | CSS |
|---|---|
| `.w(36.0)` | `width: 36px; flex-shrink: 0` |
| `.h_pct(0.3)` | `height: 30%` |
| `.grow()` | `flex: 1 1 0` |
| `.basis_pct(0.3)` | `flex: 0 1 30%` |
| `.min_w(240.0)` | `min-width: 240px` |
| `.max_h_pct(0.45)` | `max-height: 45%` |
| `.no_shrink()` | `flex-shrink: 0` |
| `.gap(4.0)` / `.pad(margin)` | `gap` / `padding` |
| `.reverse(true)` | `row-reverse` / `column-reverse` |

Every part starts at `min-width: 0; min-height: 0`: unlike CSS, a part is never
widened to fit its content unless it asks with `min_w` / `min_h`.

One thing to know:

- **Layout lands a pass late, but a frame on time.** egui draws in one pass, so a part
  is drawn where the last pass's layout put it. When the layout changes, egui_taffy
  throws that pass away and draws again (up to 10 passes a frame), so a change is
  shown whole. Something measured from what's drawn (`flex::fit`) takes one more pass
  to settle than something with a size from its style, so prefer styles for anything
  that moves when the layout does.

## The decks

Every deck is as big as one of four would be, whether one, two or four show, so the
decks look the same in every mode. `deck::four_deck_size` works that size out from the
row: two decks to a side keeping their design shape (`deck::aspect()`, width over
height at scale 1), as wide as their column, or as tall as the row's share of the
window (45%) lets two of them be. Each deck starts at that height (`flex-basis`) and
stretches if its column is taller (beside a taller mixer), and draws its controls at
the biggest scale that fits both its box and a four-deck deck
(`deck::scale_to_fit`). The overview takes the height to spare.

The controls row is flexbox too: `| CUE / play | page | tabs |`, mirrored on a left
deck with `.reverse(true)`. The page takes the width to spare and stretches its
buttons across it (up to 1.3 times their own width). The page widths it scales from
are design constants in `panels/deck/mod.rs` (`PLAYER_PAGE_W`, `PAGE_W`).

The mixer is drawn at a fixed scale (`mixer::SCALE`), so its width is fixed, and it
stretches to the row's height (its faders take the height).

## Recipes

**Add a pane to a row** (say, a third pane in the library). Add a `flex::pane` to the
row in `LibraryPane::show`, with a share (`basis_pct`) or `grow()`, and a least width
(`min_w`, named in `layout.rs`).

**Make a pane resizable.** Size it from a share of its row (`basis_pct(share)`), and
call `drag_to_resize(ui, Edge::Right, Region::YourPane, row_width, cx)` last inside
the pane. The share is saved (`UiPrefs::Sizes`), so it keeps its proportion in any
window.

**Add a row to the shell** (a strip under the decks, say). Add a part to the column
in `views/perform.rs::decks_rows` (or the mode's view): a `flex::pane` with its height
(`.h(..)` or `.h_pct(..)`) and `.no_shrink()`.

**Add a button to the deck's controls.** Draw it with `d.px(SIZE)` like its
neighbours. If its page got wider, raise `PLAYER_PAGE_W` or `PAGE_W` to the page's new
width at scale 1: `a_deck_fits_the_box_it_is_given` fails while it's too small.

**Add a window (modal).** Show it with `modals::show_fitted(ctx, egui::Window::new(..),
|ui| ..)`: it can't be bigger than the app's window and scrolls if it's too tall.

**Add a view, mode or tool.** Add it to the list in
`ui::app::tests::every_view_fits_every_window`. That test draws every view at every
window size from 1024 x 640 up, at two UI sizes, and fails if anything is past the
window's edge or over the status bar.

## Not done yet

The old hand-written layout also dealt with windows too small for everything. The
flexbox layout does the normal case; these haven't come back yet:

- Hiding the waveforms when the body would be too short.
- Dropping the mixer when the decks would be too small to read beside it.
- Hiding a library side pane when there's no room for it at its least width.

## Checking a change

```bash
cargo test -p earshot-gui every_view_fits
```

Pictures of every view at every size (written next to the path given):

```bash
EARSHOT_SHOT=/tmp/shot.png cargo test -p earshot-gui every_view_fits -- --nocapture
```
