"""Stem separation (issue #2): split a track into drums, bass and vocals with
HTDemucs, on the CPU.

Only three stems are written. The app works out the fourth ("other") as the track
minus those three, so with every stem at full level it plays the original exactly.

The track is separated in chunks, each with a little audio either side for context,
cross-faded where they overlap. That keeps memory down (a whole 4-minute track at
once peaks around 2.4 GB) and gives progress as it goes.

The model's weights are downloaded the first time it's used (about 80 MB, into
torch's hub cache).
"""
from __future__ import annotations

import logging
from pathlib import Path

import numpy as np
import soundfile as sf

log = logging.getLogger(__name__)

MODEL = "htdemucs"
VERSIONS = {"stems": 1}
# The stems written, in this order. ("other" is derived by the app.)
STORED = ("drums", "bass", "vocals")
CHUNK_S = 60.0
# Overlap between chunks, cross-faded.
OVERLAP_S = 2.0

_model = None


def fade(start: int, lo: int, hi: int, n: int, chunk: int, ov: int) -> np.ndarray:
    """The weights for the chunk starting at `start` (separated over `lo..hi`, with
    `ov` frames of context either side) in a track of `n` frames. Each overlap with a
    neighbour is a straight cross-fade over `2·ov` frames centred on the boundary, so
    neighbouring chunks' weights always sum to one, wherever the track ends."""
    t = np.arange(lo, hi, dtype=np.float64)
    w = np.ones(hi - lo)
    if start > 0:  # a chunk before
        w *= np.clip((t - (start - ov)) / (2 * ov), 0.0, 1.0)
    if start + chunk < n:  # a chunk after
        w *= np.clip(((start + chunk + ov) - t) / (2 * ov), 0.0, 1.0)
    return w.astype(np.float32)


# How the stems are written, by the `codec` the app asks for: lossless for a lossless
# track, Vorbis (about 3 MB a minute for all three) for a lossy one. Both come back
# sample for sample, which the app relies on.
CODECS = {
    "flac": ("flac", dict(format="FLAC", subtype="PCM_16")),
    "vorbis": ("ogg", dict(format="OGG", subtype="VORBIS", compression_level=0.5)),
}


def check(req: dict) -> None:
    out = req.get("out_dir")
    if not isinstance(out, str) or not out:
        raise ValueError("stems needs an 'out_dir'")
    if req.get("codec", "flac") not in CODECS:
        raise ValueError(f"stems' 'codec' must be one of {', '.join(CODECS)}")


def _download(progress) -> None:
    """Fetch the model's files into the Hugging Face cache (where demucs looks), with
    progress by bytes. Nothing to do once they're there."""
    import os

    import yaml
    from demucs.hf import DEFAULT_NAMESPACE, hf_repo_name
    from huggingface_hub import hf_hub_download, try_to_load_from_cache
    from tqdm.auto import tqdm

    repo = f"{DEFAULT_NAMESPACE}/{hf_repo_name(MODEL)}"
    with open(hf_hub_download(repo, f"{MODEL}.yaml")) as f:
        sigs = yaml.safe_load(f)["models"]
    files = [f"{s}.safetensors" for s in sigs]
    missing = [f for f in files if not isinstance(try_to_load_from_cache(repo, f), str)]
    if not missing:
        return
    log.info("downloading %s: %s", MODEL, ", ".join(missing))
    progress(0.0)

    class Report(tqdm):
        """tqdm, quiet, reporting each file's share of the download."""

        def __init__(self, *a, **k):
            k["file"] = open(os.devnull, "w")
            super().__init__(*a, **k)

        def update(self, n=1):
            super().update(n)
            if self.total:
                progress((done + self.n / self.total) / len(missing))

    for done, f in enumerate(missing):
        hf_hub_download(repo, f, tqdm_class=Report)
    progress(1.0)


def _get_model(progress_for):
    global _model
    if _model is None:
        from demucs.pretrained import get_model

        _download(progress_for("download"))
        log.info("loading %s", MODEL)
        _model = get_model(MODEL)
        _model.eval()
    return _model


def separate(y: np.ndarray, sr: int, req: dict, progress_for, emit) -> None:
    """Separate `y` (frames, channels) and write the stored stems as 16-bit FLAC into
    `req["out_dir"]`."""
    import torch
    from demucs.apply import apply_model

    model = _get_model(progress_for)
    progress = progress_for("stems")
    progress(0.0)
    if y.ndim == 1:
        y = y[:, None]
    if y.shape[1] == 1:
        y = np.repeat(y, 2, axis=1)
    y = y[:, :2]
    track_n = len(y)
    mix = torch.from_numpy(np.ascontiguousarray(y.T, dtype=np.float32))
    # The model only takes its own rate (44.1 kHz): separate at that, and convert the
    # stems back to the track's, so they line up with it sample for sample. Any error
    # the round trip leaves ends up in "other", which the app derives from the track.
    track_sr, sr = sr, model.samplerate
    if track_sr != sr:
        import julius

        mix = julius.resample_frac(mix, track_sr, sr)
    n = mix.shape[1]
    # As the demucs CLI does: separate a normalised copy, then undo the scaling.
    ref = mix.mean(0)
    mean, std = float(ref.mean()), float(ref.std()) or 1.0
    norm = (mix - mean) / std

    idx = [model.sources.index(s) for s in STORED]
    out = np.zeros((len(STORED), 2, n), dtype=np.float32)
    chunk, ov = int(CHUNK_S * sr), int(OVERLAP_S * sr)
    starts = list(range(0, n, chunk)) or [0]
    for k, start in enumerate(starts):
        lo, hi = max(0, start - ov), min(n, start + chunk + ov)

        # Demucs calls this after each of its ~8 s slices: progress, and a chance to
        # stop (`progress` raises when the job is cancelled).
        def slice_done(d, k=k, lo=lo, hi=hi):
            if d.get("state") == "end":
                seg = float(getattr(model, "segment", None) or 8.0) * sr
                within = min(1.0, (d["segment_offset"] + seg) / (hi - lo))
                progress((k + within) / len(starts))

        with torch.no_grad():
            part = apply_model(model, norm[None, :, lo:hi], shifts=1, split=True,
                               overlap=0.25, progress=False, callback=slice_done)[0]
        out[:, :, lo:hi] += (part[idx] * std + mean).numpy() * fade(start, lo, hi, n, chunk, ov)
        progress((k + 1) / len(starts))

    if track_sr != sr:
        import julius

        out = julius.resample_frac(torch.from_numpy(out), sr, track_sr, output_length=track_n).numpy()
    sr = track_sr

    dest = Path(req["out_dir"])
    dest.mkdir(parents=True, exist_ok=True)
    ext, opts = CODECS[req.get("codec", "flac")]
    files = {}
    for i, name in enumerate(STORED):
        f = dest / f"{name}.{ext}"
        # In blocks: libsndfile's Vorbis encoder crashes on one big write.
        with sf.SoundFile(f, "w", sr, 2, **opts) as o:
            for k in range(0, out.shape[2], 1 << 15):
                o.write(np.clip(out[i, :, k : k + (1 << 15)].T, -1.0, 1.0))
        files[name] = str(f)
    emit("stems", {"model": MODEL, "files": files})
