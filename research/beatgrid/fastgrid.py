"""Candidate A (prototype): a plain-DSP straight beat grid, to port to Rust.

1. Onset envelope at 1 kHz: kick band (low-pass 150 Hz) energy, log, positive slope.
   Plus a broadband one (high-pass 200 Hz) as a fallback for tracks with no kick.
2. Coarse tempo: autocorrelation of the envelope (as static_tempo.coarse_tempo).
3. Fold the whole track's envelope at the beat period into a one-beat profile.
   Its peak is the beat phase. Fold 16 s windows the same way; line each window's
   profile up with the whole-track one. A tempo slightly off shows as the window
   offsets creeping steadily; fit a line through them, correct the period, repeat.
4. What's left after the line fit (window offsets that don't sit on it, windows whose
   own tempo differs) measures how far the track is from a straight grid.
"""
from __future__ import annotations

import numpy as np
from scipy import signal

FPS = 1000  # envelope frames per second
WIN_S = 16.0
HOP_S = 8.0


def _frames(e: np.ndarray, sr: int) -> np.ndarray:
    """Mean of e over 1 ms frames."""
    n = int(len(e) * FPS / sr)
    edges = np.round(np.arange(n + 1) * sr / FPS).astype(int)
    c = np.concatenate([[0.0], np.cumsum(e, dtype=np.float64)])
    return ((c[edges[1:]] - c[edges[:-1]]) / np.diff(edges)).astype(np.float32)


def _onset(e: np.ndarray, smooth_ms: int = 8) -> np.ndarray:
    k = np.ones(smooth_ms) / smooth_ms
    e = np.convolve(e, k, mode="same")
    ref = np.percentile(e, 90) + 1e-12
    L = np.log1p(e / ref * 100)
    d = np.zeros_like(L)
    # slope over 4 ms (less noisy than one frame)
    d[2:-2] = L[4:] - L[:-4]
    d = np.maximum(d, 0)
    # remove slow trend (0.5 s)
    m = np.convolve(d, np.ones(500) / 500, mode="same")
    return np.maximum(d - m, 0).astype(np.float32)


def envelopes(y: np.ndarray, sr: int) -> tuple[np.ndarray, np.ndarray]:
    lo = signal.sosfilt(signal.butter(4, 150, "low", fs=sr, output="sos"), y)
    hi = signal.sosfilt(signal.butter(2, 200, "high", fs=sr, output="sos"), y)
    return _onset(_frames(lo * lo, sr)), _onset(_frames(hi * hi, sr))


def attack_env(y: np.ndarray, sr: int) -> np.ndarray:
    """Full-band loudness rise per 1 ms frame (log energy, positive steps only): sharp
    at the very start of each drum hit."""
    L = np.log10(_frames(y * y, sr).astype(np.float64) + 1e-10)
    d = np.zeros_like(L)
    d[1:] = np.maximum(L[1:] - L[:-1], 0)
    return d.astype(np.float32)


def coarse_tempo(env: np.ndarray, lo: float = 60, hi: float = 200) -> float:
    fps = 100
    e = env[: len(env) // 10 * 10].reshape(-1, 10).sum(1)
    e = e - e.mean()
    n = len(e)
    ac = np.fft.irfft(np.abs(np.fft.rfft(e, 2 * n)) ** 2)[:n]
    lags = np.arange(int(fps * 60 / hi), int(fps * 60 / lo) + 1)
    lagsf = np.arange(lags[0], lags[-1], 0.05)
    a = lambda L: np.interp(L, np.arange(n), ac)  # noqa: E731
    score = a(lagsf) + 0.5 * a(2 * lagsf) + 0.25 * a(4 * lagsf)
    bpm = 60 * fps / lagsf
    prior = np.exp(-0.5 * (np.log2(bpm / 125) / 0.6) ** 2)
    return float(bpm[np.argmax(score * prior)])


def fine_tempo(env: np.ndarray, bpm0: float, span: float = 0.03, step: float = 0.02) -> float:
    """Scan bpm0 +-span for the tempo whose beat frequency and 2nd/4th harmonics carry
    the most of the envelope (whole track: a long baseline, so very precise)."""
    fps = 200
    e = env[: len(env) // 5 * 5].reshape(-1, 5).sum(1).astype(np.float64)
    e -= e.mean()
    t = np.arange(len(e)) / fps
    bpms = np.arange(bpm0 * (1 - span), bpm0 * (1 + span), step)
    mag = np.zeros(len(bpms))
    for h, wgt in ((1, 1.0), (2, 0.5), (4, 0.25)):
        for i in range(0, len(bpms), 32):
            f = bpms[i : i + 32, None] / 60 * h
            E = np.exp(-2j * np.pi * f * t[None, :])
            mag[i : i + 32] += wgt * np.abs(E @ e)
    j = int(np.argmax(mag))
    bpm = float(bpms[j])
    if 0 < j < len(mag) - 1:
        a, b, c = mag[j - 1], mag[j], mag[j + 1]
        den = a - 2 * b + c
        if den < 0:
            bpm += step * 0.5 * (a - c) / den
    return bpm


def fold(env: np.ndarray, t0: int, t1: int, period: float) -> np.ndarray:
    """Envelope frames t0..t1 folded at `period` (frames) into int(period) bins
    (linear interpolation between bins, so a fractional period works)."""
    nb = int(round(period))
    i = np.arange(t0, t1)
    pos = (i % period) / period * nb
    b0 = np.floor(pos).astype(int) % nb
    f = pos - np.floor(pos)
    w = env[t0:t1]
    prof = np.bincount(b0, w * (1 - f), nb) + np.bincount((b0 + 1) % nb, w * f, nb)
    return prof


def circ_xcorr(a: np.ndarray, b: np.ndarray) -> np.ndarray:
    """r[k] = sum a[i] b[i-k]: how well a matches b shifted later by k."""
    return np.fft.irfft(np.fft.rfft(a) * np.conj(np.fft.rfft(b)), len(a))


def peak(x: np.ndarray) -> float:
    """Circular argmax with parabolic refinement."""
    n = len(x)
    j = int(np.argmax(x))
    a, b, c = x[(j - 1) % n], x[j], x[(j + 1) % n]
    den = a - 2 * b + c
    return j + (0.5 * (a - c) / den if den < 0 else 0.0)


def smooth_circ(x: np.ndarray, ms: int = 5) -> np.ndarray:
    k = np.ones(ms) / ms
    return np.convolve(np.concatenate([x[-ms:], x, x[:ms]]), k, mode="same")[ms:-ms]


def windows(env: np.ndarray, period: float, template: np.ndarray):
    """Each window's centre (s), offset from the template (frames, wrapped to
    +-period/2) and how well it matched (normalised correlation)."""
    w, h = int(WIN_S * FPS), int(HOP_S * FPS)
    out = []
    nb = len(template)
    tz = template - template.mean()
    for t0 in range(0, max(1, len(env) - w + 1), h):
        p = fold(env, t0, t0 + w, period)
        pz = p - p.mean()
        r = circ_xcorr(pz, tz)
        k = peak(r)
        if k > nb / 2:
            k -= nb
        den = np.linalg.norm(pz) * np.linalg.norm(tz)
        out.append(((t0 + w / 2) / FPS, k * period / nb, float(r.max() / den) if den > 0 else 0.0, float(p.sum())))
    return np.array(out).reshape(-1, 4)


def weighted_line(x, y, w, iters=8):
    ww = w.copy()
    for _ in range(iters):
        b, a = np.polyfit(x, y, 1, w=np.sqrt(ww))
        r = y - (a + b * x)
        s = 1.4826 * np.median(np.abs(r[ww > 0])) + 0.5
        ww = w * np.where(np.abs(r) <= 1.345 * s, 1, 1.345 * s / np.maximum(np.abs(r), 1e-9))
    return a, b, r


ATTACK_BEFORE, ATTACK_AFTER = 30, 8  # ms


def amp_env(y: np.ndarray, sr: int) -> np.ndarray:
    """Full-band amplitude per 1 ms frame (root mean square)."""
    return np.sqrt(_frames(y * y, sr))


SNAP_SEARCH_MS = 60  # look for the hit's peak this far either side of the phase
SNAP_QUIET_MS = 80  # and for the quiet before it this far before the peak
SNAP_RISE = 0.15  # the hit starts when it's this far from quiet to peak


def snap_median(y: np.ndarray, sr: int, bpm: float, phase: float, e=None) -> float:
    """The shared kick-section snap (hybrid.snap_kick)."""
    import hybrid

    return hybrid.snap_kick(y, sr, bpm, phase, e=e)[0]


def snap_phase(y: np.ndarray, sr: int, bpm: float, phase: float, amp=None) -> float:
    """Move a straight grid's phase to where the hit starts. Fold the full-band
    amplitude over the whole track at the grid's period: the average hit. Its peak
    (within SNAP_SEARCH_MS of the current phase) is the body of the hit; the quietest
    point in the SNAP_QUIET_MS before that is the gap before it; the hit starts where
    the average first climbs SNAP_RISE of the way from the gap to the peak. Same
    definition as the per-beat reference in attack.py. Returns the new phase (s)."""
    amp = amp_env(y, sr) if amp is None else amp
    period = 60 / bpm * FPS
    prof = fold(amp, 0, len(amp), period)
    nb = len(prof)
    c = int(round(phase * FPS * nb / period))
    s = np.arange(-SNAP_SEARCH_MS, SNAP_SEARCH_MS + 1)
    p = c + s[int(np.argmax(prof[(c + s) % nb]))]
    q_offs = np.arange(-SNAP_QUIET_MS, 1)
    q = p + q_offs[int(np.argmin(prof[(p + q_offs) % nb]))]
    seg = prof[np.arange(q, p + 1) % nb]
    thr = seg[0] + SNAP_RISE * (prof[p % nb] - seg[0])
    f = q + int(np.flatnonzero(seg >= thr)[0])
    return (f * period / nb / FPS) % (period / FPS)


def analyse(y: np.ndarray, sr: int, env_kind: str = "auto", attack: bool = True) -> dict:
    low, broad = envelopes(y, sr)
    # Use the kick band unless it's nearly silent relative to the broadband
    env = low
    if env_kind == "broad":
        env = broad
    bpm = fine_tempo(env, coarse_tempo(env))
    period = 60 / bpm * FPS  # frames
    for it in range(4):
        template = fold(env, 0, len(env), period)
        ws = windows(env, period, template)
        conf = np.clip(ws[:, 2], 0, 1) ** 2 * (ws[:, 3] > 0.2 * np.median(ws[:, 3]))
        good = conf > 0.1
        if good.sum() < 3:
            break
        # unwrap offsets (frames) along time
        off = np.unwrap(ws[good, 1] / period * 2 * np.pi) / (2 * np.pi) * period
        a, b, r = weighted_line(ws[good, 0], off, conf[good])
        # offset growing by b frames per second: the beat period is longer by that fraction
        period *= 1 + b / FPS
    template = fold(env, 0, len(env), period)
    ph = peak(smooth_circ(template)) * period / len(template)  # frames
    # The kick-band onset peaks around the start of the kick's body; move to where
    # the hit starts (snap_phase).
    if attack:
        ph = snap_median(y, sr, 60 * FPS / period, ph / FPS) * FPS
    # windows again at the final period, for the steadiness measures
    ws = windows(env, period, template)
    conf = np.clip(ws[:, 2], 0, 1) ** 2 * (ws[:, 3] > 0.2 * np.median(ws[:, 3]))
    good = conf > 0.1
    resid = np.array([0.0])
    if good.sum() >= 3:
        off = np.unwrap(ws[good, 1] / period * 2 * np.pi) / (2 * np.pi) * period
        a, b, resid = weighted_line(ws[good, 0], off, conf[good])
    return {
        "bpm": 60 * FPS / period,
        "phase": ph / FPS,  # a beat at this time (s), then every 60/bpm
        "win_resid_p90_ms": float(np.percentile(np.abs(resid), 90)),
        "win_resid_max_ms": float(np.abs(resid).max()),
        "good_frac": float(good.mean()),
        "coarse_bpm": bpm,
    }
