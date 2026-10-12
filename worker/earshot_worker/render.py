"""Re-time audio so every detected beat lands on a fixed-BPM grid.

Copied from tempo-lock/tempolock/render.py (sibling project in this repo; projects never
import each other, so this is a copy, not a link). Changed: `render` reports Rubber
Band's progress and can be stopped part way (`progress` may raise).

The stretch is done by Rubber Band (R3 engine) driven by a time map: for each beat we
give it (source_frame -> target_frame) and it varies the stretch ratio smoothly between
key frames. Pitch is untouched. Audio before the first beat and after the last beat is
left at ratio 1.
"""
from __future__ import annotations

import re
import shutil
import subprocess
import tempfile
import threading
from dataclasses import dataclass
from pathlib import Path

import numpy as np
import soundfile as sf


class RubberBandMissing(RuntimeError):
    pass


def rubberband_binary() -> str | None:
    for name in ("rubberband-r3", "rubberband"):
        if shutil.which(name):
            return name
    return None


@dataclass
class Grid:
    target_bpm: float
    source_beats: np.ndarray  # s, in the original
    target_beats: np.ndarray  # s, in the rendered file
    n_in: int
    n_out: int

    def to_dict(self) -> dict:
        return {
            "target_bpm": self.target_bpm,
            "source_beats": self.source_beats.tolist(),
            "target_beats": self.target_beats.tolist(),
        }


def build_grid(analysis, target_bpm: float, n_in: int, sr: int, level: float = 1.0) -> Grid:
    """`analysis` needs `beats` and `beat_index` (an `analysis.Analysis` has them).

    level says how the detected beats relate to the tempo the user is typing:
    1 = the detector found the beat, 2 = it found half-time (detected beats are two
    target beats apart), 0.5 = it found double-time."""
    period = 60.0 / target_bpm * level
    beats = np.asarray(analysis.beats, float)
    idx = np.asarray(analysis.beat_index, int)
    t0 = beats[0]
    target = t0 + idx * period
    src = np.round(beats * sr).astype(np.int64)
    dst = np.round(target * sr).astype(np.int64)
    tail = n_in - src[-1]
    n_out = int(dst[-1] + tail)
    return Grid(target_bpm, beats, target, n_in, n_out)


def write_timemap(grid: Grid, sr: int, path: Path) -> None:
    """Rubber Band wants strictly increasing (src dst) frame pairs. Do NOT include a
    leading '0 0' pair - R3 divides by zero on it and emits NaN-ratio warnings."""
    src = np.round(grid.source_beats * sr).astype(np.int64)
    dst = np.round(grid.target_beats * sr).astype(np.int64)
    pairs = list(zip(src.tolist(), dst.tolist())) + [(grid.n_in, grid.n_out)]
    clean: list[tuple[int, int]] = []
    last_s, last_d = 0, 0
    for s, d in pairs:
        if s > last_s and d > last_d:
            clean.append((s, d))
            last_s, last_d = s, d
    with open(path, "w") as f:
        for s, d in clean:
            f.write(f"{s} {d}\n")


# How often `render` reports while Rubber Band runs (and so how soon a cancel lands).
POLL_S = 0.2


def _run_rubberband(cmd: list[str], progress) -> None:
    """Run Rubber Band, passing its progress (0..1 over both passes) to `progress`. If
    `progress` raises, Rubber Band is killed and the exception goes on up."""
    proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    state = {"pass": 1, "pct": 0}
    tail: list[str] = []

    def read_stderr():
        # Without -q it prints "Pass 1: Studying...", then "\r0% \r1% ..." per pass.
        buf = b""
        while chunk := proc.stderr.read1(256):
            buf += chunk
            *parts, buf = re.split(rb"[\r\n]", buf)
            for p in parts:
                text = p.decode(errors="replace").strip()
                if not text:
                    continue
                if m := re.fullmatch(r"(\d+)%", text):
                    state["pct"] = int(m.group(1))
                elif m := re.match(r"Pass (\d+)", text):
                    state["pass"], state["pct"] = int(m.group(1)), 0
                else:
                    tail.append(text)
                    del tail[:-20]

    reader = threading.Thread(target=read_stderr, daemon=True)
    reader.start()
    done = 0.0
    try:
        while proc.poll() is None:
            try:
                proc.wait(timeout=POLL_S)
            except subprocess.TimeoutExpired:
                pass
            # It starts again from pass 1 when its output clips (at a lower gain), so
            # report how far it has ever got: progress never goes backwards.
            done = max(done, min(1.0, ((state["pass"] - 1) + state["pct"] / 100) / 2))
            progress(done)
    except BaseException:
        proc.kill()
        proc.wait()
        raise
    finally:
        reader.join(timeout=5)
    if proc.returncode != 0:
        raise RuntimeError(f"rubberband failed: {' / '.join(tail[-3:]) or proc.returncode}")


def render(
    y: np.ndarray,
    sr: int,
    analysis,
    target_bpm: float,
    engine: str = "r3",
    level: float = 1.0,
    workdir: str | Path | None = None,
    progress=None,
) -> tuple[np.ndarray, Grid]:
    """Return (stretched audio shaped (n, ch), grid). `progress(fraction)`, if given, is
    called while Rubber Band runs; if it raises, Rubber Band is stopped."""
    if len(analysis.beats) < 2:
        raise ValueError("need at least two beats to build a grid")
    binary = rubberband_binary()
    if binary is None:
        raise RubberBandMissing("rubberband CLI not found; install rubberband-cli (apt) or brew install rubberband")
    if y.ndim == 1:
        y = y[:, None]

    grid = build_grid(analysis, target_bpm, len(y), sr, level=level)
    with tempfile.TemporaryDirectory(dir=workdir) as td:
        td = Path(td)
        src_wav, out_wav, map_txt = td / "in.wav", td / "out.wav", td / "map.txt"
        sf.write(str(src_wav), y.astype(np.float32), sr, subtype="FLOAT")
        write_timemap(grid, sr, map_txt)
        cmd = [
            binary,
            "-3" if engine == "r3" else "-2",
            "-M",
            str(map_txt),
            "-D",
            f"{grid.n_out / sr:.6f}",
            str(src_wav),
            str(out_wav),
        ]
        _run_rubberband(cmd, progress or (lambda f: None))
        z, _ = sf.read(str(out_wav), always_2d=True, dtype="float32")
    return z, grid
