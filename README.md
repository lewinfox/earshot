# earshot

A DJ app that combines a library explorer with performance decks: browse a music
folder by key, tempo and vibe, get next-track suggestions that follow whatever is
playing, and play the set in the same window from a DDJ-400, the keyboard or the
mouse. Native Rust throughout; the UI is [egui](https://github.com/emilk/egui).

The plan is in [`docs/design.md`](docs/design.md).

```
engine/        Rust: beatgrids, decoding, real-time decks + mixer (cpal), tests
gui/           the app (egui): library, decks, suggestions, Flow, MIDI, analysis queue
worker/        Python analysis worker (Beat This! beatgrids), its own uv venv
docs/design.md decisions and roadmap
```

## Install

Download the file for your computer from the
[releases page](https://github.com/lewinfox/earshot/releases), unpack it and run
`earshot-gui` in the folder it makes. Everything it needs is in that folder: the
analysis worker with its own Python and models, ffmpeg and Rubber Band. Nothing else
is installed, and deleting the folder removes it (your library stays in the data
folder, below).

- **Linux** (x86_64): `tar xf earshot-*-linux-x86_64.tar.xz`. Needs ALSA, which every
  desktop has.
- **macOS** (Apple silicon): the download isn't signed yet, so macOS refuses to open
  it. After unpacking, run `xattr -dr com.apple.quarantine earshot-*-macos-arm64`
  once.

## Build it from source (Linux)

System packages (Ubuntu/Debian names):

```bash
sudo apt install build-essential pkg-config libasound2-dev libdbus-1-dev libclang-dev
```

You also need Rust (stable) and [uv](https://docs.astral.sh/uv/). Your user should be
in the `audio` group.

Set up the analysis worker once (downloads CPU-only torch, about 1 GB). A build from
source uses `worker/.venv`; a release ships a ready-made one instead
(`packaging/bundle.sh`, see below).

```bash
cd earshot/worker && ./setup.sh
```

Then:

```bash
cd earshot && cargo run --release -p earshot-gui   # release build
cd earshot && cargo run -p earshot-gui             # dev build: quicker to compile, still optimised enough to play
```

Environment variables:

| Variable | Default | |
|---|---|---|
| `EARSHOT_AUDIO_DEVICE` | system default | Case-insensitive part of the output device name, e.g. `DDJ` |
| `EARSHOT_BUFFER` | `256` | Buffer size in frames (ignored if the device doesn't support it) |
| `EARSHOT_NULL_AUDIO` | unset | `1` runs the engine on a timer with no sound card |
| `PIPEWIRE_LATENCY` | `256/48000` under PipeWire | The audio chunk size PipeWire uses (frames/rate); earshot sets it unless you do |
| `EARSHOT_WORKER_DIR` | the release's own, else `earshot/worker` | A worker folder with its own `.venv` to use instead |
| `EARSHOT_DATA_DIR` | `~/.local/share/io.github.lewinfox.earshot` | Where `library.db` lives (e.g. to try things on a copy) |

If no audio device can be opened, earshot falls back to the null output and shows a
⚠ in the top bar. *Menu → Settings…* (or the audio pill) picks the device, buffer
size and which channels master and headphones go to. With no deck cued, the headphones
get the master mix. The same window sets how many tracks analyse at once, re-analyses
tracks whose analysis is out of date, and shows the keymap file: a `keymap.txt` in the
data folder replaces the built-in keys (*Make a copy to edit* writes one to start
from).

The top bar shows live numbers: UI fps and the audio callback's worst time against
its budget, with slow-callback and xrun counts. The footer is a MIDI monitor.

## Using it

- **Keyboard** (press *Keys* for the full list): left hand is deck A (`A` cue,
  `S` play, `D` sync, `Q`/`W` beat jump, `E` loop, `1`–`4` hot cues), right hand is deck
  B (`J` `K` `L`, `U`/`I`, `O`, `7`–`0`). `↑`/`↓` browse, `Tab` switches between the
  library and suggestions, `Shift+←`/`Shift+→` load to deck A/B, and `/` jumps to search.
  Deck keys are live unless a text field has focus; `Esc` gives them back.
- **Add music**: *Menu → Import → Folder…*. Folders are remembered in `library.db`.
  The left-hand tree shows *All tracks* and each folder; pick a folder to list the
  tracks in it and its subfolders. Right-click a top-level folder to remove it.
- **Load** a track: double-click it (goes to a deck that isn't playing), right-click →
  *Load to deck A/B*, drag it onto a deck, or `Shift+←`/`Shift+→`. Right-click →
  *Preview in headphones* plays it on the headphones only.
- **Analysis**: loading a track with no beatgrid starts analysis in the background; it
  plays meanwhile on a placeholder grid from its BPM tag, and the deck switches to the
  analysed grid when it's done. Right-click a track → *Analyse*, or a folder →
  *Analyse unanalysed tracks*, to do it ahead of time. Like Rekordbox there are two
  modes:
  - **Static** (the default): one steady tempo for the whole track, preferring whole
    numbers, in about 3–5 s. A fast tempo pass over the whole track, then Beat This!'s
    small model on five 15 s excerpts for the phase and the downbeat
    (`worker/earshot_worker/static_tempo.py`).
  - **Dynamic** (*Analyse (dynamic)*): Beat This! over the whole track, so the grid
    follows a drifting tempo (live drummers); about 15–30 s.

  Static analysis flags tracks whose tempo seems to drift with an amber ≈; right-click
  → *Analyse (dynamic)* for those. The dot by each track shows the state (green =
  analysed, amber = queued or running, red = failed; hover for the error). Four tracks
  analyse at a time (`EARSHOT_ANALYSIS_JOBS` to change); a track you load onto a deck
  jumps the queue. Grid edits on a deck are saved and are never overwritten by
  automatic analysis (an explicit *Analyse* does overwrite them).
- **Suggestions** (right-hand pane) follow the most recently started deck at its
  actual pitch (pitch shifts the key, since there is no keylock yet). Each row shows
  bars for key, tempo, vibe and energy; *Drop / Hold / Build* sets the energy
  direction. Tracks played this session are dimmed and ranked lower. Real files have
  no vibe yet, so suggestions rely on key, tempo and energy.
- **Flow** (tab beside *Tracks*): build a set as a chain of cards. *Add track*, drag
  from the list or right-click → *Add to Flow* places a card; the **+** on a card
  opens nine suggested next tracks (*Drop / Hold / Build*), and picking one adds a
  joined card. Drag from a **+** to another card to link them. ▶ previews a card in the
  headphones; double-click or right-click loads it to a deck. Drag the background to
  pan, wheel to zoom; *Full screen* hides the decks. The Flow is kept between runs.
- **Energy** is a stand-in for now: loudness of the busy parts plus brightness,
  ranked across your library. It's measured when a track is analysed or loaded;
  tracks without it count as middling.
- **Layout**: drag the gaps between panes to resize. *Menu → View → Size* or
  Ctrl −/=/0 scales the whole UI.
- **Search** with free text plus `bpm:120-126` and `key:8A` (or `key:Am`). It searches
  within the selected folder.
- **Beatgrids** are fixed in the grid editor (Prepare mode, or *GRID* on a deck): the
  red-over-white bar puts the 1 at the playhead, plus *BPM*, nudge, ÷2 and ×2. A tempo-lock
  `<file>.beats.json` sidecar next to a file is still used if there's no analysed grid.

## Tests

```bash
cd earshot && cargo test -p earshot-engine   # beatgrid, decks, loops, cue, sync on a drifting track
cd earshot && cargo test -p earshot-gui      # app, incl. off-screen UI tests
cd earshot && EARSHOT_TEST_TRACK=/path/song.mp3 cargo test -p earshot-gui -- --include-ignored analyses_a_real_track  # analysis end to end
cd worker && .venv/bin/python -m pytest
```

See [`gui/README.md`](gui/README.md) for the code layout and an off-screen screenshot.

## Releases

Pushing a `vX.Y.Z` tag makes a GitHub release (`.github/workflows/release.yml`). For
each platform, `packaging/bundle.sh` puts the app in a folder with everything it runs:

- a standalone Python with the worker's packages installed, as `worker/uv.lock` pins them
- the worker's code
- the Beat This! checkpoints
- ffmpeg and ffprobe (LGPL builds)
- Rubber Band (built from source on Linux)

The app finds these beside its binary (`gui/src/core/worker_env.rs`). Then
`packaging/smoke_test.py` runs the folder's own worker, models and programs before
it's uploaded. A download is about 400 MB, and about 1.7 GB unpacked.

`make bundle` makes the Linux folder locally, in `target/bundle/earshot`. It needs uv
and, to build Rubber Band, `sudo apt install build-essential libsndfile1-dev`.

## Known gaps

- No CLAP embeddings, key or energy analysis yet.
- Other libraries (through cratelib) can be read and imported, not written back to.
- Whole tracks are decoded into memory (about 10 MB per stereo minute).
- No controller LED feedback or effects.
- Hot cues are not saved.
- The worker has no timeout: if it hangs, the analysis queue stops until restart.
