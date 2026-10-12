"""The worker process: protocol v2 (see ../README.md) over stdin/stdout.

One long-lived process serves every job, so each model is loaded once. The main
thread reads requests; jobs wait in a priority queue (a track just loaded onto a deck
goes ahead of background work) and one runner thread works through them. A third
thread sends a heartbeat, so the app can tell a hung process from a slow job.

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

    One process serves urgent and background jobs alike, and an unprivileged process
    can't leave the idle class again, so urgent jobs differ only in going first.
    EARSHOT_WORKER_PRIORITY=inherit keeps the priority the process was started with
    (tests, or a future second process just for urgent jobs)."""
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

import heapq  # noqa: E402
import itertools  # noqa: E402
import json  # noqa: E402
import logging  # noqa: E402
import math  # noqa: E402
import threading  # noqa: E402
import time  # noqa: E402
import traceback  # noqa: E402
from dataclasses import dataclass, field  # noqa: E402

import numpy as np  # noqa: E402
import soundfile as sf  # noqa: E402

from .analysis import analyse  # noqa: E402
from .detectors import detector_name, loaded_models  # noqa: E402
from .static_tempo import analyse_static  # noqa: E402

log = logging.getLogger("earshot_worker")

PROTOCOL = 2
# Each analyser's version: bump it when its results change, so the app can find
# results made by an older one. grid 2: dynamic grids snapped to the kicks.
ANALYSERS = {"grid": 2}
# Lower runs first; equal priorities run in the order they arrived.
PRIORITIES = {"deck": 0, "preview": 1, "background": 2}
HEARTBEAT_S = float(os.environ.get("EARSHOT_WORKER_HEARTBEAT_S", "2"))

_send_lock = threading.Lock()


def send(msg: dict) -> None:
    line = json.dumps(msg, allow_nan=False) + "\n"
    with _send_lock:
        try:
            _proto.write(line)
            _proto.flush()
        except BrokenPipeError:
            # The app has gone (it quit mid-analysis): nothing to report to, so leave
            # quietly, without flushing the dead pipe again on the way out.
            os._exit(0)


class Cancelled(Exception):
    pass


class DeadlineExceeded(Exception):
    pass


@dataclass
class Job:
    id: int
    priority: int
    wav: str
    analysers: dict  # name -> its parameters
    deadline_s: float
    cancel: threading.Event = field(default_factory=threading.Event)


class Queue:
    """Jobs waiting, by priority then arrival, and the one running."""

    def __init__(self):
        self.cond = threading.Condition()
        self.heap: list = []
        self.order = itertools.count()
        self.queued: dict[int, Job] = {}
        self.running: Job | None = None
        self.closed = False

    def submit(self, job: Job) -> None:
        with self.cond:
            if job.id in self.queued or (self.running and self.running.id == job.id):
                raise ValueError(f"job {job.id} is already queued")
            self.queued[job.id] = job
            heapq.heappush(self.heap, (job.priority, next(self.order), job.id))
            self.cond.notify()

    def cancel(self, job_id: int) -> None:
        """A queued job is dropped now; a running one stops at its next progress
        report. Either way its last message is `cancelled`. Unknown ids (already
        finished) are ignored."""
        with self.cond:
            if self.queued.pop(job_id, None) is not None:
                send({"id": job_id, "type": "cancelled"})
            elif self.running and self.running.id == job_id:
                self.running.cancel.set()

    def take(self) -> Job | None:
        """The next job (None once closed), now marked as running."""
        with self.cond:
            while True:
                while self.heap:
                    _, _, job_id = heapq.heappop(self.heap)
                    job = self.queued.pop(job_id, None)
                    if job is not None:  # else cancelled while it waited
                        self.running = job
                        return job
                if self.closed:
                    return None
                self.cond.wait()

    def finished(self) -> None:
        with self.cond:
            self.running = None

    def close(self) -> None:
        """No more requests: drop what's waiting and stop what's running."""
        with self.cond:
            self.closed = True
            self.queued.clear()
            self.heap.clear()
            if self.running:
                self.running.cancel.set()
            self.cond.notify_all()


def _finite(name: str, values: list[float]) -> None:
    for v in values:
        if not math.isfinite(v):
            raise ValueError(f"analysis produced a non-finite {name}")


def grid(y: np.ndarray, sr: int, params: dict, progress) -> dict:
    """The `grid` analyser: a straight grid (mode static) or one line per detected
    beat (mode dynamic), both snapped to the kicks."""
    mode = params["mode"]
    if mode == "static":
        r = analyse_static(y, sr, progress=progress)
        _finite("result", [r["bpm"], r["downbeat"], r["spread_ms"], r["drift_pct"], r["kick_shift"]])
        return {
            "detector": "static",
            "kick_shift_ms": r["kick_shift"] * 1000,
            "grid": {
                "kind": "constant",
                "bpm": r["bpm"],
                "downbeat": r["downbeat"],
                "steady": r["steady"],
                "spread_ms": r["spread_ms"],
                "drift_pct": r["drift_pct"],
            },
        }
    a = analyse(y, sr, progress=progress)
    beats = [float(b) for b in a.beats]
    stats = {"median_bpm": float(a.median_bpm), "min_bpm": float(a.min_bpm), "max_bpm": float(a.max_bpm)}
    _finite("beat time", beats)
    _finite("bpm", [*stats.values(), a.kick_shift])
    return {
        "detector": a.detector,
        "kick_shift_ms": float(a.kick_shift) * 1000,
        "grid": {
            "kind": "markers",
            "beats": beats,
            "beat_index": [int(i) for i in a.beat_index],
            "is_downbeat": [bool(d) for d in a.is_downbeat],
            **stats,
        },
    }


RUN = {"grid": grid}


def check_params(name: str, params) -> None:
    if not isinstance(params, dict):
        raise ValueError(f"analyser {name!r}: parameters must be an object")
    if name == "grid" and params.get("mode") not in ("static", "dynamic"):
        raise ValueError("analyser 'grid' needs a mode: 'static' or 'dynamic'")


def parse_job(req: dict) -> Job:
    job_id = req.get("id")
    if not isinstance(job_id, int) or isinstance(job_id, bool) or job_id < 0:
        raise ValueError("analyse needs an 'id': a non-negative integer")
    wav = req.get("wav")
    if not isinstance(wav, str) or not wav:
        raise ValueError("analyse needs a 'wav' path")
    priority = req.get("priority")
    if priority not in PRIORITIES:
        raise ValueError(f"'priority' must be one of {', '.join(PRIORITIES)}")
    deadline = req.get("deadline_s")
    if not isinstance(deadline, (int, float)) or isinstance(deadline, bool) or not deadline > 0:
        raise ValueError("analyse needs a 'deadline_s' above 0")
    analysers = req.get("analysers")
    if not isinstance(analysers, dict) or not analysers:
        raise ValueError("analyse needs 'analysers': an object of name -> parameters")
    for name, params in analysers.items():
        if name not in RUN:
            raise ValueError(f"unknown analyser {name!r}")
        check_params(name, params)
    return Job(job_id, PRIORITIES[priority], wav, analysers, float(deadline))


def run_job(job: Job) -> None:
    """Run every analyser the job asks for, one result message each, then `done`.
    Ends with `cancelled` or `error` instead if the job is cancelled, runs past its
    deadline or can't read its audio."""
    t0 = time.perf_counter()
    send({"id": job.id, "type": "started"})

    def progress_for(name: str):
        def progress(f: float) -> None:
            if job.cancel.is_set():
                raise Cancelled
            if time.perf_counter() - t0 > job.deadline_s:
                raise DeadlineExceeded
            send({"id": job.id, "type": "progress", "analyser": name, "fraction": round(float(f), 3)})

        return progress

    try:
        y, sr = sf.read(job.wav, dtype="float32", always_2d=False)
        if y.size == 0:
            raise ValueError(f"{job.wav}: no audio")
        for name, params in job.analysers.items():
            try:
                data = RUN[name](y, int(sr), params, progress_for(name))
            except (Cancelled, DeadlineExceeded):
                raise
            except Exception as e:
                traceback.print_exc(file=sys.stderr)
                send({"id": job.id, "type": "failed", "analyser": name, "error": str(e) or type(e).__name__})
                continue
            send({"id": job.id, "type": "result", "analyser": name, "version": ANALYSERS[name], "data": data})
        send({"id": job.id, "type": "done", "seconds": float(time.perf_counter() - t0)})
    except Cancelled:
        send({"id": job.id, "type": "cancelled"})
    except DeadlineExceeded:
        send({"id": job.id, "type": "error", "error": f"took longer than its {job.deadline_s:g} s deadline"})
    except Exception as e:
        traceback.print_exc(file=sys.stderr)
        send({"id": job.id, "type": "error", "error": str(e) or type(e).__name__})


def runner(queue: Queue) -> None:
    while (job := queue.take()) is not None:
        try:
            run_job(job)
        finally:
            queue.finished()


def heartbeat() -> None:
    while True:
        time.sleep(HEARTBEAT_S)
        send({"type": "heartbeat"})


def handle(req: dict, queue: Queue) -> None:
    kind = req.get("type")
    if kind == "hello":
        if req.get("protocol") != PROTOCOL:
            raise ValueError(f"protocol {req.get('protocol')!r} not supported: this worker speaks {PROTOCOL}")
        send(
            {
                "type": "hello",
                "protocol": PROTOCOL,
                "analysers": ANALYSERS,
                "detector": detector_name(),
                "models_loaded": loaded_models(),
            }
        )
    elif kind == "analyse":
        queue.submit(parse_job(req))
    elif kind == "cancel":
        job_id = req.get("id")
        if not isinstance(job_id, int):
            raise ValueError("cancel needs the job's 'id'")
        queue.cancel(job_id)
    else:
        raise ValueError(f"unknown request type {kind!r}")


def main() -> int:
    logging.basicConfig(level=logging.INFO, stream=sys.stderr, format="worker: %(levelname)s %(name)s: %(message)s")
    detector_name()  # import torch / beat_this now so hello and the first job are quick
    queue = Queue()
    run = threading.Thread(target=runner, args=(queue,), name="earshot-worker-runner")
    run.start()
    threading.Thread(target=heartbeat, name="earshot-worker-heartbeat", daemon=True).start()
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
            handle(req, queue)
        except Exception as e:
            traceback.print_exc(file=sys.stderr)
            if not isinstance(req_id, int) or isinstance(req_id, bool):
                req_id = None
            send({"type": "error", "id": req_id, "error": str(e) or type(e).__name__})
    # stdin closed: the app has gone. Stop the running job at its next progress report.
    queue.close()
    run.join(timeout=10)
    if run.is_alive():  # stuck in native code: don't wait for it
        _proto.flush()
        os._exit(0)
    return 0


if __name__ == "__main__":
    sys.exit(main())
