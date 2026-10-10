"""Spawn the worker as the Rust app will, and drive it over stdin/stdout."""
import json
import subprocess
import sys
from pathlib import Path

import numpy as np
import pytest
import soundfile as sf

WORKER_DIR = Path(__file__).resolve().parent.parent
BPM = 124.0
SR = 44100


def click_track(seconds=30.0, bpm=BPM, sr=SR, lead_in=0.5):
    """Four-on-the-floor kick plus a hi-hat click on the off-beat, mono float32."""
    n = int(seconds * sr)
    y = np.zeros(n, np.float32)
    period = 60.0 / bpm
    kl = int(0.25 * sr)
    tt = np.arange(kl) / sr
    f = 150 * np.exp(-tt * 30) + 45
    kick = (np.sin(2 * np.pi * np.cumsum(f) / sr) * np.exp(-tt / 0.08) * 0.9).astype(np.float32)
    hl = int(0.03 * sr)
    hat = (np.random.default_rng(0).normal(0, 1, hl) * np.exp(-np.arange(hl) / (0.008 * sr)) * 0.2).astype(np.float32)
    t = lead_in
    while t < seconds - 0.3:
        for sig, at in ((kick, t), (hat, t + period / 2)):
            i = int(at * sr)
            m = min(len(sig), n - i)
            if m > 0:
                y[i:i + m] += sig[:m]
        t += period
    return np.clip(y, -1, 1)


class Worker:
    def __init__(self):
        self.p = subprocess.Popen(
            [sys.executable, "-m", "earshot_worker"],
            cwd=WORKER_DIR, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=None, text=True,
        )

    def read(self):
        line = self.p.stdout.readline()
        assert line, "worker closed stdout"
        return json.loads(line)  # every stdout line must be valid protocol JSON

    def ask(self, req):
        """Send a request; return its reply. Progress lines before it go in `self.progress`."""
        self.p.stdin.write(json.dumps(req) + "\n")
        self.p.stdin.flush()
        self.progress = []
        while True:
            msg = self.read()
            if msg.get("type") != "progress":
                return msg
            assert msg["id"] == req.get("id")
            self.progress.append(msg["fraction"])

    def close(self):
        self.p.stdin.close()
        return self.p.wait(timeout=30)


@pytest.fixture(scope="module")
def worker():
    w = Worker()
    assert w.read() == {"type": "ready"}
    yield w
    assert w.close() == 0  # clean exit on EOF


def test_ping(worker):
    assert worker.ask({"id": 1, "type": "ping"}) == {"id": 1, "ok": True, "detector": "beat_this"}


def test_analyse_click_track(worker, tmp_path):
    wav = tmp_path / "click.wav"
    sf.write(wav, click_track(), SR, subtype="FLOAT")
    r = worker.ask({"id": 2, "type": "analyse", "wav": str(wav)})
    p = worker.progress
    assert p and p == sorted(p) and 0 < p[0] and p[-1] <= 1, p
    print("analysis seconds:", r.get("seconds"))
    assert r["ok"] is True and r["id"] == 2
    assert r["detector"] == "beat_this"
    assert set(r) == {"id", "ok", "detector", "beats", "beat_index", "is_downbeat", "median_bpm", "min_bpm", "max_bpm", "seconds"}
    beats = r["beats"]
    assert len(beats) == len(r["beat_index"]) == len(r["is_downbeat"]) > 40
    assert all(isinstance(b, float) for b in beats)
    assert all(isinstance(i, int) for i in r["beat_index"])
    assert all(isinstance(d, bool) for d in r["is_downbeat"])
    assert all(b2 > b1 for b1, b2 in zip(beats, beats[1:]))
    assert abs(r["median_bpm"] - BPM) < 1
    assert np.median(np.diff(beats)) == pytest.approx(60 / BPM, abs=0.01)
    assert isinstance(r["seconds"], float) and r["seconds"] > 0


def test_bad_requests_keep_loop_alive(worker):
    r = worker.ask({"id": 3, "type": "analyse", "wav": "/definitely/not/here.wav"})
    assert r["id"] == 3 and r["ok"] is False and r["error"]
    r = worker.ask({"id": 4, "type": "nonsense"})
    assert r["id"] == 4 and r["ok"] is False
    worker.p.stdin.write("this is not json\n")
    worker.p.stdin.flush()
    r = worker.read()
    assert r["id"] is None and r["ok"] is False
    assert worker.ask({"id": 5, "type": "ping"})["ok"] is True
