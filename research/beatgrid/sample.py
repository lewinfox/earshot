"""Choose the screening sample: the known problem tracks, a few whose names suggest
live playing or tempo changes, and a seeded random draw. Writes DATA/sample.tsv
(id, path). Which of them end up regular or irregular is decided later from Beat
This!'s output, not from these names."""
import random

from common import DATA, db

KNOWN = [45, 72, 69, 82, 468]
# Names mention live playing, an old record or a mashup: more likely to wander.
NAMED = [187, 1131, 1132, 1166, 1117, 880, 212, 1208, 1415]
N_RANDOM = 61

with db() as c:
    rows = c.execute("select id, path from tracks where duration_s between 90 and 660 order by id").fetchall()
paths = dict(rows)
pool = [i for i in paths if i not in KNOWN + NAMED]
random.seed(1)
ids = KNOWN + NAMED + sorted(random.sample(pool, N_RANDOM))
(DATA / "sample.tsv").write_text("".join(f"{i}\t{paths[i]}\n" for i in ids))
print(len(ids), "tracks")
