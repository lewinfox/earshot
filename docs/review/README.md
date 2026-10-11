# Architecture review — October 2026

This is a root-and-branch review of earshot, written before anything ships. Five
reviewers each covered one area. Their reports are in this folder:

| Area | Report | Scope |
|---|---|---|
| Audio engine and control path | [engine.md](engine.md) | `engine/src/*`, `core/player.rs`, `core/controls.rs`, `core/midi.rs` |
| Data model and persistence | [data-model.md](data-model.md) | `gui/migrations`, `core/db.rs`, `core/library.rs`, `core/flows.rs`, eframe storage |
| Analysis pipeline | [analysis.md](analysis.md) | `core/analysis.rs`, `worker/`, beatgrid, waveform, loudness, energy, `core/suggest.rs` |
| App shell and performance UI | [app-shell.md](app-shell.md) | `ui/app.rs`, `deck.rs`, `wave.rs`, `mixer.rs`, `controls/*`, `audio_ui.rs` |
| Library, curation and set-building UI | [library-ui.md](library-ui.md) | `library_ui.rs`, `flow.rs`, `flows_ui.rs`, `suggest_ui.rs`, search |

This page pulls the reports together. It covers the themes that came up in more than
one area, the bugs that were checked against the code, a proposed target
architecture, and an order for rebuilding. Line references are to commit `8bb7c31`.

**Requirements.** Two kinds of feature are coming:

1. **Performance tools:** effects, playback controls, slip mode and so on.
2. **Library management:** metadata editing, tagging, crates, playlists, smart
   playlists, relocation, and cratelib sync.

Nothing has been deployed, so every recommendation below assumes we can rebuild,
including squashing the migrations.

---

## The short version

Some parts are already good and should be kept as they are:
- The real-time discipline in the engine: decoding happens off the audio thread,
  freeing goes through a garbage ring, and the DSP doesn't allocate.
- The beatgrid lookups (`beat_at`, `time_at`, `quantize`) don't allocate, which is
  what slip and quantize need.
- The renderer test rig.
- The widget tests.
- The research behind the static-tempo analyser.

Above that level, the app has grown in layers. Every feature added its own way of
holding state and its own way of sending commands. This has led to five problems:

- **The domain model lives in the UI.** The `Track` type, play history, energy
  ranking, the Flow graph operations, the search parser and the
  suggestion-reference builder are all in `ui/`. None of them can be tested
  without egui, and a future Prep or Library mode couldn't reuse them.
- **Everything is a string.** Control-bus keys, grid sources (`"analysis"`,
  `"user"`, `"analysis (dynamic)"`…), analysis states (`"queued"`, `"running"`),
  cache stamps and deck group names are all strings, each copied into two to four
  places.
- **Two mutexes carry all the state.** `Arc<Mutex<Connection>>` and
  `Arc<Mutex<Player>>` are each locked by the UI, MIDI, loader, analysis, scan and
  art threads. The UI thread runs SQLite, and `edit_grid` writes to the DB while
  holding the player lock. design.md promises a single DB writer thread, and there
  isn't one.
- **The model has no identity or provenance.** A track is identified by its path.
  A rescan overwrites tags. BPM has three sources that drift apart. A user's grid
  edit overwrites the analysed grid in place. Library management can't be built on
  this.
- **Two decks and fixed chains are assumed throughout.** That assumption appears
  in `[_; 2]`, `1 - d` sync, `xf: [f32; 2]` and `index == 0` mirroring. There is a
  fixed strip (gain → EQ → filter), a separate preview player, and a flat
  35-variant `Command` enum. Effects, slip and a sampler would each need another
  special case.

None of this is a fault of the individual features. It is what you get from
iterating fast with no shared core. The fix is to put that core in place before
the next round of features.

---

## Problems that came up in more than one area

### 1. There is no single command path
Several reviewers found this independently.
- `core::controls::apply` interprets `(group, key, f64)` strings at runtime.
- Toggle logic is written four times, as a read-modify-write on a snapshot that
  can be stale:
  - MIDI: `midi.rs:191`
  - keyboard: `app.rs:843`
  - deck widgets: `deck.rs:808, 1086, 1098, 1108`
  - mixer: `mixer.rs:231`
- Some actions skip the bus altogether: grid edits, `TempoRange`, load, preview,
  and the jump and loop size steppers.
- The key help text (`app.rs:1340`) is a second hand-written copy of the key map
  (`app.rs:1290`).
- There are also four separate UI action enums: `deck::Action`, `LibAction`,
  `FlowsAction` and `AudioAction`.

**What this blocks:** effects, slip, MIDI mapping of anything that isn't on the
bus, LED feedback, undo, and new modes.

### 2. State is owned in the wrong places, or in two places
- **Jump and loop size** is held in both `DeckUi` (`deck.rs:240`) and
  `player::Inner` (`player.rs:39`).
- **What's loaded on a deck** is held in both `DeckSlot` and the engine's
  `Deck`, each with its own generation counter.
- **The engine is the only owner of performance state.** Hot cues, the cue point,
  loops, EQ, volume and auto-gain exist only on the audio thread. An audio-device
  restart (`player.rs:171`) loses all of them, and hot cues can't be saved.
- **The engine publishes state as about 35 separate `Relaxed` atomics per deck.**
  The UI can read them mid-update and mix fields from two different tracks.
- **Played this session** lives in `LibraryUi`.
- **The open Flow's id** (a DB key) is kept in eframe storage, while the Flow
  itself is in the DB.

### 3. There is no layer between the UI and the database
- `app.rs:252-370` does Flow CRUD with raw `rusqlite`.
- `db::previews` (`db.rs:341`) scans every preview blob on the UI thread and
  writes inside a read. `app.rs:762` calls it for every finished analysis.
- `flows::list` loads every card of every Flow to compute `linear`, after each
  save. `save` deletes and reinserts every card.
- `load_track` (`player.rs:195`) does all of the following in one function:
  decodes the file, re-measures energy and loudness and writes them to the DB on
  every load, migrates old grids, computes the waveform, and queues analysis.

### 4. Identity and provenance
- A track is identified by its `path` string, built with `to_string_lossy`. A
  moved file becomes a new track with no analysis, and its Flow cards stop being
  drawn while their edges are still drawn.
- `upsert_tracks` (`db.rs:127`) overwrites title, artist, BPM and key from the tags
  whenever size or mtime change, so any in-app edit would be lost.
- BPM can come from `analysis.bpm`, `tag_bpm` or the active grid, and they don't
  agree. `edit_grid` doesn't update `analysis.bpm`, so after you fix a grid by hand
  the library, search and suggestions keep the old tempo.
- Grid provenance is a free-text string. "Is this track analysed" is worked out
  from a hard-coded list of those strings, which is copied in `library_ui.rs:18`
  and `player.rs:254`.

### 5. Duplicated code and computation

| What | Where |
|---|---|
| Track list rows (title/artist cell, played-dimming, key chip) | `library_ui.rs:494`, `suggest_ui.rs:185`, `flow.rs:1028`, plus the Flow search popup |
| Track context menus, each with a different set of actions | `library_ui.rs:594`, `suggest_ui.rs:244`, `flow.rs:971` |
| Selection model, three different ones | library (`HashSet` + indices), suggestions (focus index), Flow (card id) |
| Waveform thumbnail cache | `library_ui.rs:117`, `flow.rs:101` |
| Flow card type | `core/flows.rs:31` and `ui/flow.rs:38` |
| Loop check for graph and folders | `flow.rs:295`, `flows.rs:148`, `flows_ui.rs:280` |
| Building the suggestion `Reference` | `app.rs:529`, `flow.rs:772` |
| Deck group strings `[Channel1]` | `controls.rs:139`, `midi.rs:45`, `deck.rs:251`, `app.rs:1291` |
| Energy and loudness measurement | on every deck load (`player.rs:203`) and again in analysis (`analysis.rs:288`) |
| `kick_sections` | `static_tempo.py:150`, `snap.py:79` |
| Tempo rounding | Python `choose_tempo` and Rust `Beatgrid::straightened` (the Rust copy is kept only for a legacy migration) |
| Colours | `util.rs` palette, `controls/mod.rs`, `ui/mod.rs`, and raw RGB in `deck.rs`, `wave.rs`, `button.rs`, `meter.rs` and `flow.rs` (12 in `flow.rs`) |

---

## Bugs confirmed in the code

These are worth fixing whatever happens to the rebuild. Each one was checked
against `8bb7c31`.

1. **A hand-fixed grid can be overwritten by analysis that was already running.**
   `keep_user` is decided from `existing`, which is read when the job *starts*
   (`analysis.rs:265`). The DB write happens at `:352-359`. A grid edited during
   that window is overwritten and pushed to the deck by `update_decks`.
2. **A forced re-analysis is silently dropped** if the track is already queued or
   running (`analysis.rs:214`: `pending.insert` fails and the function returns).
   *Analyse (dynamic)* on a queued track does nothing.
3. **A background lane takes a job before it waits for a slot** (`analysis.rs:166`).
   If that track is then loaded onto a deck, the urgent re-route can't find it, and
   the deck waits behind a throttled idle job.
4. **Grid edits don't update the track's BPM** (`player.rs:302-344`). The library
   and suggestions show the old tempo. Analysis also overwrites `analysis.bpm` even
   when it keeps a user grid (`analysis.rs:355`).
5. **An audio-device restart resets the decks.** `start_engine` (`player.rs:171`)
   resends only `Load` (with `generation: 0`) and the routing. Hot cues, the cue
   point, loops, EQ, faders, keylock, sync and auto-gain are all lost.
6. **`edit_grid` writes to SQLite while holding the player mutex**
   (`player.rs:303` → `:341`). It runs on the UI thread and blocks MIDI while it
   does.
7. **A bad deck index panics the audio thread.** Some commands use
   `self.decks[deck]` and others use `get_mut` (`engine.rs:912` and others).
8. **The analysis pill has a dead branch**: both arms are `util::WARN`
   (`app.rs:978`).
9. **Static and dynamic grids have different phase.** Static grids are snapped to
   the kick (`static_tempo.py:338`). Dynamic grids are snapped only to the
   onset-strength peak (`analysis.py:192`). The research found Beat This! runs
   20–40 ms late, so dynamic grids are probably late.
10. **There is no worker timeout and readiness is checked with a substring match**
    (`analysis.rs:549, 581`). One hung worker stalls its lane for good.

---

## Target architecture

```
┌──────────────────────── ui (egui) ─────────────────────────────────────────┐
│ Shell { mode: Perform|Prep|Library, modals, toasts, UiPrefs, Theme }       │
│ panels/  deck/{transport,pads,loops,tempo,grid,fx} mixer waveform          │
│          browser(TrackTable) collections suggest flow                      │
│ widgets/ knob fader button meter key_chip pill  bound(ControlSpec)         │
│   every interaction → Vec<Command>    reads: CoreView + EngineSnapshot     │
└────────────────────────────┬───────────────────────────────────────────────┘
                             │ Command                ▲ Event (one channel)
┌────────────────────────────▼───────────────────────────────────────────────┐
│ core (no egui)                                                             │
│  command.rs  Command enum + dispatch; input/{keymap,midi} → Command        │
│  bus.rs      ControlSpec registry (id, kind, range, get, to_cmd, mixxx)    │
│  session.rs  DeckSession ×N (authoritative cues, loops, gains, sizes),     │
│              play history, suggestion reference                            │
│  library.rs  LibraryIndex (canonical Track, search_text, rev counter)      │
│  query.rs    Expr AST: parse / eval (in memory) / to_sql (smart lists)     │
│  collections.rs  folders, playlists, crates, smart, flow (graph overlay)   │
│  edit.rs     Edit + History (undo/redo for metadata, grid, collections)    │
│  store/      Store actor = the only writer; read-only pool for loaders     │
│  analysis/   Analyser registry, keyed job queue, worker supervisor         │
│  suggest/    Feature trait + Scorer                                        │
└──────┬──────────────────────────────────────────┬──────────────────────────┘
       │ Command ring(s) / EngineEvent ring /     │ protocol v2 (hello, analyse,
       │ triple-buffered EngineSnapshot           │ progress, result, cancel)
┌──────▼───────────────────────┐   ┌──────────────▼───────────────────────────┐
│ earshot-engine (real-time)   │   │ worker (Python, 1–2 processes,          │
│  Player (deck/preview/       │   │  models loaded once)                    │
│  sampler) + Transport        │   │  grid, key, CLAP, phrases, cratelib     │
│  Channel { chain: Processor*}│   └──────────────────────────────────────────┘
│  Mixer, buses, limiters      │
├──────────────────────────────┤
│ earshot-audio (offline)      │  decode, waveform, loudness, energy, Beatgrid
└──────────────────────────────┘
```

### Key decisions

**1. One `Command` vocabulary, with Mixxx strings only at the edges.**
- `enum Command { Deck(DeckId, DeckCmd), Mixer(..), Param(ParamId, f32), Browse(..),
  Library(..), Collection(..), Flow(..), Preview(..), Edit(..), Undo, Redo, View(..) }`.
- A `ControlSpec` registry gives widgets, MIDI and the keymap one source of range,
  default, format and getter. The key help is generated from the keymap.
- Toggles become engine commands (`Set::Toggle`) and are applied on the audio
  thread, which removes the read-modify-write races.
- Engine detail: [engine.md §3](engine.md). UI detail: [app-shell.md §3](app-shell.md).

**2. The engine works from a command ring in, an event ring out, and a snapshot.**
- `EngineEvent` (hot cue set, loop set, track ended, started) is tagged with a
  generation so stale events can be dropped.
- One `Copy` `EngineSnapshot` is published per callback through a triple buffer.
- `DeckSession` in core holds the authoritative state. It persists cues and is
  sent back to the engine as `Restore(EngineState)` after a device restart.
- `Load` carries one `Arc<LoadedTrack>` containing the audio, grid, cues,
  auto-gain, generation and track id. This makes the load atomic and removes the
  split between `DeckSlot` and the engine.

**3. The engine is built from parts that can be extended.**
- One `Player` (a `Transport` plus speed and keylock) is used by decks, preview
  and sampler slots alike.
- Every move of the playhead goes through `Transport::jump(to, reason)`. This is
  where slip, reverse, censor and roll loops attach.
- A `Processor` trait has indexed, smoothed parameters. A `Channel` holds a chain
  of processors (trim, EQ, filter, effects) and the master has its own chain.
- `DeckId` is a validated newtype. Sync uses an explicit leader. The deck count is
  a build parameter.
- Split the crate: `earshot-engine` for real-time code and `earshot-audio` for
  decode and analysis.

**4. The Store actor is the only writer.**
- The UI thread never calls SQLite.
- Commands go to the Store, and changes come back as `StoreEvent::TracksChanged`,
  which patches `LibraryIndex`.
- Every analysis job is written in one transaction. Grid saves are
  compare-and-set.

**5. One canonical `Track` in core.**
- It holds resolved metadata, the effective BPM and key, the grid source enum,
  features, rating, colour, tags and a precomputed `search_text`.
- `PendingLoad`, `Candidate`, `RefInfo` and `DeckInfo` borrow from it rather than
  rebuilding fields.
- Transient analysis state is held in a side map keyed by `TrackId`, as an enum.

**6. Collections replace Flows as the thing that's stored.**
- `collections(kind: folder|playlist|crate|smart|flow)` plus
  `collection_entries(position, x, y)` plus `flow_edges`.
- A Flow is a playlist with a graph layer on top, so every collection gets a list
  view, export and "Add to…".
- Filesystem roots move to a separate "Explorer" source, so the word "folder" no
  longer means two different things.

**7. One query language.**
- `query::parse` produces an `Expr` (And, Or, Not, Text, Cmp, Range, InCollection,
  Missing).
- `eval` filters in memory for the interactive list, which takes about 1–5 ms over
  50k tracks.
- `to_sql` is used for smart playlists and export.
- Sorting is part of the query state.

**8. One `TrackTable` component, one track menu, one selection type, one drag
payload.**
- `ColumnDef` handles configurable, sortable columns.
- `track_menu(sel) -> TrackAction` gives every list the same menu.
- `Selection` holds track ids, not row indices.
- `DragPayload::Tracks { ids, origin }` carries a whole multi-selection.
- The app keeps one `focused_pane`, and keyboard, MIDI browse and *Selected*
  suggestions all follow it. Today they ignore the Flow.

**9. Analysers are first-class.**
- `AnalyserSpec { id, version, runs_in: Rust|Worker, needs, depends_on }`.
- Results are stored per analyser with its version and the audio fingerprint, so
  "re-analyse stale tracks" is a single query.
- The job queue is keyed by `(track, analyser)`. Repeat requests merge: the higher
  priority wins and the force flags are combined. The file is decoded once per
  track.
- Worker protocol v2 adds a hello/version handshake, a typed `Reply` enum, a
  deadline and heartbeat with kill-and-restart, cancellation, and one result
  message per analyser.
- Run 1–2 worker processes with the models loaded once, instead of one torch
  process per lane.
- Detail: [analysis.md §3](analysis.md).

**10. The grid model keeps intent.**
- `GridModel::Constant { bpm, anchor, meter } | Markers(Vec<TempoMarker>)`, plus
  a `Timebase` (decoder and offset) for export.
- Beat lookups use a cached `Vec<f64>`, so the audio-thread API doesn't change.
- A user edit adds a new grid row and changes `tracks.active_grid_id`. Revert is
  then free, and analysis can't overwrite a user grid.

**11. The scorer is built from parts.**
- A `Feature` trait with weights. A missing feature returns `None` and its weight
  is shared among the others, instead of being fixed at 0.5.
- Score breakdowns are returned as data.
- "Exclude these tracks" is a separate input from "played", which fixes the Flow's
  misuse of the played penalty.

**12. One place for each kind of persisted state.**
- **Document state goes in the DB:** collections, the per-collection view
  (columns, sort, Flow camera), the session and mixer, cues, and the last-open
  item.
- **Device and window preferences go in eframe storage**, as one `UiPrefs` struct.
- Drop the legacy Flow migration in eframe storage.

### Schema

Start again from one baseline migration. The full DDL sketch is in
[data-model.md §3](data-model.md). In summary:

- **Track identity**
  - `tracks`: stable id, `added_at`, `active_grid_id`, and denormalised effective
    `bpm` and `key`, plus `rating`, `color` and `deleted_at`.
  - `files`: path as bytes, size, mtime, `content_hash`, `missing_since`. A track
    can have many files, which allows relocation and duplicate detection.
- **Metadata**
  - `track_meta(track_id, field, source, value)`: provenance for tag, user and
    each import, resolved as user > import > tag, with an FTS5 index.
  - Hot numeric fields are kept as typed columns on `tracks`.
- **Analysis**
  - `analysis_runs`, `track_features`, versioned `beatgrids` rows, `blobs` (kind,
    format version), and `embeddings`.
- **Performance**
  - `cues(kind, slot, start, end, label, color, source)`, which maps one-to-one
    onto the engine's `CueSet`.
- **Curation**
  - `tags`, `track_tags`, `collections`, `collection_entries` and `flow_edges`.
- **History and sync**
  - `sessions`, `plays`, `external_links` (needed for cratelib), and `jobs` (so
    the queue survives a restart).
- **Settings**
  - `library_roots` and `settings`.
- **Pragmas and errors**
  - `STRICT` tables, `busy_timeout`, `synchronous=NORMAL`.
  - Use `thiserror` errors instead of `Result<_, String>`.

---

## Where the planned features would land

| Feature | Where it goes in the target design |
|---|---|
| Per-deck and master effects | `Processor` implementations placed in `Channel.chain` with `Command::InstallFx`. Their `ParamSpec`s become `ControlSpec`s, so the panel is a loop over specs and MIDI mapping comes for free. |
| Slip, reverse, censor | `Transport.slip` and `dir`. `DeckCmd::Slip(Set)`. A ghost playhead from `EngineSnapshot`. Scrubbing becomes `DeckCmd::Scratch { vel }` instead of pausing and seeking. |
| Roll loops, loop types | `LoopState { kind }`, `LoopCmd::Roll`. |
| Saved hot cues and memory cues | `cues` table ↔ `CueSet` in `LoadedTrack`. `EngineEvent::HotcueSet` → `DeckSession` → Store. |
| Sampler, preview | More `Player` instances routed to channels. |
| LED feedback | `ControlSpec.get(&EngineSnapshot)` plus `EngineEvent` edges, sent out through the MIDI codec. |
| Metadata editing, bulk edit | `Command::Edit(SetField { ids, field, value })` → `History` → Store → `TracksChanged`. Values go in `track_meta` as `source='user'` and are never overwritten by a rescan. |
| Tags, ratings, colours | `tags`/`track_tags` and the columns on `tracks`. New `Field` variants make them available to queries, columns and the menu automatically. |
| Crates, playlists, smart playlists | `collections`, with smart playlists using `query` + `sort`, shown through `TrackTable`. |
| Relocating missing files, duplicates | `files.content_hash` and `missing_since`, with a relocate command. |
| cratelib sync | `external_links`, import provenance in `track_meta`, a grid `Timebase`, and `cues`. Runs as a worker job type. |
| Key detection, CLAP vibe | New `AnalyserSpec`s, an `embeddings` table, and a `Feature` added to the scorer. |
| Prep or library mode | `Mode` picks a set of panels. Layout is saved per mode in `UiPrefs`. |
| Undo | `core::edit::History` covers metadata, grid edits (keeping the old `Arc<Beatgrid>` / grid row), collection edits and Flow `GraphOp`s. |

---

## Rebuild order

Each step leaves the app working.

0. **Quick fixes** (about a day): confirmed bugs 1–4 and 6–8 above. Several of
   these go away in later steps anyway, but they cost little to fix now.
1. **Schema baseline and Store actor.** Squash the migrations into the new schema.
   Add the Store thread with a read pool and typed errors. Move all SQL out of
   `ui/`, `player.rs` and `analysis.rs`. Add file identity and the
   `track_meta` provenance. *Everything else depends on this step.*
2. **Core domain model.** Move into core: `Track` and `LibraryIndex`, the
   `GridSource` and `AnalysisState` enums, play history in `Session`, the energy
   ranking, `query` (the parser moved and extended), and `FlowGraph` and
   collections. The UI then reads from core only.
3. **Command bus.** Add `Command`, the `ControlSpec` registry, and a data-file
   keymap from which the help text is generated. MIDI produces typed commands. The
   four UI action enums and the stepper and toggle copies are removed.
4. **Engine boundary.** Add the `EngineEvent` ring, a triple-buffered
   `EngineSnapshot`, `LoadedTrack`, `DeckSession` with `Restore`, the `DeckId`
   newtype, and per-source command rings. Remove the player mutex from the MIDI
   path.
5. **Engine internals.** Add `Transport`, `Player` reuse for preview, the
   `Processor` chain with smoothed parameters, an N-deck mixer, and an explicit
   sync leader. Effects and slip are then built on this.
6. **Analysis.** Add the analyser registry, the keyed job queue, worker protocol
   v2 and the supervisor, the grid model v2 and timebase, shared kick snapping for
   both modes, and the scorer built from parts.
7. **UI rebuild.** Add the theme module, the `Shell` with modes, panels and
   modals, `TrackTable`, the shared selection, track menu and drag payload,
   on-demand repaint, and injected `Services` so the app-level kittest tests run in
   CI.

Steps 1–3 cover most of what both future feature tracks need. Steps 4–5 are what
the performance-tools track needs. Step 6 can run alongside 4–5.

## Decisions for the team

1. **Metadata storage.** Use key/value `track_meta` with a provenance column
   (flexible, and suits cratelib imports), or typed `tag_*` and `user_*` column
   pairs (simpler queries)? We recommend key/value, resolved once into the
   in-memory `Track`, with hot numeric fields as columns.
2. **Flow vs playlist.** Should a Flow be a playlist with a graph layer
   (recommended), or a separate kind of document that can be exported to a
   playlist?
3. **Track identity hash.** File bytes (size plus first and last 64 KB: cheap, but
   changes when tags are edited) or decoded audio frames (stable across tag edits,
   but costs a decode)? We recommend audio frames, computed as part of analysis.
4. **Worker processes.** One process with an internal priority queue, or two
   (urgent and background)? Memory use grows with CLAP and key models.
5. **Crate split.** Move decode and offline analysis out of `earshot-engine` now,
   or once the analysis rebuild starts?
6. **Two decks or N.** Make the engine generic over N decks now (cheap at this
   stage), even if the UI only lays out two?
