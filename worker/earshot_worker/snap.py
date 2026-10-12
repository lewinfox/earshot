"""Snap a grid to where the kicks start: the one kick snap for static and dynamic grids.

Beat This! puts its beats 20-40 ms after the start of the kick, so a grid phased from
it is late by about that much. A grid line belongs where the kick's attack begins:
on the click if the hit opens with one, never on a faint sound before it.

For each grid line inside a kick section, find where its hit starts (attacks), then
move the grid by the median offset: one for a straight grid, one per stretch of the
track for a grid that follows the music (kick_shifts). Worked out in research/beatgrid (hybrid.py,
attack.py); see its README for the numbers.
"""
from __future__ import annotations

import numpy as np

RATE = 2000  # energy frames per second
MS = RATE // 1000
GAP_MS = 150  # look this far before a hit's body for the gap before it (kicks can swell for 100 ms)
RISE = 0.30  # the hit proper reaches this far from the gap to the body; a faint pre-sound doesn't
FOOT_SLOPE = 0.10  # walk back down the rise while it's steeper than this share of its steepest
# Kick sections: kick-band rises summed over KICK_WIN_S, above KICK_LEVEL of their 90th
# percentile, for at least KICK_MIN_S. Less than KICK_MIN_SHARE of the track (breakbeats,
# drum & bass): every grid line counts for the snap, and the static analysis chooses
# its excerpts from the music as a whole.
KICK_WIN_S = 2.0
KICK_LEVEL = 0.35
KICK_MIN_S = 8.0
KICK_MIN_SHARE = 0.3
# Fewer hits than this to go on: leave the grid where it is.
MIN_HITS = 8


def energy(y: np.ndarray, sr: int, low: bool = False) -> np.ndarray:
    """Mean energy per 0.5 ms frame, smoothed to 1 ms. low: the kick band (< 150 Hz) only."""
    if low:
        from scipy import signal

        y = signal.sosfilt(signal.butter(4, 150, "low", fs=sr, output="sos"), y)
    n = int(len(y) * RATE / sr)
    edges = np.round(np.arange(n + 1) * sr / RATE).astype(int)
    c = np.concatenate([[0.0], np.cumsum(y.astype(np.float64) ** 2)])
    e = (c[edges[1:]] - c[edges[:-1]]) / np.diff(edges)
    return np.convolve(e, np.ones(2) / 2, mode="same")


def attacks(lines: np.ndarray, e: np.ndarray, before: float, after: float) -> tuple[np.ndarray, np.ndarray]:
    """Where the hit near each grid line starts (s), and whether one was found within
    `before` s before to `after` s after the line. e: energy(y, sr).

    1. The hit's body is its loudest point (amplitude, 1 ms) from `before` before to
       60 ms after the line; the gap is the quietest point in the GAP_MS before that.
    2. The hit proper starts where the amplitude first reaches RISE of the way from
       the gap to the body.
    3. Walk back down that rise to its foot. A faint, flat sound before the click
       stops the walk, so it isn't counted.
    """
    a = np.sqrt(e)
    a = np.convolve(a, np.ones(2) / 2, mode="same")
    slope = np.zeros_like(a)
    slope[2 * MS :] = a[2 * MS :] - a[: -2 * MS]  # rise over 1 ms
    out, ok = lines.astype(float).copy(), np.zeros(len(lines), bool)
    for i, t in enumerate(lines):
        w0, w1 = int((t - before) * RATE), int((t + after) * RATE)
        if w0 < (GAP_MS + 100) * MS or w1 + 60 * MS >= len(a):
            continue
        p = w0 + int(np.argmax(a[w0 : int(t * RATE) + 60 * MS]))
        q = p - GAP_MS * MS + int(np.argmin(a[p - GAP_MS * MS : p]))
        if a[p] < 3 * a[q]:
            continue  # no clear hit
        c = q + int(np.flatnonzero(a[q : p + 1] >= a[q] + RISE * (a[p] - a[q]))[0])
        steep = slope[max(q, c - 10 * MS) : c + 2 * MS].max()
        if steep <= 0:
            continue
        f = c
        while f > q and slope[f] > FOOT_SLOPE * steep:
            f -= 1
        f = max(q, f - MS)  # slope[f] spans the 1 ms before f
        if w0 <= f <= w1:
            out[i], ok[i] = f / RATE, True
    return out, ok


def kick_sections(rise: np.ndarray, fps: float) -> np.ndarray:
    """Per frame of `rise`: inside a stretch of steady kicks. rise: kick-band onset
    strength, `fps` frames per second. Judged from the audio alone (not from a grid, so
    a grid that has drifted off the kicks can't hide them). Shared by the static
    analysis (choosing excerpts) and the kick snap (choosing grid lines)."""
    level = np.convolve(rise, np.ones(int(KICK_WIN_S * fps)), mode="same")
    on = level > KICK_LEVEL * np.percentile(level, 90)
    edges = np.flatnonzero(np.diff(np.concatenate([[0], on.astype(int), [0]])))
    for a, b in zip(edges[::2], edges[1::2]):
        if b - a < KICK_MIN_S * fps:
            on[a:b] = False
    return on


def kick_frames(el: np.ndarray) -> np.ndarray:
    """Per 10 ms: whether hits there count for the snap. el: energy(y, sr, low=True).
    Inside kick sections; everywhere when they cover less than KICK_MIN_SHARE of the
    track (breakbeats, drum & bass)."""
    step = 10 * MS
    a = np.sqrt(el[: len(el) // step * step]).reshape(-1, step).mean(1)
    on = kick_sections(np.maximum(np.diff(a, prepend=a[0]), 0), 100)
    if on.mean() < KICK_MIN_SHARE:
        on[:] = True
    return on


def kick_shifts(y: np.ndarray, sr: int, lines: np.ndarray, window: int | None = None) -> np.ndarray:
    """How far (s) to move each grid line so the grid sits where the kicks start. The
    one kick snap for both grid kinds.

    Per line, the offset of the hit there (attacks), counting lines inside kick
    sections only; then the median offset, twice (the second time in a tighter
    window, around the lines as moved by the first).
    - window None (a straight grid): one median over the whole track, so every line
      moves by the same amount.
    - window n (a grid that follows the music, one line per detected beat): the median
      over the n lines around each line, so each section of the track gets its own;
      a line with fewer than MIN_HITS hits around it takes the whole track's.
    Fewer than MIN_HITS hits in the whole track: no move. Not wrapped to a beat, so a
    downbeat moved with its beat stays on the same beat."""
    lines = np.asarray(lines, float)
    shift = np.zeros(len(lines))
    if len(lines) == 0:
        return shift
    e = energy(y, sr)
    sections = kick_frames(energy(y, sr, low=True))
    for before, after in ((0.060, 0.040), (0.025, 0.025)):
        at_lines = lines + shift
        at, ok = attacks(at_lines, e, before, after)
        use = ok & sections[np.clip((at_lines * 100).astype(int), 0, len(sections) - 1)]
        if use.sum() < MIN_HITS:
            break
        off = at - at_lines
        whole = float(np.median(off[use]))
        if window is None:
            shift += whole
            continue
        step = np.full(len(lines), whole)
        half = window // 2
        for i in range(len(lines)):
            lo, hi = max(0, i - half), min(len(lines), i + half + 1)
            near = off[lo:hi][use[lo:hi]]
            if len(near) >= MIN_HITS:
                step[i] = float(np.median(near))
        shift += step
    return shift


def snap_to_kicks(y: np.ndarray, sr: int, bpm: float, beat: float) -> float:
    """How far (s) to move a straight grid (a line at `beat`, then every 60/bpm) so it
    sits where the kicks start (kick_shifts over the whole track)."""
    period = 60 / bpm
    dur = len(y) / sr
    lines = beat + period * np.arange(np.ceil(-beat / period), (dur - beat) / period)
    return float(kick_shifts(y, sr, lines)[0]) if len(lines) else 0.0
