"""Static analysis: one tempo for the whole track, its phase and its downbeat.

For the great majority of tracks, with a perfectly regular beat. Much faster than
running Beat This! over the whole track (about 3 s instead of 11 s on 4 laptop cores):

1. Tempo from cheap signal processing over the whole track: an onset envelope
   (spectral flux at 11 kHz, 5.8 ms hop), a rough tempo by autocorrelation with a soft
   preference around 125 BPM, then a fine scan of +-3% in 0.01 BPM steps. This is
   usually right to a few hundredths, but can pick a side peak; step 3 checks it.
2. Phase and downbeat from the small Beat This! model on five 15 s excerpts, all
   from stretches with a steady kick (kick_sections; Beat This! is unreliable without
   one): the first and last of them, plus the strongest-kick 15 s of each third of
   what's between. Intros without percussion, bass-less breakdowns and fade-outs are
   skipped. Each excerpt's beats are lined up with the tempo from step 1; excerpts
   whose beats don't line up cleanly (coherence below MIN_COHERENCE) are ignored. The
   downbeat is a majority vote.
3. The excerpts also check and correct the tempo. Each has its own (from ~30 beats);
   if they agree with each other but not with step 1, theirs wins. If most don't line
   up with step 1 at all (it can lock to 2/3 of a drum & bass tempo), their own tempo,
   refined by the whole-track scan, is tried instead (and half and double it). Then,
   if it's a little off, their phases creep steadily from start to end. A line
   through them gives the tempo, then the roundest BPM (to 0.1 at the finest) whose
   grid still lands on every excerpt's beats (choose_tempo).
4. Beat This! puts beats 20-40 ms after the start of the kick, so the grid is then
   moved to where the kicks start (snap.snap_to_kicks). The grid runs back to the
   start of the file, over any silent or beatless intro; first_beat is the grid line
   where the beats really start (the model's first beat on the grid, walked back over
   any it missed).
5. The track is flagged as not steady, so the app can suggest dynamic analysis, if
   the excerpts still disagree with the grid by more than STEADY_MS, two or more of
   them don't line up with its tempo at all, or one's own tempo is more than
   STEADY_TEMPO_PCT from the track's (is_steady).

Worked out in research/beatgrid (see its README). On 75 library tracks: grids within
10 ms of the kick start on 57 of 63 regular tracks; 11 of 12 irregular tracks flagged,
3 regular ones wrongly.
"""
from __future__ import annotations

import numpy as np

from .detectors import SMALL_CHECKPOINT, get_beat_this
from .snap import KICK_MIN_SHARE, kick_sections, snap_to_kicks

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
STEADY_MS = 60.0
# ...as does an excerpt whose own tempo is this far (%) from the track's (half and
# double allowed; 15 s excerpts are noisy to about 3%), or two or more excerpts not
# lining up with the track's tempo at all. (A drift of more than half a beat wraps
# the phase round, so the phase check alone misses it.)
STEADY_TEMPO_PCT = 5.0


def onset_envelope(y: np.ndarray, sr: int) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    """Spectral flux of log magnitudes, with the slow trend removed: over all
    frequencies, and below 150 Hz only (kicks, not hats)."""
    import soxr

    # Silence before the start, so a kick in the first frame still rises from
    # something: without it a track that opens on the beat loses its first one.
    x = np.concatenate([np.zeros(NFFT, np.float32), soxr.resample(y, sr, SR).astype(np.float32)])
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
    t = (np.arange(n) * HOP - NFFT / 2) / SR
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


def excerpt_starts(t: np.ndarray, env: np.ndarray, low: np.ndarray, sr: int) -> list[int]:
    """Start sample of each EXCERPT_S excerpt: the first and last 15 s with a steady
    kick, then the window with the strongest kick in each of MIDDLE_EXCERPTS equal
    parts of what's between (wholly inside kick sections). Tracks with too little
    steady kick (breakbeats, drum & bass) use the music's first and last 15 s and the
    strongest onsets between."""
    fps = SR / HOP
    w = int(EXCERPT_S * fps)
    kick = kick_sections(low, fps)
    on = np.flatnonzero(kick)
    if kick.mean() >= KICK_MIN_SHARE and on[-1] + 1 - on[0] >= w * EXCERPTS:
        first, last = int(on[0]), int(on[-1]) + 1
        inside = np.convolve(kick.astype(float), np.ones(w), mode="valid") >= 0.9 * w
        strength = np.convolve(low, np.ones(w), mode="valid") * inside  # strength[i]: window from frame i
    else:
        # Where the music starts and stops: one-second onset strength above a fifth
        # of its typical level.
        level = np.convolve(env, np.ones(int(fps)), mode="same")
        on = np.flatnonzero(level > 0.2 * np.median(level[level > 0]))
        first, last = (int(on[0]), int(on[-1]) + 1) if len(on) else (0, len(env))
        if last - first < w * EXCERPTS:
            first, last = 0, len(env)
        strength = np.convolve(env, np.ones(w), mode="valid")
    frames = [first, last - w]
    lo0, hi0 = first + w, last - 2 * w  # middle windows mustn't overlap the ends
    for k in range(MIDDLE_EXCERPTS):
        lo = lo0 + int(k * (hi0 - lo0) / MIDDLE_EXCERPTS)
        hi = max(lo + 1, lo0 + int((k + 1) * (hi0 - lo0) / MIDDLE_EXCERPTS))
        frames.append(lo + int(np.argmax(strength[lo:hi])))
    # The first frames' times are a little before 0 (the envelope is padded).
    return sorted(max(0, int(t[i] * sr)) for i in frames)


# How far (s) a straight grid may miss the excerpts' beats and still be "on the beat",
# when choosing a rounder tempo.
GRID_TOLERANCE = 0.015  # the one copy of tempo rounding (choose_tempo)


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
    starts = [0] if n <= span * EXCERPTS else excerpt_starts(t, env, low, sr)
    found = []  # (start, beats, downbeats) per excerpt
    for i, s in enumerate(starts):
        seg = mono[s : s + span] if len(starts) > 1 else mono
        beats, downbeats = model(seg, sr)
        found.append((s, np.asarray(beats, float) + s / sr, np.asarray(downbeats, float) + s / sr))
        report(0.25 + 0.65 * (i + 1) / len(starts))

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
    # Most excerpts don't line up at all: the whole-track pass probably locked to a
    # wrong multiple (2/3 of a drum & bass tempo, say). Try the excerpts' own tempo,
    # and half and double it, each refined by the whole-track scan (the small model's
    # own tempo is only rough on fast breakbeats: 176 for 174).
    if len(anchors) < max(2, len(found) // 2):
        own = own_tempo(found)
        for guess in (own, own / 2, own * 2) if own else ():
            if 60 <= guess <= 200:
                trial_bpm = fine_tempo(t, env, guess)
                trial = gather(60 / trial_bpm)
                if len(trial[0]) > len(anchors):
                    start, (anchors, weights, downs, local) = trial_bpm, trial
    if not anchors:
        raise ValueError("no clear beats found: try dynamic analysis")

    anchors, weights = np.array(anchors), np.array(weights)
    bpm = choose_tempo(anchors, weights, start)
    period = 60 / bpm
    # Phase: the grid that best fits every excerpt at that tempo. Spread: how far the
    # excerpts still disagree with it, which a straight grid can't fix (drift).
    phase, miss = grid_fit(anchors, weights, period)
    spread_ms = float(np.ptp(miss) * 1000) if len(anchors) > 1 else 0.0
    lined = len(gather(period)[0])
    # Each excerpt's own tempo against the track's, half and double allowed.
    drift_pct = max((min(abs(b * f / bpm - 1) for f in (0.5, 1, 2)) * 100 for b in excerpt_tempos(found)), default=0.0)
    votes = [v for d in downs for v in np.round((d - phase) / period).astype(int) % 4]
    if votes:
        k, counts = np.unique(votes, return_counts=True)
        down = int(k[np.argmax(counts)])
    else:
        down = 0
    # Beat This! is late: move the grid to where the kicks start. Beat and downbeat
    # by the same amount, so the downbeat stays on the same beat.
    shift = snap_to_kicks(mono, sr, bpm, phase)
    first = first_beat(found, phase, period, t, env)
    report(1.0)
    return {
        "bpm": float(bpm),
        "raw_bpm": float(raw),
        "beat": float((phase + shift) % period),
        "downbeat": float((phase + shift + down * period) % (4 * period)),
        "spread_ms": spread_ms,
        "drift_pct": float(drift_pct),
        "steady": is_steady(spread_ms, lined, len(found), drift_pct),
        "kick_shift": float(shift),
        "first_beat": float(first + shift),
    }


def first_beat(found, phase: float, period: float, t: np.ndarray, env: np.ndarray) -> float:
    """The grid line where the beats start, before the kick snap.

    1. The earliest beat the model found within a quarter beat of a grid line (not an
       off-beat it counted when it doubled) and not in near-silence (onsets within
       60 ms before to 20 ms after under a tenth of those beats' median), taken to
       its grid line.
    2. The model often misses an excerpt's first beat or two, and a short track is
       one excerpt: walk back a line at a time while there's still a hit there (over
       a third of that median).
    No such beat: the grid's first line in the track."""
    before, after = int(0.06 * SR / HOP), int(0.02 * SR / HOP)

    def strength(times: np.ndarray) -> np.ndarray:
        # From 60 ms before (the model's beats are late) to 20 ms after.
        idx = np.clip(np.searchsorted(t, times), 0, len(t) - 1)
        return np.array([env[max(0, i - before) : i + after + 1].max() for i in idx])

    beats = np.sort(np.concatenate([b for _, b, _ in found])) if found else np.array([])
    beats = beats[np.abs(wrap(beats - phase, period)) < period / 4]
    if not len(beats):
        return float(phase % period)
    level = strength(beats)
    typical = float(np.median(level))
    beats = beats[level > 0.1 * typical]
    if not len(beats):
        return float(phase % period)
    line = phase + np.round((beats[0] - phase) / period) * period
    while line - period >= 0 and strength(np.array([line - period]))[0] > typical / 3:
        line -= period
    return float(line)


def excerpt_tempos(found) -> list[float]:
    """Each excerpt's own tempo: a straight line through its beats, numbered by their
    median spacing (the spacing alone is quantised to the model's 20 ms frames)."""
    out = []
    for _, beats, _ in found:
        if len(beats) >= 8:
            k = np.round((beats - beats[0]) / np.median(np.diff(beats)))
            out.append(60 / np.polyfit(k, beats, 1)[0])
    return out


def own_tempo(found) -> float | None:
    """The excerpts' tempo: the median of their own, if at least two have one."""
    tempos = excerpt_tempos(found)
    return float(np.median(tempos)) if len(tempos) >= 2 else None


def is_steady(spread_ms: float, lined: int, excerpts: int, drift_pct: float) -> bool:
    """One straight grid fits: the excerpts agree on the phase (within STEADY_MS), at
    most one fails to line up with the track's tempo, and none has its own tempo more
    than STEADY_TEMPO_PCT from the track's."""
    return bool(spread_ms <= STEADY_MS and lined > excerpts - 2 and drift_pct <= STEADY_TEMPO_PCT)
