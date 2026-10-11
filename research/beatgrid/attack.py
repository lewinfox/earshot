"""Where each hit starts, and which beats have a kick.

The user's definition (confirmed on earshot's waveform view): a grid line belongs
where the kick's attack begins, before the low wave; on a hit that opens with a click,
on the click; never on a faint sound before it.

attacks(): for each beat (a Beat This! beat, or a grid line),
  1. the hit's body is the loudest point (full-band amplitude, 1 ms) from `before`
     before to 60 ms after the beat; the gap before it is the quietest point in the
     150 ms before that;
  2. the main rise is where the amplitude last climbs past 40% of the way from the gap
     to the body and stays up (a blip that drops back for 3 ms doesn't count);
  3. the attack is the foot of that rise: walk back from it while the amplitude is
     still rising steeply (more than 10% of the rise's steepest 1 ms slope). A faint,
     flat pre-sound before the click stops the walk, so it isn't counted.

kicks(): a beat has a kick when the kick band (below 150 Hz) jumps at it: its peak
from 30 ms before to 60 ms after the beat is at least twice its mean level 130-50 ms
before, and at least 20% of the track's typical kick peak (90th percentile). Kick
sections: beats where at least 60% of the 8 beats around them have kicks. Intros
without percussion, bass-less breakdowns and fade-outs fall outside them.

usage: python -I attack.py [ids...]   writes, per Beat This! beat, DATA/grids/attack/<id>.npy
       (attack times where found), attack_ok/<id>.npy and kick/<id>.npy (bool masks)
"""
from __future__ import annotations

import sys

sys.path.insert(0, __file__.rsplit("/", 1)[0])
import numpy as np  # noqa: E402
from scipy import signal  # noqa: E402

RATE = 2000  # frames per second
MS = RATE // 1000
RISE = 0.30  # the hit proper starts by reaching this far from the gap to the body
FOOT_SLOPE = 0.10  # walk back while the slope is above this share of the steepest
GAP_MS = 150  # look this far before the body for the gap (kicks can swell for 100 ms)


def energy(y: np.ndarray, sr: int, low: bool = False) -> np.ndarray:
    """Mean energy per 0.5 ms frame, smoothed to 1 ms (kick band only if low)."""
    if low:
        y = signal.sosfilt(signal.butter(4, 150, "low", fs=sr, output="sos"), y)
    n = int(len(y) * RATE / sr)
    edges = np.round(np.arange(n + 1) * sr / RATE).astype(int)
    c = np.concatenate([[0.0], np.cumsum(y.astype(np.float64) ** 2)])
    e = (c[edges[1:]] - c[edges[:-1]]) / np.diff(edges)
    return np.convolve(e, np.ones(2) / 2, mode="same")


def attacks(beats: np.ndarray, e: np.ndarray, before: float = 0.060, after: float = 0.030) -> tuple[np.ndarray, np.ndarray]:
    a = np.sqrt(e)
    a = np.convolve(a, np.ones(2) / 2, mode="same")
    slope = np.zeros_like(a)
    slope[2 * MS :] = a[2 * MS :] - a[: -2 * MS]  # rise over 1 ms
    out, ok = beats.copy(), np.zeros(len(beats), bool)
    for i, b in enumerate(beats):
        w0, w1 = int((b - before) * RATE), int((b + after) * RATE)
        if w0 < (GAP_MS + 100) * MS or w1 + 60 * MS >= len(a):
            continue
        p = w0 + int(np.argmax(a[w0 : int(b * RATE) + 60 * MS]))
        q = p - GAP_MS * MS + int(np.argmin(a[p - GAP_MS * MS : p]))
        if a[p] < 3 * a[q]:
            continue
        thr = a[q] + RISE * (a[p] - a[q])
        c = q + int(np.flatnonzero(a[q : p + 1] >= thr)[0])
        steep = slope[max(q, c - 10 * MS) : c + 2 * MS].max()
        if steep <= 0:
            continue
        f = c
        while f > q and slope[f] > FOOT_SLOPE * steep:
            f -= 1
        # slope[f] spans f-2..f frames (1 ms): the rise starts at its beginning
        f = max(q, f - MS)
        if w0 <= f <= w1:
            out[i], ok[i] = f / RATE, True
    return out, ok


def kicks(beats: np.ndarray, el: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    """(beat has a kick, beat is in a kick section). el: energy(y, sr, low=True)."""
    la = np.sqrt(el)
    pk = np.zeros(len(beats))
    pre = np.ones(len(beats))
    for i, b in enumerate(beats):
        j = int(b * RATE)
        if j - 130 * MS < 0 or j + 60 * MS >= len(la):
            continue
        pk[i] = la[j - 30 * MS : j + 60 * MS].max()
        pre[i] = la[j - 130 * MS : j - 50 * MS].mean() + 1e-9
    typical = np.percentile(pk, 90) if len(pk) else 0
    kick = (pk >= 2 * pre) & (pk >= 0.2 * typical)
    frac = np.convolve(kick.astype(float), np.ones(8) / 8, mode="same")
    return kick, (frac >= 0.6) & kick


def main(ids):
    import gtlib
    from common import DATA, load_wav

    for d in ("attack", "attack_ok", "kick"):
        (DATA / "grids" / d).mkdir(parents=True, exist_ok=True)
    for tid in ids:
        gt = gtlib.truth(tid, with_ref=False)
        y, sr = load_wav(tid)
        at, ok = attacks(gt.beats, energy(y, sr))
        _, ksec = kicks(gt.beats, energy(y, sr, low=True))
        np.save(DATA / "grids" / "attack" / f"{tid}.npy", at[ok])
        np.save(DATA / "grids" / "attack_ok" / f"{tid}.npy", ok)
        np.save(DATA / "grids" / "kick" / f"{tid}.npy", ksec)
        d = (gt.beats[ok & ksec] - at[ok & ksec]) * 1000
        print(f"{tid:5d} kick sections {ksec.mean():4.0%} of beats, attacks found {ok.mean():4.0%}; Beat This! minus attack median {np.median(d) if len(d) else 0:+5.1f} ms", flush=True)


if __name__ == "__main__":
    from common import BT, sample_ids

    main([int(a) for a in sys.argv[1:]] or [i for i in sample_ids() if (BT / "final0" / f"{i}.npz").exists()])
