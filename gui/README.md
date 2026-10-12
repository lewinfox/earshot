# earshot (egui)

earshot's UI, in Rust with egui. It uses the engine (`../engine`) and the analysis
worker (`../worker`); the library lives in `library.db`. How to install and use it is in
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
src/ui/     egui. app.rs is the shell: it starts the services, reads the engine once
            a frame and carries out every command in one `dispatch`. views/ lays
            out each mode (perform.rs, prepare.rs) from panels/ (deck/, library/,
            flow/, mixer.rs, status.rs); widgets/ are shared controls and layout
            pieces; modals/ the windows; theme.rs every colour; prefs.rs what the
            UI keeps between runs.
```

What's left to do is in `TODO.md`.

Tests: `cargo test -p earshot-gui`. The app-level tests (`ui/app/tests.rs`) start the
whole app on the null audio output, a temporary database, a stand-in analysis worker
and a few generated tracks. To also render the window off-screen (with frame-build
timing), give them a path; each test writes its own picture beside it
(`/tmp/shot-perform.png`, `/tmp/shot-prepare.png`, `/tmp/shot-flow.png`):

```bash
EARSHOT_SHOT=/tmp/shot.png cargo test --release -p earshot-gui ui::app -- --nocapture
```
