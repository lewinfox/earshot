#!/usr/bin/env bash
# Recompute the reference and every candidate from the saved model runs, then score.
# Assumes decode_sample.sh and run_bt.sh final0 / small0 have run.
set -e
here="$(cd "$(dirname "$0")" && pwd)"
DATA="${BG_DATA:-/tmp/claude-1000/-home-lewin-git-earshot/615b3b7a-7ddd-4410-b7a6-a8f3767fcbb6/scratchpad/bg/data}"
export OMP_NUM_THREADS=4
"$here/py" "$here/attack.py" > "$DATA/attack.log"
rm -f "$DATA"/results/{hybrid,small_line,full_line}.jsonl
for m in hybrid small_line full_line; do
  "$here/py" "$here/candidates.py" $m > "$DATA/$m.log" 2>&1
done
"$here/py" "$here/candidates.py" resnap static_snap > /dev/null
"$here/py" "$here/score.py" > "$DATA/score_all.txt"
cat "$DATA/score_all.txt"
