#!/usr/bin/env bash
# Run bt_run.py for one checkpoint as 2 parallel 2-thread processes, pinned to cores
# 0-3 by ./py (4 cores in total; the app gives each worker 2 threads on this laptop).
# usage: run_bt.sh <checkpoint>
here="$(cd "$(dirname "$0")" && pwd)"
DATA="${BG_DATA:-/tmp/claude-1000/-home-lewin-git-earshot/615b3b7a-7ddd-4410-b7a6-a8f3767fcbb6/scratchpad/bg/data}"
for s in 0 1; do
  OMP_NUM_THREADS=2 "$here/py" "$here/bt_run.py" "$1" $s 2 > "$DATA/$1_$s.log" 2>&1 &
done
wait
