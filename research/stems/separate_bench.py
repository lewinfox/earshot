"""Stems Phase 0 measurements (issue #2), separation side.

Separates the engine's own decode of a track (from engine/examples/stems_bench.rs
--wav) with HTDemucs on CPU and reports:

- speed: seconds of compute per second of audio (the real-time factor);
- peak memory of the process;
- alignment: whether the stems come back the same length as the input, and how far
  the four stems' sum is from the original;
- the "derived other" (original minus drums, bass, vocals) against the model's own;
- disk size of the three stored stems as 16-bit FLAC and as Opus 160 kbps.

Writes the stems, plus the derived other, to OUT_DIR for listening.

From worker/ (demucs is not a worker dependency yet, so it's layered on for the run):

    uv run --with demucs python ../research/stems/separate_bench.py TRACK.wav OUT_DIR \
        [--model htdemucs] [--seconds N] [--threads N]
"""
from __future__ import annotations

import argparse
import resource
import shutil
import subprocess
import time
from pathlib import Path

import numpy as np
import soundfile as sf
import torch
from demucs.apply import apply_model
from demucs.pretrained import get_model


def db(x: np.ndarray, ref: np.ndarray) -> float:
    """Level of `x` relative to `ref`, in dB (more negative is smaller)."""
    return 10 * np.log10((np.mean(x**2) + 1e-20) / (np.mean(ref**2) + 1e-20))


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("wav")
    p.add_argument("out_dir")
    p.add_argument("--model", default="htdemucs")
    p.add_argument("--seconds", type=float, help="only this much, from a minute in")
    p.add_argument("--threads", type=int, help="torch threads (default: torch's choice)")
    a = p.parse_args()
    if a.threads:
        torch.set_num_threads(a.threads)
    out = Path(a.out_dir)
    out.mkdir(parents=True, exist_ok=True)

    y, sr = sf.read(a.wav, dtype="float32", always_2d=True)  # (n, 2)
    if a.seconds:
        start = min(60 * sr, max(0, len(y) - int(a.seconds * sr)))
        y = y[start : start + int(a.seconds * sr)]
    dur = len(y) / sr

    t = time.perf_counter()
    model = get_model(a.model)
    model.eval()
    print(f"model {a.model}: loaded in {time.perf_counter() - t:.1f} s; sources {model.sources}; "
          f"model rate {model.samplerate} Hz, track rate {sr} Hz; torch threads {torch.get_num_threads()}")
    if sr != model.samplerate:
        print("WARNING: rates differ; this script does not resample, so results are off")

    mix = torch.from_numpy(y.T.copy())  # (2, n)
    # As the demucs CLI does: separate a normalised copy, then undo the scaling.
    ref = mix.mean(0)
    mean, std = ref.mean(), ref.std()
    t = time.perf_counter()
    with torch.no_grad():
        stems = apply_model(model, ((mix - mean) / std)[None], shifts=1, split=True,
                            overlap=0.25, progress=False)[0]
    took = time.perf_counter() - t
    stems = (stems * std + mean).numpy()  # (sources, 2, n)
    rss_gb = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / 1e6
    print(f"separated {dur:.1f} s of audio in {took:.1f} s: {took / dur:.2f} s per audio-second "
          f"({took / dur * 60:.0f} s per minute); peak memory {rss_gb:.2f} GB")

    s = dict(zip(model.sources, stems))
    print(f"length: input {mix.shape[1]} frames, stems {stems.shape[2]} frames "
          f"({'same' if stems.shape[2] == mix.shape[1] else 'DIFFERENT'})")
    m = y.T
    print(f"sum of all four stems vs original: error at {db(stems.sum(0) - m, m):.1f} dB")
    derived = m - s["drums"] - s["bass"] - s["vocals"]
    print(f"derived other vs model's other: difference at {db(derived - s['other'], s['other']):.1f} dB "
          "(relative to the model's other)")

    for name, x in s.items():
        sf.write(out / f"{name}.wav", x.T, sr, subtype="FLOAT")
    sf.write(out / "other_derived.wav", derived.T, sr, subtype="FLOAT")

    flac = sum(_flac_bytes(out, name, s[name], sr) for name in ("drums", "bass", "vocals"))
    print(f"3 stored stems, 16-bit FLAC: {flac / 1e6:.1f} MB ({flac / 1e6 / (dur / 60):.1f} MB per minute)")
    if shutil.which("ffmpeg"):
        opus = sum(_opus_bytes(out, name) for name in ("drums", "bass", "vocals"))
        print(f"3 stored stems, Opus 160 kbps: {opus / 1e6:.1f} MB ({opus / 1e6 / (dur / 60):.1f} MB per minute)")
    print(f"stems written to {out}")


def _flac_bytes(out: Path, name: str, x: np.ndarray, sr: int) -> int:
    f = out / f"{name}.flac"
    sf.write(f, np.clip(x.T, -1, 1), sr, subtype="PCM_16")
    return f.stat().st_size


def _opus_bytes(out: Path, name: str) -> int:
    f = out / f"{name}.opus"
    subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-i", str(out / f"{name}.wav"),
                    "-c:a", "libopus", "-b:a", "160k", str(f)], check=True, stdin=subprocess.DEVNULL)
    return f.stat().st_size


if __name__ == "__main__":
    main()
