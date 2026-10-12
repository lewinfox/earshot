"""Spawn the worker as the Rust app will, and drive it over stdin/stdout."""
import json
import os
import subprocess
import sys
from pathlib import Path

import numpy as np
import pytest
import soundfile as sf

WORKER_DIR = Path(__file__).resolve().parent.parent
BPM = 124.0
SR = 44100
LEAD_IN = 0.5


def click_track(seconds=30.0, bpm=BPM, sr=SR, lead_in=LEAD_IN, hats=True):
    """Four-on-the-floor kick plus (hats) a hi-hat click on the off-beat, mono float32."""
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
        for sig, at in ((kick, t), (hat, t + period / 2))[: 2 if hats else 1]:
            i = int(at * sr)
            m = min(len(sig), n - i)
            if m > 0:
                y[i:i + m] += sig[:m]
        t += period
    return np.clip(y, -1, 1)


TERMINAL = {"done", "error", "cancelled"}


class Worker:
    def __init__(self, env=None):
        env = {**os.environ, "EARSHOT_WORKER_PRIORITY": "inherit", "EARSHOT_WORKER_HEARTBEAT_S": "0.5", **(env or {})}
        self.p = subprocess.Popen(
            [sys.executable, "-m", "earshot_worker"],
            cwd=WORKER_DIR, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=None, text=True, env=env,
        )
        self.seen = []  # every message read, in order (heartbeats too)
        self.waiting = {}  # job id -> its messages not yet collected

    def send(self, req):
        self.p.stdin.write((req if isinstance(req, str) else json.dumps(req)) + "\n")
        self.p.stdin.flush()

    def read(self):
        line = self.p.stdout.readline()
        assert line, "worker closed stdout"
        msg = json.loads(line)  # every stdout line must be valid protocol JSON
        self.seen.append(msg)
        return msg

    def next(self, kind):
        """The next message of this type that isn't about a job (no id, or id null)."""
        while True:
            msg = self.read()
            if msg["type"] == kind and msg.get("id") is None:
                return msg
            if msg.get("id") is not None:
                self.waiting.setdefault(msg["id"], []).append(msg)

    def job(self, job_id):
        """Every message for this job, up to and including its last."""
        out = self.waiting.pop(job_id, [])
        while not out or out[-1]["type"] not in TERMINAL:
            msg = self.read()
            if msg.get("id") == job_id:
                out.append(msg)
            elif "id" in msg:
                self.waiting.setdefault(msg["id"], []).append(msg)
        return out

    def wait_started(self, job_id):
        while not any(m["type"] == "started" and m.get("id") == job_id for m in self.seen):
            msg = self.read()
            if msg.get("id") is not None:
                self.waiting.setdefault(msg["id"], []).append(msg)

    def analyse(self, job_id, wav, mode, priority="background", deadline_s=300):
        self.send(
            {
                "id": job_id,
                "type": "analyse",
                "wav": str(wav),
                "priority": priority,
                "analysers": {"grid": {"mode": mode}},
                "deadline_s": deadline_s,
            }
        )

    def close(self):
        self.p.stdin.close()
        return self.p.wait(timeout=30)


@pytest.fixture(scope="module")
def worker():
    w = Worker()
    w.send({"type": "hello", "protocol": 2})
    hello = w.next("hello")
    assert hello["protocol"] == 2 and hello["analysers"]["grid"] >= 2 and hello["detector"] == "beat_this"
    yield w
    assert w.close() == 0  # clean exit on EOF


@pytest.fixture(scope="module")
def click_wav(tmp_path_factory):
    wav = tmp_path_factory.mktemp("audio") / "click.wav"
    sf.write(wav, click_track(seconds=60.0), SR, subtype="FLOAT")
    return wav


@pytest.fixture(scope="module")
def kick_wav(tmp_path_factory):
    """Kicks only: on the click track, the full model counts the off-beat hats as the
    beats, which says nothing about where its beats sit on a kick."""
    wav = tmp_path_factory.mktemp("audio") / "kicks.wav"
    sf.write(wav, click_track(seconds=60.0, hats=False), SR, subtype="FLOAT")
    return wav


def kick_error(beats):
    """How far each beat is from the nearest kick start (the click track's kicks start
    at LEAD_IN, then every beat)."""
    period = 60 / BPM
    off = (np.asarray(beats) - LEAD_IN) % period
    return np.minimum(off, period - off)


def result(msgs):
    """The grid result from a job's messages, checking the order they came in."""
    kinds = [m["type"] for m in msgs]
    assert kinds[0] == "started" and kinds[-1] == "done", kinds
    assert set(kinds[1:-2]) <= {"progress"} and kinds[-2] == "result", kinds
    p = [m["fraction"] for m in msgs if m["type"] == "progress"]
    assert p and p == sorted(p) and 0 < p[0] and p[-1] <= 1, p
    assert all(m["analyser"] == "grid" for m in msgs if m["type"] == "progress")
    r = msgs[-2]
    assert r["analyser"] == "grid" and r["version"] >= 2
    assert isinstance(msgs[-1]["seconds"], float) and msgs[-1]["seconds"] > 0
    return r["data"]


def test_wrong_protocol_is_refused(worker):
    worker.send({"type": "hello", "protocol": 1})
    err = worker.next("error")
    assert err["id"] is None and "protocol" in err["error"]


def test_heartbeat(worker):
    assert worker.next("heartbeat") == {"type": "heartbeat"}


def test_dynamic_grid_is_on_the_kicks(worker, kick_wav):
    worker.analyse(2, kick_wav, "dynamic")
    data = result(worker.job(2))
    assert data["detector"] == "beat_this"
    g = data["grid"]
    assert set(g) == {"kind", "beats", "beat_index", "is_downbeat", "median_bpm", "min_bpm", "max_bpm"}
    assert data["first_beat"] == g["beats"][0]
    assert g["kind"] == "markers"
    beats = g["beats"]
    assert len(beats) == len(g["beat_index"]) == len(g["is_downbeat"]) > 100
    assert all(isinstance(b, float) for b in beats)
    assert all(isinstance(i, int) for i in g["beat_index"])
    assert all(isinstance(d, bool) for d in g["is_downbeat"])
    assert all(b2 > b1 for b1, b2 in zip(beats, beats[1:]))
    assert abs(g["median_bpm"] - BPM) < 1
    assert np.median(np.diff(beats)) == pytest.approx(60 / BPM, abs=0.01)
    # On the kicks' start, as static grids are, not where Beat This! puts them.
    err = kick_error(beats)
    assert np.median(err) < 0.003 and np.percentile(err, 90) < 0.006, err
    assert data["kick_shift_ms"] < -3  # the onset refinement alone leaves them late


def test_static_grid_is_on_the_kicks(worker, click_wav):
    worker.analyse(3, click_wav, "static")
    data = result(worker.job(3))
    g = data["grid"]
    assert data["detector"] == "static" and g["kind"] == "constant"
    assert set(g) == {"kind", "bpm", "downbeat", "steady", "spread_ms", "drift_pct"}
    assert abs(data["first_beat"] - LEAD_IN) < 0.005, data  # the first kick
    assert g["bpm"] == BPM
    assert g["steady"] is True
    assert kick_error([g["downbeat"]])[0] < 0.005, g


@pytest.fixture(scope="module")
def intro_wav(tmp_path_factory):
    """Silence, then kicks from INTRO s."""
    wav = tmp_path_factory.mktemp("audio") / "intro.wav"
    sf.write(wav, click_track(seconds=60.0, lead_in=INTRO, hats=False), SR, subtype="FLOAT")
    return wav


INTRO = 12.3


@pytest.fixture(scope="module")
def on_the_beat_wav(tmp_path_factory):
    """Kicks from ON_THE_BEAT s: the track opens on the beat."""
    wav = tmp_path_factory.mktemp("audio") / "on_the_beat.wav"
    sf.write(wav, click_track(seconds=60.0, lead_in=ON_THE_BEAT, hats=False), SR, subtype="FLOAT")
    return wav


ON_THE_BEAT = 0.03


def test_first_beat_can_be_at_the_very_start(worker, on_the_beat_wav):
    worker.analyse(6, on_the_beat_wav, "static")
    data = result(worker.job(6))
    assert abs(data["first_beat"] - ON_THE_BEAT) < 0.005, data["first_beat"]


@pytest.mark.parametrize("job_id, mode", [(4, "static"), (5, "dynamic")])
def test_first_beat_is_where_the_beats_start(worker, intro_wav, job_id, mode):
    # A straight grid runs back to the start of the file, over the silence; the first
    # beat is the first kick.
    worker.analyse(job_id, intro_wav, mode)
    data = result(worker.job(job_id))
    assert abs(data["first_beat"] - INTRO) < 0.005, data["first_beat"]
    if mode == "static":
        assert data["grid"]["downbeat"] < INTRO - 1


def started(worker, ids):
    return [m["id"] for m in worker.seen if m["type"] == "started" and m["id"] in ids]


def test_preview_jumps_the_queue_but_waits_its_turn(worker, click_wav):
    worker.analyse(10, click_wav, "static")
    worker.wait_started(10)
    worker.analyse(11, click_wav, "static")
    worker.analyse(12, click_wav, "static", priority="preview")
    for i in (10, 11, 12):
        assert worker.job(i)[-1]["type"] == "done"
    assert started(worker, (10, 11, 12)) == [10, 12, 11]  # 12 doesn't stop 10


def test_deck_job_goes_first_without_stopping_running_work(worker, click_wav):
    # The app gives deck loads their own worker, so a deck job never has to stop
    # background work: it just goes ahead of what's waiting.
    worker.analyse(13, click_wav, "static")
    worker.wait_started(13)
    worker.analyse(14, click_wav, "static")
    worker.analyse(15, click_wav, "static", priority="deck")
    for i in (13, 14, 15):
        assert worker.job(i)[-1]["type"] == "done"
    assert started(worker, (13, 14, 15)) == [13, 15, 14]
    kinds = [m["type"] for m in worker.seen if m.get("id") == 13]
    assert kinds.count("started") == 1 and kinds[-2:] == ["result", "done"]


def test_cancel_queued_and_running(worker, click_wav):
    worker.analyse(20, click_wav, "dynamic")
    worker.analyse(21, click_wav, "dynamic")
    worker.send({"type": "cancel", "id": 21})  # still waiting behind 20
    worker.send({"type": "cancel", "id": 20})  # running
    assert [m["type"] for m in worker.job(21)] == ["cancelled"]
    assert worker.job(20)[-1]["type"] == "cancelled"
    worker.send({"type": "cancel", "id": 999})  # unknown: ignored


def test_deadline(worker, click_wav):
    worker.analyse(30, click_wav, "dynamic", deadline_s=0.001)
    last = worker.job(30)[-1]
    assert last["type"] == "error" and "deadline" in last["error"]


def test_bad_requests_keep_loop_alive(worker, click_wav):
    worker.analyse(40, "/definitely/not/here.wav", "static")
    msgs = worker.job(40)
    assert [m["type"] for m in msgs] == ["started", "error"] and msgs[-1]["error"]
    worker.send({"id": 41, "type": "nonsense"})
    assert worker.job(41) == [{"type": "error", "id": 41, "error": "unknown request type 'nonsense'"}]
    worker.send({"id": 42, "type": "analyse", "wav": str(click_wav), "priority": "background", "deadline_s": 9,
                 "analysers": {"grid": {"mode": "sideways"}}})
    assert worker.job(42)[-1]["type"] == "error"
    worker.send("this is not json")
    err = worker.next("error")
    assert err["id"] is None
    worker.analyse(43, click_wav, "static")
    assert worker.job(43)[-1]["type"] == "done"
