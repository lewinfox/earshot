# earshot — design

A DJ library explorer and performance app in one: browse a music folder by key,
tempo and "vibe", get next-track suggestions while you play, and play the set from
the same window with a DDJ-400, the keyboard or the mouse.

This note records the decisions made while scoping the project. The first piece of
code is a performance spike (see [Phase 0](#phase-0--performance-spike)); everything
after that is a plan, not a promise.

## Goals

- **Explore** a local library (2k tracks today, design for 10k) as a table and as a
  graph where each node is a track.
- **Suggest next tracks** from key, tempo, energy and vibe (CLAP embedding
  similarity), and do it *live*: suggestions follow whatever is playing, at its
  actual pitch.
- **Search** by structured filters (`bpm:122-126 key:8A`), natural language (CLAP
  text embeddings) and "more like this".
- **Sync** metadata, cues and playlists with Rekordbox, Serato and Mixxx through
  [cratelib](https://github.com/lewinfox/cratelib). All external-library access goes
  through cratelib, which already owns the safety guards (e.g. not writing while
  Rekordbox is open) and decoder offset corrections.
- **Prep** tracks: beatgrid editing, cues and loops, and tempo-lock's
  "straighten a live-drummed track" render.
- **Perform**: two decks, sync, loops, hot cues, beat jump, driven by a DDJ-400,
  the keyboard or the on-screen controls, with the library and suggestions
  on screen the whole time.

Non-goals for now: Windows, streaming services, effects beyond the basics,
more than two decks.

## Shape

One native desktop app with two modes, like Rekordbox:

- **Export mode** — library, graph, search, prep, sync with other DJ software.
- **Performance mode** — the same library panes, with decks above them.

```
┌──────────────────────────── earshot ─────────────────────────────────────┐
│  UI (Rust, egui)                                                         │
│   library · graph · search · suggestions · decks · waveforms             │
│        │ events                                                          │
│        ▼                                                                 │
│  Rust core ───────────────────────────────────────────┐                  │
│   control bus ◄── keyboard map / on-screen controls   │                  │
│        ▲     ◄── MIDI (midir) + Mixxx-style mappings   │                  │
│        ▼                                               │ local socket     │
│   audio engine (cpal)          DB writer ──► earshot.db│ (jobs/results)   │
│   decks · mixer · grids        (sole writer, SQLite)   ▼                  │
│                                              Python worker (uv venv)     │
│                                               Beat This! grids, key,     │
│                                               energy, CLAP, tempo-lock   │
│                                               render, cratelib           │
└──────────────────────────────────────────────────────────────────────────┘
```

### Languages and why

| Part | Language | Reason |
|---|---|---|
| Audio engine, MIDI, control bus | Rust | Real-time audio needs no GC and no GIL. cpal (audio), midir (MIDI), symphonia (decoding) cover Linux and macOS. |
| UI | Rust (egui) | A webview UI was tried first (Phase 0) and couldn't hold 60 fps on Linux while playing. egui draws the waveforms and library natively, in the same process as the engine. |
| Search, suggestions, DB | Rust | Live suggestions must not depend on the worker being alive. Brute-force cosine over 10k × 512 floats is ~1 ms. |
| Analysis, CLAP, tempo-lock, cratelib | Python worker | The best tools (Beat This!, Essentia, CLAP, cratelib itself) are Python. Can be ported to Rust (ONNX via `ort`) piece by piece later. |

### The Python worker

- A **separate process**, never embedded: a crash or GC pause in torch must not be
  able to touch the audio thread.
- **Stateless.** It receives jobs ("analyse this file", "embed this text",
  "render this straightened version", "export this crate to Rekordbox") and returns
  results. It never opens `earshot.db`.
- **Own environment**: `earshot/worker/` is a uv project with its own `.venv`;
  earshot launches the worker from that venv and never touches the host Python.
  System packages it needs (`ffmpeg`, `rubberband-cli`) are documented in the
  README.
- Protocol: length-prefixed JSON over a Unix socket (stdin/stdout is the fallback).
  About a dozen message types.
- **What still works without it**: playback, cues, loops, grids, browsing, filters,
  "more like this", next-track suggestions. **What needs it**: importing/analysing
  new files, natural-language search (the query text must be embedded), renders,
  cratelib sync.
- **CLAP** comes from the existing earshot CLAP project, which will be folded in
  here. Until then the worker exposes `embed_audio` / `embed_text` / `categorise`
  as stubs with the real interface.
- **tempo-lock** (`../tempo-lock/`) is folded in by *copying* its `analysis`,
  `detectors` and `render` modules and tests into the worker (projects in this repo
  never import each other). Its web UI becomes the Prep view.

### No Docker

The audio engine cannot run in a container, and a SQLite file shared between a host
process and a container is unsafe on macOS (WAL needs shared memory across the VM
boundary). The uv-managed venv gives the isolation Docker was wanted for.

## The control bus

Every action is a named control, using Mixxx's names so Mixxx controller mappings
can be loaded later:

```
[Channel1] play              [Channel1] cue_default     [Channel1] sync_enabled
[Channel1] beatjump_forward  [Channel1] beatloop_4_toggle
[Channel1] hotcue_1_activate [Channel1] rate            [Channel1] volume
[Master]   crossfader        [Library]  MoveVertical    [Channel1] LoadSelectedTrack
```

Inputs write controls, outputs read them:

```
DDJ-400 (MIDI → mapping) ─┐
Keyboard (key → control) ─┼──► control bus ──► audio engine
On-screen controls ───────┘        └──► feedback: UI state, controller LEDs
```

- **Keyboard** mapping is a file of key → control. Deck shortcuts are live unless a
  text field has focus; `Esc` always returns focus to the decks; the UI shows which
  layer is active.
- **MIDI**: the spike hardcodes a few DDJ-400 messages. The real thing embeds QuickJS
  (`rquickjs`) and implements Mixxx's `engine` / `midi` scripting API against the
  control bus, so Mixxx's XML + JS mappings load as-is. Implement only what the
  DDJ-400 mapping calls first. Mixxx mappings are GPL-2.0+: loading a user's files
  is fine, bundling them carries GPL obligations.
- **Discovery controls** have no Mixxx equivalent and get their own group, e.g.
  `[Suggest] MoveVertical`, `[Suggest] LoadToDeck`; the DDJ-400's browse encoder
  can toggle between the library and the suggestion list.

## Beatgrids

A grid is **every beat stored individually**, each with its **position in the bar**
(1, 2, 3, 4 …). "Is this a 1-beat" is `position == 1`. Storing the position rather
than a boolean costs nothing and handles odd-length bars. Rekordbox's own grid
format is the same shape (per beat: time, tempo, beat number 1–4).

Tempo segments ("124.0 drifting to 126.3") are *derived* from the beats, for
display, for suggestion scoring, and for export to Serato/Mixxx, whose formats are
compressed marker lists.

### Storage format (`beatgrids.beats` BLOB, v1)

All little-endian. Struct-of-arrays so times can be read without touching positions.

| Offset | Size | Field |
|---|---|---|
| 0 | 4 | magic `EBGR` |
| 4 | 2 | format version, `1` |
| 6 | 2 | flags, `0` (reserved) |
| 8 | 4 | `n`, number of beats |
| 12 | 8·n | beat times, `f64` seconds from the start of the decoded audio, strictly increasing |
| 12+8n | n | bar position per beat, `u8`; `0` = unknown, `1..=255` = position in bar |

Total `12 + 9n` bytes: ~7 KB for a 6-minute track at 128 BPM. Times are seconds, not
sample frames, so a grid survives resampling. Implemented in
`engine/src/beatgrid.rs` (`Beatgrid::to_bytes` / `from_bytes`).

A grid's provenance (`analysis`, `user`, `import:rekordbox` …) lives in its own
column, so re-analysis never overwrites a hand-fixed grid.

### Using a grid

Everything is "find the beats either side of *t*, interpolate":

- **Beat position at *t*** → fractional beat index (binary search + interpolation).
  Before the first and after the last beat, extrapolate at the edge tempo, so beat
  jump works in a beatless intro.
- **Local tempo** → 60 / inter-beat interval, averaged over a few beats.
- **Sync** → match the leader's local tempo × rate and beat phase, continuously, so
  it follows a drifting track.
- **Beat jump N** → beat index ± N back to a time: always whole real beats.
- **Loops** → from beat *i* to beat *i + N*.
- **Quantise** → snap to the nearest beat.

### Editing a grid

Nearly all edits act on the whole grid:

| Edit | Behaviour |
|---|---|
| Set BPM to *x* | Scale every inter-beat interval about an anchor beat so the mean tempo is *x*. Exact for a steady grid; preserves the drift shape of a drifting one. |
| Nudge ±*ms* | Shift every beat. |
| Set this beat as the 1 | Renumber bar positions in 4/4 outward from the chosen beat. |
| Halve / double | Drop every other beat (keeping the 1s) / insert midpoints. |

After any edit the grid is re-extended or trimmed to cover the whole track. Moving a
single beat is supported by the format but deliberately not a first-class tool.

### Decoder offsets

Beat times are relative to a decoder. Inside earshot the worker and the Rust engine
must decode identically (to be verified: symphonia vs the worker's ffmpeg). Offsets
against Rekordbox/Serato/Mixxx are cratelib's job; it already has logic for this.

## Database

One SQLite file, `earshot.db`, WAL mode. **The Rust app is the only writer**, through
a single DB thread; the audio thread never touches it (track data is loaded on a
loader thread and handed over through lock-free queues). The worker never opens it.
Migrations are plain `.sql` files applied in order at startup.

Sketch of the first schema:

```sql
tracks(id, path UNIQUE, file_hash, size, mtime, duration_s, sample_rate,
       title, artist, album, genre, year, comment, added_at, missing,
       derived_from INTEGER REFERENCES tracks, derivation JSON)   -- straightened versions
analysis(track_id PK, bpm, bpm_min, bpm_max, key_camelot, key_confidence,
         energy, loudness_lufs, analyser_version, analysed_at)
beatgrids(track_id PK, beats BLOB, source, updated_at)           -- format above
waveforms(track_id PK, data BLOB, bins_per_s, version)
embeddings(track_id, model, dim, vector BLOB, PRIMARY KEY (track_id, model))
tags(track_id, tag, source, confidence)                          -- CLAP categories etc.
cues(id, track_id, kind, slot, position_s, length_s, label, color) -- memory/hot cues, saved loops
crates(id, name, parent_id, kind, query JSON); crate_tracks(crate_id, track_id, position)
sets(id, started_at, ended_at); set_plays(set_id, track_id, deck, started_at, ended_at)
external_links(track_id, library, external_id, synced_at)        -- cratelib mapping
jobs(id, kind, track_id, status, error, created_at, finished_at) -- worker queue, survives restarts
```

`derived_from` + `derivation` link a straightened render to its original: it
inherits tags, crates and (re-timed) cues, and suggestions treat the pair as one
track. Embeddings are keyed by model so CLAP can be joined by other models later.

## Suggestions

Score every candidate against the reference (selected track, or the playing deck at
its effective pitch), and show the breakdown:

- **Key** — Camelot compatibility: same, ±1, relative major/minor; optional energy
  moves (+2, +7). With keylock off, pitch shifts the key, so use the effective key.
- **Tempo** — within ±N % of the effective BPM, counting half/double time.
- **Vibe** — cosine similarity of CLAP embeddings.
- **Energy** — the change in energy, so you can ask for "build" rather than "same".
- **Live context** — penalise tracks already played in this set; prefer tracks you
  can mix in before the current one ends.

The graph uses the same scores for its edges; clicking a node shows its
neighbourhood.

## Performance engine notes

- Audio callback budget at 256 frames / 44.1 kHz is 5.8 ms. The callback never
  allocates, locks or frees: commands arrive on an `rtrb` ring buffer, replaced
  track data goes back out on another to be freed elsewhere, state is published
  through atomics.
- DDJ-400: built-in 4-channel USB audio, master on 1–2 and headphones on 3–4; the
  engine routes master and cue separately from the start. Jogs send relative ticks
  with touch; pads have hot cue / beat loop / beat jump / sampler modes.
- Later: keylock and time-stretch (Rubber Band R3 or Signalsmith Stretch), streaming
  decode with a cache instead of whole-track decode, output-latency compensation of
  the drawn playhead, effects, recording.

## Phases

### Phase 0 — performance spike

Done. Throwaway code to answer one question: can a webview UI on Linux (WebKitGTK)
draw our worst-case screen while real audio plays?

- Graph view with 2k–10k synthetic tracks and ~5 edges each, measured in fps while
  panning/zooming.
- Two decks with colour waveforms scrolling against a playhead fed from the audio
  clock, beat markers with 1-beats highlighted.
- Real audio through cpal with master and headphones routed separately; callback
  timing and overruns reported.
- DDJ-400 play/cue/sync, browse encoder and load buttons through midir; a MIDI
  monitor to check the message table.
- Keyboard control with the focus rule, next to a search box.
- A built-in benchmark that runs a scripted 20 s scenario and saves a report.

**Pass**: steady 60 fps for graph + waveforms together, no audio dropouts at a
256-frame buffer, playhead visibly in time with the audio.

**Result**: fail. The webview couldn't hold 60 fps while playing, so the UI was
rebuilt natively in egui (`gui/`). The spike's beatgrid module, control names and
engine structure survived; its UI code was deleted.

### Phase 1 — library

DB + migrations, folder scan, worker with its uv env, analysis (Beat This! grids
via tempo-lock's code, key, energy), CLAP stubs, library table, filters, graph,
suggestions.

### Phase 2 — sync

cratelib import (existing BPM/key/cues/playlists) and export (crates → playlists,
cues, grids).

### Phase 3 — performance

Streaming decode, sync/quantise, persisted hot cues and loops, keylock, Mixxx
mapping loader (QuickJS), keyboard map file, set history feeding suggestions.

### Phase 4 — prep

Grid editor, cue editor, tempo-lock straighten with A/B and click track, track
versions.

### Phase 5 — macOS

CoreAudio/CoreMIDI are covered by cpal/midir; the work is packaging the worker venv
and signing.
