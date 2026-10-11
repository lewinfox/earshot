"""Run Beat This! over whole tracks and save beats, downbeats, frame logits and timing.

usage: python -I bt_run.py <checkpoint> <shard> <nshards> [ids...]
Writes DATA/bt/<checkpoint>/<id>.npz. Set OMP_NUM_THREADS to bound torch's threads
(the app gives each of its 4 workers cores/4 threads: 2 on this laptop).
"""
import sys
import time

sys.path.insert(0, __file__.rsplit("/", 1)[0])
import numpy as np  # noqa: E402
import torch  # noqa: E402

from common import BT, load_wav, sample_ids  # noqa: E402

ckpt, shard, nshards = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
ids = [int(a) for a in sys.argv[4:]] or sample_ids()
out = BT / ckpt
out.mkdir(parents=True, exist_ok=True)
torch.set_num_threads(int(__import__("os").environ.get("OMP_NUM_THREADS", "2")))

from beat_this.inference import Audio2Beats  # noqa: E402

model = Audio2Beats(checkpoint_path=ckpt, device="cpu", dbn=False)
for tid in ids[shard::nshards]:
    dst = out / f"{tid}.npz"
    if dst.exists():
        continue
    y, sr = load_wav(tid)
    t0 = time.perf_counter()
    spect = model.signal2spect(y, sr)
    t1 = time.perf_counter()
    bl, dl = model.spect2frames(spect)
    t2 = time.perf_counter()
    beats, downs = model.frames2beats(bl, dl)
    t3 = time.perf_counter()
    np.savez(
        dst,
        beats=np.asarray(beats, float),
        downbeats=np.asarray(downs, float),
        beat_logits=bl.numpy().astype(np.float32),
        downbeat_logits=dl.numpy().astype(np.float32),
        t_spect=t1 - t0,
        t_model=t2 - t1,
        t_post=t3 - t2,
        duration=len(y) / sr,
    )
    print(f"{tid}\t{len(y) / sr:.0f}s\t{t3 - t0:.1f}s", flush=True)
