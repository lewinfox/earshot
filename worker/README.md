# earshot worker

Stateless Python process that analyses audio for earshot (beatgrids via Beat This!, librosa fallback).

## Setup

```sh
./setup.sh          # needs uv; creates ./.venv with CPU-only torch (large first download)
.venv/bin/python -m pytest
```

The app runs it as `earshot/worker/.venv/bin/python -m earshot_worker`. The first
analysis downloads the Beat This! checkpoint (~80 MB) into the torch cache.

`analysis.py` and `detectors.py` are copied from `../../tempo-lock/tempolock/`
(projects in this repo never import each other).

## Protocol

JSON lines. stdin: one request per line. stdout: one reply per line, nothing else
(all logs go to stderr). One request at a time. Exits on stdin EOF.

On startup: `{"type": "ready"}`

| request | reply |
|---|---|
| `{"id": 1, "type": "ping"}` | `{"id": 1, "ok": true, "detector": "beat_this"}` (or `"librosa"`) |
| `{"id": 2, "type": "analyse", "wav": "/abs/path.wav"}` | `{"id": 2, "ok": true, "detector": ..., "beats": [s...], "beat_index": [int...], "is_downbeat": [bool...], "median_bpm": f, "min_bpm": f, "max_bpm": f, "seconds": f}` |
| `{"id": 3, "type": "analyse_static", "wav": "/abs/path.wav"}` | `{"id": 3, "ok": true, "detector": "static", "bpm": f, "raw_bpm": f, "beat": s, "downbeat": s, "spread_ms": f, "drift_pct": f, "steady": bool, "seconds": f}` |
| (while analysing, any number of) | `{"id": 2, "type": "progress", "fraction": 0.42}`, rising from 0 to 1, before the reply |
| anything that fails | `{"id": <id or null>, "ok": false, "error": "message"}` |

- `wav`: mono float32 at the track's own sample rate; beat times are seconds from its start.
- `beats` is strictly increasing. `beat_index` counts beats including missed ones
  (so it can jump by 2+). `is_downbeat` is always false with the librosa detector.
- `min_bpm` / `max_bpm` are the 2nd / 98th percentile of per-beat tempo.
- `analyse_static` (see `static_tempo.py`): one tempo for the whole track, in a few
  seconds. `beat` / `downbeat` are the times of a beat / a 1 on that grid; `steady`
  is false when the tempo seems to drift (`spread_ms`: how far the sampled excerpts
  disagree on phase; `drift_pct`: the furthest excerpt's own tempo from `bpm`).
- `analyse` uses Beat This!'s small checkpoint (`small0`).
- `seconds` is wall-clock time for that analysis (the first one also loads the model).
