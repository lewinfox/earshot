#!/usr/bin/env bash
# Create/refresh the worker venv at earshot/worker/.venv. Re-runnable.
set -euo pipefail
cd "$(dirname "$0")"

# The tempo lock tool runs Rubber Band and ffmpeg as programs (see README.md). Only
# installed when missing, so a re-run doesn't ask for sudo.
if ! { command -v rubberband >/dev/null || command -v rubberband-r3 >/dev/null; } \
   || ! command -v ffmpeg >/dev/null || ! command -v ffprobe >/dev/null; then
  if command -v apt-get >/dev/null; then
    echo ">> system packages (rubberband-cli, ffmpeg)"
    sudo apt-get install -y rubberband-cli ffmpeg
  elif command -v brew >/dev/null; then
    echo ">> system packages (rubberband, ffmpeg)"
    brew install rubberband ffmpeg
  else
    echo "!! install the Rubber Band CLI and ffmpeg yourself (the tempo lock tool needs them)" >&2
  fi
fi

UV="$(command -v uv || echo "$HOME/.local/bin/uv")"
if [ ! -x "$UV" ]; then
  echo "!! uv not found; install it from https://docs.astral.sh/uv/" >&2
  exit 1
fi

# keep the venv inside this directory even if the caller has a different one active
unset VIRTUAL_ENV
export UV_PROJECT_ENVIRONMENT="$PWD/.venv"
"$UV" sync

# Fetch the Beat This! checkpoints now (~80 MB, into the torch cache), so the first
# analysis doesn't need the network.
.venv/bin/python -c "from earshot_worker.detectors import CHECKPOINT, SMALL_CHECKPOINT, get_beat_this
for c in (CHECKPOINT, SMALL_CHECKPOINT): get_beat_this(c)"

echo
echo "done. the app runs: $PWD/.venv/bin/python -m earshot_worker"
