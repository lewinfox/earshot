"""Export the grids stored in (the copy of) the app DB to DATA/grids/db/<id>.npy."""
import struct
import sys

sys.path.insert(0, __file__.rsplit("/", 1)[0])
import numpy as np  # noqa: E402

from common import DATA, db, sample_ids  # noqa: E402

out = DATA / "grids" / "db"
out.mkdir(parents=True, exist_ok=True)
with db() as c:
    for tid in sample_ids():
        row = c.execute("select beats from beatgrids where track_id=?", (tid,)).fetchone()
        if row:
            b = row[0]
            n = struct.unpack_from("<I", b, 8)[0]
            np.save(out / f"{tid}.npy", np.array(struct.unpack_from(f"<{n}d", b, 12)))
            print(tid, n)
