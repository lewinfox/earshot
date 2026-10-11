"""Static analysis: one tempo for the whole track, its phase and its downbeat.

For the ~99.9% of tracks with a perfectly regular beat. Much faster than running Beat
This! over the whole track (about 4 s instead of 20 s on a laptop CPU):

1. Tempo from cheap signal processing over the whole track: an onset envelope
   (spectral flux at 11 kHz, 5.8 ms hop), a rough tempo by autocorrelation with a soft
   preference around 125 BPM, then a fine scan of +-3% in 0.01 BPM steps. This is
   usually right to a few hundredths, but can pick a side peak; step 3 checks it.
2. Phase and downbeat from the small Beat This! model on five 15 s excerpts: the
   very start and very end of the music (DJ-friendly intros and outros usually carry
   the cleanest beat), plus the strongest-beat 15 s of each third of what's between
   (so not a breakdown). Each excerpt's beats are lined up with the tempo from step
   1; excerpts whose beats don't line up cleanly (coherence below MIN_COHERENCE) are
   ignored. The downbeat is a majority vote.
3. The excerpts also check and correct the tempo. Each has its own (from ~30 beats);
   if they agree with each other but not with step 1, theirs wins. Then, if it's a
   little off, their phases creep steadily from start to end. A line through them
   gives the tempo, then the roundest BPM (to 0.1 at the finest) whose grid still lands on every excerpt's
   beats (choose_tempo).
4. If the excerpts still disagree with that grid by more than STEADY_MS, or one's own
   tempo is more than STEADY_TEMPO_PCT from the track's, the tempo probably drifts,
   and the track is flagged so the app can suggest dynamic analysis.

Worked out in the beat-detection research for earshot; on four test tracks it matched
Beat This! final0 over the whole track on BPM and downbeat, with phase within ~15 ms.
"""
from __future__ import annotations

import numpy as np

from .detectors import SMALL_CHECKPOINT, get_beat_this

SR = 11025
NFFT = 1024
HOP = 64  # 5.8 ms
EXCERPT_S = 15.0
# The start, the end, and this many from between them.
MIDDLE_EXCERPTS = 3
EXCERPTS = MIDDLE_EXCERPTS + 2
# How well an excerpt's beats must line up with the tempo (1 = perfectly) to count.
MIN_COHERENCE = 0.6
# Excerpt phases further apart than this suggest a drifting tempo...
STEADY_MS = 35.0
# ...as does an excerpt whose own tempo is this far (%) from the track's. (A drift
# of more than half a beat wraps the phase round, so the phase check alone misses it.)
STEADY_TEMPO_PCT = 0.4


def onset_envelope(y: np.ndarray, sr: int) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    """Spectral flux of log magnitudes, with the slow trend removed: over all
    frequencies, and below 150 Hz only (kicks, not hats)."""
    import soxr

    x = soxr.resample(y, sr, SR).astype(np.float32)
    n = 1 + (len(x) - NFFT) // HOP
    if n < 64:
        raise ValueError("track too short to analyse")
    win = np.hanning(NFFT).astype(np.float32)
    flux = np.zeros(n, np.float32)
    low = np.zeros(n, np.float32)
    nlow = int(150 / (SR / NFFT)) + 1
    prev = None
    # In blocks, so a long mix doesn't need a huge frames matrix at once.
    for s0 in range(0, n, 4096):
        s1 = min(n, s0 + 4096)
        idx = np.arange(NFFT)[None, :] + HOP * np.arange(s0, s1)[:, None]
        L = np.log1p(100 * np.abs(np.fft.rfft(x[idx] * win, axis=1)))
        both = L if prev is None else np.vstack([prev, L])
        rise = np.maximum(both[1:] - both[:-1], 0)
        pad = (lambda d: d) if prev is not None else (lambda d: np.concatenate([[0.0], d]))
        flux[s0:s1] = pad(rise.sum(1))
        low[s0:s1] = pad(rise[:, :nlow].sum(1))
        prev = L[-1:]
    k = int(0.5 * SR / HOP)
    detrend = lambda f: np.maximum(f - np.convolve(f, np.ones(k) / k, mode="same"), 0)  # noqa: E731
    t = (np.arange(n) * HOP + NFFT / 2) / SR
    return t, detrend(flux), detrend(low)


def coarse_tempo(env: np.ndarray, fps: float, lo: float = 60, hi: float = 200) -> float:
    """Autocorrelation, counting the 2- and 4-beat lags too, with a soft preference for
    tempos near 125 BPM (so 175 doesn't come out as 87.5)."""
    e = env - env.mean()
    n = len(e)
    ac = np.fft.irfft(np.abs(np.fft.rfft(e, 2 * n)) ** 2)[:n]
    lags = np.arange(int(fps * 60 / hi), int(fps * 60 / lo) + 1)
    bpm = 60 * fps / lags
    score = ac[lags] + 0.5 * np.interp(2 * lags, np.arange(n), ac) + 0.25 * np.interp(4 * lags, np.arange(n), ac)
    prior = np.exp(-0.5 * (np.log2(bpm / 125) / 0.6) ** 2)
    return float(bpm[np.argmax(score * prior)])


def fine_tempo(t: np.ndarray, env: np.ndarray, bpm0: float) -> float:
    """Scan +-3% in 0.01 BPM steps for the strongest periodicity; refine the peak."""
    bpms = np.arange(bpm0 * 0.97, bpm0 * 1.03, 0.01)
    m = len(env) // 4
    td = t[: m * 4 : 4]
    fd = env[: m * 4].reshape(-1, 4).sum(1)
    mag = np.empty(len(bpms))
    for i in range(0, len(bpms), 64):  # in blocks to bound memory
        E = np.exp(-2j * np.pi * np.outer(bpms[i : i + 64] / 60, td)).astype(np.complex64)
        mag[i : i + 64] = np.abs(E @ fd.astype(np.complex64))
    j = int(np.argmax(mag))
    bpm = float(bpms[j])
    if 0 < j < len(mag) - 1:
        a, b, c = mag[j - 1], mag[j], mag[j + 1]
        den = a - 2 * b + c
        if den != 0:
            bpm += 0.01 * 0.5 * (a - c) / den
    return bpm


def snap(bpm: float) -> float:
    """Prefer whole numbers, then halves; otherwise two decimals."""
    if abs(bpm - round(bpm)) < 0.05:
        return float(round(bpm))
    if abs(bpm * 2 - round(bpm * 2)) < 0.05:
        return round(bpm * 2) / 2
    return round(bpm, 2)


def bass_on_grid(t: np.ndarray, low: np.ndarray, phase: float, period: float, a: float, b: float) -> float:
    """Average bass onset strength at the grid's beats between times a and b."""
    k0 = np.ceil((a - phase) / period)
    g = phase + period * np.arange(k0, k0 + max(1, int((b - a) / period)))
    i = np.clip(np.searchsorted(t, g), 0, len(t) - 1)
    # The strongest frame within 20 ms of each grid beat.
    w = int(0.02 * SR / HOP)
    return float(np.mean([low[max(0, j - w) : j + w + 1].max() for j in i]))


def excerpt_starts(t: np.ndarray, env: np.ndarray, sr: int) -> list[int]:
    """Start sample of each EXCERPT_S excerpt: the first and last of the music
    (leading and trailing quiet skipped), then the window with the strongest onsets
    in each of MIDDLE_EXCERPTS equal parts of what's between."""
    fps = SR / HOP
    w = int(EXCERPT_S * fps)
    # Where the music starts and stops: one-second onset strength above a fifth of
    # its typical level.
    level = np.convolve(env, np.ones(int(fps)), mode="same")
    on = np.flatnonzero(level > 0.2 * np.median(level[level > 0]))
    first, last = (int(on[0]), int(on[-1]) + 1) if len(on) else (0, len(env))
    if last - first < w * EXCERPTS:
        first, last = 0, len(env)
    frames = [first, last - w]
    strength = np.convolve(env, np.ones(w), mode="valid")  # strength[i]: window from frame i
    lo0, hi0 = first + w, last - 2 * w  # middle windows mustn't overlap the ends
    for k in range(MIDDLE_EXCERPTS):
        lo = lo0 + int(k * (hi0 - lo0) / MIDDLE_EXCERPTS)
        hi = max(lo + 1, lo0 + int((k + 1) * (hi0 - lo0) / MIDDLE_EXCERPTS))
        frames.append(lo + int(np.argmax(strength[lo:hi])))
    return sorted(int(t[i] * sr) for i in frames)


# How far (s) a straight grid may miss the excerpts' beats and still be "on the beat",
# when choosing a rounder tempo.
GRID_TOLERANCE = 0.015  # as the engine's Beatgrid::straightened


def wrap(x: np.ndarray, period: float) -> np.ndarray:
    """To -period/2..period/2."""
    return (x + period / 2) % period - period / 2


def grid_fit(anchors: np.ndarray, weights: np.ndarray, period: float) -> tuple[float, np.ndarray]:
    """Best offset for a straight grid at `period` through beat times `anchors`, and
    each anchor's miss (s)."""
    z = (weights * np.exp(2j * np.pi * anchors / period)).sum()
    off = (np.angle(z) / (2 * np.pi)) * period % period
    return off, wrap(anchors - off, period)


def choose_tempo(anchors: np.ndarray, weights: np.ndarray, raw: float) -> float:
    """The track's tempo from beat times found across it (one per excerpt).

    The whole-track tempo pass is good to a few hundredths of a BPM; excerpts from
    the start to the end pin it down: a tempo a little off shows as phase creeping
    steadily from one excerpt to the next. Fit a line through them, then take the
    roundest tempo (whole, half, tenth) whose grid still lands within GRID_TOLERANCE
    of every excerpt (or no worse than the fitted tempo plus 5 ms).

    Nothing finer than a tenth: produced electronic music doesn't sit at 121.99. A
    track that really fits no tenth (an edit, a re-pitch, a live recording) then
    misses some excerpts, shows as spread, and gets flagged for dynamic analysis.
    """
    if len(anchors) < 3 or np.ptp(anchors) < 60:
        return snap(raw)
    p0 = 60 / raw
    k = np.round((anchors - anchors[0]) / p0)
    fitted = 60 / np.polyfit(k, anchors, 1, w=weights)[0]
    miss = lambda bpm: float(np.abs(grid_fit(anchors, weights, 60 / bpm)[1]).max())  # noqa: E731
    tolerance = max(GRID_TOLERANCE, miss(fitted) + 0.005)
    candidates = [round(fitted * f) / f for f in (1, 2, 10)]
    return next((float(b) for b in candidates if miss(b) <= tolerance), candidates[-1])


def line_up(beats, s, span, sr, period, t, low):
    """How an excerpt's beats line up with a grid at `period`: (z, doubled). |z| is the
    coherence (1 = perfectly), its angle the phase. `doubled`: the model counted
    off-beats too, and z is for the half with more bass."""
    z = np.exp(2j * np.pi * beats / period).mean() if len(beats) >= 4 else 0
    if abs(z) < MIN_COHERENCE and len(beats) >= 8:
        # Beats at twice the tempo (off-beat hats counted too)? They line up on a
        # half-beat grid; choose the half with more bass as the beat.
        z2 = np.exp(4j * np.pi * beats / period).mean()
        if abs(z2) >= MIN_COHERENCE:
            ph = (np.angle(z2) / (2 * np.pi)) * period / 2
            a, b = s / sr, (s + span) / sr
            if bass_on_grid(t, low, ph + period / 2, period, a, b) > bass_on_grid(t, low, ph, period, a, b):
                ph += period / 2
            return abs(z2) * np.exp(2j * np.pi * ph / period), True
    return z, False


def analyse_static(y: np.ndarray, sr: int, progress=None) -> dict:
    report = progress or (lambda f: None)
    mono = (y.mean(axis=1) if y.ndim == 2 else y).astype(np.float32)
    t, env, low = onset_envelope(mono, sr)
    report(0.15)
    raw = fine_tempo(t, env, coarse_tempo(env, SR / HOP))
    report(0.25)

    model = get_beat_this(SMALL_CHECKPOINT)
    n = len(mono)
    span = int(EXCERPT_S * sr)
    starts = [0] if n <= span * EXCERPTS else excerpt_starts(t, env, sr)
    found = []  # (start, beats, downbeats) per excerpt
    for i, s in enumerate(starts):
        seg = mono[s : s + span] if len(starts) > 1 else mono
        beats, downbeats = model(seg, sr)
        found.append((s, np.asarray(beats, float) + s / sr, np.asarray(downbeats, float) + s / sr))
        report(0.25 + 0.75 * (i + 1) / len(starts))

    def gather(period):
        """Per excerpt that lines up: a beat time near its middle, how cleanly it
        lined up, its own tempo, and its downbeats (only from excerpts that counted
        the beats right)."""
        anchors, weights, downs, local = [], [], [], []
        for s, beats, downbeats in found:
            z, doubled = line_up(beats, s, span, sr, period, t, low)
            if abs(z) < MIN_COHERENCE and len(found) > 1:
                continue
            ph = (np.angle(z) / (2 * np.pi)) * period % period
            middle = (beats[0] + beats[-1]) / 2 if len(beats) else s / sr
            anchors.append(ph + np.round((middle - ph) / period) * period)
            weights.append(max(abs(z), 1e-3))
            # The excerpt's own tempo: a line through its beats, numbered on the grid.
            step = period / 2 if doubled else period
            k = np.round((beats - beats[0]) / step)
            if len(k) and k[-1] > 8:
                local.append(60 / (np.polyfit(k, beats, 1)[0] * (2 if doubled else 1)))
            if not doubled:
                downs.append(downbeats)
        return anchors, weights, downs, local

    start = raw
    anchors, weights, downs, local = gather(60 / start)
    # The whole-track pass can pick a side peak (one track came out at 126.87 when
    # every excerpt said 126.0): trust the excerpts' own tempos when they agree with
    # each other but not with it.
    if len(local) >= 3 and np.ptp(local) < 0.002 * raw and abs(np.median(local) / raw - 1) > 0.001:
        start = float(np.median(local))
        anchors, weights, downs, local = gather(60 / start)
    if not anchors:
        raise ValueError("no clear beats found: try dynamic analysis")

    anchors, weights = np.array(anchors), np.array(weights)
    bpm = choose_tempo(anchors, weights, start)
    period = 60 / bpm
    # Phase: the grid that best fits every excerpt at that tempo. Spread: how far the
    # excerpts still disagree with it, which a straight grid can't fix (drift).
    phase, miss = grid_fit(anchors, weights, period)
    spread_ms = float(np.ptp(miss) * 1000) if len(anchors) > 1 else 0.0
    drift_pct = float(max((abs(b / bpm - 1) * 100 for b in local), default=0.0))
    votes = [v for d in downs for v in np.round((d - phase) / period).astype(int) % 4]
    if votes:
        k, counts = np.unique(votes, return_counts=True)
        down = int(k[np.argmax(counts)])
    else:
        down = 0
    return {
        "bpm": float(bpm),
        "raw_bpm": float(raw),
        "beat": float(phase),
        "downbeat": float(phase + down * period),
        "spread_ms": spread_ms,
        "drift_pct": drift_pct,
        "steady": spread_ms <= STEADY_MS and drift_pct <= STEADY_TEMPO_PCT,
    }
