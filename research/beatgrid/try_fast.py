"""Quick check of fastgrid.py against ground truth. usage: try_fast.py [ids...]"""
import sys
import time

sys.path.insert(0, __file__.rsplit("/", 1)[0])
import numpy as np  # noqa: E402

import fastgrid  # noqa: E402
import gtlib  # noqa: E402
from common import BT, DATA, load_wav, sample_ids  # noqa: E402

ids = [int(a) for a in sys.argv[1:]] or [i for i in sample_ids() if (BT / "final0" / f"{i}.npz").exists()]
for tid in ids:
    gt = gtlib.truth(tid)
    y, sr = load_wav(tid)
    t0 = time.perf_counter()
    r = fastgrid.analyse(y, sr)
    dt = time.perf_counter() - t0
    grid = gtlib.grid_times(r["bpm"], r["phase"], gt.duration)
    (DATA / "grids" / "fast").mkdir(parents=True, exist_ok=True)
    np.save(DATA / "grids" / "fast" / f"{tid}.npy", grid)
    s = gtlib.score(gt, grid, r["bpm"], "line")
    st = gt.stats()
    print(
        f"{tid:5d} gt {gt.bpm:7.2f} rp90 {st['resid_p90_ms']:5.1f} | fast {r['bpm']:7.2f} "
        f"bias {s['bias_ms']:+6.1f} med {s['med_ms']:5.1f} p90 {s['p90_ms']:6.1f} in10 {s['in10']:.2f} "
        f"wres {r['win_resid_p90_ms']:5.1f} good {r['good_frac']:.2f} {dt:.2f}s"
    )
