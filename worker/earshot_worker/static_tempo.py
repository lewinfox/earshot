"""Static analysis: one tempo for the whole track, its phase and its downbeat.

For the ~99.9% of tracks with a perfectly regular beat. Much faster than running Beat
This! over the whole track (about 3 s instead of 20 s on a laptop CPU):

1. Tempo from cheap signal processing over the whole track: an onset envelope
   (spectral flux at 11 kHz, 5.8 ms hop), a rough tempo by autocorrelation with a soft
   preference around 125 BPM, then a fine scan of +-3% in 0.01 BPM steps. This gets
   the tempo right, but often lands on the wrong part of the beat.
2. Phase and downbeat from the small Beat This! model on three 15 s excerpts: the
   strongest-beat 15 s of each third of the track (so not a breakdown). Each excerpt's
   beats are lined up with the tempo from step 1; excerpts whose beats don't line up
   cleanly (coherence below MIN_COHERENCE) are ignored. The downbeat is a majority vote.
3. If the excerpts disagree on phase by more than STEADY_MS, or one's own tempo is
   more than STEADY_TEMPO_PCT from the track's, the tempo probably drifts, and the
   track is flagged so the app can suggest dynamic analysis.

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
EXCERPTS = 3
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


def strongest_windows(t: np.ndarray, env: np.ndarray, seconds: float, sr: int) -> list[int]:
    """Start sample of the EXCERPT_S window with the strongest onsets in each of
    EXCERPTS equal parts of the track."""
    fps = SR / HOP
    w = int(EXCERPT_S * fps)
    strength = np.convolve(env, np.ones(w), mode="valid")  # strength[i]: window from frame i
    starts = []
    for k in range(EXCERPTS):
        lo = int(k * len(strength) / EXCERPTS)
        hi = max(lo + 1, int((k + 1) * len(strength) / EXCERPTS))
        i = lo + int(np.argmax(strength[lo:hi]))
        starts.append(int(t[i] * sr))
    return starts


def analyse_static(y: np.ndarray, sr: int, progress=None) -> dict:
    report = progress or (lambda f: None)
    mono = (y.mean(axis=1) if y.ndim == 2 else y).astype(np.float32)
    t, env, low = onset_envelope(mono, sr)
    report(0.15)
    raw = fine_tempo(t, env, coarse_tempo(env, SR / HOP))
    bpm = snap(raw)
    period = 60 / bpm
    report(0.25)

    model = get_beat_this(SMALL_CHECKPOINT)
    n = len(mono)
    span = int(EXCERPT_S * sr)
    starts = [0] if n <= span * EXCERPTS else strongest_windows(t, env, n / sr, sr)
    phases, weights, votes, local = [], [], [], []
    for i, s in enumerate(starts):
        seg = mono[s : s + span] if len(starts) > 1 else mono
        beats, downbeats = model(seg, sr)
        beats = np.asarray(beats, float) + s / sr
        downbeats = np.asarray(downbeats, float) + s / sr
        z = np.exp(2j * np.pi * beats / period).mean() if len(beats) >= 4 else 0
        doubled = False
        if abs(z) < MIN_COHERENCE and len(beats) >= 8:
            # Beats at twice the tempo (off-beat hats counted too)? They line up on a
            # half-beat grid; choose the half with more bass as the beat.
            z2 = np.exp(4j * np.pi * beats / period).mean()
            if abs(z2) >= MIN_COHERENCE:
                ph = (np.angle(z2) / (2 * np.pi)) * period / 2
                a, b = s / sr, (s + span) / sr
                if bass_on_grid(t, low, ph + period / 2, period, a, b) > bass_on_grid(t, low, ph, period, a, b):
                    ph += period / 2
                z = abs(z2) * np.exp(2j * np.pi * ph / period)
                doubled = True
        if abs(z) >= MIN_COHERENCE or len(starts) == 1:
            phases.append(np.angle(z))
            weights.append(abs(z))
            # The excerpt's own tempo: a line through its beats, numbered on the grid.
            step = period / 2 if doubled else period
            k = np.round((beats - beats[0]) / step)
            if k[-1] > 8:
                local.append(60 / (np.polyfit(k, beats, 1)[0] * (2 if doubled else 1)))
            # Downbeats only from excerpts that counted the beats right.
            if not doubled:
                ph = (np.angle(z) / (2 * np.pi)) * period
                votes += list(np.round((downbeats - ph) / period).astype(int) % 4)
        report(0.25 + 0.75 * (i + 1) / len(starts))
    if not phases:
        raise ValueError("no clear beats found: try dynamic analysis")

    # Phase: the weighted circular mean of the excerpts'; spread: how far they disagree.
    phases = np.array(phases)
    z = (np.array(weights) * np.exp(1j * phases)).sum()
    phase = (np.angle(z) / (2 * np.pi)) * period % period
    dev = (phases - np.angle(z) + np.pi) % (2 * np.pi) - np.pi
    spread_ms = float(np.ptp(dev) / (2 * np.pi) * period * 1000) if len(phases) > 1 else 0.0
    drift_pct = float(max((abs(b / bpm - 1) * 100 for b in local), default=0.0))
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
