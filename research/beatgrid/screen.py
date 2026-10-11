"""Ground-truth summary per track: how straight Beat This!'s beats are.

usage: python -I screen.py   prints a table and writes DATA/results/screen.json
"""
import json
import sys

sys.path.insert(0, __file__.rsplit("/", 1)[0])
import numpy as np  # noqa: E402

import gtlib  # noqa: E402
from common import BT, DATA, sample_ids, title  # noqa: E402

rows = []
for tid in sample_ids():
    if not (BT / "final0" / f"{tid}.npz").exists():
        continue
    gt = gtlib.truth(tid)
    s = gt.stats()
    s["id"] = tid
    s["title"] = title(tid)[:50]
    s["lag_ms"] = gt.lag * 1000
    s["bt_seconds"] = gt.seconds
    # the biggest stretch the beats wander from the straight line: a 16-beat moving
    # median of the residual (so single bad beats don't count)
    r = gt.resid * 1000
    s["drift_ms"] = float(np.ptp(np.convolve(r, np.ones(16) / 16, mode="valid"))) if len(r) > 16 else 0.0
    rows.append(s)
rows.sort(key=lambda s: s["drift_ms"])
print(f"{'id':>5} {'bpm':>7} {'resid50':>7} {'resid90':>7} {'online':>6} {'lbpm%':>6} {'drift':>7} {'nobeat':>6} {'lag':>5}  title")
for s in rows:
    print(f"{s['id']:5d} {s['bpm']:7.2f} {s['resid_med_ms']:7.1f} {s['resid_p90_ms']:7.1f} {s['on_line_25']:6.2f} {s['lbpm_range_pct']:6.2f} {s['drift_ms']:7.1f} {s['nobeat_s']:6.1f} {s['lag_ms']:5.1f}  {s['title']}")
(DATA / "results").mkdir(exist_ok=True)
(DATA / "results" / "screen.json").write_text(json.dumps(rows, indent=1))
