# earshot worker

One long-lived Python process that analyses audio for earshot (beatgrids via Beat This!,
librosa fallback). The app starts one and keeps it, so each model is loaded once.

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
`{"type": "hello", "protocol": 2, "analysers": {"grid": 2}, "detector": "beat_this", "models_loaded": [...]}`.
`analysers` maps each analyser to its version, bumped whenever its results change. A
hello with another protocol gets an `error` (id null) instead.

**Jobs.** Many may be sent at once. They wait in a queue ordered by `priority`
(`deck`, then `preview`, then `background`; first come first served within one) and
run one at a time.

```json
→ {"id": 7, "type": "analyse", "wav": "/abs/path.wav", "priority": "deck",
   "analysers": {"grid": {"mode": "static"}}, "deadline_s": 900}
← {"id": 7, "type": "started"}                         // left the queue; deadline starts
← {"id": 7, "type": "progress", "analyser": "grid", "fraction": 0.42}   // rising, 0..1
← {"id": 7, "type": "result", "analyser": "grid", "version": 2, "data": {...}}
←   or {"id": 7, "type": "failed", "analyser": "grid", "error": "..."}  // one per analyser
← {"id": 7, "type": "done", "seconds": 3.1}
→ {"type": "cancel", "id": 7}
```

- Every job ends with exactly one of `done`, `cancelled` or `error`
  (`{"id": 7, "type": "error", "error": "..."}`: bad request, unreadable audio, or past
  its deadline). One analyser failing is a `failed` message, and the job goes on.
- A `deck` job doesn't wait for a lower-priority job that is running: that job stops
  at its next progress report, sends `{"id": 7, "type": "requeued"}`, goes back in the
  queue (ahead of later jobs of its priority) and later sends `started` again and runs
  from the beginning. It is neither failed nor cancelled; its deadline restarts.
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
new one. The app gives each job `300 s + 4 × track length` as its deadline: the worker
runs in Linux's idle CPU class, and a busy machine can starve it, so this catches hangs,
not slow jobs.

**Grid results** (`data` of a `grid` result). Both modes are snapped to where the
kicks start (`snap.kick_shifts`): Beat This! puts its beats 20-40 ms late.

| mode | `data` |
|---|---|
| `static` | `{"detector": "static", "kick_shift_ms": f, "grid": {"kind": "constant", "bpm": f, "downbeat": s, "steady": bool, "spread_ms": f, "drift_pct": f}}` |
| `dynamic` | `{"detector": "beat_this", "kick_shift_ms": f, "grid": {"kind": "markers", "beats": [s...], "beat_index": [int...], "is_downbeat": [bool...], "median_bpm": f, "min_bpm": f, "max_bpm": f}}` |

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

**Priority.** The process puts itself in the idle CPU class (nice 19 off Linux) unless
`EARSHOT_WORKER_PRIORITY=inherit`, and an unprivileged process can't leave the idle
class. So deck jobs run in the idle CPU class too, like background ones (before v2
they ran at nice 5): they go first and preempt background work, but get no more CPU
than it would. If that proves too slow, a
second process just for deck loads, started with `inherit`, needs no protocol change:
job ids and priorities are per process.
