#!/usr/bin/env bash
# Recompute the given methods from saved model runs (no new model runs for
# small_line / full_line), then score everything. usage: rescore.sh [methods...]
set -e
here="$(cd "$(dirname "$0")" && pwd)"
DATA="${BG_DATA:-/tmp/claude-1000/-home-lewin-git-earshot/615b3b7a-7ddd-4410-b7a6-a8f3767fcbb6/scratchpad/bg/data}"
export OMP_NUM_THREADS=4
for m in "$@"; do
  rm -f "$DATA/results/$m.jsonl"
  "$here/py" "$here/candidates.py" "$m" > "$DATA/$m.log" 2>&1
done
"$here/py" "$here/score.py" > "$DATA/score_all.txt"
grep -v '^ \{4,\}[0-9]' "$DATA/score_all.txt"
