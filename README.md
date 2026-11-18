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
`earshot` in the folder it makes. Everything it needs is in that folder: the
analysis worker with its own Python and models, ffmpeg and Rubber Band. Nothing else
is installed, and deleting the folder removes it (your library stays: see *Where
earshot keeps things*, below).

- **Linux** (x86_64; Ubuntu 24.04, Debian 13, Fedora 40 or newer): install the
  `.deb` (`sudo apt install ./earshot_*_amd64.deb`, or open it in your software
  centre) or the `.rpm` (`sudo dnf install ./earshot-*.x86_64.rpm`). earshot is in the
  app menu and runs as `earshot`; `sudo apt remove earshot` (or `dnf remove`)
  removes it. On other distributions, unpack the `.tar.xz` and run `earshot` in it.
- **macOS** (Apple silicon, macOS 14 or later): open the `.dmg` and drag earshot to
  Applications.

## Development

earshot is a Rust app (`gui/`, `engine/`, `audio/`) with a Python analysis worker
(`worker/`) it runs as a separate process. Building it needs a few system libraries,
Rust and uv; running it from a checkout uses the worker's own venv.

### What to install

The reference system is **Ubuntu 24.04**: CI builds and tests the Linux release on it
(the oldest system the Linux packages run on: glibc only runs what was built against
it or older), so it's the safest base for development too. Other distributions work
with the same libraries under their own names.

```bash
# Building earshot: ALSA, D-Bus, PipeWire, libclang (for PipeWire's bindings)
sudo apt install build-essential pkg-config libasound2-dev libdbus-1-dev \
  libpipewire-0.3-dev libclang-dev
# What the window loads as it starts (a desktop has these already)
sudo apt install libxkbcommon-dev libwayland-dev libx11-dev libxcursor-dev \
  libxrandr-dev libxi-dev libgl1-mesa-dev
# Only for `make bundle` (it builds Rubber Band): libsndfile's headers
sudo apt install libsndfile1-dev
```

- **Rust**, stable, from [rustup](https://rustup.rs).
- **[uv](https://docs.astral.sh/uv/)**, recent (0.13 or newer: `make bundle` uses
  `uv export --emit-index-url`). `uv self update` updates it.
- **Docker**, only for `make packages` (nfpm runs from its image unless it's installed).
- Your user in the **`audio` group**, for real-time audio and the controller's sound card.

On macOS (Apple silicon), Xcode's command line tools, Rust and uv are all it needs.

### Running it

```bash
make run            # dev build: quicker to compile, still optimised enough to play
make run-release    # release build
```

The first time (and after the worker's dependencies change) `make run` sets up the
worker first, with `worker/setup.sh`: it downloads CPU-only torch and the Beat This!
models into `worker/.venv` (about 1 GB), and installs ffmpeg and Rubber Band if
they're missing (with `sudo apt`, so it may ask for your password). A release ships
all of this ready-made instead (`packaging/bundle.sh`, below).

`make install` adds the release build to your app menu. `make help` lists the rest.

Environment variables:

| Variable | Default | |
|---|---|---|
| `EARSHOT_AUDIO_DEVICE` | system default | Case-insensitive part of the output device name, e.g. `DDJ` |
| `EARSHOT_BUFFER` | `256` | Buffer size in frames (ignored if the device doesn't support it) |
| `EARSHOT_NULL_AUDIO` | unset | `1` runs the engine on a timer with no sound card |
| `PIPEWIRE_LATENCY` | `256/48000` under PipeWire | The audio chunk size PipeWire uses (frames/rate); earshot sets it unless you do |
| `EARSHOT_WORKER_DIR` | the release's own, else `earshot/worker` | A worker folder with its own `.venv` to use instead |
| `EARSHOT_DATA_DIR` | unset | Keep everything (library, config, cache, logs) in this one folder, e.g. to try things on a copy |

If no audio device can be opened, earshot falls back to the null output and shows a
⚠ in the top bar. *Menu → Settings…* (or the audio pill) picks the device, buffer
size and which channels master and headphones go to. With no deck cued, the headphones
get the master mix. The same window sets how many tracks analyse at once, re-analyses
tracks whose analysis is out of date, and shows the keymap file: a `keymap.txt` in the
config folder replaces the built-in keys (*Make a copy to edit* writes one to start
from).

### Where earshot keeps things

Nothing goes anywhere in your home folder but these four folders (on Linux, the XDG
base directories; `$XDG_DATA_HOME` and the rest move them):

| | Linux | macOS | What's in it |
|---|---|---|---|
| data | `~/.local/share/earshot` | `~/Library/Application Support/io.github.lewinfox.earshot` | `library.db`: your library and settings |
| config | `~/.config/earshot` | as data | `keymap.txt`, `controllers/` (your own mappings) |
| cache | `~/.cache/earshot` | `~/Library/Caches/io.github.lewinfox.earshot` | stems, tempo lock's working files: made again when needed |
| state | `~/.local/state/earshot` | as data | logs, debug exports, the window's size and place |

To remove every trace of earshot on Linux, delete those four (after uninstalling):

```bash
rm -rf ~/.local/share/earshot ~/.config/earshot ~/.cache/earshot ~/.local/state/earshot
```

Versions before 0.1.1 kept everything in `~/.local/share/io.github.lewinfox.earshot`;
earshot moves it into these folders the first time it starts.

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
  *Preview in headphones* plays it on the headphones only (on the main output if the
  headphones are off, e.g. a laptop with no controller).
- **Analysis**: a track new to the library gets a *quick grid* in the background: one
  steady tempo worked out in a fraction of a second by signal processing (ported from
  [ClubCraft](https://github.com/beneaze/ClubCraft), MIT). It's saved, so the track
  has a BPM and a grid on a deck straight away (*Menu → Settings… → Analysis →
  Quick-analyse new tracks* turns this off). The full analysis is yours to run:
  right-click a track → *Analyse*, or a folder → *Analyse unanalysed tracks*. It
  replaces the quick grid, as fixing the grid by hand does. While the computer is
  short of resources (a low-power profile, capped CPUs, the audio struggling to keep
  up, or under 1.5 GB of memory left: the top bar says *background work on hold*),
  the audio gets what there is: the full analysis waits in its queue and the tracks
  asked for get a quick grid meanwhile, and stems aren't started (ones under way stop
  if the audio is struggling). It all carries on by itself once there's room again.
  Nothing is taken from the sound: keylock and the effects stay as they are. A track with no grid at
  all (the quick grid wasn't sure of it, or the setting was off) is analysed in the
  background when it's loaded: it plays at once, on a quick grid or a placeholder from
  its BPM tag, and the deck switches to the analysed grid when it's done. Like
  Rekordbox there are two modes:
  - **Static** (the default): one steady tempo for the whole track, preferring whole
    numbers, in about 3–5 s. A fast tempo pass over the whole track, then Beat This!'s
    small model on five 15 s excerpts for the phase and the downbeat
    (`worker/earshot_worker/static_tempo.py`).
  - **Dynamic** (*Analyse (dynamic)*): Beat This! over the whole track, so the grid
    follows a drifting tempo (live drummers); about 15–30 s.

  Static analysis flags tracks whose tempo seems to drift with an amber ≈; right-click
  → *Analyse (dynamic)* for those. The status column shows the state (green dot =
  fully analysed, amber dot = quick grid only, amber ring or percentage = queued or
  running, red = failed, drifting or marked as needing attention; hover for the
  reason). Four tracks
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
- **Effects** (the FX panel beside the mixer): two units, each running *Echo*
  (repeats on the beat that fade and dull; the repeats ring on after the unit is
  switched off or the fader comes down) or *Trans* (cuts the sound on the beat). Per
  unit: the effect, its length in beats (◀ ▶ halve and double it, from 1/16 to 16), *Level* (how loud the
  repeats are, how deep the cuts go), what it's on (deck A/B…, or *MAIN*, which
  follows the sync leader or the playing deck) and *ON*. Keys: `G`/`H` switch units
  1/2 on and off, `T`/`Y` pick the next effect, `Shift+G`/`Shift+H` the next target, `C`/`V` halve and double unit 1's length and
  `B`/`N` unit 2's.
- **Sampler** (*Menu → Sampler…*): 16 slots, each a short sound (a file, or the
  track selected in the library; its first two minutes, kept in memory) with its own
  volume and a *Loop* switch. With the DDJ-400's pads in SAMPLER mode a pad plays its
  slot from the start and SHIFT + pad stops it: slots 1–8 on the left deck's pads,
  9–16 on the right's. A pad is lit while its slot has a sound and flashes while it
  plays. Several can play at once, into the master mix. The slots are kept between
  runs (`sampler_N_play` / `sampler_N_stop` for keys and other controllers).
- **Mixer and FX** switches in the top bar show or hide those panels mid-set.
- **Layout**: drag the gaps between panes to resize. *Menu → View → Size* or
  Ctrl −/=/0 scales the whole UI.
- **Search** with free text plus `bpm:120-126` and `key:8A` (or `key:Am`). It searches
  within the selected folder.
- **Beatgrids** are fixed in the grid editor (Prepare mode, or *GRID* on a deck): the
  red-over-white bar puts the 1 at the playhead, plus *BPM*, nudge, ÷2 and ×2. A tempo-lock
  `<file>.beats.json` sidecar next to a file is still used if there's no analysed grid.

## Tests

`make test` runs the Rust tests, `make test-worker` the worker's. Or one at a time:

```bash
cd earshot && cargo test -p earshot-engine   # beatgrid, decks, loops, cue, sync on a drifting track
cd earshot && cargo test -p earshot-gui      # app, incl. off-screen UI tests
cd earshot && EARSHOT_TEST_TRACK=/path/song.mp3 cargo test -p earshot-gui -- --include-ignored analyses_a_real_track  # analysis end to end
cd worker && .venv/bin/python -m pytest
```

See [`gui/README.md`](gui/README.md) for the code layout and an off-screen screenshot.

## Releases

Pushing a `vX.Y.Z` tag makes a GitHub release (`.github/workflows/release.yml`).
`make release` does it: bump `version` in `gui/Cargo.toml` (through a PR), then, on an
up-to-date `main`, it tags `v<version>` and pushes the tag, after checking and asking.
For each platform, `packaging/bundle.sh` puts the app in a folder with everything it runs:

- a standalone Python with the worker's packages installed, as `worker/uv.lock` pins them
- the worker's code
- the Beat This! checkpoints
- ffmpeg and ffprobe (LGPL builds)
- Rubber Band (built from source on Linux)

The app finds these beside its binary (`gui/src/core/worker_env.rs`). Then
`packaging/smoke_test.py` runs the folder's own worker, models and programs before
it's uploaded. A download is about 400 MB, and about 1.7 GB unpacked.

Linux also gets a `.deb` and an `.rpm` of the same folder (`packaging/linux/`,
installed in `/opt/earshot`), and macOS a signed, notarized `.dmg`
(`packaging/macos/package.sh`; signing needs the `MACOS_CERTIFICATE*` and `NOTARY_*`
secrets, so it only happens in CI).

To make the Linux downloads locally (see *What to install* for what they need):

```bash
make bundle      # the folder, checked by the smoke test: target/bundle/earshot
make packages    # the .deb and .rpm of it: target/packages
```

## Known gaps

- No CLAP embeddings, key or energy analysis yet.
- Exports (Prepare's Export tab) write whole playlists; there's no two-way sync yet.
- Whole tracks are decoded into memory (about 10 MB per stereo minute).
- No controller LED feedback. Effects are Echo and Trans only, and the DDJ-400's FX
  section isn't mapped yet. Its PAD FX 1 / 2 pads play a fixed bank of 16 effects
  each (`gui/src/core/pads.rs`): not rekordbox's own defaults, and not editable yet.
- Hot cues are not saved.
- The worker has no timeout: if it hangs, the analysis queue stops until restart.
