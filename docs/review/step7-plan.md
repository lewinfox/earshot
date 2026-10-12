# Step 7: UI rebuild plan

The back end (review steps 0–6) is done. This step rebuilds `gui/src/ui/` on top
of it. Sources: [app-shell.md §3](app-shell.md), [library-ui.md §3](library-ui.md),
[post-review-ui-changes.md](post-review-ui-changes.md), [remaining.md](remaining.md)
and the user's notes in [FEATURES.md](../../FEATURES.md).

**Where the UI is now.** About 7,800 lines. Three files hold most of it:
`app.rs` (1,550), `flow.rs` (1,350) and `deck.rs` (1,340). Some pieces already
exist: the command bus, the keymap with generated help, `Selection` by track id,
one track menu (`tracks.rs`), the Prepare/Perform modes and Prepare's cue pane.

**Rule for every phase.** Every UI interaction produces `Command`s. The UI reads
core state and one engine snapshot per frame. No egui code in core, no SQL in UI.

## Layout (from FEATURES.md)

```
Perform:   | decks + mixer                                              |
           | Library (icon stack | track table)                          |

Prepare:   | player waveform                                | cue pane  |
           | [ Cues & Grid ] [ Tempo lock ] [ Flow ] ...   ← tool tabs    |
           | Library (icon stack | track table) | tool body | suggest ▸  |
```

- **Library** is one component, the same in both modes. A vertical icon stack on
  the left picks the source or collection: all tracks, playlists, crates, Flows,
  external libraries (via cratelib), and later SoundCloud, Beatport, Spotify.
  *What we're operating on.*
- **Tool tabs** in Prepare (horizontal): Cues & Grid (today's Prepare tools),
  Tempo lock, Flow editor, more later. *What we're doing to it.*
- **Flow editor**: `| library | canvas | suggestions (hideable) |`. Suggestions pop
  out from a vertical icon stack on the right, like Rekordbox.

## Phase A: the shell (one agent, alone, first)

Everything else builds on this, and it touches every UI file, so it runs alone.

1. **Theme** (`ui/theme.rs`): palette, deck colours (4), hot-cue and memory-cue
   colours, spacing, `panel_frame()`, `pill()`. Remove every raw RGB literal.
2. **Shell**: `app.rs` shrinks to a shell; `Mode` picks a view (`views/perform.rs`,
   `views/prepare.rs`). One `dispatch` is the only place state changes.
3. **Panels**: a `Panel` trait and a `Cx { core, engine frame, theme, out:
   Vec<Command> }`. Existing screens move into panels without redesign.
4. **Layout primitives**: the vertical icon stack and the tool tab bar, empty but
   working, so phase B fills them in.
5. **One `UiPrefs`** under one eframe key: mode, layout per mode, selected source,
   selected tab, zoom, panel sizes. Deck and audio settings move to the DB.
6. **Modal stack**: audio settings, keys help, later the metadata editor.
7. **Services injection**: tests start the app with a null audio output, an
   in-memory Store and a stand-in worker, so the `#[ignore]`d app tests run in CI.
8. **On-demand repaint**: redraw only while playing, previewing, scrubbing or
   analysing; otherwise wake on events.

Done when: the app behaves as before inside the new layout, app-level tests run
without a real library, no raw colours outside `theme.rs`.

## Phase B: panels (four agents in parallel, after A)

### B1. Decks, Prepare player and Prepare tools
- Split `deck.rs` into `panels/deck/{transport, pads, loops, tempo, grid_editor}`
  and a waveform panel.
- Perform lays out 2 or 4 decks (decision 2), with a crossfader side per deck and a
  sync-leader control. Mixer for N channels.
- Prepare: the Cues & Grid tab; walk through post-review-ui-changes.md and tick
  every item.
- Waveform: grid lines and bar numbers in the lead-in and past the end.
- Bugs/keys from FEATURES.md: Shift+←/→ grid nudge in Prepare (bound in the keymap
  but reported not working — find out why), Ctrl+↑/↓ load previous/next track from
  the library into Prepare.

### B2. Library component
- The left icon stack: all tracks, playlists, crates, Flows, external libraries
  (placeholder until cratelib import is built), SoundCloud/Beatport/Spotify as
  disabled placeholders.
- Folders and sub-folders for crates, playlists and Flows.
- One `TrackTable` with `ColumnDef`s: configurable columns, sort and filter by every
  column including analysis status (none, done, in progress, needs attention),
  widths saved per view.
- One `focused_pane` with a `Selection` each; keyboard, MIDI browse and suggestions
  follow it.
- `DragPayload::Tracks { ids, origin }` carries a whole selection, with a ghost
  under the mouse while dragging.
- Track menu: Add to Playlist, Add to Crate, Add to Flow (unconnected card).
- Drag to reorder playlist rows.
- Columns and actions for rating, colour and tags; metadata editor and bulk edit
  (modal), sending `Command::Edit`.
- Bug: suggestions' "Master" reference ignores Prepare's player.

### B3. Undo and the Flow editor
- `core/edit.rs`: `Edit` + `History` for metadata, grid edits, collection edits and
  Flow graph operations. Undo/redo from keys, MIDI and menus. Lands first as a small
  commit, because B2's metadata editor uses it.
- Flow editor as a Prepare tab: `| library | canvas | suggestions ▸ |`, with the
  suggestions pane opened from a right-hand icon stack.
- Split `flow.rs` into `panels/flow/`; use the theme, `DragPayload` (with ghost) and
  the shared track menu.
- Flow title editable on the canvas, updating the library tree live.
- Flow camera and last-open Flow saved in the DB.

### B4. Tempo lock tab
See decision 5.

## Phase C: settings and loose ends (one agent, after B)

- Settings modal: audio, analysis (background worker cap), keymap file.
- "Re-analyse stale tracks" action.
- Worker start-up check that its grid version matches the app's.
- Save play history to the `plays` table.
- Duplicates view (`Reader::duplicates()`).
- Resync deck sessions if the engine event ring overflows.

## Later (not step 7)
- Reading external libraries through cratelib (rekordbox, Mixxx, drives).
- Streaming sources (SoundCloud, Beatport, Spotify).
- Effects, slip, LED feedback.

## Decisions (agreed 2026-10-13)

1. **Flow editor** is a Prepare tool tab.
2. **Four decks:** a 2/4 toggle in Perform that shrinks the decks when four are
   shown, saved in `UiPrefs`.
3. **Waveform on the GPU:** measure frame time with 4 decks first; only move to the
   GPU if it's slow.
4. **Look:** keep the current look while restructuring.
5. **Tempo lock tab** (its own agent, B4, in phase B): analyse → choose a new BPM →
   straighten → save MP3. Keep tempo-lock's file naming and ID3 tag edits as they
   are. After straightening, a "Save to library" button with a dropdown of the
   configured library folders (`library_roots`). Keep all of tempo-lock's tempo
   graphs and histograms for now. "Normalise tempo" in the track menu opens the tab
   with the track loaded. Port tempo-lock (`../tempo-lock/`) into the worker by
   copying its modules and tests, as docs/design.md says.

## Phase A: what landed

The app works as before inside the new layout. Every colour is in `ui/theme.rs`.

### Module layout

```
core/
  view.rs        Mode, Source, Aside, Tool, Modal, Pane, Region, ViewCmd: what's on
                 screen, as plain data (no egui)
  settings.rs    deck and audio settings in the Store's `settings` table (JSON)
  services.rs    Services: database path, null audio, worker launcher, MIDI on/off
  command.rs     + AppCmd (reconnect MIDI, dismiss failures); ViewCmd moved to view.rs
ui/
  app.rs         the shell: EarshotApp, new(cc, Services), dispatch, update, draw
  app/           the shell's command handlers: events.rs, library_ops.rs,
                 preview.rs, input.rs (keys), frame.rs (engine read, repaint), tests.rs
  theme.rs       Theme + THEME, CUE_PALETTE, rgb()
  prefs.rs       UiPrefs (one eframe key, "earshot.ui")
  views/         mod.rs (Panels, show), perform.rs, prepare.rs
  panels/        mod.rs (Panel, Cx, CoreView, EngineFrame, PreviewState, resized)
    deck/        mod.rs (DeckUi + DeckPanel, WavePanel, PreparePlayer, CuePane), wave.rs
    library/     mod.rs (LibraryPane: icon stacks + panes), browser.rs (tree, track
                 list), collections.rs, suggest.rs, tracks.rs (track menu, Media)
    flow/        mod.rs (today's canvas, unchanged)
    mixer.rs     + MixerPanel
    status.rs    TopBar, MidiLog, AnalysisSummary
  widgets/       knob, fader, button, meter, chips (key_chip), icon_stack, tool_tabs
  modals/        mod.rs (Modals stack), audio.rs, keys.rs
```

### Panel and Cx

```rust
pub trait Panel { fn ui(&mut self, ui: &mut Ui, cx: &mut Cx); }

pub struct Cx<'a> {
    pub core: CoreView<'a>,      // library index, history, collections, open Flow
    pub engine: &'a EngineFrame, // snapshot, each player's DeckInfo, preview: read once a frame
    pub theme: &'a Theme,
    pub prefs: &'a UiPrefs,      // read only; change them by command
    pub out: &'a mut Vec<Command>,
}
```

- Panels keep their own view state (selection, scroll, text being typed, the Flow's
  camera). Everything else goes out as a `Command` (`cx.send(..)`), and the shell's
  `dispatch` carries it out after the frame. Mode, layout, focus, windows, zoom and
  panel sizes are `ViewCmd`s.
- A panel that needs layout numbers (a deck's width and side) is a small struct
  holding `&mut` its state plus those numbers, e.g. `DeckPanel { deck, width,
  height, side }`.
- `EngineFrame::deck(id)` gives a player's `DeckInfo`; `cx.engine.snap` the engine
  snapshot. Don't lock the player in a panel.
- `UiPrefs` holds the mode, a `Layout` per mode (source, aside, tool, panel sizes),
  wave zoom, UI scale, the 2/4 deck toggle (not used yet) and the Flow's saved
  camera. Audio settings and tempo ranges are in the Store (`core::settings`); the
  old eframe keys are no longer read, so the audio device is picked again once.

### Extension points

- **A library source (left icon stack):** add a variant to `core::view::Source`, an
  item to `SOURCES` in `panels/library/mod.rs`, and an arm in `LibraryPane::source`.
  `StackItem { enabled: false, .. }` greys out a placeholder.
- **A right-hand pane (right icon stack):** `core::view::Aside`, `ASIDES`, and
  `LibraryPane::aside`. The Flow editor's suggestions can be another stack built the
  same way (`widgets::icon_stack`, `Opens::Left`).
- **A Prepare tool tab:** a variant of `core::view::Tool`, an entry in `TOOLS` in
  `views/prepare.rs`, and an arm in `views::prepare::tool`. Today only "Cues & Grid".
- **A modal:** a variant of `core::view::Modal`, a module in `modals/`, an arm in
  `Modals::show` (and in `Modals::open` if it should start afresh). Open it with
  `ViewCmd::ToggleModal`.
- **A command for the UI:** a `ViewCmd` (screen state) or `AppCmd`, handled in
  `EarshotApp::view_cmd` / `dispatch_ui`.
- **A colour:** a field on `Theme`. Panels read `cx.theme`; code without a `Cx`
  (widgets) reads `THEME`.

### Tests and repaint

- `ui/app/tests.rs` starts the whole app with `Services { null_audio: true, worker:
  stand_in, midi: false, db: <temp> }` on four generated WAV tracks, analysed by the
  stand-in worker (`worker_client::testing::stand_in`, needs `python3`). They run in
  `cargo test`. With `EARSHOT_SHOT=/tmp/shot.png` each writes
  `/tmp/shot-<test>.png` (perform, prepare, flow).
- The app asks for a frame only while something moves (a deck, Prepare's player or
  the preview playing, a preview decoding, a load, a scrub, analysis) and for 0.5 s
  after. Otherwise input or an event wakes it: every `Sink` event requests a repaint.

### Not the same as before

- The library's left pane and the suggestions pane now sit beside icon stacks; the
  suggestions pane can be closed. Panel sizes are per mode and start from defaults
  once (new panel ids).
- Prepare's tool row is 34 points taller (the tab bar).
- The frame-time readout shows the real (low) idle rate, since idle frames stop.
