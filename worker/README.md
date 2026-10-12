# earshot worker

A long-lived Python process that analyses audio for earshot (beatgrids via Beat This!,
librosa fallback). Each process loads its models once. The app runs several (see
**Processes** below): one for tracks loaded onto a deck, always up, and background ones
that come and go with library work.

## Setup

```sh
./setup.sh          # needs uv; creates ./.venv with CPU-only torch (large first download)
.venv/bin/python -m pytest
```

The app runs it as `earshot/worker/.venv/bin/python -m earshot_worker`. `setup.sh` also
fetches the Beat This! checkpoints (~80 MB) into the torch cache, so the first analysis
doesn't need the network.

`analysis.py` and `detectors.py` are copied from `../../tempo-lock/tempolock/`
(projects in this repo never import each other). `analysis.py` has since gained the
kick snap.

## Protocol (v2)

JSON lines. stdin: one request per line. stdout: one message per line, nothing else
(all logs go to stderr). Exits on stdin EOF. The Rust side is
`gui/src/core/worker_client.rs`.

**Handshake.** The app sends `{"type": "hello", "protocol": 2}`. Once torch is
imported the worker answers
`{"type": "hello", "protocol": 2, "analysers": {"grid": 3}, "detector": "beat_this", "models_loaded": [...]}`.
`analysers` maps each analyser to its version, bumped whenever its results change. A
hello with another protocol gets an `error` (id null) instead.

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

Job ids and priorities are per process. With a process just for deck loads, a deck job
never has to stop background work, so the worker doesn't preempt (protocol v2 as first
merged did, with a `requeued` message; that's gone).
