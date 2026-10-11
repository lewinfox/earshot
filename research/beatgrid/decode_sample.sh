#!/usr/bin/env bash
# Decode every track in DATA/sample.tsv with the app's own decoder (rust/src/bin/decode.rs).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
DATA="${BG_DATA:-/tmp/claude-1000/-home-lewin-git-earshot/615b3b7a-7ddd-4410-b7a6-a8f3767fcbb6/scratchpad/bg/data}"
TARGET="$DATA/../target"
taskset -c 0-3 cargo build --release --offline --manifest-path "$here/rust/Cargo.toml" --target-dir "$TARGET" -q
mkdir -p "$DATA/wav"
awk -F'\t' -v D="$DATA" '{print $2; print D"/wav/"$1".wav"}' "$DATA/sample.tsv" | tr '\n' '\0' |
  taskset -c 0-3 xargs -0 -n 16 "$TARGET/release/decode"
