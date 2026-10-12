# earshot worker

A long-lived Python process that analyses audio for earshot (beatgrids via Beat This!,
librosa fallback) and runs the tempo lock tool (straighten a live-drummed track to one
BPM). Each process loads its models once. The app runs several (see **Processes**
below): one for tracks loaded onto a deck, always up, and background ones that come
and go with library work and the tempo lock tool.

## Setup

```sh
./setup.sh          # needs uv; creates ./.venv with CPU-only torch (large first download)
uv run pytest
```

The app runs it as `earshot/worker/.venv/bin/python -m earshot_worker`. `setup.sh` also
fetches the Beat This! checkpoints (~80 MB) into the torch cache, so the first analysis
doesn't need the network.

**System packages.** The tempo lock tool runs two programs: the Rubber Band CLI
(`rubberband`, R3 engine, does the stretch) and ffmpeg (`ffmpeg` writes the MP3 with
the source's tags, `ffprobe` reads them). `setup.sh` installs them when missing
(`apt-get install rubberband-cli ffmpeg`, or `brew install rubberband ffmpeg`).
Without Rubber Band a straighten job fails with a message saying so, and its tests
are skipped (`tests/test_tempo_render.py`; the protocol tests use a stand-in).

From `../../tempo-lock/tempolock/` (projects in this repo never import each other, so
these are copies): `analysis.py` (since given the kick snap), `detectors.py`,
`render.py` (with progress and cancel added) and `audio.py`; `tempo_lock.py` does
what its `server.py` did around them. The tests for them are ported from its `tests/`
(`test_tempo_*.py`, `synth.py`).

## Protocol (v2)

JSON lines. stdin: one request per line. stdout: one message per line, nothing else
(all logs go to stderr). Exits on stdin EOF. The Rust side is
`gui/src/core/worker_client.rs`.

**Handshake.** The app sends `{"type": "hello", "protocol": 2}`. Once torch is
imported the worker answers
`{"type": "hello", "protocol": 2, "analysers": {"grid": 3}, "steps": {"tempo_lock": 1, "straighten": 1, "measure": 1}, "detector": "beat_this", "models_loaded": [...]}`.
`analysers` maps each analyser to its version, bumped whenever its results change;
`steps` does the same for the tempo lock jobs' results. A hello with another protocol
gets an `error` (id null) instead.

**Jobs.** Many may be sent at once. They wait in a queue ordered by `priority`
(`deck`, then `preview`, then `background`; first come first served within one) and
run one at a time. A running job is never stopped for a more urgent one. (The app
sends each process one job at a time anyway, and deck loads go to their own process.)

```json
→ {"id": 7, "type": "analyse", "wav": "/abs/path.wav", "priority": "deck",
   "analysers": {"grid": {"mode": "static"}}, "deadline_s": 900}
← {"id": 7, "type": "started"}                         // left the queue; deadline starts
← {"id": 7, "type": "progress", "analyser": "grid", "fraction": 0.42}   // rising, 0..1
← {"id": 7, "type": "result", "analyser": "grid", "version": 3, "data": {...}}
←   or {"id": 7, "type": "failed", "analyser": "grid", "error": "..."}  // one per analyser
← {"id": 7, "type": "done", "seconds": 3.1}
→ {"type": "cancel", "id": 7}
```

- Every job ends with exactly one of `done`, `cancelled` or `error`
  (`{"id": 7, "type": "error", "error": "..."}`: bad request, unreadable audio, or past
  its deadline). One analyser failing is a `failed` message, and the job goes on.
- `cancel` drops a waiting job at once and stops a running one at its next progress
  report. Unknown ids are ignored.
- The worker gives up on a job past `deadline_s` (counted from `started`) at its next
  progress report.
- `{"type": "heartbeat"}` every 2 s, whatever else is going on.
- A line the worker can't read gets `{"type": "error", "id": null, "error": "..."}`.
- `wav`: mono float32 at the track's own sample rate; all times are seconds from its
  start.

**The app's side.** Every message is parsed into a typed enum; a missing field is an
error, never a default. The app kills and restarts the worker when it is silent (not
even a heartbeat) for 60 s while it has jobs, or when a job runs 15 s past its
deadline. The running job fails; jobs only waiting in the dead process are sent to the
new one. The app gives each job `300 s + 4 × track length` as its deadline: background
workers run in Linux's idle CPU class, and a busy machine can starve them, so this
catches hangs, not slow jobs. The app cancels a job when a newer request supersedes it:
a deck load of a track a background worker is analysing (the deck worker does it
instead), or a forced re-analysis of a track being analysed.

**Grid results** (`data` of a `grid` result). Both modes are snapped to where the
kicks start (`snap.kick_shifts`): Beat This! puts its beats 20-40 ms late.

| mode | `data` |
|---|---|
| `static` | `{"detector": "static", "kick_shift_ms": f, "first_beat": s, "grid": {"kind": "constant", "bpm": f, "downbeat": s, "steady": bool, "spread_ms": f, "drift_pct": f}}` |
| `dynamic` | `{"detector": "beat_this", "kick_shift_ms": f, "first_beat": s, "grid": {"kind": "markers", "beats": [s...], "beat_index": [int...], "is_downbeat": [bool...], "median_bpm": f, "min_bpm": f, "max_bpm": f}}` |

- `static` (see `static_tempo.py`): one tempo for the whole track, in a few seconds.
  `downbeat` is the time of a 1 on that grid. `steady` is false when the tempo seems to
  drift (`spread_ms`: how far the sampled excerpts disagree on phase; `drift_pct`: the
  furthest excerpt's own tempo from `bpm`).
- `dynamic` uses Beat This!'s full checkpoint (`final0`) over the whole track. `beats`
  is strictly increasing. `beat_index` counts beats including missed ones (so it can
  jump by 2+). `is_downbeat` is always false with the librosa detector.
  `min_bpm` / `max_bpm` are the 2nd / 98th percentile of per-beat tempo. Each stretch
  of about 32 beats is moved onto the kicks by its own amount.
- `kick_shift_ms`: how far the kick snap moved the grid (the median, negative is
  earlier).
- `first_beat`: where the music's beats start, after the kick snap. A straight grid
  runs back to the start of the file, over any silent or beatless intro; this is
  where beats were actually found. `static`: the grid line at the model's first beat
  on the grid, walked back over any beats it missed (`static_tempo.first_beat`).
  `dynamic`: the first of `beats`. The app puts a track's initial cue on the grid line
  nearest it. (Added in grid version 3.)

**Tempo lock jobs.** Two more job types, queued, cancelled and timed like `analyse`
(same `id`, `priority`, `deadline_s`; same `started`, `progress`, `result`, `failed`,
`done` / `cancelled` / `error` messages). In their messages `analyser` names the
step. `wav` may be any audio file libsndfile or ffmpeg reads; the app sends its own
decode as a float WAV with all channels, so the charts line up with what it plays
and the stretch keeps the stereo.

```json
→ {"id": 8, "type": "tempo_lock", "wav": "/abs/path.wav", "priority": "background", "deadline_s": 900}
← {"id": 8, "type": "progress", "analyser": "tempo_lock", "fraction": 0.42}
← {"id": 8, "type": "result", "analyser": "tempo_lock", "version": 1, "data": {"analysis": {...}}}

→ {"id": 9, "type": "straighten", "wav": "/abs/path.wav", "priority": "background", "deadline_s": 900,
   "target_bpm": 124, "beats": [s...], "beat_index": [int...], "out_dir": "/abs/dir",
   "tags_from": "/music/Song.mp3", "name": "Song.mp3", "level": 1, "engine": "r3", "measure": true}
← {"id": 9, "type": "progress", "analyser": "straighten", "fraction": 0.3}    // Rubber Band's own progress
← {"id": 9, "type": "result", "analyser": "straighten", "version": 1, "data": {"output": "/abs/dir/Artist - Song (straightened 124 bpm).mp3", ...}}
← {"id": 9, "type": "progress", "analyser": "measure", "fraction": 0.5}
← {"id": 9, "type": "result", "analyser": "measure", "version": 1, "data": {"analysis": {...}}}
← {"id": 9, "type": "done", "seconds": 31.0}
```

- `tempo_lock`: everything tempo-lock's page charts. `analysis` is tempo-lock's
  `Analysis.to_dict()`: `beats`, `beat_index`, `is_downbeat`, `dropped_beats` (s; the
  detections it threw away), `bpm_times` + `bpm_curve` (tempo over time: one point per
  beat interval, at its middle; the histogram is of `bpm_curve`), `median_bpm`,
  `min_bpm` / `max_bpm` (2nd / 98th percentile), `suggested_bpm` (rounded median),
  `sections` (`start_beat`, `end_beat`, `start`, `end`, `bpm`: stretches at one tempo),
  `tempo_changes` (`beat`, `time`, `from_bpm`, `to_bpm`, `ratio`, `kind`: "tempo
  change", "half-time" or "double-time"), `duration`, `sr`, `detector`, `kick_shift`.
  Beats are snapped to the kicks as dynamic grids are. (No waveform: earshot draws
  its own from its decode.) Fewer than two beats is an `error`.
- `straighten`: re-times the audio so beat k lands on `beats[0] + beat_index[k] ·
  60/target_bpm · level` (tempo-lock's `render.build_grid`) with Rubber Band, then
  writes an MP3 into `out_dir` named and tagged as tempo-lock's download is:
  `"Artist - Title (straightened 124 bpm).mp3"` from `tags_from`'s tags (`name`'s
  stem, else `tags_from`'s, without them), its tags copied, `title` marked the same
  way, `TBPM` set, at its bitrate (320 kbps from a lossless source). An existing file
  of that name is replaced (`out_dir` is the app's scratch folder); a stopped job
  leaves nothing. Its earshot audio hash tag (`TXXX:AUDIO_SHA256`) is not copied: it
  would be wrong. `level`: 1, 2 (the beats are half-time) or 0.5 (double-time).
  `engine`: `r3` (best) or `r2` (faster). A target more than 30% from the detected
  tempo (× `level`) is refused, as are beats that don't strictly increase.
  `straighten`'s `data`: `output` (path), `bpm`, `level`, `engine`, `grid`
  (`target_bpm`, `source_beats`, `target_beats`: where each beat was, and is now),
  `duration`, `bitrate_kbps`. Then, unless `measure` is false, `measure`
  finds the beats in the output again (`{"analysis": {...}}` as above), to show how
  straight it came out; it failing is a `failed`, as the MP3 is already good. The
  straighten step failing (no Rubber Band, ffmpeg error) ends the job with `error`.
- `cancel` stops Rubber Band at once (it's killed) and the measure step at its next
  progress report.

**Processes and priority.** The process puts itself in the idle CPU class (nice 19 off
Linux) unless `EARSHOT_WORKER_PRIORITY=inherit`; an unprivileged process can't leave
the idle class again, so each process has one priority for life. The app
(`gui/src/core/analysis/mod.rs`) runs:

- **One deck worker**, started with the app, at normal CPU priority (`inherit`), with
  half the cores for torch. It only ever gets tracks just loaded onto a deck, so they
  are analysed straight away and never wait behind library work. If it dies it is
  started again at once.
- **Background workers** for library work (imports, "Analyse …"), in the idle CPU
  class. None run until there is background work; another starts for every 4 tracks
  waiting, up to the cap; each exits after 60 s with nothing to do (analysis is mostly
  needed on first import, and each process holds its own copy of torch and the models).
  While a deck plays, only one background track is analysed at a time. The cap is the
  `analysis.background_workers` setting (default 4, kept to 1..=CPU cores, read at
  startup); they share the cores between them for torch.

- **One tempo lock worker** (`gui/src/core/tempo_lock.rs`), for the tempo lock tool's
  jobs, which take tens of seconds: a background worker like the others (idle CPU
  class, started on its first job, exits after 60 s idle), never the deck worker.

Job ids and priorities are per process. With a process just for deck loads, a deck job
never has to stop background work, so the worker doesn't preempt (protocol v2 as first
merged did, with a `requeued` message; that's gone).
