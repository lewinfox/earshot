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
  src/         UI (TypeScript, no framework): decks, library, graph, suggestions
  src-tauri/   Rust: commands, control bus, MIDI, state publisher
docs/design.md decisions and roadmap
```

## Run it (Linux)

System packages (Ubuntu/Debian names):

```bash
sudo apt install build-essential pkg-config libwebkit2gtk-4.1-dev libgtk-3-dev \
  libasound2-dev libssl-dev librsvg2-dev libayatana-appindicator3-dev libdbus-1-dev
```

You also need Rust (stable) and Node 20+. Your user should be in the `audio` group.

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

If no audio device can be opened, earshot falls back to the null output and shows a
⚠ in the top bar. Master goes to outputs 1/2 and headphones to 3/4 when the device
has four channels (the DDJ-400 does). With no deck cued, the headphones get the
master mix.

`npm run dev` on its own serves the UI in a normal browser against a small mock of
the Rust core (no audio), which is handy for UI work.

## What to try and what to report

1. **Benchmark**: press *Benchmark → Run* with the window at your normal size. It
   loads two demo tracks, plays them synced, and pans and zooms the graph with Sigma.js
   and cosmos.gl at 2k and 10k nodes while changing the selection every 2 s. It
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
- **Drag** a track from the library, the suggestions or the graph onto a deck.
- **Search** with free text plus `bpm:120-126` and `key:8A` (or `key:Am`).
- **Suggestions** follow the most recently started playing deck at its actual pitch
  (pitch shifts the key, since there is no keylock yet). Each row shows bars for key,
  tempo, vibe and energy; *Drop / Hold / Build* sets the energy direction.
- **Your own music**: type a folder path and press *Scan*. Tracks get a beatgrid from
  a tempo-lock sidecar if one exists next to the file:

  ```bash
  cd ../tempo-lock && . .venv/bin/activate
  python -m tempolock analyse "/music/track.mp3" --compact > "/music/track.mp3.beats.json"
  ```

  Otherwise they get a placeholder steady grid from the BPM tag, starting at 0:00. Fix
  it with the grid buttons on the deck (*BPM*, ◁ ▷ nudge 10 ms, *1* = make the
  nearest beat a downbeat, ÷2, ×2).
- The library is padded with 2k/5k/10k synthetic tracks (fake genres with their own
  tempo, energy and "vibe" vectors). Every one is playable: the engine synthesises a
  loop at the track's tempo and key, and one in ten drifts like a live drummer.

## Tests

```bash
cd earshot && cargo test -p earshot-engine   # beatgrid, decks, loops, cue, sync on a drifting track
cd app && npm run typecheck
```

## Known gaps (by design, for the spike)

- No database, Python worker, CLAP embeddings or cratelib sync yet (phases 1–2).
- Whole tracks are decoded into memory (about 10 MB per stereo minute).
- No keylock, jog wheels, controller LED feedback or effects.
- Real files have no vibe or energy yet, so their suggestions rely on key and tempo.
- Grid edits and hot cues are not saved.
