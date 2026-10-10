"""Beat / downbeat detection back-ends.

Copied from tempo-lock/tempolock/detectors.py (sibling project in this repo; projects
never import each other, so this is a copy, not a link).

Primary: Beat This! (CPJKU, ISMIR 2024) - transformer beat tracker, robust to tempo drift,
outputs beats and downbeats. Fallback: librosa's dynamic-programming tracker, which is
always installable but assumes a near-constant tempo and knows nothing about downbeats.
"""
from __future__ import annotations

import logging
from dataclasses import dataclass

import numpy as np

log = logging.getLogger(__name__)


@dataclass
class RawBeats:
    beats: np.ndarray  # seconds
    downbeats: np.ndarray  # seconds (may be empty)
    detector: str


_beat_this_model = None
_beat_this_ok: bool | None = None


def beat_this_available() -> bool:
    global _beat_this_ok
    if _beat_this_ok is None:
        try:
            import beat_this.inference  # noqa: F401
            import torch  # noqa: F401
            _beat_this_ok = True
        except Exception as e:  # pragma: no cover - depends on environment
            log.warning("Beat This! unavailable, falling back to librosa: %s", e)
            _beat_this_ok = False
    return _beat_this_ok


def _get_beat_this(checkpoint: str = "final0", dbn: bool = False):
    """Load (once) and cache the Beat This! model. Loading takes several seconds; the
    first ever load also downloads the checkpoint."""
    global _beat_this_model
    if _beat_this_model is None:
        import torch
        from beat_this.inference import Audio2Beats

        device = "cuda" if torch.cuda.is_available() else "cpu"
        log.info("loading Beat This! checkpoint %s on %s", checkpoint, device)
        _beat_this_model = Audio2Beats(checkpoint_path=checkpoint, device=device, dbn=dbn)
    return _beat_this_model


def detect_beat_this(mono: np.ndarray, sr: int, dbn: bool = False, progress=None) -> RawBeats:
    """`progress(fraction)` is called as each chunk of the model's input is done; on a
    CPU that is most of the analysis time."""
    model = _get_beat_this(dbn=dbn)
    mono = mono.astype(np.float32)
    if progress is None:
        beats, downbeats = model(mono, sr)
        return RawBeats(np.asarray(beats, float), np.asarray(downbeats, float), "beat_this")

    from beat_this.inference import split_piece

    spect = model.signal2spect(mono, sr)
    # Same chunking as beat_this's Spect2Frames.spect2frames (chunk_size=1500,
    # border_size=6). If a later version changes it, progress is just off, not wrong.
    total = max(1, len(split_piece(spect, 1500, border_size=6, avoid_short_end=True)[1]))
    inner = model.model
    done = 0

    def counted(x):
        nonlocal done
        out = inner(x)
        done += 1
        progress(min(1.0, done / total))
        return out

    model.model = counted
    try:
        beats, downbeats = model.frames2beats(*model.spect2frames(spect))
    finally:
        model.model = inner
    return RawBeats(np.asarray(beats, float), np.asarray(downbeats, float), "beat_this")


def detect_librosa(mono: np.ndarray, sr: int) -> RawBeats:
    """Fallback tracker. tightness is lowered from librosa's default of 100 so the DP is
    allowed to follow a drifting tempo instead of forcing a fixed one."""
    import librosa

    tempo, beats = librosa.beat.beat_track(y=mono, sr=sr, hop_length=256, tightness=40, units="time", trim=False)
    return RawBeats(np.asarray(beats, float), np.array([], float), "librosa")


def detector_name() -> str:
    return "beat_this" if beat_this_available() else "librosa"


def detect(mono: np.ndarray, sr: int, backend: str = "auto", progress=None) -> RawBeats:
    if backend == "auto":
        backend = detector_name()
    if backend == "beat_this":
        return detect_beat_this(mono, sr, progress=progress)
    if backend == "librosa":
        return detect_librosa(mono, sr)
    raise ValueError(f"unknown detector backend {backend!r}")
