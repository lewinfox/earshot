# egui rewrite: what's left

The egui app (`gui/`) replaces the Tauri app (`app/`). These are the steps to reach
parity, then retire the Tauri app. The TS files named below are the reference for each
feature.

## 1. Check it on a real screen

The first pass was only checked off-screen (kittest renders and frame timing).

- [x] Run it in a window: 59–60 fps, also while analysing several tracks.
- [x] Import a folder through *Menu → Import → Folder…* (rfd dialog).
- [ ] DDJ-400 over MIDI: every mapped control, and that the jog wheels feel right.
      (Waiting on the controller.)
- [x] Keyboard shortcuts, including held buttons (cue, nudge) and Shift + hot cue to clear.
- [ ] Panel sizes, zoom and UI size survive a restart.

## 2. Suggestions (`app/src/suggest.ts`)

- [x] Scorer in `core/suggest.rs` (key 0.35, tempo 0.3, vibe 0.25, energy 0.1), half and
      double tempo, per-part breakdown, unit tests. Vibe is a neutral 0.5 until there
      are sound-alike embeddings.
- [x] Reference track: the deck that started playing most recently, at its actual
      tempo and pitch; otherwise the track selected in the library.
- [x] "Played this session": counts after 30 s of play; dimmed in both lists and
      scored lower.
- [x] Suggestions pane on the right: Drop / Hold / Build, ranked rows with score bars
      (hover for numbers), double-click, drag or right-click to load. Tab (or the
      controller's browse button) switches the keyboard between library and suggestions.
- [ ] Try it in a real set: are the rankings sensible?

## 3. Flow canvas (`app/src/flow.ts`)

- [ ] Add a Tracks | Flow tab switch in the library area.
- [ ] Flow canvas: pan and zoom, track nodes (art, title, key and BPM chips), and edges
      between nodes. Add tracks with "Add to Flow" or by dragging.
- [ ] A "+" button on each node's right-hand side opens a 3×3 grid of nine suggestions,
      with Drop / Hold / Build. Picking one adds it as a connected node.
- [ ] Load a node to a deck by dragging it or double-clicking.
- [ ] Keep the Flow across restarts. The Tauri app saves it in localStorage
      (`earshot.flow`); use eframe storage here.

## 4. Library graph (`app/src/graph.ts`)

- [ ] Decide whether it is still wanted. If it is, port it as an egui painter: wheel
      layout (Camelot) and vibe layout (PCA), key colours, hover and select synced with
      the list. Plain egui shapes should cope with ~2,000 points; check that.

## 5. Smaller gaps

- [ ] Audio output: show the device, rate and buffer in the audio pill with the same
      tooltip text. Add a way to restart audio or pick a device (`start_audio`,
      `list_audio_devices`).
- [ ] Reconnect MIDI by clicking the MIDI pill.
- [ ] Benchmark (`perf.ts`, `runBenchmark`, `save_report`): a scripted run that writes
      a frame-time report. The kittest `screenshot` test covers some of this; decide
      whether a live in-app benchmark is still needed.
- [ ] Library control from MIDI only handles browse and load. Check `libraryControl`
      in `main.ts` for anything else, such as switching to the Suggestions pane.

## 6. Retire the Tauri app

- [ ] Once 1–5 are done, delete `app/`. Remove it from the workspace `members` and
      update both READMEs.
- [ ] Move `migrations/` ownership fully to `gui/`. It is a copy today; after `app/` is
      gone there is only one.
- [ ] Drop the `ordered-float` 5.0.0 pin in `Cargo.lock` when Rust is updated past 1.89.
