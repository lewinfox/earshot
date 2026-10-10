"""Turn raw detector output into a clean, indexed beat list plus tempo statistics.

Copied from tempo-lock/tempolock/analysis.py (sibling project in this repo; projects
never import each other, so this is a copy, not a link). Trimmed to what the worker
needs: waveform_peaks and tempo-section segmentation were dropped.
"""
from __future__ import annotations

from dataclasses import dataclass, field

import numpy as np

from .detectors import RawBeats, detect


@dataclass
class Analysis:
    sr: int
    duration: float
    detector: str
    beats: np.ndarray  # refined beat times (s)
    beat_index: np.ndarray  # integer beat count for each beat (accounts for missed beats)
    is_downbeat: np.ndarray  # bool per beat
    bpm_curve: np.ndarray  # instantaneous BPM per interval
    median_bpm: float
    min_bpm: float
    max_bpm: float
    dropped_beats: list = field(default_factory=list)  # spurious detections we discarded (s)


def onset_envelope(mono: np.ndarray, sr: int, hop: int = 256) -> tuple[np.ndarray, np.ndarray]:
    import librosa

    env = librosa.onset.onset_strength(y=mono, sr=sr, hop_length=hop)
    times = librosa.frames_to_time(np.arange(len(env)), sr=sr, hop_length=hop)
    return env, times


def refine_to_onsets(beats: np.ndarray, env: np.ndarray, times: np.ndarray, window: float = 0.035) -> np.ndarray:
    """Beat trackers emit frame-quantised times (Beat This!: 20 ms). Snap each beat to the
    strongest onset within +/-window so the grid lands on the actual drum transient."""
    out = np.empty_like(beats)
    for i, b in enumerate(beats):
        lo, hi = np.searchsorted(times, [b - window, b + window])
        if hi <= lo:
            out[i] = b
            continue
        seg = env[lo:hi]
        k = int(np.argmax(seg))
        # only snap when there is a clear transient; a flat window would send argmax to its edge
        if seg[k] > 0 and seg[k] > 1.5 * np.median(seg) + 1e-9:
            out[i] = times[lo + k]
        else:
            out[i] = b
    return out


def clean_beats(beats: np.ndarray, env: np.ndarray, times: np.ndarray) -> tuple[np.ndarray, np.ndarray, list]:
    """Assign an integer beat index to each detection, tolerating both missed beats
    (index jumps by 2+) and spurious extra beats (dropped). Also drops detections that
    sit in near-silence, e.g. a phantom beat at t=0 before the music starts.

    Returns (beats, indices, dropped)."""
    beats = np.sort(np.asarray(beats, float))
    if len(beats) < 2:
        return beats, np.arange(len(beats)), []

    strength = np.array([env[min(len(env) - 1, int(np.searchsorted(times, b)))] for b in beats])
    audible = strength > 0.02 * np.max(strength)
    dropped = beats[~audible].tolist()
    beats = beats[audible]
    if len(beats) < 2:
        return beats, np.arange(len(beats)), dropped

    ibi = np.diff(beats)
    period = float(np.median(ibi))
    kept = [beats[0]]
    index = [0]
    recent = [period] * 4  # rolling local period estimate
    for b in beats[1:]:
        local = float(np.median(recent))
        gap = b - kept[-1]
        if gap < 0.6 * local:
            dropped.append(float(b))  # too soon: spurious double detection
            continue
        steps = max(1, int(round(gap / local)))
        kept.append(b)
        index.append(index[-1] + steps)
        recent.append(gap / steps)
        recent = recent[-8:]
    return np.array(kept), np.array(index, int), dropped


# Share of the time each step takes on a CPU, measured on a 4.5-minute track: the beat
# tracker ~80%, the onset envelope and beat clean-up the rest.
DETECT_SHARE = 0.8
ONSETS_SHARE = 0.1


def analyse(y: np.ndarray, sr: int, backend: str = "auto", raw: RawBeats | None = None, progress=None) -> Analysis:
    """`progress(fraction)`, if given, is called from 0 to 1 as the steps finish."""
    report = progress or (lambda f: None)
    mono = y.mean(axis=1) if y.ndim == 2 else y
    if raw is None:
        raw = detect(mono, sr, backend=backend, progress=lambda f: report(DETECT_SHARE * f))
    report(DETECT_SHARE)
    env, times = onset_envelope(mono, sr)
    report(DETECT_SHARE + ONSETS_SHARE)
    beats = refine_to_onsets(raw.beats, env, times)
    beats, index, dropped = clean_beats(beats, env, times)

    is_down = np.zeros(len(beats), bool)
    if len(raw.downbeats):
        for d in raw.downbeats:
            if len(beats) == 0:
                break
            k = int(np.argmin(np.abs(beats - d)))
            if abs(beats[k] - d) < 0.06:
                is_down[k] = True

    if len(beats) >= 2:
        ibi = np.diff(beats) / np.diff(index)
        bpm = 60.0 / ibi
        median = float(np.median(bpm))
        return Analysis(
            sr=sr,
            duration=len(y) / sr,
            detector=raw.detector,
            beats=beats,
            beat_index=index,
            is_downbeat=is_down,
            bpm_curve=bpm,
            median_bpm=median,
            min_bpm=float(np.percentile(bpm, 2)),
            max_bpm=float(np.percentile(bpm, 98)),
            dropped_beats=dropped,
        )
    return Analysis(sr, len(y) / sr, raw.detector, beats, np.asarray(index, int), is_down, np.array([]), 0.0, 0.0, 0.0, dropped)
