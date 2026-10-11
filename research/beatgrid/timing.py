"""Time each approach, one track at a time, nothing else running (run via ./py: cores
0-3, OMP_NUM_THREADS=4, normal priority). Models are loaded and warmed up first, as
in the long-running worker. Times exclude decoding (the app has the audio already).

usage: python -I timing.py [ids...]     prints a table, writes DATA/results/timing.json
"""
from __future__ import annotations

import json
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent.parent / "worker"))
import numpy as np  # noqa: E402
import torch  # noqa: E402

import attack  # noqa: E402
import gtlib  # noqa: E402
import hybrid  # noqa: E402
from common import DATA, load_wav  # noqa: E402
from earshot_worker.detectors import get_beat_this  # noqa: E402
from earshot_worker.static_tempo import analyse_static  # noqa: E402

IDS = [int(a) for a in sys.argv[1:]] or [82, 45, 1035, 468, 284]
torch.set_num_threads(4)


def line_and_snap(model, y, sr):
    bl, dl = model.spect2frames(model.signal2spect(y, sr))
    beats, _ = model.frames2beats(bl, dl)
    b = gtlib.refine(np.asarray(beats, float), bl.numpy())
    b, idx = gtlib.index_beats(b)
    el = attack.energy(y, sr, low=True)
    _, ksec = attack.kicks(b, el)
    if ksec.mean() < 0.3:
        ksec[:] = True
    a, s, _ = gtlib.robust_line(idx[ksec].astype(float), b[ksec])
    return hybrid.snap_kick(y, sr, 60 / s, a, el=el)


def rust(tid):
    exe = DATA.parent / "target" / "release" / "fastgrid"
    out = subprocess.run(["taskset", "-c", "0-3", str(exe), str(DATA / "wav" / f"{tid}.wav")], capture_output=True, text=True, check=True).stdout
    return json.loads(out)["seconds"]


small, full = get_beat_this("small0"), get_beat_this("final0")
y0, sr0 = load_wav(IDS[0])
analyse_static(y0[: sr0 * 90], sr0)  # warm up
full(y0[: sr0 * 30], sr0)

rows = []
for tid in IDS:
    y, sr = load_wav(tid)
    r = {"id": tid, "duration": len(y) / sr}

    def timed(name, fn):
        t0 = time.perf_counter()
        try:
            fn()
        except ValueError:
            pass  # static gives up on some tracks: time to the failure
        r[name] = time.perf_counter() - t0

    timed("static", lambda: analyse_static(y, sr))
    timed("hybrid", lambda: hybrid.analyse(y, sr))
    timed("small_line", lambda: line_and_snap(small, y, sr))
    timed("full_line", lambda: line_and_snap(full, y, sr))
    r["fast_rs"] = rust(tid)
    rows.append(r)
    print(json.dumps({k: round(v, 2) if isinstance(v, float) else v for k, v in r.items()}), flush=True)
med = {k: float(np.median([r[k] for r in rows])) for k in rows[0] if k != "id"}
print("median", json.dumps({k: round(v, 2) for k, v in med.items()}))
(DATA / "results" / "timing.json").write_text(json.dumps({"rows": rows, "median": med}, indent=1))
