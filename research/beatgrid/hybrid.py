"""The recommended method: today's static analysis, changed in three places, plus a
snap to the kick's attack and a steadiness flag. A research copy: the worker's
static_tempo.py is imported, not edited.

Changes to static_tempo.analyse_static:
1. Excerpts only from kick sections (kick_regions): the start excerpt begins at the
   first stretch with a steady kick, the end one finishes at the last, and the
   middle ones must lie inside kick sections. Percussion-less intros, bass-less
   breakdowns and fade-outs are skipped. Tracks with little four-to-the-floor kick
   (breakbeats, drum & bass) fall back to the old choice.
2. Never "no clear beats found" just because the first tempo guess was off (it can
   lock to 2/3 or 3/4 of a drum & bass tempo): if few excerpts line up with it, try
   the excerpts' own tempo (their beats' median spacing), and half and double that.
3. Afterwards, snap_kick: find the start of the hit (attack.attacks) at every grid
   line inside a kick section and move the grid by the median offset.

Then steadiness(): the same per-beat offsets, over kick-section beats, say whether one
straight grid fits the whole track.
"""
from __future__ import annotations

import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent.parent / "worker"))
import numpy as np  # noqa: E402

import attack  # noqa: E402
from earshot_worker import static_tempo as st  # noqa: E402
from earshot_worker.detectors import SMALL_CHECKPOINT, get_beat_this  # noqa: E402

KICK_WIN_S = 2.0  # kick level: kick-band onset strength summed over this long
KICK_LEVEL = 0.35  # a kick section is louder than this share of the 90th percentile
KICK_MIN_S = 8.0  # and lasts at least this long
KICK_MIN_SHARE = 0.3  # less of the track than this in kick sections: old excerpt choice


def kick_regions(t: np.ndarray, low: np.ndarray) -> np.ndarray:
    """Per envelope frame: inside a stretch of steady kicks."""
    fps = st.SR / st.HOP
    level = np.convolve(low, np.ones(int(KICK_WIN_S * fps)), mode="same")
    on = level > KICK_LEVEL * np.percentile(level, 90)
    # drop runs shorter than KICK_MIN_S (and fill gaps shorter than one window)
    out = on.copy()
    edges = np.flatnonzero(np.diff(np.concatenate([[0], on.astype(int), [0]])))
    for a, b in zip(edges[::2], edges[1::2]):
        if (b - a) < KICK_MIN_S * fps:
            out[a:b] = False
    return out


def excerpt_starts(t: np.ndarray, env: np.ndarray, low: np.ndarray, sr: int) -> list[int]:
    fps = st.SR / st.HOP
    w = int(st.EXCERPT_S * fps)
    kick = kick_regions(t, low)
    if kick.mean() < KICK_MIN_SHARE:
        return st.excerpt_starts(t, env, low, sr)
    idx = np.flatnonzero(kick)
    first, last = int(idx[0]), int(idx[-1]) + 1
    if last - first < w * st.EXCERPTS:
        return st.excerpt_starts(t, env, low, sr)
    frames = [first, last - w]
    # windows wholly (90%) inside kick sections, scored by kick strength
    inside = np.convolve(kick.astype(float), np.ones(w), mode="valid") >= 0.9 * w
    strength = np.convolve(low, np.ones(w), mode="valid") * inside
    lo0, hi0 = first + w, last - 2 * w
    for k in range(st.MIDDLE_EXCERPTS):
        lo = lo0 + int(k * (hi0 - lo0) / st.MIDDLE_EXCERPTS)
        hi = max(lo + 1, lo0 + int((k + 1) * (hi0 - lo0) / st.MIDDLE_EXCERPTS))
        frames.append(lo + int(np.argmax(strength[lo:hi])))
    return sorted(int(t[i] * sr) for i in frames)


def own_tempo(found) -> float | None:
    """The excerpts' own tempo: a straight line through each one's beats (numbered
    by their median spacing; the spacing alone is quantised to the model's 20 ms
    frames), then the median over excerpts."""
    bpms = []
    for _, b, _ in found:
        if len(b) < 8:
            continue
        k = np.round((b - b[0]) / np.median(np.diff(b)))
        bpms.append(60 / np.polyfit(k, b, 1)[0])
    if len(bpms) < 2:
        return None
    return float(np.median(bpms))


def analyse_static(y: np.ndarray, sr: int) -> dict:
    """static_tempo.analyse_static with changes 1 and 2 (see the module docstring)."""
    mono = (y.mean(axis=1) if y.ndim == 2 else y).astype(np.float32)
    t, env, low = st.onset_envelope(mono, sr)
    raw = st.fine_tempo(t, env, st.coarse_tempo(env, st.SR / st.HOP))
    model = get_beat_this(SMALL_CHECKPOINT)
    n = len(mono)
    span = int(st.EXCERPT_S * sr)
    starts = [0] if n <= span * st.EXCERPTS else excerpt_starts(t, env, low, sr)
    found = []
    for s in starts:
        seg = mono[s : s + span] if len(starts) > 1 else mono
        beats, downbeats = model(seg, sr)
        found.append((s, np.asarray(beats, float) + s / sr, np.asarray(downbeats, float) + s / sr))

    def gather(period):  # as in static_tempo.analyse_static
        anchors, weights, downs, local = [], [], [], []
        for s, beats, downbeats in found:
            z, doubled = st.line_up(beats, s, span, sr, period, t, low)
            if abs(z) < st.MIN_COHERENCE and len(found) > 1:
                continue
            ph = (np.angle(z) / (2 * np.pi)) * period % period
            middle = (beats[0] + beats[-1]) / 2 if len(beats) else s / sr
            anchors.append(ph + np.round((middle - ph) / period) * period)
            weights.append(max(abs(z), 1e-3))
            step = period / 2 if doubled else period
            k = np.round((beats - beats[0]) / step)
            if len(k) and k[-1] > 8:
                local.append(60 / (np.polyfit(k, beats, 1)[0] * (2 if doubled else 1)))
            if not doubled:
                downs.append(downbeats)
        return anchors, weights, downs, local

    start = raw
    anchors, weights, downs, local = gather(60 / start)
    if len(local) >= 3 and np.ptp(local) < 0.002 * raw and abs(np.median(local) / raw - 1) > 0.001:
        start = float(np.median(local))
        anchors, weights, downs, local = gather(60 / start)
    # Change 2: too few excerpts line up -> the first guess was probably a wrong
    # multiple. Try the excerpts' own tempo, then half and double it.
    if len(anchors) < max(2, len(found) // 2):
        own = own_tempo(found)
        if own:
            for guess in (own, own / 2, own * 2):
                if not 60 <= guess <= 200:
                    continue
                # The small model's own tempo is only rough on fast breakbeats
                # (176 for 174): refine it with the whole-track scan around it.
                cand = st.fine_tempo(t, env, guess)
                trial = gather(60 / cand)
                if len(trial[0]) > len(anchors):
                    start, (anchors, weights, downs, local) = cand, trial
    if not anchors:
        raise ValueError("no clear beats found: try dynamic analysis")
    anchors, weights = np.array(anchors), np.array(weights)
    bpm = st.choose_tempo(anchors, weights, start)
    period = 60 / bpm
    phase, miss = st.grid_fit(anchors, weights, period)
    votes = [v for d in downs for v in np.round((d - phase) / period).astype(int) % 4]
    if votes:
        k, counts = np.unique(votes, return_counts=True)
        down = int(k[np.argmax(counts)])
    else:
        down = 0
    # For the flag: how many excerpts line up with the final tempo, and how far each
    # excerpt's own tempo is from it (half and double count as the same).
    lined = len(gather(period)[0])
    devs = []
    for _, b, _ in found:
        if len(b) < 8:
            continue
        k = np.round((b - b[0]) / np.median(np.diff(b)))
        own = 60 / np.polyfit(k, b, 1)[0]
        devs.append(min(abs(own * f / bpm - 1) for f in (0.5, 1, 2)) * 100)
    return {"bpm": float(bpm), "raw_bpm": float(raw), "beat": float(phase), "down": down,
            "spread_ms": float(np.ptp(miss) * 1000) if len(anchors) > 1 else 0.0,
            "lined": lined, "excerpts": len(found), "tempo_dev_pct": float(max(devs, default=0.0))}


# ---- snap and flag ---------------------------------------------------------------------


def grid(bpm: float, phase: float, dur: float) -> np.ndarray:
    p = 60 / bpm
    return phase + p * np.arange(np.ceil(-phase / p), (dur - phase) / p)


def kick_time_regions(el: np.ndarray) -> np.ndarray:
    """Per 10 ms: inside a stretch of steady kicks, judged from the audio alone (not
    from any grid, so a grid that has drifted off the kicks can't hide them). Kick
    band amplitude rises, summed over KICK_WIN_S, above KICK_LEVEL of the 90th
    percentile, for at least KICK_MIN_S."""
    step = 10 * attack.MS
    a = np.sqrt(el[: len(el) // step * step]).reshape(-1, step).mean(1)
    rise = np.maximum(np.diff(a, prepend=a[0]), 0)
    level = np.convolve(rise, np.ones(int(KICK_WIN_S * 100)), mode="same")
    on = level > KICK_LEVEL * np.percentile(level, 90)
    out = on.copy()
    edges = np.flatnonzero(np.diff(np.concatenate([[0], on.astype(int), [0]])))
    for a0, b0 in zip(edges[::2], edges[1::2]):
        if (b0 - a0) < KICK_MIN_S * 100:
            out[a0:b0] = False
    return out


def in_regions(times: np.ndarray, regions: np.ndarray) -> np.ndarray:
    return regions[np.clip((times * 100).astype(int), 0, len(regions) - 1)]


def snap_kick(y, sr, bpm: float, phase: float, e=None, el=None):
    """Move the grid to where the kicks start. Returns (phase, offsets_ms, ok, used):
    each grid line's hit-start offset after the move, whether a hit start was found
    near it, and which lines count (inside kick sections)."""
    e = attack.energy(y, sr) if e is None else e
    el = attack.energy(y, sr, low=True) if el is None else el
    dur = len(y) / sr
    regions = kick_time_regions(el)
    if regions.mean() < KICK_MIN_SHARE:
        regions[:] = True  # little steady kick (breakbeats): every line counts
    for before, after in ((0.060, 0.040), (0.025, 0.025)):
        g = grid(bpm, phase, dur)
        at, ok = attack.attacks(g, e, before, after)
        use = ok & in_regions(g, regions)
        if use.sum() < 8:
            break
        phase = (phase + float(np.median(at[use] - g[use]))) % (60 / bpm)
    g = grid(bpm, phase, dur)
    # Offsets for the flag: a wide window, so a grid that's off by a lot shows it.
    at, ok = attack.attacks(g, e, 0.080, 0.080)
    return phase, (at - g) * 1000, ok, in_regions(g, regions)


def steadiness(off_ms: np.ndarray, ok: np.ndarray, used: np.ndarray) -> dict:
    """How well one straight grid fits, over the grid lines in kick sections: the
    share with a hit starting within 25 ms of the line (lines with no hit found near
    them count as misses), and how far a 16-line running median of the offsets
    (where found) wanders, 5th to 95th percentile."""
    n = int(used.sum())
    if n < 32:
        return {"on_grid": 0.0, "wander_ms": 999.0, "n": n}
    hit = ok & used & (np.abs(off_ms) <= 25)
    d = off_ms[ok & used]
    if len(d) < 16:
        return {"on_grid": float(hit.sum() / n), "wander_ms": 999.0, "n": n}
    m = np.convolve(d, np.ones(16) / 16, mode="valid")
    lo, hi = np.percentile(m, [5, 95])
    return {"on_grid": float(hit.sum() / n), "wander_ms": float(hi - lo), "n": n}


ON_GRID_MIN = 0.80
WANDER_MAX_MS = 25.0


def analyse(y: np.ndarray, sr: int) -> dict:
    s = analyse_static(y, sr)
    e, el = attack.energy(y, sr), attack.energy(y, sr, low=True)
    phase, off, ok, used = snap_kick(y, sr, s["bpm"], s["beat"], e, el)
    m = steadiness(off, ok, used)  # kept for the record; too noisy to flag with
    return {**s, "phase_before": s["beat"], "phase": phase, **m, "flagged": bool(flag(s))}


SPREAD_MAX_MS = 60.0  # excerpts' phases further apart than this: drift
TEMPO_DEV_MAX_PCT = 5.0  # an excerpt's own tempo this far off: tempo change (15 s excerpts: +-3% is noise)


def flag(s: dict) -> bool:
    """Not one steady tempo: the kick-section excerpts disagree on phase, one of them
    two or more don't line up with the track's tempo at all, or one has its own tempo."""
    return (
        s["spread_ms"] > SPREAD_MAX_MS
        or s["lined"] <= s["excerpts"] - 2
        or s["tempo_dev_pct"] > TEMPO_DEV_MAX_PCT
    )
