#!/usr/bin/env bash
# Check the Windows installer as a user would meet it: install it (silently, for this
# user, into a fresh folder), run the smoke test on the installed copy, check the
# Start menu entry, uninstall, and check nothing is left behind.
#
#   packaging/windows/test_installer.sh <setup.exe>
#
# Runs on Windows, in Git Bash (the release workflow's Windows build).
set -euo pipefail

setup="$(cd "$(dirname "${1:?setup.exe}")" && pwd)/$(basename "$1")"
repo="$(cd "$(dirname "$0")/../.." && pwd)"
dir="$LOCALAPPDATA/Programs/earshot-test"
menu="$APPDATA/Microsoft/Windows/Start Menu/Programs/earshot.lnk"
export MSYS2_ARG_CONV_EXCL='*'  # Inno's /OPTIONS aren't paths

echo ">> install"
"$setup" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /CURRENTUSER "/DIR=$(cygpath -w "$dir")" \
  "/LOG=$(cygpath -w "$RUNNER_TEMP/install.log")"
[[ -x "$dir/earshot.exe" ]] || { echo "not installed: $dir/earshot.exe"; cat "$RUNNER_TEMP/install.log"; exit 1; }
[[ -f "$menu" ]] || { echo "no Start menu entry: $menu"; exit 1; }
echo "   ok: $dir, Start menu entry"

# This machine has Microsoft's Visual C++ runtime, a fresh Windows may not: nothing may
# need it unless it brings its own copy (as python\ does). earshot.exe links it in
# (.cargo/config.toml).
echo ">> Visual C++ runtime"
missing=""
while IFS= read -r -d '' f; do
  if grep -qai 'vcruntime140' "$f" && [[ ! -e "$(dirname "$f")/vcruntime140.dll" ]]; then
    missing+="  ${f#"$dir"/}"$'\n'
  fi
done < <(find "$dir" -path "$dir/python" -prune -o -type f \( -iname '*.exe' -o -iname '*.dll' \) ! -iname 'unins*' -print0)
[[ -z "$missing" ]] || { printf 'need VCRUNTIME140.dll, which a fresh Windows may not have:\n%s' "$missing"; exit 1; }
echo "   ok: nothing needs it"

echo ">> smoke test, installed copy"
PYTHONDONTWRITEBYTECODE=1 "$dir/python/python.exe" "$repo/packaging/smoke_test.py" "$dir"

echo ">> uninstall"
"$dir/unins000.exe" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART
# The uninstaller hands over to a copy of itself and returns at once: wait for it.
for _ in $(seq 120); do
  [[ -e "$dir" || -e "$menu" ]] || break
  sleep 1
done
left=$( { [[ -e "$dir" ]] && find "$dir"; [[ -e "$menu" ]] && echo "$menu"; } || true)
if [[ -n "$left" ]]; then
  echo "left behind:"; echo "$left" | head -50; exit 1
fi
echo "   ok: nothing left behind"
