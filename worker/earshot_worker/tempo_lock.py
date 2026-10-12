"""The tempo lock tool's two jobs: chart a track's tempo, and straighten it to one BPM.

From tempo-lock (sibling project in this repo; projects never import each other):
the analysis is `analysis.analyse` (copied from tempolock/analysis.py), the stretch
`render.render` (tempolock/render.py), the MP3 `audio.write_mp3` (tempolock/audio.py).
This module does what tempolock/server.py did around them: what each step returns,
the output's name and tags, and measuring the straightened audio again.

Each step reports progress with `progress_for(step)(fraction)`, which raises to stop
the job (cancel, deadline); `emit(step, data)` sends a result and `fail(step, error)`
says a step failed while the job goes on.
"""
from __future__ import annotations

import math
import os
from pathlib import Path
from types import SimpleNamespace

import numpy as np

from . import audio
from .analysis import analyse, waveform_peaks
from .render import RubberBandMissing, render, rubberband_binary

# Versions of what each step returns, for the hello (bump when results change).
VERSIONS = {"tempo_lock": 1, "straighten": 1, "measure": 1}
LEVELS = (0.5, 1.0, 2.0)
ENGINES = ("r3", "r2")
# tempo-lock refuses a target this far from the detected tempo: it would change the
# track's speed rather than straighten it.
MAX_CHANGE = 0.3


def _finite(name: str, values) -> None:
    for v in values:
        if not math.isfinite(v):
            raise ValueError(f"analysis produced a non-finite {name}")


def chart(y: np.ndarray, sr: int, progress) -> dict:
    """Everything tempo-lock's page draws for one track: beats (and downbeats, and the
    detections it dropped), tempo over time, sections and tempo changes, stats, and
    the waveform."""
    a = analyse(y, sr, progress=progress)
    if len(a.beats) < 2:
        raise ValueError("found fewer than two beats")
    _finite("beat time", a.beats)
    _finite("bpm", a.bpm_curve)
    return {"analysis": a.to_dict(), "peaks": waveform_peaks(y)}


def tempo_of(beats: np.ndarray, beat_index: np.ndarray) -> float:
    """The median per-beat tempo (as analyse() works out `median_bpm`)."""
    return float(np.median(60.0 / (np.diff(beats) / np.diff(beat_index))))


def check_straighten(p: dict) -> None:
    """Raise ValueError for a straighten request that can't work."""
    bpm = p.get("target_bpm")
    if not isinstance(bpm, (int, float)) or isinstance(bpm, bool) or not 20 <= bpm <= 400:
        raise ValueError("straighten needs a 'target_bpm' from 20 to 400")
    if p.get("level", 1.0) not in LEVELS:
        raise ValueError("'level' must be 0.5, 1 or 2")
    if p.get("engine", "r3") not in ENGINES:
        raise ValueError("'engine' must be 'r3' or 'r2'")
    beats, index = p.get("beats"), p.get("beat_index")
    if not isinstance(beats, list) or not isinstance(index, list) or len(beats) != len(index) or len(beats) < 2:
        raise ValueError("straighten needs 'beats' and 'beat_index': lists of the same length, at least 2")
    b, i = np.asarray(beats, float), np.asarray(index, float)
    if not np.all(np.isfinite(b)) or not np.all(np.diff(b) > 0) or not np.all(np.diff(i) > 0):
        raise ValueError("'beats' and 'beat_index' must be strictly increasing")
    if not isinstance(p.get("out_dir"), str) or not p["out_dir"]:
        raise ValueError("straighten needs an 'out_dir'")
    # Half-time detected (level 2): the track's tempo is twice the detected beats'.
    # (tempo-lock's server divided here, which refuses every half-time target.)
    detected = tempo_of(b, i) * p.get("level", 1.0)
    if abs(bpm / detected - 1) > MAX_CHANGE:
        raise ValueError(
            f"target {bpm:g} BPM is more than 30% away from the detected {detected:.1f} BPM; "
            "that would change the speed of the track rather than straighten it. If the detector "
            "locked onto half- or double-time, change the beat level instead."
        )


# Share of a straighten's time each part takes (the stretch is most of it).
LOADED = 0.05
STRETCHED = 0.8
ENCODED = 0.95


def straighten(y: np.ndarray, sr: int, p: dict, progress_for, emit, fail) -> None:
    """Stretch the audio onto a grid at `target_bpm`, write the MP3, then (unless
    `measure` is false) find the beats in the output again, as tempo-lock's page does,
    to show how straight it came out."""
    bpm, level, engine = float(p["target_bpm"]), float(p.get("level", 1.0)), p.get("engine", "r3")
    if rubberband_binary() is None:
        raise RubberBandMissing("Rubber Band isn't installed: install rubberband-cli (see worker/README.md)")
    progress = progress_for("straighten")
    progress(LOADED)
    beats = SimpleNamespace(beats=np.asarray(p["beats"], float), beat_index=np.asarray(p["beat_index"], int))
    out_dir = Path(p["out_dir"])
    out_dir.mkdir(parents=True, exist_ok=True)
    z, grid = render(
        y, sr, beats, bpm, engine=engine, level=level, workdir=out_dir,
        progress=lambda f: progress(LOADED + (STRETCHED - LOADED) * f),
    )
    progress(STRETCHED)

    # Named and tagged as tempo-lock's download is: "Artist - Title (straightened 124
    # bpm).mp3", the source's tags with the title marked the same way and TBPM set, at
    # the source's bitrate.
    tags_from = p.get("tags_from")
    name = p.get("name") or (Path(tags_from).name if tags_from else "track")
    info = audio.probe(tags_from) if tags_from else {}
    bitrate = audio.mp3_bitrate_for(info)
    out = out_dir / f"{audio.straightened(audio.output_stem(name, info), bpm)}.mp3"
    partial = out_dir / ".straightening.mp3"  # a stopped job leaves no half-written file
    audio.write_mp3(
        partial, z, sr, bitrate_kbps=bitrate, copy_tags_from=tags_from, bpm=bpm,
        title=audio.straightened((info.get("tags") or {}).get("title") or Path(name).stem, bpm),
    )
    os.replace(partial, out)
    progress(ENCODED)
    peaks = waveform_peaks(z)
    progress(1.0)
    emit("straighten", {
        "output": str(out),
        "bpm": bpm,
        "level": level,
        "engine": engine,
        "grid": grid.to_dict(),
        "peaks": peaks,
        "duration": len(z) / sr,
        "bitrate_kbps": bitrate,
    })
    if p.get("measure", True):
        # A failure here is only `failed` for this step: the file is still good. (The
        # job's own stops - cancel, deadline - aren't Exceptions, so they go on up.)
        try:
            measured = analyse(audio.to_mono(z), sr, progress=progress_for("measure"))
        except Exception as e:
            fail("measure", str(e) or type(e).__name__)
            return
        emit("measure", {"analysis": measured.to_dict()})
