#!/usr/bin/env bash
# Create/refresh the worker venv at earshot/worker/.venv. Re-runnable.
set -euo pipefail
cd "$(dirname "$0")"

UV="$(command -v uv || echo "$HOME/.local/bin/uv")"
if [ ! -x "$UV" ]; then
  echo "!! uv not found; install it from https://docs.astral.sh/uv/" >&2
  exit 1
fi

# keep the venv inside this directory even if the caller has a different one active
unset VIRTUAL_ENV
export UV_PROJECT_ENVIRONMENT="$PWD/.venv"
"$UV" sync

echo
echo "done. the app runs: $PWD/.venv/bin/python -m earshot_worker"
