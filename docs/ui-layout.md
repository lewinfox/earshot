# UI layout: how it works and how to change it

earshot's UI is egui, which draws everything in one pass, top to bottom. Unlike CSS,
nothing in egui shares space out by itself: a widget takes the size it asks for, and
if that's more than there is, it overlaps its neighbour or runs off the window. So the
sharing is done by us, **once, from the top down, before anything is drawn**.

## The rules

1. **Sizes are decided in `gui/src/ui/layout.rs` and nowhere else.** It's plain
   arithmetic (no drawing), so it's unit-tested at every window size.
2. **A panel fills the size it's given.** It never picks its own outer size and never
   looks at the window's.
3. **Shared sizes have names** in `gui/src/ui/theme.rs` (`THEME.spacing`): margins,
   bar heights, the icon stack's width. A panel's own design sizes live with the
   panel (`panels/deck/design.rs`, the constants at the top of `panels/mixer.rs`).
4. **Anything that can hold more than fits scrolls** (`egui::ScrollArea`), and the
   body is clipped to its place (`views::body`), so nothing paints over the status bar.
5. **The UI size is egui's zoom** (`gui/src/ui/window.rs`). It scales text, controls
   and spacing together, so no code multiplies sizes by a text scale. A bigger UI
   size means the same window holds fewer points, and the layout simply gets less room.

## The pieces

```
window (ui/window.rs: smallest 1024 x 640, kept on the screen, UI size)
├─ top bar            panels/status.rs   its effect units wrap to a second line
├─ shell              layout::shell()    everything between the bars:
│  ├─ waveforms         share of the height, draggable; hidden if the body would be too short
│  ├─ decks + mixer     one scale for all decks, the biggest that fits (layout::DeckRow)
│  ├─ tab bar           Prepare's tools, Controller's tabs
│  └─ body              what's left: the library (layout::library_row), a tool beside
│                       it (layout::beside_row), the Flow canvas (layout::flow_aside)
└─ status bar
```

`layout::split(length, [Track, ...])` is the one sharing primitive, and it works like
a flexbox row:

| Track | CSS equivalent |
|---|---|
| `Track::fixed(36.0)` | `flex: 0 0 36px` |
| `Track::fill(240.0)` | `flex: 1; min-width: 240px` |
| `Track::sized(want, min)` | `flex: 0 1 want; min-width: min` |
| `.yields(n)` | which tracks shrink first when there isn't room (highest first) |

It never hands out more than `length`.

## Recipes

**Add a pane to a row** (say, a third pane in the library). Add a field to
`LibraryRow`, a `Track` to the `split` in `layout::library_row`, and draw it with
`egui::SidePanel::...exact_width(row.your_pane)`. Give it a least width and a
`yields` order. Add it to `the_library_row_fits_every_width`.

**Make a pane resizable.** Call `drag_to_resize(ui, Edge::Right, Region::YourPane,
row_width, cx)` last inside the pane. The size is saved as a share of the row
(`UiPrefs::Sizes`), so it keeps its proportion in any window.

**Add a row to the shell** (a strip under the decks, say). Add its height to
`ShellInput`/`Shell` and a `Track` to the `split` in `layout::rows`; draw it in
`views/perform.rs::decks_rows` with `TopBottomPanel::top(..).exact_height(shell.x_h)`.

**Add a button to the deck's controls.** Add its design size to `deck/design.rs` and
draw it with `d.px(SIZE)` like its neighbours. If the row got wider, the test
`the_controls_are_as_wide_as_the_layout_expects` fails and tells you the new width:
put it in `CONTROLS_W` (`panels/deck/mod.rs`). That one number is how the layout
knows the deck's width.

**Add a window (modal).** Show it with `modals::show_fitted(ctx, egui::Window::new(..),
|ui| ..)`: it can't be bigger than the app's window and scrolls if it's too tall.
If its content has a fixed width wider than about 1000 points, lay it out narrower
when `ui.available_width()` is small (see `modals/sampler.rs`, `modals/keys.rs`).

**Add a view, mode or tool.** Add it to the list in
`ui::app::tests::every_view_fits_every_window`. That test draws every view at every
window size from 1024 x 640 up, at two UI sizes, and fails if anything is past the
window's edge or over the status bar.

## What to avoid

- `exact_width` / `exact_height` / `set_width` with a number that isn't from
  `layout.rs` or a theme token.
- `.resizable(true)` on a panel: egui then remembers a pixel size that ignores the
  window. Use `drag_to_resize`.
- `egui::Grid` without `.min_col_width(0.0)` for anything that scales: its columns
  are otherwise at least 40 points wide.
- Letting a bar's contents be wider than the window: egui widens everything drawn
  after it. Clip to the bar's rect (see the top bar).

## Checking a change

```bash
cargo test -p earshot-gui layout::
```

```bash
cargo test -p earshot-gui every_view_fits
```

Pictures of every view at every size (written next to the path given):

```bash
EARSHOT_SHOT=/tmp/shot.png cargo test -p earshot-gui every_view_fits -- --nocapture
```
