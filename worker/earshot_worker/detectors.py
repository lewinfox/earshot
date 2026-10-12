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


# Loaded Beat This! models, by checkpoint. Whole-track (dynamic) analysis uses the full
# "final0" (~20M parameters): the small one (~2M, ~1.6x faster) matched it on real
# tracks but doubled the tempo of a kick + off-beat hat test track. Static analysis
# uses the small one on short excerpts and checks its tempo against its own.
_beat_this_models: dict = {}
_beat_this_ok: bool | None = None
CHECKPOINT = "final0"
SMALL_CHECKPOINT = "small0"


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


def get_beat_this(checkpoint: str = CHECKPOINT, dbn: bool = False):
    """Load (once) and cache a Beat This! model. Loading takes a second or two; the
    first ever load also downloads the checkpoint."""
    key = (checkpoint, dbn)
    if key not in _beat_this_models:
        import torch
        from beat_this.inference import Audio2Beats

        device = "cuda" if torch.cuda.is_available() else "cpu"
        log.info("loading Beat This! checkpoint %s on %s", checkpoint, device)
        _beat_this_models[key] = Audio2Beats(checkpoint_path=checkpoint, device=device, dbn=dbn)
    return _beat_this_models[key]


def loaded_models() -> list[str]:
    """Names of the models loaded so far (each is loaded once, on first use)."""
    return sorted(f"beat_this:{c}" + (":dbn" if dbn else "") for c, dbn in _beat_this_models)


def detect_beat_this(mono: np.ndarray, sr: int, dbn: bool = False, progress=None) -> RawBeats:
    """`progress(fraction)` is called as each chunk of the model's input is done; on a
    CPU that is most of the analysis time."""
    model = get_beat_this(dbn=dbn)
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
