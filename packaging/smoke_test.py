"""Check a release folder (packaging/bundle.sh) works on its own: run with its own
Python, `python/bin/python3 packaging/smoke_test.py <folder>`, ideally offline and
with the folder read-only. Starts the worker the way the app does
(gui/src/core/worker_env.rs), analyses a click track with Beat This!, and runs the
bundled ffmpeg, ffprobe and Rubber Band. Exits non-zero on the first failure."""

import json
import os
import queue
import shutil
import subprocess
import sys
import tempfile
import threading
import time
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
# Ours too: Windows finds a program named without its folder on the PATH of the
# process starting it, not the one it's given (the worker has the bundle's PATH as
# its own, as the app starts it).
os.environ["PATH"] = env["PATH"]
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

# The worker, as the app runs it: at normal priority (the deck's worker), then in
# the idle priority class (the background ones). Each answer has a time limit, so a
# stuck worker fails the test instead of hanging it.
HELLO_S, RESULT_S = 180, 300


def analyse(label: str, extra: dict) -> None:
    proc = subprocess.Popen(
        [str(python), "-m", "earshot_worker"],
        cwd=root / "worker",
        env=env | extra,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        text=True,
    )
    lines: queue.Queue = queue.Queue()
    threading.Thread(target=lambda: [lines.put(x) for x in proc.stdout] + [lines.put(None)], daemon=True).start()

    def fail(what: str, detail: str) -> None:
        proc.kill()
        check(f"{label}: {what}", False, detail)

    def send(msg: dict) -> None:
        proc.stdin.write(json.dumps(msg) + "\n")
        proc.stdin.flush()

    def receive(want: str, limit: float) -> dict:
        until = time.monotonic() + limit
        while True:
            try:
                line = lines.get(timeout=max(0.0, until - time.monotonic()))
            except queue.Empty:
                fail(f"worker answers {want}", f"nothing in {limit:g} s")
            if line is None:
                fail(f"worker answers {want}", "it stopped")
            msg = json.loads(line)
            if msg.get("type") in (want, "error", "failed"):
                return msg

    t0 = time.monotonic()
    send({"type": "hello", "protocol": 2})
    hello = receive("hello", HELLO_S)
    check(f"{label}: worker says hello", hello.get("type") == "hello", f"{time.monotonic() - t0:.1f} s")
    check(f"{label}: Beat This! is there", hello.get("detector") == "beat_this", str(hello.get("detector")))
    t0 = time.monotonic()
    send({"id": 1, "type": "analyse", "wav": str(wav), "priority": "deck", "analysers": {"grid": {"mode": "dynamic"}}, "deadline_s": RESULT_S})
    result = receive("result", RESULT_S)
    if result.get("type") != "result":
        fail("worker analyses a track", json.dumps(result)[:300])
    check(f"{label}: worker analyses a track", True, f"{time.monotonic() - t0:.1f} s")
    beats = result["data"].get("grid", {}).get("beats") or [0, 0]
    bpm = 60 / float(np.median(np.diff(beats))) if len(beats) > 2 else 0
    if abs(bpm - 120) >= 1:
        fail("it hears 120 BPM", f"{bpm:.2f}")
    check(f"{label}: it hears 120 BPM", True, f"{bpm:.2f}")
    proc.stdin.close()
    proc.wait(timeout=30)


analyse("normal priority", {"EARSHOT_WORKER_PRIORITY": "inherit"})
analyse("idle priority", {})

r = subprocess.run([str(python), "-m", "earshot_worker.libraries", "find"], cwd=root / "worker", env=env, capture_output=True, text=True, timeout=120)
last = json.loads(r.stdout.strip().splitlines()[-1]) if r.stdout.strip() else {}
check("the library reader runs", "libraries" in last, (r.stdout + r.stderr)[-300:])

shutil.rmtree(tmp)
print("all ok")
