"""JSON-lines worker loop: one request per stdin line, one reply per stdout line.

stdout carries protocol lines only. Everything else (print, logging, warnings, tqdm,
torch/native library noise) goes to stderr: at startup we keep a private handle to the
real stdout and point both sys.stdout and file descriptor 1 at stderr.
"""
from __future__ import annotations

import os
import sys


def _claim_stdout():
    """Return a text stream on the real stdout and send everything else to stderr."""
    sys.stdout.flush()
    proto_fd = os.dup(1)
    os.dup2(2, 1)  # native code writing to fd 1 now lands on stderr
    sys.stdout = sys.stderr
    return os.fdopen(proto_fd, "w", encoding="utf-8", buffering=1)


_proto = _claim_stdout()


def _lowest_priority():
    """Analysis must never take CPU from playback or the UI. Linux's idle scheduling
    class only runs us when nothing else wants the CPU; elsewhere, nice 19. Done
    before torch starts its threads, so they inherit it.

    The app sets EARSHOT_WORKER_PRIORITY=inherit for the worker that analyses a track
    just loaded onto a deck: that one keeps the (still lowered) priority it was
    started with."""
    if os.environ.get("EARSHOT_WORKER_PRIORITY") == "inherit":
        return
    try:
        os.sched_setscheduler(0, os.SCHED_IDLE, os.sched_param(0))
    except (AttributeError, OSError):
        try:
            os.nice(19)
        except OSError:
            pass


_lowest_priority()

import json  # noqa: E402
import logging  # noqa: E402
import math  # noqa: E402
import time  # noqa: E402
import traceback  # noqa: E402

import numpy as np  # noqa: E402
import soundfile as sf  # noqa: E402

from .analysis import analyse  # noqa: E402
from .detectors import detector_name  # noqa: E402
from .static_tempo import analyse_static  # noqa: E402

log = logging.getLogger("earshot_worker")


def send(msg: dict) -> None:
    try:
        _proto.write(json.dumps(msg, allow_nan=False) + "\n")
        _proto.flush()
    except BrokenPipeError:
        # The app has gone (it quit mid-analysis): nothing to report to, so leave
        # quietly, without flushing the dead pipe again on the way out.
        os._exit(0)


def _finite(name: str, values: list[float]) -> None:
    for v in values:
        if not math.isfinite(v):
            raise ValueError(f"analysis produced a non-finite {name}")


def handle_analyse(req: dict) -> dict:
    path = req.get("wav")
    if not isinstance(path, str) or not path:
        raise ValueError("analyse needs a 'wav' path")
    t0 = time.perf_counter()
    y, sr = sf.read(path, dtype="float32", always_2d=False)
    if y.size == 0:
        raise ValueError(f"{path}: no audio")
    req_id = req.get("id")
    a = analyse(y, int(sr), progress=lambda f: send({"id": req_id, "type": "progress", "fraction": round(float(f), 3)}))

    beats = [float(b) for b in a.beats]
    beat_index = [int(i) for i in a.beat_index]
    is_downbeat = [bool(d) for d in a.is_downbeat]
    stats = {"median_bpm": float(a.median_bpm), "min_bpm": float(a.min_bpm), "max_bpm": float(a.max_bpm)}
    _finite("beat time", beats)
    _finite("bpm", list(stats.values()))
    return {
        "ok": True,
        "detector": a.detector,
        "beats": beats,
        "beat_index": beat_index,
        "is_downbeat": is_downbeat,
        **stats,
        "seconds": float(time.perf_counter() - t0),
    }


def handle_analyse_static(req: dict) -> dict:
    path = req.get("wav")
    if not isinstance(path, str) or not path:
        raise ValueError("analyse_static needs a 'wav' path")
    t0 = time.perf_counter()
    y, sr = sf.read(path, dtype="float32", always_2d=False)
    if y.size == 0:
        raise ValueError(f"{path}: no audio")
    req_id = req.get("id")
    r = analyse_static(y, int(sr), progress=lambda f: send({"id": req_id, "type": "progress", "fraction": round(float(f), 3)}))
    _finite("result", [r["bpm"], r["beat"], r["downbeat"], r["spread_ms"], r["drift_pct"]])
    return {"ok": True, "detector": "static", **r, "seconds": float(time.perf_counter() - t0)}


def handle(req: dict) -> dict:
    kind = req.get("type")
    if kind == "ping":
        return {"ok": True, "detector": detector_name()}
    if kind == "analyse":
        return handle_analyse(req)
    if kind == "analyse_static":
        return handle_analyse_static(req)
    raise ValueError(f"unknown request type {kind!r}")


def main() -> int:
    logging.basicConfig(level=logging.INFO, stream=sys.stderr, format="worker: %(levelname)s %(name)s: %(message)s")
    detector_name()  # import torch / beat_this now so the first ping is fast
    send({"type": "ready"})
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        req_id = None
        try:
            req = json.loads(line)
            if not isinstance(req, dict):
                raise ValueError("request must be a JSON object")
            req_id = req.get("id")
            reply = handle(req)
            send({"id": req_id, **reply})
        except Exception as e:
            traceback.print_exc(file=sys.stderr)
            msg = str(e) or type(e).__name__
            try:
                send({"id": req_id, "ok": False, "error": msg})
            except (TypeError, ValueError):  # id was not JSON-serialisable
                send({"id": None, "ok": False, "error": msg})
    return 0


if __name__ == "__main__":
    sys.exit(main())
