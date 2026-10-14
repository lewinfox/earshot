"""Check a release folder (packaging/bundle.sh) works on its own: run with its own
Python, `python/bin/python3 packaging/smoke_test.py <folder>`, ideally offline and
with the folder read-only. Starts the worker the way the app does
(gui/src/core/worker_env.rs), analyses a click track with Beat This!, and runs the
bundled ffmpeg, ffprobe and Rubber Band. Exits non-zero on the first failure."""

import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

root = Path(sys.argv[1]).resolve()
windows = os.name == "nt"
python = root / ("python/python.exe" if windows else "python/bin/python3")

# As WorkerEnv::bundle sets it.
env = dict(os.environ)
env.update(
    PYTHONPATH=str(root / "worker"),
    PYTHONNOUSERSITE="1",
    PYTHONDONTWRITEBYTECODE="1",
    TORCH_HOME=str(root / "models"),
    PATH=os.pathsep.join([str(root / "bin"), env.get("PATH", "")]),
)
sys.path.insert(0, str(root / "worker"))

import numpy as np  # noqa: E402
import soundfile as sf  # noqa: E402


def check(what: str, ok: bool, detail: str = "") -> None:
    print(f"{'ok  ' if ok else 'FAIL'} {what}{': ' + detail if detail else ''}", flush=True)
    if not ok:
        sys.exit(1)


tmp = Path(tempfile.mkdtemp())

# The programs come from the bundle, not the system.
for name in ("ffmpeg", "ffprobe", "rubberband"):
    found = shutil.which(name, path=env["PATH"])
    check(f"{name} is the bundled one", found is not None and Path(found).parent == root / "bin", str(found))
    flag = "--version" if name == "rubberband" else "-version"
    r = subprocess.run([found, flag], env=env, capture_output=True, text=True)
    check(f"{name} runs", r.returncode == 0, (r.stdout + r.stderr).splitlines()[0] if r.stdout + r.stderr else "")

# A click track at 120 BPM.
sr = 44100
y = np.zeros(sr * 20, dtype=np.float32)
for beat in range(40):
    i = beat * sr // 2
    y[i : i + 400] = np.sin(np.arange(400) * 2 * np.pi * 1000 / sr) * (1.0 if beat % 4 == 0 else 0.6)
wav = tmp / "clicks.wav"
sf.write(wav, y, sr, subtype="FLOAT")

r = subprocess.run(["rubberband", "-3", "-t", "1.1", str(wav), str(tmp / "slow.wav")], env=env, capture_output=True, text=True)
check("rubberband stretches", r.returncode == 0 and (tmp / "slow.wav").exists(), r.stderr[-300:])
r = subprocess.run(["ffmpeg", "-loglevel", "error", "-i", str(wav), "-c:a", "libmp3lame", str(tmp / "clicks.mp3")], env=env, capture_output=True, text=True)
check("ffmpeg writes an MP3", r.returncode == 0, r.stderr[-300:])
r = subprocess.run(["ffprobe", "-v", "error", "-show_format", "-of", "json", str(tmp / "clicks.mp3")], env=env, capture_output=True, text=True)
check("ffprobe reads it", r.returncode == 0 and "mp3" in r.stdout, r.stderr[-300:])

# The worker, as the app runs it.
proc = subprocess.Popen(
    [str(python), "-m", "earshot_worker"],
    cwd=root / "worker",
    env=env,
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    text=True,
)


def send(msg: dict) -> None:
    proc.stdin.write(json.dumps(msg) + "\n")
    proc.stdin.flush()


def receive(want: str) -> dict:
    for line in proc.stdout:
        msg = json.loads(line)
        if msg.get("type") in (want, "error", "failed"):
            return msg
    check(f"worker answers {want}", False, "it stopped")


send({"type": "hello", "protocol": 2})
hello = receive("hello")
check("worker says hello", hello.get("type") == "hello", json.dumps(hello))
check("Beat This! is there", hello.get("detector") == "beat_this", str(hello.get("detector")))
send({"id": 1, "type": "analyse", "wav": str(wav), "priority": "deck", "analysers": {"grid": {"mode": "dynamic"}}, "deadline_s": 300})
result = receive("result")
check("worker analyses a track", result.get("type") == "result", json.dumps(result)[:300])
beats = result["data"].get("grid", {}).get("beats") or [0, 0]
bpm = 60 / float(np.median(np.diff(beats))) if len(beats) > 2 else 0
check("it hears 120 BPM", abs(bpm - 120) < 1, f"{bpm:.2f}")
proc.stdin.close()
proc.wait(timeout=30)

r = subprocess.run([str(python), "-m", "earshot_worker.libraries", "find"], cwd=root / "worker", env=env, capture_output=True, text=True)
last = json.loads(r.stdout.strip().splitlines()[-1]) if r.stdout.strip() else {}
check("the library reader runs", "libraries" in last, (r.stdout + r.stderr)[-300:])

shutil.rmtree(tmp)
print("all ok")
