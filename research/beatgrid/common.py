"""Shared paths and helpers for the beat-grid study."""
from __future__ import annotations

import os
import sqlite3
from pathlib import Path

import numpy as np

# All data (DB copy, decoded WAVs, model outputs, images) lives outside the repo.
DATA = Path(os.environ.get("BG_DATA", "/tmp/claude-1000/-home-lewin-git-earshot/615b3b7a-7ddd-4410-b7a6-a8f3767fcbb6/scratchpad/bg/data"))
DB = DATA.parent / "earshot.db"  # a copy of the library DB, never the real one
WAV = DATA / "wav"
BT = DATA / "bt"
HERE = Path(__file__).resolve().parent


def db():
    return sqlite3.connect(f"file:{DB}?mode=ro", uri=True)


def sample_ids() -> list[int]:
    return [int(line.split("\t")[0]) for line in (DATA / "sample.tsv").read_text().splitlines() if line]


def wav_path(tid: int) -> Path:
    return WAV / f"{tid}.wav"


def load_wav(tid: int) -> tuple[np.ndarray, int]:
    import soundfile as sf

    y, sr = sf.read(wav_path(tid), dtype="float32", always_2d=False)
    return y, int(sr)


def title(tid: int) -> str:
    with db() as c:
        t, a, p = c.execute("select title, artist, path from tracks where id=?", (tid,)).fetchone()
    return f"{a} - {t}" if t else Path(p).stem
