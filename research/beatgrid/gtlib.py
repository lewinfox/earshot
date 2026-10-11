"""Ground truth from full Beat This! (final0) and the scoring metric.

Beat This! reports beats on its 50 fps frame grid (20 ms steps). Its beat logits are
smooth peaks, so a parabola through the peak frame and its neighbours gives the beat
time to about a millisecond. Those refined beats are the ground truth.
"""
from __future__ import annotations

from dataclasses import dataclass

import numpy as np

from common import BT, DATA

FPS = 50.0


def refine(beats: np.ndarray, logits: np.ndarray) -> np.ndarray:
    """Sub-frame beat times: parabolic interpolation of the logit peak."""
    out = []
    for b in beats:
        i = int(round(b * FPS))
        if 0 < i < len(logits) - 1:
            a, c, e = logits[i - 1], logits[i], logits[i + 1]
            den = a - 2 * c + e
            d = 0.5 * (a - e) / den if den < 0 else 0.0
            out.append((i + np.clip(d, -0.5, 0.5)) / FPS)
        else:
            out.append(b)
    return np.array(out)


def index_beats(beats: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    """Integer beat number for each beat, allowing for missed beats (gaps of 2+).
    Beats less than 0.6 of the local period after the last one are dropped.
    Returns (kept beats, their numbers)."""
    if len(beats) < 2:
        return beats, np.arange(len(beats))
    recent = [float(np.median(np.diff(beats)))] * 8
    kept, idx = [beats[0]], [0]
    for b in beats[1:]:
        local = float(np.median(recent))
        g = b - kept[-1]
        if g < 0.6 * local:
            continue
        steps = max(1, int(round(g / local)))
        kept.append(b)
        idx.append(idx[-1] + steps)
        recent = (recent + [g / steps])[-8:]
    return np.array(kept), np.array(idx)


def kick_line(beats: np.ndarray, idx: np.ndarray, kick: np.ndarray) -> tuple[float, float, np.ndarray]:
    """Straight line through the kick-section beats, numbering each unbroken run of
    them on its own and joining runs by the tempo inside them. Beat This! can count
    beats wrong in a breakdown (no kick); this keeps that from bending the line.
    Returns (a, b, numbers for all beats): beat k at a + b*k."""
    runs = []
    j = np.flatnonzero(kick)
    if len(j) < 8:
        a, b, _ = robust_line(idx.astype(float), beats)
        return a, b, idx
    # unbroken: consecutive beats, both in kick sections, numbered one apart
    breaks = np.flatnonzero((np.diff(j) != 1) | (np.diff(idx[j]) != 1)) + 1
    for part in np.split(j, breaks):
        if len(part) >= 8:
            runs.append(part)
    if not runs:
        a, b, _ = robust_line(idx.astype(float), beats)
        return a, b, idx
    slopes = [robust_line(idx[r].astype(float) - idx[r][0], beats[r])[1] for r in runs]
    lens = np.array([len(r) for r in runs], float)
    period = float(np.average(slopes, weights=lens))
    new = idx.astype(float).copy()
    for _ in range(2):
        base_t, k = beats[runs[0][0]], 0.0
        num = np.full(len(beats), np.nan)
        for r in runs:
            off = np.round((beats[r[0]] - base_t) / period)
            num[r] = off + (idx[r] - idx[r][0])
        use = ~np.isnan(num)
        a, period, _ = robust_line(num[use], beats[use])
        new = num
    # beats outside the runs: nearest number on the line
    out = np.where(np.isnan(new), np.round((beats - a) / period), new)
    return a, period, out.astype(int)


def robust_line(k: np.ndarray, t: np.ndarray, iters: int = 10) -> tuple[float, float, np.ndarray]:
    """Huber-weighted straight line t = a + b*k. Returns (a, b, residuals)."""
    w = np.ones_like(t)
    a = b = 0.0
    for _ in range(iters):
        b, a = np.polyfit(k, t, 1, w=np.sqrt(w))
        r = t - (a + b * k)
        s = 1.4826 * np.median(np.abs(r)) + 1e-4
        c = 1.345 * s
        w = np.where(np.abs(r) <= c, 1.0, c / np.abs(r))
    return a, b, t - (a + b * k)


def local_bpm(beats: np.ndarray, idx: np.ndarray, span: int = 8) -> tuple[np.ndarray, np.ndarray]:
    """Local tempo over `span` beats (a line through them), at each beat's time."""
    times, bpm = [], []
    for j in range(0, len(beats) - span):
        k, t = idx[j : j + span + 1], beats[j : j + span + 1]
        if k[-1] - k[0] > 1.5 * span:  # a gap: skip
            continue
        times.append(t.mean())
        bpm.append(60 / np.polyfit(k, t, 1)[0])
    return np.array(times), np.array(bpm)


@dataclass
class Truth:
    tid: int
    beats: np.ndarray  # refined beat times (s)
    idx: np.ndarray  # beat numbers
    downbeats: np.ndarray
    duration: float
    fit_a: float  # straight fit: beat k at fit_a + fit_b * k
    fit_b: float
    resid: np.ndarray  # beats minus the straight fit (s)
    lt: np.ndarray  # local tempo times
    lbpm: np.ndarray  # local tempo
    seconds: float  # Beat This! run time
    # Phase reference: the kick/drum attack at each beat (attack.py), or the beat
    # moved by the track's typical Beat This!-to-attack lag where no clear attack.
    ref: np.ndarray | None = None
    ref_line: np.ndarray | None = None  # a straight line through ref, at each beat
    lag: float = 0.0  # Beat This! minus attack, median (s)
    kick: np.ndarray | None = None  # per beat: in a kick section (attack.kicks)

    @property
    def bpm(self) -> float:
        return 60 / self.fit_b

    def stats(self) -> dict:
        """Over kick-section beats only."""
        r = np.abs(self.resid[self.kick]) * 1000
        lb = self.lbpm
        lo, hi = (np.percentile(lb, [5, 95]) if len(lb) else (0, 0))
        gaps = np.diff(self.beats)
        nobeat = float(gaps[gaps > 4 * self.fit_b].sum()) if len(gaps) else 0.0
        # beats per second of the track that fall within 25 ms of the straight line
        return {
            "bpm": self.bpm,
            "n": len(self.beats),
            "resid_med_ms": float(np.median(r)),
            "resid_p90_ms": float(np.percentile(r, 90)),
            "resid_max_ms": float(r.max()),
            "on_line_25": float(np.mean(r <= 25)),
            "lbpm_p5": float(lo),
            "lbpm_p95": float(hi),
            "lbpm_range_pct": float((hi - lo) / self.bpm * 100),
            "nobeat_s": nobeat,
        }


def truth(tid: int, ckpt: str = "final0", with_ref: bool = True) -> Truth:
    """Beat This! beats, refined and numbered. With the outputs of attack.py (and
    with_ref), only beats in kick sections count: the straight line, its residuals'
    statistics, the hit-start reference and the scoring all use those beats."""
    z = np.load(BT / ckpt / f"{tid}.npz")
    beats = refine(z["beats"], z["beat_logits"])
    beats, idx = index_beats(beats)
    kp = DATA / "grids" / "kick" / f"{tid}.npy"
    kick = np.load(kp) if with_ref and kp.exists() else np.ones(len(beats), bool)
    if kick.mean() < 0.3:  # few four-to-the-floor kicks (breakbeats, DnB): judge on every beat
        kick = np.ones(len(beats), bool)
    a, b, idx = kick_line(beats, idx, kick)
    r = beats - (a + b * idx)
    lt, lb = local_bpm(beats, idx)
    secs = float(z["t_spect"] + z["t_model"] + z["t_post"])
    gt = Truth(tid, beats, idx, z["downbeats"], float(z["duration"]), a, b, r, lt, lb, secs)
    gt.kick = kick
    okp = DATA / "grids" / "attack_ok" / f"{tid}.npy"
    if with_ref and okp.exists():
        ok = np.load(okp)
        at_all = beats.copy()
        at_all[ok] = np.load(DATA / "grids" / "attack" / f"{tid}.npy")
        use = ok & kick
        gt.lag = float(np.median(beats[use] - at_all[use])) if use.any() else 0.0
        ref = beats - gt.lag
        ref[use] = at_all[use]
        gt.ref = ref
        if use.sum() > 8:
            ra, rb, _ = robust_line(idx[use].astype(float), at_all[use])
        else:
            ra, rb = a - gt.lag, b
        gt.ref_line = ra + rb * idx
    return gt


# ---- scoring a candidate grid -------------------------------------------------------


def grid_times(bpm: float, phase: float, duration: float) -> np.ndarray:
    p = 60 / bpm
    k0 = np.floor(-phase / p) - 1
    return phase + p * np.arange(k0, k0 + duration / p + 3)


def score(gt: Truth, grid: np.ndarray, bpm: float | None = None, against: str = "line") -> dict:
    """Per ground-truth beat: candidate grid line nearest it, minus the reference (ms).
    against: "line" (straight line through the attacks: regular tracks), "attack"
    (each beat's attack: irregular tracks) or "bt" (Beat This!'s own beat times)."""
    refs = {"line": gt.ref_line, "attack": gt.ref, "bt": gt.beats}[against][gt.kick]
    grid = np.sort(grid)
    j = np.clip(np.searchsorted(grid, refs), 1, len(grid) - 1)
    near = np.where(np.abs(grid[j] - refs) < np.abs(grid[j - 1] - refs), grid[j], grid[j - 1])
    e = (near - refs) * 1000
    a = np.abs(e)
    out = {
        "bias_ms": float(np.median(e)),
        "med_ms": float(np.median(a)),
        "p90_ms": float(np.percentile(a, 90)),
        "in10": float(np.mean(a <= 10)),
        "in20": float(np.mean(a <= 20)),
    }
    if bpm is not None:
        out["bpm_err"] = float(bpm - gt.bpm)
    return out
