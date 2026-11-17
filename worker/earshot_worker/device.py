"""Where torch runs: an NVIDIA GPU (CUDA), else Apple Silicon's GPU (MPS), else the CPU.

`EARSHOT_TORCH_DEVICE` (cpu, mps or cuda) overrides the choice. MPS doesn't have every
operation yet: `PYTORCH_ENABLE_MPS_FALLBACK` runs those on the CPU, and a model that
still fails on the GPU is run again on the CPU, which this process then sticks to.

Imported by the worker before anything imports torch, so the fallback setting is in
place when torch starts.
"""

from __future__ import annotations

import logging
import os

os.environ.setdefault("PYTORCH_ENABLE_MPS_FALLBACK", "1")

log = logging.getLogger(__name__)

_gpu_failed = False


def best() -> str:
    """The device to put a model on now."""
    forced = os.environ.get("EARSHOT_TORCH_DEVICE")
    if forced:
        return forced
    if _gpu_failed:
        return "cpu"
    import torch

    if torch.cuda.is_available():
        return "cuda"
    mps = getattr(torch.backends, "mps", None)
    if mps is not None and mps.is_available():
        return "mps"
    return "cpu"


def on_gpu() -> bool:
    return best() != "cpu"


def gpu_failed(e: Exception) -> None:
    """A model failed on the GPU: use the CPU from now on."""
    global _gpu_failed
    log.warning("torch on %s failed (%s); using the CPU from now on", best(), e)
    _gpu_failed = True
