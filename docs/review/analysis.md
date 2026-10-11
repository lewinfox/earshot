# Analysis pipeline

Part of the [October 2026 architecture review](README.md). Line references are to `8bb7c31`.

## 1. How it works today

**Queue** (`gui/src/core/analysis.rs`)

- There are two in-memory `VecDeque` lanes:
  - urgent: tracks loaded onto a deck
  - background: everything else
- One urgent thread and `EARSHOT_ANALYSIS_JOBS` background threads (default 4) work through them (`:150-188`).
- Each thread starts its own `python -m earshot_worker` process on first use and keeps it (`:164`, `:310-313`). That means up to 5 torch processes at once.
- While a deck is playing, only one background job runs at a time (`:234`).

**Talking to the worker**

- Messages are JSON lines over stdin/stdout, one request in flight per process (`:562-611`).
- Message types are `{"type":"ready"}`, `progress` and a reply.
- Audio is passed as a temporary mono f32 WAV that Rust writes (`:304-309`, `:432`).
- design.md:85-86 describes a Unix socket, but none exists.

**What runs in Rust and what runs in Python**

- Rust decodes the file once per job with symphonia (`:281`). It then computes the placeholder energy, LUFS loudness and the waveform summary (`:285-301`).
- Python computes only the tempo and the grid:
  - static: `analyse_static` (`static_tempo.py:256`)
  - dynamic: `analyse` (`analysis.py:253`)
- Rust turns the reply into a `Beatgrid` (`:333-348`) and writes `analysis`, `mode` and `beatgrids` (`:353-360`). It then sends the grid to any deck playing that track (`:409`).

**Database**

- There is one connection, `Arc<Mutex<Connection>>` (`db.rs:13`).
- The worker never touches it, so "Rust is the only writer" is true.
- The design's "single DB thread" (design.md:200) does not exist. Analysis threads, the loader and the UI thread all lock the same mutex. The UI thread also writes, through `db::previews` (`ui/app.rs:672`, `db.rs:341-375`).

**Suggestions** (`core/suggest.rs`)

- A fixed `Candidate {bpm, key, energy}` is scored with four hard-coded weights.
- Vibe is always 0.5 (`:15`, `:124`).
- Key comes only from file tags (`db.rs:127`, `tag_key`).

## 2. Findings, most important first

### F1. Static and dynamic grids land at different points in the beat

- Static grids are moved to the start of the kick by `snap_to_kicks` (`static_tempo.py:338`).
- Dynamic grids only go through `refine_to_onsets` (`analysis.py:192-208`), which snaps to the peak of librosa's onset strength.
- `research/beatgrid/README.md` shows Beat This! lands 20–40 ms late, and that snapping to the kick fixes it.

**Impact.** Switching a track between static and dynamic moves its grid by tens of milliseconds. Dynamic grids are probably late.

**Fix.** Put both modes through one shared step that places the grid on the kicks. For dynamic grids, snap each section separately.

### F2. Queue requests get dropped or stuck

- `queue()` returns early if the track is already waiting (`analysis.rs:214-220`). As a result:
  - an explicit *Analyse (dynamic)* (`force=true`) on a track that is waiting or running is silently ignored;
  - a forced request can't upgrade a queued non-forced one.
- Background threads take a job off the queue (`pop`) *before* they get a slot (`take_background_slot`, `:166-168`). While a deck plays, they wait holding that job.
- If that track is then loaded onto a deck, the switch to the urgent lane (`:215`) can't find it in `jobs.1`. The deck has to wait for a low-priority job that is limited to one at a time.
- There is no cancellation. Removing a folder or a track leaves its jobs in the queue.

**Fix.**
- Make the queue a map from `{track, analyser}` to `JobState {priority, force, mode}`.
- When a request arrives for a track that is already queued, merge it: take the higher priority, set force if either asked for it, and use the latest mode.
- Wait for a slot *before* taking a job off the queue.

### F3. The worker protocol has no timeouts, versioning, cancellation or strict types

- **No timeout.** The README lists this under "Known gaps". `read_line` blocks forever (`:581`), so one hung worker stalls its lane permanently.
- **No version check.** Readiness is detected with `line.contains("\"ready\"")` (`:549`). There is no protocol or analyser version handshake. `ping` returns only the detector name (`__main__.py:118`).
- **Loose reply type.** `Reply` is one struct for both modes, with every field marked `#[serde(default)]` (`:462-496`), and `steady` defaults to true. A missing field silently becomes 0 or true instead of an error. A static reply with no `bpm` fails only by accident, inside `Beatgrid::constant`.
- **Replies without an id are accepted.** A reply with `id: None` is taken as the answer to the current request (`:603`).
- **Temporary WAV.** It is left behind if the app crashes, and it would be written again for every analyser.
- **Unused fields.** `raw_bpm`, `drift_pct` and `beat` are sent but never read (`static_tempo.py:340-348`).

### F4. Analyser outputs have no version, so stale results can't be found

- Only waveforms have a version (`db.rs:16`).
- Grids record a source string and `analysis.detector` (`"static"` or `"beat_this"`, `db.rs:239`), but no algorithm version.
- Energy and loudness are plain columns on `tracks` (migrations 002 and 006).
- Results are keyed by `track_id`, and `track_id` is keyed by path. `upsert_tracks` updates size and mtime but keeps the grid, waveform and energy (`db.rs:127-131`). A re-encoded or replaced file keeps its old grid.
- Old grids are only upgraded when a track is loaded, inside `load_track` (`player.rs:214-225`, using `Beatgrid::straightened`).

### F5. Work is done more than once

**In Rust**

- `load_track` measures energy and loudness on every load (`player.rs:203-207`). It then queues analysis on the same audio, which measures both again (`analysis.rs:288-289`).
- Preview decodes the file (`player.rs:136`) and then queues analysis with `audio: None` (`ui/app.rs:556`), so the file is decoded a second time.
- `waveform::detail` is recomputed on every load and never cached (`player.rs:249`).

**In Python**

- Python never decodes a file, which is good. But each code path computes its own spectra:
  - `analysis.py:184`: librosa onset strength at the file's own sample rate
  - `static_tempo.py:67`: spectral flux at 11025 Hz
  - `snap.py:29`: energy plus a scipy low-pass
- `kick_sections` exists twice, with the same constants but different features (`static_tempo.py:150` vs `snap.py:79`).

**Across both**

- Tempo rounding is written in both languages:
  - Python `choose_tempo` (`static_tempo.py:214`), whose comment says "as the engine's Beatgrid::straightened"
  - Rust `Beatgrid::straightened` (`beatgrid.rs:293-332`), which is used only by the legacy migration

### F6. One process per lane, each with its own copy of the models

- Five torch processes, each loading both `final0` and `small0` on first use (`detectors.py:146-176`). Memory use will grow further with CLAP and key models.
- `get_beat_this` downloads the checkpoint the first time it is used (`detectors.py:166`). `setup.sh` doesn't fetch it ahead of time, so the first analysis needs the network.
- Lanes are tied one-to-one to processes. Embedding text for natural-language search (design.md:79) would have nowhere to run.

### F7. Grid source is a string, interpreted in several places

- The places that use it:
  - `DYNAMIC_SOURCE` (`analysis.rs:47`)
  - `"analysis"` and `"user"` literals (`analysis.rs:338`, `:352`, `player.rs:215`, `:338`, `:341`)
  - `ANALYSED_SOURCES`, defined twice (`ui/library_ui.rs:18`, `player.rs:254`)
- Whether a grid is analysed, dynamic or user-edited is worked out from these strings plus `analysis.mode` and `drifts` (`db.rs:257`).
- A user edit overwrites the analysed grid in place (`db.rs:228`), so there is no way to go back to the analysed version.

### F8. The Beatgrid format loses intent

- A static grid is stored as N explicit beats (`beatgrid.rs:65-75`). Whether a grid is constant is recovered by comparing floats to 1 µs (`player.rs:346`).
- Rekordbox and Serato export want tempo markers. A constant grid should export as one marker, which today means working that out again.
- `BEATS_PER_BAR = 4` is assumed by every edit: `:11`, `with_downbeat` (`:242`), `halved` and `doubled` (`:253-282`). Any odd-length bars the detector finds are erased.
- The blob has a `flags` field that is never used. There is no upgrade path: any other version is simply rejected (`:146`).
- The grid doesn't record which decoder, sample rate or offset its times are relative to. design.md:189-193 says this is "to be verified". Export to Rekordbox or Serato will need it.
- The lookups (`beat_at`, `time_at`, `quantize`) are correct and don't allocate, which is good for slip and quantize.

### F9. Analysis results are written outside a transaction

- `save_analysis`, `set_mode` and `save_grid` are separate statements (`analysis.rs:354-359`).
- Waveform, energy and loudness are committed earlier, also separately (`:287-300`).
- A failure partway through leaves the rows out of step, for example an `analysis` row with no grid.

### F10. The suggestion scorer can't grow without a rewrite

- The weights are `const` (`suggest.rs:11-14`).
- `Scored` has no vibe field, and the penalties are one-off multipliers (`:125-130`).
- `Candidate` is a three-field `Copy` struct (`:41`). There is no room for embeddings, key confidence, or a tempo range for dynamic tracks. (`bpm_min` and `bpm_max` are stored but unused.)
- Ranking energy across the library is done in the UI (`ui/library_ui.rs:146-160`), not in core.
- Candidates are built in UI code (`library_ui.rs:167`).

### F11. Smaller issues

- A doc comment contradicts the code. `Mode::Static` says "three excerpts" (`analysis.rs:40`); the code uses five (`static_tempo.py:48`).
- For static runs, the log line prints "dynamic median" as 0 (`analysis.rs:365`).
- The placeholder energy (`energy.rs`) largely duplicates LUFS: both measure the loudness of the loud blocks.

## 3. Proposed target design

### Analysers as first-class entities

```rust
enum AnalyserId { Waveform, Loudness, Grid, Key, Embedding{model}, Energy, Phrases }
struct AnalyserSpec { id: AnalyserId, version: u32, runs_in: Where /*Rust|Worker*/,
                      needs: AudioNeeds{rate: u32, channels: u8}, depends_on: &'static [AnalyserId] }
```

- Keep a registry, `&[AnalyserSpec]`.
- A track is stale for an analyser when any of these is true:
  - it has no result row
  - `row.version < spec.version`
  - `row.audio_fingerprint` differs from the track's
- "Re-analyse everything stale" is then a single query.

### Storage

```sql
analysis_results(track_id, analyser TEXT, version INT, audio_fp TEXT,
                 status TEXT, error TEXT, data BLOB/JSON, analysed_at,
                 PRIMARY KEY(track_id, analyser))
-- hot scalars (bpm, key, energy, lufs) projected into a track_features view/table for fast list/suggest
beatgrids(id, track_id, origin TEXT /*analysis_static|analysis_dynamic|user|import:*/,
          analyser_version, blob, created_at); tracks.active_grid_id
embeddings(track_id, model, version, dim, vector BLOB)
```

- A user edit adds a new grid row and keeps the analysed one, so reverting is free.
- All the writes for one job go in a single transaction, through one writer: a DB thread fed by a channel. That matches design.md:200.

### Jobs

`Job { track_id, analysers: BitSet, priority: Deck|Preview|Background, force }`

- The queue is keyed by track. A new request for a queued track is merged into the existing entry.
- For each track, the scheduler:
  1. decodes the file once
  2. runs the Rust analysers
  3. sends **one** request to the worker, covering every worker analyser that track needs
- Jobs can be cancelled by job id.

### Worker protocol v2

Length-prefixed frames, over stdin/stdout or a socket:

```json
→ {"type":"hello","protocol":2}
← {"type":"hello","protocol":2,"analysers":{"grid":3,"key":1,"clap":1},"models_loaded":[...]}
→ {"id":7,"type":"analyse","audio":{"shm":"/earshot-7","rate":44100,"channels":1,"frames":N},
   "analysers":{"grid":{"mode":"static"},"key":{}},"deadline_s":120}
← {"id":7,"type":"progress","analyser":"grid","fraction":0.4}
← {"id":7,"type":"result","analyser":"grid","version":3,"data":{...}}   // one per analyser
← {"id":7,"type":"done"}            → {"type":"cancel","id":7}
```

- Replies are a typed serde `enum Reply`, with no default fields.
- A supervisor kills and restarts any worker that misses its deadline or heartbeat.
- Run one worker process (two at most), with its own priority-aware queue and each model loaded once. Priority comes from the job, not from which process happens to run it.
- `setup.sh` downloads the model checkpoints ahead of time.

### Grid model

```rust
enum GridModel {
  Constant { bpm: f64, anchor_s: f64, anchor_bar_pos: u8, meter: u8 },
  Markers(Vec<TempoMarker { beat: i64, time_s: f64, meter: u8, bar_pos: u8 }>), // piecewise-linear beat->time
}
struct Grid { model: GridModel, timebase: Timebase{decoder: "symphonia-0.6", gapless: bool, offset_s: f64} }
```

- A dynamic grid is `Markers` with one marker per detected beat, which is equivalent to today's format. It can be reduced to fewer markers for Serato.
- Lookups use a cached `Vec<f64>`, so the API the audio thread uses doesn't change.
- Blob v2 puts a `kind` byte in `flags` and adds the timebase fields. v1 blobs can still be read.
- Edits change the model directly. For example, Set BPM on a `Constant` grid just changes the number. Edits keep the meter instead of assuming 4 beats to the bar.

### The worker returns a grid, not mode-specific fields

- Python returns either `{"grid":{"kind":"constant",...}}` or `{"kind":"markers",...}`, plus diagnostics: `steady`, `spread_ms`, `drift_pct`.
- Grid placement is shared between the two modes, which fixes F1.
- Delete `Beatgrid::straightened` and the legacy path in `player.rs:214`.

### Suggestions

```rust
trait Feature { fn name(&self)->&str; fn score(&self, r:&RefCtx, c:&TrackFeatures)->Option<f64>; }
struct Scorer { features: Vec<(Box<dyn Feature>, f64 /*weight*/)>, penalties: Vec<Box<dyn Penalty>> }
```

- `TrackFeatures` lives in core and is built from `track_features`. It holds:
  - BPM and BPM range
  - key and key confidence
  - energy rank
  - an index into one contiguous `Vec<f32>` matrix of embeddings
- When a track lacks a feature, `score` returns `None` and that feature's weight is shared among the rest, instead of scoring a fixed 0.3 or 0.5.
- Score breakdowns come back as `Vec<(name, score)>`.

## 4. Issues for the other areas

- **DB and schema.**
  - Track identity is the path only. There is no `file_hash`, so renaming or replacing a file breaks its analysis (F4).
  - There is no DB writer thread, and the UI thread writes through `db::previews` (`db.rs:341-375`).
  - Schema columns are spread across migrations 002, 004 and 006.
- **Core vs UI.**
  - The energy rank and suggestion candidates are built in `ui/library_ui.rs:146-177`.
  - Grid-source strings are defined twice: `ui/library_ui.rs:18` and `core/player.rs:254`.
- **Player and engine.**
  - `load_track` does decoding, loudness, energy, waveform, legacy grid migration, sidecar parsing and analysis queueing, all in one function (`player.rs:195-286`).
  - The whole decoded track is shared through `Arc<TrackAudio>`. That suits analysis, but it conflicts with the planned streaming decode (`decode.rs:3-4`).
  - `edit_grid` holds the player lock while it writes to the DB (`player.rs:301-342`).
- **Performance tools (slip, cues).** Hot cues should be stored relative to a grid and its timebase. Without a grid timebase (F8), cues and exports will drift if the decoder changes.
- **Worker for cratelib and CLAP.** Today's per-lane processes, and the lack of request types such as "embed text" or "export", don't fit those jobs. Protocol v2 above is meant to cover them.
