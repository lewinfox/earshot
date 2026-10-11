"""Compute candidate grids and save them, with timings, for scoring.

usage: python -I candidates.py <method> [ids...]

methods:
  fast        A: plain DSP (fastgrid.analyse), kick-band tempo + attack phase
  static      today's static method (worker/earshot_worker/static_tempo.py), unchanged
  static_snap B: static, then snap its phase to the attack over the whole track
  small_line  C: Beat This! small0 over the whole track, straight line through its
              beats, then attack snap (needs DATA/bt/small0 from bt_run.py)
  full_line   D: Beat This! final0 over the whole track, straight line, attack snap
              (reuses the ground-truth run; timing is that run's)

Each writes DATA/grids/<method>/<id>.npy (beat times) and appends a JSON line to
DATA/results/<method>.jsonl: bpm, phase, seconds, plus the method's own steadiness
measures.
"""
from __future__ import annotations

import json
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent.parent / "worker"))
import numpy as np  # noqa: E402

import fastgrid  # noqa: E402
import gtlib  # noqa: E402
from common import BT, DATA, load_wav, sample_ids  # noqa: E402


def snap(y, sr, bpm, phase):
    """The kick-section snap every method shares (hybrid.snap_kick)."""
    import hybrid

    return hybrid.snap_kick(y, sr, bpm, phase)[0]


def line_grid(z, el) -> dict:
    """Straight line through a Beat This! run's beats in kick sections; residual
    measures (same beats) for the flag."""
    import attack

    beats = gtlib.refine(z["beats"], z["beat_logits"])
    beats, idx = gtlib.index_beats(beats)
    _, ksec = attack.kicks(beats, el)
    if ksec.mean() < 0.3:
        ksec[:] = True
    a, b, idx = gtlib.kick_line(beats, idx, ksec)
    ra = np.abs(beats[ksec] - (a + b * idx[ksec])) * 1000
    lt, lb = gtlib.local_bpm(beats, idx)
    lo, hi = np.percentile(lb, [5, 95]) if len(lb) else (0, 0)
    bpm = 60 / b
    return {
        "bpm": bpm,
        "phase": a,
        "resid_med_ms": float(np.median(ra)),
        "resid_p90_ms": float(np.percentile(ra, 90)),
        "on_line_25": float(np.mean(ra <= 25)),
        "lbpm_range_pct": float((hi - lo) / bpm * 100),
    }


def run(method: str, tid: int) -> dict:
    y, sr = load_wav(tid)
    t0 = time.perf_counter()
    if method == "fast_rs":
        import subprocess

        exe = DATA.parent / "target" / "release" / "fastgrid"
        r = json.loads(subprocess.run([str(exe), str(DATA / "wav" / f"{tid}.wav")], capture_output=True, text=True, check=True).stdout)
        t0 = time.perf_counter() - r.pop("seconds")  # its own timing, not the WAV read
        r.pop("id")
    elif method == "fast":
        r = fastgrid.analyse(y, sr)
    elif method == "worker":
        # The hybrid as implemented in the worker (worker/earshot_worker/static_tempo.py)
        from earshot_worker.static_tempo import analyse_static

        s = analyse_static(y, sr)
        r = {"bpm": s["bpm"], "phase": s["beat"], "downbeat": s["downbeat"], "spread_ms": s["spread_ms"], "drift_pct": s["drift_pct"], "steady": s["steady"]}
    elif method == "hybrid":
        import hybrid

        r = hybrid.analyse(y, sr)
    elif method in ("static", "static_snap"):
        from earshot_worker.static_tempo import analyse_static

        s = analyse_static(y, sr)
        r = {"bpm": s["bpm"], "phase": s["beat"], "spread_ms": s["spread_ms"], "drift_pct": s["drift_pct"], "steady": s["steady"]}
        if method == "static_snap":
            t_static = time.perf_counter() - t0
            # Save today's static grid too (same run), so it needn't run twice.
            (DATA / "grids" / "static").mkdir(parents=True, exist_ok=True)
            np.save(DATA / "grids" / "static" / f"{tid}.npy", gtlib.grid_times(r["bpm"], r["phase"], len(y) / sr))
            with open(DATA / "results" / "static.jsonl", "a") as f:
                f.write(json.dumps({**{k: (bool(v) if isinstance(v, np.bool_) else v) for k, v in r.items()}, "seconds": t_static, "id": tid}) + "\n")
            r["phase_before"] = r["phase"]
            r["phase"] = snap(y, sr, r["bpm"], r["phase"])
    elif method in ("small_line", "full_line"):
        import attack

        z = np.load(BT / ("small0" if method == "small_line" else "final0") / f"{tid}.npz")
        r = line_grid(z, attack.energy(y, sr, low=True))
        r["phase_before"] = r["phase"]
        r["phase"] = snap(y, sr, r["bpm"], r["phase"])
        # the model's own run time, plus the line fit and snap
        r["model_seconds"] = float(z["t_spect"] + z["t_model"] + z["t_post"])
    else:
        raise SystemExit(f"unknown method {method}")
    r["seconds"] = time.perf_counter() - t0 + r.get("model_seconds", 0.0)
    r["id"] = tid
    dur = len(y) / sr
    out = DATA / "grids" / method
    out.mkdir(parents=True, exist_ok=True)
    np.save(out / f"{tid}.npy", gtlib.grid_times(r["bpm"], r["phase"], dur))
    return {k: (bool(v) if isinstance(v, np.bool_) else float(v) if isinstance(v, (np.floating, float)) else v) for k, v in r.items()}


def resnap(method: str, ids: list[int]) -> None:
    """Redo only the snap for a method's saved results (phase_before), e.g. after
    changing the snap rule, without rerunning the model. Keeps the saved run time
    and adds the snap's own."""
    res = {}
    p = DATA / "results" / f"{method}.jsonl"
    for line in p.read_text().splitlines():
        r = json.loads(line)
        res[r["id"]] = r
    with open(p, "a") as f:
        for tid in ids:
            r = res.get(tid)
            if not r or "phase_before" not in r:
                continue
            y, sr = load_wav(tid)
            t0 = time.perf_counter()
            old_snap = r.get("snap_seconds", 0.0)
            r["phase"] = snap(y, sr, r["bpm"], r["phase_before"])
            r["snap_seconds"] = time.perf_counter() - t0
            r["seconds"] = r["seconds"] - old_snap + r["snap_seconds"]
            np.save(DATA / "grids" / method / f"{tid}.npy", gtlib.grid_times(r["bpm"], r["phase"], len(y) / sr))
            f.write(json.dumps(r) + "\n")
            print("resnap", method, tid, flush=True)


if __name__ == "__main__":
    if sys.argv[1] == "resnap":
        resnap(sys.argv[2], [int(a) for a in sys.argv[3:]] or sample_ids())
        sys.exit()
    method = sys.argv[1]
    ids = [int(a) for a in sys.argv[2:]] or sample_ids()
    res = DATA / "results"
    res.mkdir(parents=True, exist_ok=True)
    with open(res / f"{method}.jsonl", "a") as f:
        for tid in ids:
            try:
                r = run(method, tid)
            except ValueError as e:  # e.g. static: "no clear beats found"
                print(method, tid, "failed:", e, flush=True)
                f.write(json.dumps({"id": tid, "failed": str(e)}) + "\n")
                continue
            f.write(json.dumps(r) + "\n")
            f.flush()
            print(method, tid, f"{r['bpm']:.3f}", f"{r['seconds']:.2f}s", flush=True)
