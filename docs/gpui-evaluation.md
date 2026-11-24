# Should earshot's UI move from egui to GPUI?

Written November 2026. This weighs [GPUI](https://github.com/zed-industries/zed/blob/main/crates/gpui/README.md)
(the UI framework Zed is built on) against the egui UI we have now. Nothing has been
built or measured yet: the claims about GPUI come from its README, its examples and
its `Cargo.toml` on Zed's `main`.

**Short answer:** GPUI would make most layout code simpler. It would probably make
frames cheaper for the CPU too. But it means rewriting the whole UI layer (about 70k
lines in `gui/`) against an API that is pre-1.0, mostly documented by Zed's own source,
and in practice only usable from a git revision. We already hold 60 fps, so the speed
gain doesn't justify that. The layout gain might, but cheaper fixes are worth trying
first (see *Cheaper alternatives*).

## What GPUI is

- **A hybrid of immediate and retained mode.** State lives in `Entity<T>`s that GPUI
  owns. A view is an entity that implements `Render`. Each frame it returns a tree of
  elements (`div()`, `canvas`, `uniform_list`, ...). A view is re-rendered only when it
  calls `cx.notify()`, and an unchanged view can reuse last frame's output.
- **Layout is [Taffy](https://github.com/DioxusLabs/taffy).** Zed pins `taffy = 0.15`,
  which does flexbox and CSS grid. You style it with Tailwind-like methods:
  `div().flex().flex_col().gap_3().w_full().min_w(px(240.))`.
- **It draws on the GPU.** It batches quads, shadows, paths, glyph sprites and images,
  and caches glyphs in an atlas. On macOS it uses Metal. On Linux and Windows, Zed's
  `main` now uses `wgpu`, pinned to a git revision. Linux needs the `x11` and/or
  `wayland` features.
- **Other parts:** `#[gpui::test]` and `TestAppContext` for tests, a headless mode,
  AccessKit (it's in the dependencies), actions with keymaps, and its own async
  executor.
- **Licence:** Apache-2.0.

## Pros

### Layout

- **Layout becomes declarative, and it's the model `docs/ui-layout.md` already copies.**
  `layout::split` with `Track::fixed`, `fill` and `sized(..).yields(n)` is a hand-made
  flexbox row, and the doc even keeps a table of CSS equivalents. In Taffy these are
  `flex_none`, `flex_1 + min_w` and `flex_shrink` + `min_w`. Children are measured and
  share the space themselves, so we wouldn't have to work out sizes top-down before
  drawing.
- **Several rules in `ui-layout.md` exist only to work around egui:**
  - "A widget takes the size it asks for and overlaps its neighbour."
  - "Don't use `.resizable(true)`."
  - "`egui::Grid` without `.min_col_width(0.0)`."
  - "A bar wider than the window widens everything after it."
  - The `CONTROLS_W` constant, which a test keeps in sync with what the deck really
    draws.

  In Taffy, content-measured sizes, `overflow_hidden` and `min_w(0)` replace those
  rules.
- **CSS grid is built in,** which suits the mixer, the pads and the deck's control
  rows.
- **Wrapping falls out of `flex_wrap`.** The top bar's effect units wrapping to a
  second line would be one style call, not custom code.

### Performance

- **Less CPU per frame for many shapes.** egui tessellates every shape into one mesh on
  the CPU each frame. Our scrolling waveforms add "a few thousand rectangles per frame"
  per deck (`panels/deck/wave.rs`), plus the library table and the Flow canvas. GPUI
  sends quads to the GPU as instances, so the CPU cost per rectangle is smaller.
- **Views that haven't changed aren't rebuilt.** egui rebuilds the whole UI every
  frame. While a deck plays, that includes the library table, the collections tree,
  the top bar and so on. In GPUI, only the views that called `notify()` would run:
  the waveforms, the playheads and the meters. A large library with a playing deck is
  where this would show most.
- **Virtualised lists are built in** (`uniform_list`, `list`). The data table example
  scrolls large row counts. We do something similar by hand with
  `egui_extras::TableBuilder`.

### Other

- **It's proven on a demanding app:** Zed runs at high frame rates on all three
  platforms.
- **Text rendering is good,** with platform shaping (CoreText, DirectWrite, cosmic-text
  on Linux). egui's text works, but needs the fallback fonts we install in
  `ui/fonts.rs`.
- **There's a widget kit, `gpui-kit`** (Longbridge, formerly `gpui-component`): tables,
  virtual lists, dock layout and charts, with AccessKit coverage. It pins a matching
  GPUI release.

## Cons

### Cost and risk

- **It's a full rewrite of the UI.** About 70k lines in `gui/`. Most of `core/`
  (store, query, MIDI, bus, analysis) would survive, but every panel, modal, view and
  widget would be rewritten. The coding style changes too:
  - Today `if ui.button(..).clicked() { out.push(cmd) }` sits next to the drawing.
  - In GPUI, state is an entity, events go to listener closures
    (`cx.listener(|this, ev, window, cx| ..)`), and borrowing goes through
    `Entity::update`.

  The `Command`/bus design helps here, but it is still a rewrite.
- **It's pre-1.0 and changes often.** The README says: "There will often be breaking
  changes between versions." The docs say the best way to learn the APIs is to "read
  the Zed source code or drop a question in the Zed Discord".
- **The crates.io release is far behind.** The latest is `gpui 0.2.2`, published
  2025-10-22, a year ago. Since then Zed's `main` has split out crates
  (`gpui_platform`, `gpui_util`, `gpui_web`, ...) and moved Linux and Windows to `wgpu`
  at a git revision. In practice we'd depend on a Zed git revision, or on `gpui-kit`'s
  pinned snapshots, and follow Zed's changes ourselves. Our release builds (`.deb`,
  `.rpm`, `.dmg`, Windows installer) would ship a pinned git dependency.
- **GPUI exists for Zed.** gpui.rs says: "For the near future, GPUI is tied to Zed."
  Zed's needs come first. A DJ app's needs (constant 60 fps animation, custom vector
  drawing, MIDI-driven updates) aren't what it is tuned for.

### Things we use that GPUI doesn't have built in

- **Pan and zoom.** The Flow canvas uses egui's `Scene` for this. I found no
  equivalent in GPUI, so cards, edges and the minimap would need their own transform
  code.
- **Our controls.** Sliders, knobs, toggles, round buttons and drag-to-resize
  splitters. We draw most of these ourselves already (`ui/widgets/`), so this is
  porting work rather than a blocker. `gpui-kit` may have some.
- **The test suite.** `egui_kittest` drives the real app, takes pictures, and powers
  `every_view_fits_every_window` and the snapshot tests: about 3.4k lines of tests in
  `ui/app/tests.rs`. GPUI has `TestAppContext` and headless windows, but I couldn't
  confirm that screenshot tests are supported. The "nothing past the window's edge"
  test would need a new way to inspect layout bounds.

### The gains are smaller than they look

- **"Draw only what changed" doesn't help while a deck plays.** Waveforms, playheads,
  meters and the BPM display change every frame, so those views re-render at 60 fps
  either way. The saving is in what doesn't change: the library and the panels.
  That's real but bounded.
- **Flexbox doesn't do everything `layout.rs` does.** Some of its rules aren't
  flexbox:
  - "One scale for all decks, the biggest that fits."
  - Hiding the waveforms when the body would be too short.
  - Proportional drag-to-resize saved as a share of the row.

  These would stay custom code, as measure functions or arithmetic before render.
  Taffy also won't stop overflow by itself: we'd still need `min_w` and
  `overflow_hidden` discipline, and still a test for it.
- **We already hold 60 fps.** That's in a window, while analysing several tracks
  (`gui/TODO.md`). The webview spike failed on frame rate. egui didn't.
- **It would be a second GPU path to support on Linux.** Linux users have varied GPU
  drivers. eframe's OpenGL (glow) backend is the most widely compatible. Zed has had
  its share of Linux GPU-driver issue reports; the move to wgpu may reduce them.

### Smaller costs

- Bigger dependencies and longer builds.
- Needs the latest stable Rust.
- A second async executor next to our threads, though we don't have to use it.

## Cheaper alternatives

These go after the same two goals without a rewrite.

1. **Layout: Taffy inside egui.** The `egui_taffy` crate puts flexbox and grid inside
   egui `Ui`s. We could move `layout::split` users to it one row at a time. Or we could
   keep `layout.rs`: it's already tested at every window size, and most of the pain in
   `ui-layout.md` is about discipline, not missing features.
2. **Performance: draw the waveforms on the GPU inside egui.** An
   `egui::PaintCallback` with a small `wgpu` (or glow) shader could draw the scrolling
   waveform from the min/max and band textures. That removes the biggest per-frame mesh
   and is local to `panels/deck/wave.rs`.
3. **Performance: skip work that hasn't changed.** For example, build the library
   table's row strings only when the query or sort changes, not every frame. Profile
   first (`puffin` works with egui) to see what actually costs time.

## If we want to find out for sure

A one- or two-day spike, separate from the app:

- In GPUI, from a Zed git revision, draw two scrolling waveforms from real
  `DeckWave` data at 60 fps.
- Add a 10k-row virtualised library table.
- Add a three-pane flex layout that resizes with the window.
- Measure CPU time per frame and the frame rate on Linux (X11 and Wayland) and macOS.
- Compare with the same scene in egui using `puffin`.
- Note how much code the layout took compared with `layout.rs`.

That would show whether the speed gain is real for our workload, and how awkward
GPUI's API is for our kind of drawing, before we commit to a rewrite.
