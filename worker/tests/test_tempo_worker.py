"""The tempo lock jobs (`tempo_lock`, `straighten`) over the protocol, as the app runs them.

Rubber Band is a stand-in here (`FAKE_RUBBERBAND`, first on PATH): it follows the time
map like the real one, prints progress the same way and can be slowed down to test
cancelling. tests/test_tempo_render.py checks the real one's output."""
import os
import stat
import subprocess
import sys

import numpy as np
import pytest
import soundfile as sf
from synth import live_drums
from test_worker import Worker

FAKE_RUBBERBAND = """#!{python}
import os, sys, time
import numpy as np, soundfile as sf
args = sys.argv[1:]
src, out = args[-2], args[-1]
seconds = float(args[args.index("-D") + 1])
pairs = [tuple(map(int, l.split())) for l in open(args[args.index("-M") + 1]) if l.strip()]
delay = float(os.environ.get("FAKE_RUBBERBAND_SECONDS", "0"))
y, sr = sf.read(src, always_2d=True, dtype="float32")
for p in (1, 2):
    sys.stderr.write(f"Pass {{p}}: working...\\n")
    for pct in range(0, 100, 5):
        sys.stderr.write(f"\\r{{pct}}% "); sys.stderr.flush(); time.sleep(delay / 40)
n = int(round(seconds * sr))
s = [0] + [a for a, _ in pairs]
d = [0] + [b for _, b in pairs]
at = np.clip(np.interp(np.arange(n), d, s), 0, len(y) - 1).astype(int)
sf.write(out, y[at], sr, subtype="FLOAT")
"""


@pytest.fixture(scope="module")
def fake_bin(tmp_path_factory):
    d = tmp_path_factory.mktemp("bin")
    rb = d / "rubberband"
    rb.write_text(FAKE_RUBBERBAND.format(python=sys.executable))
    rb.chmod(rb.stat().st_mode | stat.S_IEXEC)
    return d


@pytest.fixture(scope="module")
def worker(fake_bin):
    path = f"{fake_bin}{os.pathsep}{os.environ.get('PATH', '')}"
    w = Worker({"PATH": path, "FAKE_RUBBERBAND_SECONDS": "6"})
    w.send({"type": "hello", "protocol": 2})
    hello = w.next("hello")
    assert hello["steps"] == {"tempo_lock": 1, "straighten": 1, "measure": 1}
    yield w
    assert w.close() == 0


@pytest.fixture(scope="module")
def drums(tmp_path_factory):
    """A drifting live-drum track as the app sends it (float WAV, both channels), and
    the MP3 it came from, with tags."""
    d = tmp_path_factory.mktemp("audio")
    y, sr, _ = live_drums(duration=30.0, drift_bpm=5.0)
    wav = d / "decoded.wav"
    sf.write(wav, y, sr, subtype="FLOAT")
    mp3 = d / "Slow Burn.mp3"
    subprocess.run(
        ["ffmpeg", "-y", "-loglevel", "error", "-i", str(wav), "-c:a", "libmp3lame", "-b:a", "128k",
         "-metadata", "title=Slow Burn", "-metadata", "artist=The Metronomes", str(mp3)],
        check=True,
    )
    return wav, mp3


def steps(msgs):
    """{step: data} from a job's results, after checking the job ran and finished."""
    kinds = [m["type"] for m in msgs]
    assert kinds[0] == "started" and kinds[-1] == "done", kinds
    return {m["analyser"]: m["data"] for m in msgs if m["type"] == "result"}


def progress(msgs, step):
    p = [m["fraction"] for m in msgs if m["type"] == "progress" and m["analyser"] == step]
    assert p and p == sorted(p) and p[-1] <= 1, p
    return p


@pytest.fixture(scope="module")
def chart(worker, drums):
    wav, _ = drums
    worker.send({"id": 1, "type": "tempo_lock", "wav": str(wav), "priority": "background", "deadline_s": 300})
    msgs = worker.job(1)
    progress(msgs, "tempo_lock")
    return steps(msgs)["tempo_lock"]


def test_chart_has_what_the_charts_draw(chart):
    a = chart["analysis"]
    for k in ("beats", "beat_index", "is_downbeat", "bpm_times", "bpm_curve", "median_bpm", "min_bpm",
              "max_bpm", "suggested_bpm", "dropped_beats", "sections", "tempo_changes", "duration", "detector"):
        assert k in a, k
    assert len(a["beats"]) == len(a["beat_index"]) == len(a["is_downbeat"]) == len(a["bpm_curve"]) + 1
    assert len(a["bpm_times"]) == len(a["bpm_curve"])
    assert abs(a["median_bpm"] - 120) < 2 and a["suggested_bpm"] == 120
    assert a["max_bpm"] - a["min_bpm"] > 5


def straighten(worker, job_id, drums, chart, out_dir, **extra):
    wav, mp3 = drums
    a = chart["analysis"]
    worker.send({
        "id": job_id, "type": "straighten", "wav": str(wav), "priority": "background", "deadline_s": 300,
        "target_bpm": 120, "beats": a["beats"], "beat_index": a["beat_index"],
        "out_dir": str(out_dir), "tags_from": str(mp3), **extra,
    })


def test_straighten_writes_the_named_tagged_mp3_then_measures_it(worker, drums, chart, tmp_path):
    straighten(worker, 2, drums, chart, tmp_path)
    msgs = worker.job(2)
    done = steps(msgs)
    p = progress(msgs, "straighten")
    assert len(p) > 3 and p[-1] == 1, p  # Rubber Band's own progress came through
    progress(msgs, "measure")
    # the MP3 is reported before it is measured, so it can be saved straight away
    order = [m["analyser"] for m in msgs if m["type"] == "result"]
    assert order == ["straighten", "measure"]

    s = done["straighten"]
    out = tmp_path / "The Metronomes - Slow Burn (straightened 120 bpm).mp3"
    assert s["output"] == str(out) and out.exists()
    assert sorted(p.name for p in tmp_path.iterdir()) == [out.name]  # nothing left over
    assert s["bpm"] == 120 and s["level"] == 1 and s["engine"] == "r3"
    assert len(s["grid"]["source_beats"]) == len(s["grid"]["target_beats"]) == len(chart["analysis"]["beats"])
    assert np.allclose(np.diff(s["grid"]["target_beats"]) / np.diff(chart["analysis"]["beat_index"]), 0.5)
    assert 120 <= s["bitrate_kbps"] <= 136
    assert s["duration"] > 25

    info = subprocess.run(
        ["ffprobe", "-v", "error", "-show_entries", "format_tags", "-of", "default=nw=1", str(out)],
        capture_output=True, text=True, check=True,
    ).stdout.lower()
    assert "title=slow burn (straightened 120 bpm)" in info and "tbpm=120" in info
    assert "artist=the metronomes" in info

    m = done["measure"]["analysis"]
    assert abs(m["median_bpm"] - 120) < 1.5
    assert m["max_bpm"] - m["min_bpm"] < chart["analysis"]["max_bpm"] - chart["analysis"]["min_bpm"]


def test_cancel_stops_rubber_band_and_leaves_no_file(worker, drums, chart, tmp_path):
    straighten(worker, 3, drums, chart, tmp_path, measure=False)
    while True:  # wait until Rubber Band is running
        msg = worker.read()
        if msg.get("id") == 3 and msg["type"] == "progress" and msg["fraction"] > 0.05:
            break
    worker.send({"type": "cancel", "id": 3})
    msgs = worker.job(3)
    assert msgs[-1]["type"] == "cancelled"
    assert not any(m["type"] == "result" for m in msgs)
    assert not any(p.suffix == ".mp3" for p in tmp_path.iterdir())


def test_straighten_refuses_a_change_of_speed(worker, drums, chart, tmp_path):
    straighten(worker, 4, drums, chart, tmp_path, target_bpm=200)
    [err] = worker.job(4)
    assert err["type"] == "error" and "30%" in err["error"]
    # unless the detector found half-time
    straighten(worker, 5, drums, chart, tmp_path, target_bpm=240, level=2, measure=False)
    s = steps(worker.job(5))["straighten"]
    assert s["output"].endswith("(straightened 240 bpm).mp3")
    # and a request without beats
    worker.send({"id": 6, "type": "straighten", "wav": "x", "priority": "background", "deadline_s": 9,
                 "target_bpm": 120, "out_dir": str(tmp_path)})
    [err] = worker.job(6)
    assert err["type"] == "error" and "beats" in err["error"]


def test_without_rubber_band_it_says_what_to_install(drums, chart, tmp_path):
    bare = os.pathsep.join(p for p in os.environ.get("PATH", "").split(os.pathsep)
                           if not (p and os.path.exists(os.path.join(p, "rubberband"))))
    w = Worker({"PATH": bare})
    try:
        w.send({"type": "hello", "protocol": 2})
        w.next("hello")
        straighten(w, 1, drums, chart, tmp_path)
        last = w.job(1)[-1]
        assert last["type"] == "error" and "rubberband-cli" in last["error"]
    finally:
        assert w.close() == 0
