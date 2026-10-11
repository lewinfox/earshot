"""Try window-fold steadiness measures for the hybrid's flag, over the whole sample.

For each track, at the hybrid's tempo: the kick-band onset envelope (fastgrid, 1 kHz)
folded into one beat over the whole track (the template) and over 8 s windows (hop
4 s) that lie inside kick sections. Each window's offset from the template (circular
cross-correlation peak) says where its kicks sit relative to one straight grid.
Prints the measures per label, and writes DATA/results/flagtest.json.
"""
import json
import sys

sys.path.insert(0, __file__.rsplit("/", 1)[0])
import numpy as np  # noqa: E402

import attack  # noqa: E402
import fastgrid as fg  # noqa: E402
import hybrid  # noqa: E402
import score  # noqa: E402
from common import DATA, load_wav  # noqa: E402

WIN, HOP = 8000, 4000  # frames (ms)


def window_offsets(y, sr, bpm):
    low, _ = fg.envelopes(y, sr)
    period = 60 / bpm * fg.FPS
    regions = hybrid.kick_time_regions(attack.energy(y, sr, low=True))
    if regions.mean() < hybrid.KICK_MIN_SHARE:
        regions[:] = True
    template = fg.fold(low, 0, len(low), period)
    tz = template - template.mean()
    nb = len(template)
    offs, corrs = [], []
    for t0 in range(0, len(low) - WIN, HOP):
        r0, r1 = t0 // 10, (t0 + WIN) // 10
        if regions[r0:r1].mean() < 0.9:
            continue
        p = fg.fold(low, t0, t0 + WIN, period)
        pz = p - p.mean()
        r = fg.circ_xcorr(pz, tz)
        k = fg.peak(r)
        if k > nb / 2:
            k -= nb
        den = np.linalg.norm(pz) * np.linalg.norm(tz)
        offs.append(k * period / nb)
        corrs.append(r.max() / den if den > 0 else 0)
    return np.array(offs), np.array(corrs)


def measures(offs, corrs):
    if len(offs) < 4:
        return {"n": len(offs), "off_p90": 0.0, "range": 0.0, "far": 0.0, "corr_med": 0.0}
    a = np.abs(offs)
    lo, hi = np.percentile(offs, [5, 95])
    return {"n": len(offs), "off_p90": float(np.percentile(a, 90)), "range": float(hi - lo),
            "far": float(np.mean(a > 20)), "corr_med": float(np.median(corrs))}


if __name__ == "__main__":
    lab = json.loads((DATA / "results" / "scores.json").read_text())["labels"]
    res = score.load("hybrid")
    out = {}
    for i, l in lab.items():
        y, sr = load_wav(int(i))
        m = measures(*window_offsets(y, sr, res[int(i)]["bpm"]))
        out[i] = {**m, "label": l}
    (DATA / "results" / "flagtest.json").write_text(json.dumps(out, indent=1))
    for cls in ("irregular", "regular"):
        rows = sorted((round(v["off_p90"], 1), round(v["far"], 2), round(v["corr_med"], 2), int(i)) for i, v in out.items() if v["label"] == cls)
        print(cls, rows)
