"""Measure only: would the kick-start snap help dynamic (whole-track Beat This!) grids?

Dynamic grids as the worker makes them (analysis.analyse: Beat This! final0, beats
moved to the strongest librosa onset within 35 ms, cleaned), rebuilt from the saved
final0 runs. Then each beat moved by the median hit-start offset (snap.attacks) of the
16 beats around it. Error per beat against the hit-start reference (kick sections).
Caveat: the reference uses the same hit-start rule, so the snapped numbers partly
agree by construction.

usage: python -I dyn_snap.py      prints a summary, writes DATA/results/dyn_snap.json
"""
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent.parent / "worker"))
import numpy as np  # noqa: E402

import gtlib  # noqa: E402
from common import BT, DATA, load_wav  # noqa: E402
from earshot_worker.analysis import analyse  # noqa: E402
from earshot_worker.detectors import RawBeats  # noqa: E402
from earshot_worker.snap import attacks, energy  # noqa: E402


def errors(gt, beats):
    """Per reference beat (kick sections): nearest grid beat minus it (ms)."""
    refs = gt.ref[gt.kick]
    j = np.clip(np.searchsorted(beats, refs), 1, len(beats) - 1)
    near = np.where(np.abs(beats[j] - refs) < np.abs(beats[j - 1] - refs), beats[j], beats[j - 1])
    return (near - refs) * 1000


labels = json.loads((DATA / "results" / "scores.json").read_text())["labels"]
rows = []
for i, lab in labels.items():
    tid = int(i)
    z = np.load(BT / "final0" / f"{tid}.npz")
    y, sr = load_wav(tid)
    a = analyse(y, sr, raw=RawBeats(z["beats"], z["downbeats"], "beat_this"))
    beats = np.asarray(a.beats)
    at, ok = attacks(beats, energy(y, sr), 0.060, 0.040)
    off = np.where(ok, at - beats, np.nan)
    shift = np.array([np.nanmedian(off[max(0, k - 8) : k + 8]) if np.isfinite(off[max(0, k - 8) : k + 8]).any() else 0.0 for k in range(len(beats))])
    snapped = np.sort(beats + shift)
    gt = gtlib.truth(tid)
    e0, e1 = errors(gt, beats), errors(gt, snapped)
    rows.append({"id": tid, "label": lab, "bias0": float(np.median(e0)), "med0": float(np.median(np.abs(e0))),
                 "in10_0": float(np.mean(np.abs(e0) <= 10)), "bias1": float(np.median(e1)), "med1": float(np.median(np.abs(e1))),
                 "in10_1": float(np.mean(np.abs(e1) <= 10))})
    print(rows[-1], flush=True)
(DATA / "results" / "dyn_snap.json").write_text(json.dumps(rows, indent=1))
for lab in ("regular", "irregular"):
    r = [x for x in rows if x["label"] == lab]
    print(f"{lab} ({len(r)}): dynamic as is: median bias {np.median([x['bias0'] for x in r]):+.1f} ms, "
          f"tracks with median error <= 10 ms {sum(x['med0'] <= 10 for x in r)}, beats within 10 ms {np.mean([x['in10_0'] for x in r]):.0%} | "
          f"snapped per beat: bias {np.median([x['bias1'] for x in r]):+.1f} ms, tracks <= 10 ms {sum(x['med1'] <= 10 for x in r)}, "
          f"beats within 10 ms {np.mean([x['in10_1'] for x in r]):.0%}")
