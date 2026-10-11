"""Score every candidate's grids against the reference.

usage: python -I score.py [methods...]

Labels each sample track regular / irregular from the ground truth (rule below), then
for each method: phase error per beat against the straight line through the kick
attacks (regular tracks), tempo error, run time, and whether its own steadiness
measure flags the irregular tracks. Writes DATA/results/scores.json.
"""
from __future__ import annotations

import json
import sys

sys.path.insert(0, __file__.rsplit("/", 1)[0])
import numpy as np  # noqa: E402

import gtlib  # noqa: E402
from common import BT, DATA, sample_ids  # noqa: E402

# Ground-truth label. A track is regular when one straight grid stays on Beat This!'s
# beats: its 16-beat running average never wanders more than DRIFT_MS (5th-95th
# percentile range, so a stray patch at the very end doesn't count) and 95% of beats
# are within 25 ms of the line.
DRIFT_MS = 30.0
# The user's verdicts from the review page (both: breakdowns / percussion-less intro,
# kick on the grid throughout).
USER_LABELS = {1035: "regular", 1018: "regular"}
# Mine, by the user's rule: straight except a percussion-less intro (1659) or a
# breakdown where Beat This! wanders (1687). See README.
USER_LABELS.update({1659: "regular", 1687: "regular"})


def drift_ms(gt) -> float:
    r = gt.resid[gt.kick] * 1000
    if len(r) <= 16:
        return 0.0
    m = np.convolve(r, np.ones(16) / 16, mode="valid")
    lo, hi = np.percentile(m, [5, 95])
    return float(hi - lo)


RULE_LABELS: dict[int, str] = {}


def label(gt) -> str:
    """The rule, over kick-section beats. The user's own verdicts (USER_LABELS) win."""
    s = gt.stats()
    RULE_LABELS[gt.tid] = "regular" if drift_ms(gt) <= DRIFT_MS and s["on_line_25"] >= 0.95 else "irregular"
    if gt.tid in USER_LABELS:
        return USER_LABELS[gt.tid]
    return "regular" if drift_ms(gt) <= DRIFT_MS and s["on_line_25"] >= 0.95 else "irregular"


# Each method's own "this isn't straight" flag, from what it measures itself.
def flagged(method: str, r: dict) -> bool:
    if "failed" in r:
        return True  # gave up: the app would offer dynamic analysis
    if method in ("fast", "fast_rs"):
        return r["win_resid_p90_ms"] > FAST_WIN_MS or r["good_frac"] < FAST_GOOD
    if method in ("static", "static_snap"):
        return not r["steady"]
    if method in ("small_line", "full_line"):
        return r["resid_p90_ms"] > LINE_P90
    if method == "hybrid":
        import hybrid

        return hybrid.flag(r)
    return False


FAST_WIN_MS, FAST_GOOD = 10.0, 0.6
LINE_P90 = 17.0  # ms: 90th percentile distance of kick-section beats from the line


def load(method: str) -> dict[int, dict]:
    p = DATA / "results" / f"{method}.jsonl"
    out = {}
    if p.exists():
        for line in p.read_text().splitlines():
            r = json.loads(line)
            out[r["id"]] = r  # last run wins
    return out


def main(methods):
    ids = [i for i in sample_ids() if (BT / "final0" / f"{i}.npz").exists()]
    gts = {i: gtlib.truth(i) for i in ids}
    labels = {i: label(gts[i]) for i in ids}
    reg = [i for i in ids if labels[i] == "regular"]
    irr = [i for i in ids if labels[i] == "irregular"]
    print(f"{len(reg)} regular, {len(irr)} irregular: {irr}")
    summary = {}
    for m in methods:
        res = load(m)
        rows = []
        failed = [i for i in reg if i in res and "failed" in res[i]]
        for i in reg:
            if i not in res or "failed" in res[i]:
                continue
            g = np.load(DATA / "grids" / m / f"{i}.npy")
            # Half or double tempo is fine (the UI has x2 / /2): score the grid at
            # the reference's tempo, filling in or thinning out lines.
            ratio = res[i]["bpm"] / gts[i].bpm
            bpm = res[i]["bpm"]
            if abs(ratio - 0.5) < 0.01:
                g = np.sort(np.concatenate([g, g[:-1] + np.diff(g) / 2]))
                bpm *= 2
            elif abs(ratio - 2) < 0.02:
                bpm /= 2  # every other line: nearest-line scoring already handles it
            s = gtlib.score(gts[i], g, bpm, "line")
            s["octave"] = round(ratio, 2) if abs(ratio - 1) > 0.02 else 1
            s["id"] = i
            s["bt_bias_ms"] = gtlib.score(gts[i], g, None, "bt")["bias_ms"]
            rows.append(s)
        if not rows:
            continue
        med = np.array([r["med_ms"] for r in rows])
        bpm_ok = np.array([abs(r["bpm_err"]) < 0.05 for r in rows])
        # a failed run counts as a miss: no tempo, no phase
        med = np.concatenate([med, np.full(len(failed), np.inf)])
        summary[m] = {
            "n_regular": len(rows) + len(failed),
            "failed": failed,
            "tempo_right": int(bpm_ok.sum()),
            "median_of_track_medians_ms": float(np.median(med)),
            "tracks_med_le_5ms": int((med <= 5).sum()),
            "tracks_med_le_10ms": int((med <= 10).sum()),
            "mean_in10": float(np.mean([r["in10"] for r in rows])),
            "mean_in20": float(np.mean([r["in20"] for r in rows])),
            "median_p90_ms": float(np.median([r["p90_ms"] for r in rows])),
            "seconds_median": float(np.median([res[i]["seconds"] for i in res if "seconds" in res[i]])),
            "flag_hits": [i for i in irr if i in res and flagged(m, res[i])],
            "flag_misses": [i for i in irr if i in res and not flagged(m, res[i])],
            "false_flags": [i for i in reg if i in res and flagged(m, res[i])],
            "per_track": rows,
        }
        s = summary[m]
        print(
            f"{m:12s} tempo right {s['tempo_right']}/{s['n_regular']}  phase: median {s['median_of_track_medians_ms']:.1f} ms, "
            f"{s['tracks_med_le_5ms']}/{s['tracks_med_le_10ms']}/{s['n_regular']} tracks within 5/10 ms, in10 {s['mean_in10']:.0%} in20 {s['mean_in20']:.0%}  "
            f"time {s['seconds_median']:.1f}s  flags {len(s['flag_hits'])}/{len(irr)} irregular, {len(s['false_flags'])} false"
        )
        for i in failed:
            print(f"    {i:5d} failed")
        bad = [r for r in rows if r["med_ms"] > 10 or abs(r["bpm_err"]) >= 0.05]
        for r in bad:
            print(f"    {r['id']:5d} bpm err {r['bpm_err']:+.3f} bias {r['bias_ms']:+.1f} med {r['med_ms']:.1f} p90 {r['p90_ms']:.1f}")
    (DATA / "results" / "scores.json").write_text(json.dumps({"labels": labels, "summary": summary}, indent=1, default=float))


if __name__ == "__main__":
    main(sys.argv[1:] or ["fast_rs", "static", "static_snap", "hybrid", "small_line", "full_line"])
