#!/usr/bin/env bash
# Make the Linux packages from a release folder (packaging/bundle.sh):
#
#   packaging/linux/package.sh <release folder> <out dir> [dev suffix]
#
# Writes earshot_<version>_amd64.deb and earshot-<version>.x86_64.rpm to <out dir>
# (packaging/linux/nfpm.yaml says what's in them). The version is gui/Cargo.toml's;
# a dev suffix (a commit, for a build that isn't a release) makes it a pre-release
# that sorts before the version itself. Uses nfpm: $NFPM, else nfpm on the PATH, else
# its Docker image (the version the release workflow uses).
set -euo pipefail

src=${1:?release folder}
out=${2:?out dir}
dev=${3:-}

repo="$(cd "$(dirname "$0")/../.." && pwd)"
mkdir -p "$out"
out="$(cd "$out" && pwd)"
src="$(cd "$src" && pwd)"

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT

# Keep in step with NFPM_VERSION in .github/workflows/release.yml.
NFPM_VERSION=2.48.0
if [[ -n "${NFPM:-}" ]]; then
  nfpm=("$NFPM")
elif command -v nfpm > /dev/null; then
  nfpm=(nfpm)
elif command -v docker > /dev/null; then
  # The folders it reads and writes, at the same paths inside; as this user, so the
  # packages aren't root's.
  nfpm=(docker run --rm -u "$(id -u):$(id -g)" -v "$repo:$repo:ro" -v "$src:$src:ro"
    -v "$stage:$stage:ro" -v "$out:$out" -w "$repo" "goreleaser/nfpm:v$NFPM_VERSION")
else
  echo "package.sh needs nfpm (https://nfpm.goreleaser.com) or Docker" >&2
  exit 1
fi
# The menu entry runs the /usr/bin link.
sed 's|@EXEC@|earshot|' "$repo/gui/assets/earshot.desktop" > "$stage/earshot.desktop"

# The newest glibc anything in the folder needs (the app and Rubber Band are built
# on the build machine; Python and ffmpeg on older systems).
# (objdump fails on the scripts among them, which is fine.)
glibc=$({ find "$src" -type f \( -perm -u+x -o -name '*.so*' \) -print0 |
  xargs -0 objdump -T 2>/dev/null || true; } | grep -o 'GLIBC_2\.[0-9]*' | sort -uV | tail -1)
glibc=${glibc#GLIBC_}
[[ -n "$glibc" ]] || { echo "couldn't tell which glibc $src needs" >&2; exit 1; }
echo ">> needs glibc $glibc"

# The config with its ${...} filled in.
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo/gui/Cargo.toml" | head -1)
maintainer="${MAINTAINER:-Lewin Appleton-Fox <lewin.a.f@gmail.com>}"
config=$(< "$repo/packaging/linux/nfpm.yaml")
config=${config//'${BUNDLE}'/"$src/"}
config=${config//'${DESKTOP}'/"$stage/earshot.desktop"}
config=${config//'${VERSION}'/"$version"}
config=${config//'${PRERELEASE}'/"${dev:+dev.$dev}"}
config=${config//'${MAINTAINER}'/"$maintainer"}
config=${config//'${GLIBC}'/"$glibc"}
printf '%s\n' "$config" > "$stage/nfpm.yaml"

cd "$repo"
for p in deb rpm; do
  "${nfpm[@]}" package --config "$stage/nfpm.yaml" --packager "$p" --target "$out/"
done
ls -lh "$out"
