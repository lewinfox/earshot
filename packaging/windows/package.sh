#!/usr/bin/env bash
# Make the Windows installer from a release folder (packaging/bundle.sh):
#
#   packaging/windows/package.sh <release folder> <out.exe> [dev suffix]
#
# packaging/windows/earshot.iss says what it does. The version is gui/Cargo.toml's,
# with "-dev.<suffix>" for a build that isn't a release. Needs Inno Setup 6 (on
# GitHub's Windows runners; elsewhere, https://jrsoftware.org/isdl.php), so runs on
# Windows, in Git Bash.
set -euo pipefail

src=${1:?release folder}
exe=${2:?out.exe}
dev=${3:-}

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
mkdir -p "$(dirname "$exe")"
out_dir="$(cd "$(dirname "$exe")" && pwd)"
name="$(basename "$exe" .exe)"
src="$(cd "$src" && pwd)"

iscc=$(command -v iscc || command -v ISCC || true)
for p in "/c/Program Files (x86)/Inno Setup 6/ISCC.exe" "/c/Program Files/Inno Setup 6/ISCC.exe"; do
  [[ -n "$iscc" ]] || { [[ -x "$p" ]] && iscc="$p"; }
done
[[ -n "$iscc" ]] || { echo "package.sh needs Inno Setup 6's ISCC.exe" >&2; exit 1; }

version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo/gui/Cargo.toml" | head -1)
# The file's version details take numbers only.
numeric="$version.0"
[[ -z "$dev" ]] || version="$version-dev.$dev"

# Windows paths for ISCC (a Windows program), and Git Bash told not to turn its
# /D options into paths.
win() { cygpath -w "$1"; }
MSYS2_ARG_CONV_EXCL='*' "$iscc" /Q \
  "/DBundle=$(win "$src")" \
  "/DVersion=$version" \
  "/DNumericVersion=$numeric" \
  "/DOutputDir=$(win "$out_dir")" \
  "/DOutputName=$name" \
  "$(win "$here/earshot.iss")"
ls -l "$out_dir/$name.exe"
