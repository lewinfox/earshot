# earshot

A DJ app that combines a library explorer with performance decks: browse a music
folder by key, tempo and vibe, get next-track suggestions that follow whatever is
playing, and play the set in the same window from a DDJ-400, the keyboard or the
mouse.

**Status: Phase 0, a performance spike.** It answers one question: can a Tauri app on
Linux (WebKitGTK) draw the worst-case screen (a 10k-node graph plus two scrolling
waveforms) while real audio plays without dropouts? The plan for everything after
that is in [`docs/design.md`](docs/design.md).

```
engine/        Rust: beatgrids, decoding, real-time decks + mixer (cpal), tests
app/           Tauri app
  src/         UI (TypeScript, no framework): decks, folder tree, library, graph, suggestions
  src-tauri/   Rust: commands, control bus, MIDI, state publisher, earshot.db, analysis queue
worker/        Python analysis worker (Beat This! beatgrids), its own uv venv
docs/design.md decisions and roadmap
```

## Run it (Linux)

System packages (Ubuntu/Debian names):

```bash
sudo apt install build-essential pkg-config libwebkit2gtk-4.1-dev libgtk-3-dev \
  libasound2-dev libssl-dev librsvg2-dev libayatana-appindicator3-dev libdbus-1-dev
```

You also need Rust (stable), Node 20+ and [uv](https://docs.astral.sh/uv/). Your user
should be in the `audio` group.

Set up the analysis worker once (downloads CPU-only torch, about 1 GB):

```bash
cd earshot/worker && ./setup.sh
```

Then:

```bash
cd earshot/app
npm ci
npm run tauri dev                                # development build with hot reload
# or a release build, which is what to benchmark:
npx tauri build --no-bundle && ../target/release/earshot
```

Environment variables:

| Variable | Default | |
|---|---|---|
| `EARSHOT_AUDIO_DEVICE` | system default | Case-insensitive part of the output device name, e.g. `DDJ` |
| `EARSHOT_BUFFER` | `256` | Buffer size in frames (ignored if the device doesn't support it) |
| `EARSHOT_NULL_AUDIO` | unset | `1` runs the engine on a timer with no sound card |
| `EARSHOT_ANALYSIS_JOBS` | `4` | Tracks analysed in parallel (one worker process each) |
| `EARSHOT_WORKER_DIR` | `earshot/worker` | Where the analysis worker and its `.venv` live |

If no audio device can be opened, earshot falls back to the null output and shows a
⚠ in the top bar. Master goes to outputs 1/2 and headphones to 3/4 when the device
has four channels (the DDJ-400 does). With no deck cued, the headphones get the
master mix.

`npm run dev` on its own serves the UI in a normal browser against a small mock of
the Rust core (no audio, a five-track fake library), which is handy for UI work.

## What to try and what to report

1. **Benchmark**: press *Benchmark → Run* with the window at your normal size. It
   loads two synthesised demo tracks (replacing what's on the decks), plays them synced,
   and pans and zooms the graph with Sigma.js and cosmos.gl, padded with synthetic tracks
   to 2k and 10k nodes, while changing the selection every 2 s. It
   saves a JSON report to `~/.local/share/io.github.lewinfox.earshot/reports/`.
   Pass criteria: about 60 fps average, 1%-low well above 30, and zero audio xruns
   and slow callbacks.
2. **Listen** with the DDJ-400 as the output: any clicks while you pan the graph or
   scroll the library?
3. **Playhead**: do the waveforms scroll smoothly and in time with what you hear?
4. **MIDI**: the footer is a MIDI monitor. The DDJ-400 table in
   [`app/src-tauri/src/midi.rs`](app/src-tauri/src/midi.rs) is from memory of
   Mixxx's mapping and has not been tested on hardware. Press each control and check
   the monitor says what it should (play, cue, sync, headphone cue, loop in/out,
   reloop, tempo, channel faders, crossfader, browse encoder, load buttons, hot cue
   pads).

The top bar shows live numbers: UI fps (average · 1% low · worst frame) and the
audio callback's worst time against its budget, with slow-callback and xrun counts.

If the window is blank or the graph is very slow, try `WEBKIT_DISABLE_DMABUF_RENDERER=1`
(a common WebKitGTK workaround, especially on NVIDIA). WebKit reports its GPU as
"Apple GPU" for privacy, so the report's `gpu` field won't name your card.

## Using it

- **Keyboard** (press *Keys* for the full list): left hand is deck A (`A` cue,
  `S` play, `D` sync, `Q`/`W` beat jump, `E` loop, `1`–`4` hot cues), right hand is deck
  B (`J` `K` `L`, `U`/`I`, `O`, `7`–`0`). `↑`/`↓` browse, `Tab` switches between the
  library and suggestions, `Shift+←`/`Shift+→` load to deck A/B, and `/` jumps to search.
  Deck keys are live unless a text field has focus; `Esc` gives them back.
- **Add music**: *Menu → Import → Folder…* and pick one or more folders. They're
  remembered in `earshot.db` (in `~/.local/share/io.github.lewinfox.earshot/`). The
  left-hand tree shows *All tracks* and each folder; pick a folder to list the tracks
  in it and its subfolders. Right-click a top-level folder to remove it.
- **Load** a track: double-click it (goes to a deck that isn't playing), right-click →
  *Load to deck A/B*, drag it onto a deck, or `Shift+←`/`Shift+→`.
- **Analysis**: loading a track with no beatgrid starts analysis in the background; it
  plays meanwhile on a placeholder grid from its BPM tag, and the deck switches to the
  analysed grid when it's done. Right-click a track → *Analyse*, or a folder →
  *Analyse unanalysed tracks*, to do it ahead of time. Like Rekordbox there are two
  modes:
  - **Static** (the default): one steady tempo for the whole track, preferring whole
    numbers, in about 3–5 s. A fast tempo pass over the whole track, then Beat This!'s
    small model on three 15 s excerpts for the phase and the downbeat
    (`worker/earshot_worker/static_tempo.py`).
  - **Dynamic** (*Analyse (dynamic)*): Beat This! over the whole track, so the grid
    follows a drifting tempo (live drummers); about 15–30 s.

  Static analysis flags tracks whose tempo seems to drift with an amber ≈; right-click
  → *Analyse (dynamic)* for those. The dot by each track shows the state (green =
  analysed, amber = queued or running, red = failed; hover for the error). While a
  track analyses, a bar fills under its row, the top bar shows its percentage, and so
  does its deck if it's loaded. Four tracks analyse at a time
  (`EARSHOT_ANALYSIS_JOBS` to change); a track you load onto a deck jumps the queue.
  Grid edits on a deck are saved and are never overwritten by automatic analysis (an
  explicit *Analyse* does overwrite them).
- **Flow** (tab beside *Tracks*): build a set as a chain of cards. *Add track* (or
  right-click a track → *Add to flow*) places a starting card; the **+** on a card's
  right opens nine suggested next tracks, with *Drop / Hold / Build* for the energy
  direction, and picking one adds it joined to that card. A card can have several
  next tracks. Drag cards by their title, drag the background to pan, wheel to zoom;
  ▶ or double-click loads the track onto a free deck. *Full screen* hides the decks.
  The flow is kept in the app's local storage.
- **Energy** is a stand-in for now: loudness of the busy parts plus brightness,
  ranked across your library. It's measured when a track is analysed or loaded;
  tracks without it count as middling.
- **Layout**: drag the gaps between panes (and the line under the decks, which sets
  the waveform height) to resize; double-click one to reset it. *Menu → View → Size*
  or Ctrl −/=/0 scales the whole UI. Both are remembered.
- **Waveform thumbnails** show left of each track that has been analysed or loaded.
- **Search** with free text plus `bpm:120-126` and `key:8A` (or `key:Am`). It searches
  within the selected folder.
- **Suggestions** follow the most recently started playing deck at its actual pitch
  (pitch shifts the key, since there is no keylock yet). Each row shows bars for key,
  tempo, vibe and energy; *Drop / Hold / Build* sets the energy direction. Real files
  have no vibe or energy yet, so suggestions rely on key and tempo.
- **Beatgrids** are fixed with the grid buttons under each deck (*BPM*, ◁ ▷ nudge
  10 ms, *1* = make the nearest beat a downbeat, ÷2, ×2). A tempo-lock
  `<file>.beats.json` sidecar next to a file is still used if there's no analysed grid.

## Tests

```bash
cd earshot && cargo test -p earshot-engine   # beatgrid, decks, loops, cue, sync on a drifting track
cd app && npm run typecheck
cd app/src-tauri && EARSHOT_TEST_TRACK=/path/song.mp3 cargo test --lib -- --include-ignored  # analysis end to end
cd worker && .venv/bin/python -m pytest
```

## Known gaps (by design, for the spike)

- No CLAP embeddings, key or energy analysis, playlists or cratelib sync yet (phases 1–2).
- Whole tracks are decoded into memory (about 10 MB per stereo minute).
- No keylock, jog wheels, controller LED feedback or effects.
- Hot cues are not saved.
- The worker has no timeout: if it hangs, the analysis queue stops until restart.
