# earshot (egui)

earshot's native UI, rebuilt in Rust with egui because the Tauri/WebKitGTK UI couldn't
hold 60 fps on Linux while playing. It shares the engine (`../engine`), the analysis
worker (`../worker`) and the library database (`earshot.db`) with the Tauri app.

```bash
cd earshot/worker && ./setup.sh     # once: the analysis worker
cd earshot && cargo run --release -p earshot-gui
```

Same environment variables as the Tauri app (`EARSHOT_AUDIO_DEVICE`, `EARSHOT_BUFFER`,
`EARSHOT_NULL_AUDIO`, `EARSHOT_ANALYSIS_JOBS`, `EARSHOT_WORKER_DIR`), plus
`EARSHOT_DATA_DIR` to use a different library database.

```
src/core/   no drawing: database, library scan, analysis queue, decks (player),
            control bus, MIDI. Talks to the UI through `Event`s.
src/ui/     egui: app.rs (window and panels), deck.rs, wave.rs (waveforms),
            library_ui.rs (folder tree, track list), util.rs (colours, keys, search)
```

Done so far: decks (waveforms, overview, hot cues, grid editor, transport, jog, pitch),
mixer, library folders and track list (thumbnails, multi-select, drag to deck,
right-click analysis), Suggestions (Drop / Hold / Build), static/dynamic analysis with
progress, keyboard and MIDI, UI size (Ctrl −/=/0; panel sizes and zoom are remembered).

Not yet ported from the Tauri app: the Flow canvas, the library graph and the
benchmark. See `TODO.md` for the remaining steps.

Tests: `cargo test -p earshot-gui`. An off-screen screenshot of the whole window (and
frame-build timing), on a copy of the library:

```bash
EARSHOT_DATA_DIR=/tmp/copy EARSHOT_NULL_AUDIO=1 EARSHOT_SHOT=/tmp/shot.png \
  cargo test --release -p earshot-gui -- --ignored screenshot --nocapture
```
