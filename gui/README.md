# earshot (egui)

earshot's UI, in Rust with egui. It uses the engine (`../engine`) and the analysis
worker (`../worker`); the library lives in `earshot.db`. How to install and use it is in
the [top-level README](../README.md).

```bash
cd earshot/worker && ./setup.sh     # once: the analysis worker
cd earshot && cargo run --release -p earshot-gui
```

Environment variables are listed in the top-level README.

```
src/core/   no drawing: database, library scan, analysis queue, decks (player),
            control bus, MIDI, Camelot keys, suggestion scoring. Talks to the UI
            through `Event`s.
src/ui/     egui: app.rs (window and panels), deck.rs, wave.rs (waveforms),
            library_ui.rs (folder tree, track list), suggest_ui.rs, flow.rs,
            util.rs (colours, search)
```

What's left to do is in `TODO.md`.

Tests: `cargo test -p earshot-gui`. An off-screen screenshot of the whole window (and
frame-build timing), on a copy of the library:

```bash
EARSHOT_DATA_DIR=/tmp/copy EARSHOT_NULL_AUDIO=1 EARSHOT_SHOT=/tmp/shot.png \
  cargo test --release -p earshot-gui -- --ignored screenshot --nocapture
```
