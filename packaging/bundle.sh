#!/usr/bin/env bash
# Build an earshot release folder: the app and everything its Python worker needs,
# ready to run, so nothing is set up on the user's computer.
#
#   packaging/bundle.sh <platform> <earshot binary> <out dir>
#
# <platform> is linux-x86_64, macos-arm64 or windows-x86_64. Needs uv and curl (and,
# for Linux, what building Rubber Band needs: a C++ compiler, libsndfile's headers).
# The folder it makes (core/worker_env.rs finds it beside the binary):
#
#   earshot           the app
#   python/           a standalone Python with every package the worker uses
#   worker/           the worker's code (earshot_worker)
#   models/           the Beat This! checkpoints (torch's cache layout)
#   bin/, lib/        ffmpeg, ffprobe and rubberband, and the libraries they use
#   licenses/         earshot's licence, the bundled programs', and where their source is
#
# Downloads are cached in packaging/.cache.
set -euo pipefail
# What it copies takes this umask (packages install it as it is): nothing writable
# but by its owner, whatever the builder's own umask.
umask 022

platform=${1:?platform}
exe=${2:?the earshot binary}
out=${3:?out dir}

repo="$(cd "$(dirname "$0")/.." && pwd)"
cache="$repo/packaging/.cache"
mkdir -p "$cache"

# Pinned versions. Python: astral-sh/python-build-standalone, the same builds uv uses.
PYTHON=3.12.15
PYTHON_BUILD=20261013
RUBBERBAND=4.0.0
# ffmpeg: BtbN/FFmpeg-Builds (Linux, Windows), the newest build of the 8.1 branch.
# LGPL: everything the worker uses (decoding, MP3 via LAME) is in it.
FFMPEG_BRANCH=8.1

case "$platform" in
  linux-x86_64)   py_target=x86_64-unknown-linux-gnu ;;
  macos-arm64)    py_target=aarch64-apple-darwin ;;
  windows-x86_64) py_target=x86_64-pc-windows-msvc ;;
  *) echo "unknown platform: $platform" >&2; exit 1 ;;
esac

# Fetch a URL into the cache (once) and print the file's path.
fetch() {
  local url=$1 file="$cache/${2:-$(basename "${1%%\?*}")}"
  if [ ! -s "$file" ]; then
    echo ">> download $url" >&2
    curl -fsSL --retry 3 -o "$file.part" "$url"
    mv "$file.part" "$file"
  fi
  echo "$file"
}

rm -rf "$out"
mkdir -p "$out"/{bin,lib,licenses,worker}
out="$(cd "$out" && pwd)"
cp "$exe" "$out/"
cp "$repo/README.md" "$out/"
cp "$repo/LICENSE" "$out/licenses/earshot.txt"

# --- Python and the worker's packages -----------------------------------------------
echo ">> python $PYTHON"
tar -xzf "$(fetch "https://github.com/astral-sh/python-build-standalone/releases/download/$PYTHON_BUILD/cpython-$PYTHON+$PYTHON_BUILD-$py_target-install_only_stripped.tar.gz")" -C "$out"
if [ "$platform" = windows-x86_64 ]; then
  python="$out/python/python.exe"
else
  python="$out/python/bin/python3"
fi
cp "$out/python/LICENSE.txt" "$out/licenses/python.txt" 2>/dev/null || true

echo ">> the worker's packages (as worker/uv.lock pins them)"
reqs="$(mktemp)"
(cd "$repo/worker" && uv export --frozen --no-dev --no-emit-project --emit-index-url -q -o "$reqs")
# Installed straight into that Python: a venv would remember where it was made.
uv pip install --python "$python" --break-system-packages --compile-bytecode \
  --index-strategy unsafe-best-match -q -r "$reqs"
rm -f "$reqs"
# Tests, headers and pip itself aren't needed to run.
find "$out/python" -depth -type d \( -name tests -o -name test \) -path '*site-packages*' -exec rm -rf {} +
rm -rf "$out/python/include"
# torch's C++ headers and build tools (~100 MB), for compiling extensions against it.
site="$("$python" -c 'import sysconfig; print(sysconfig.get_paths()["purelib"])')"
rm -rf "$site/torch/include" "$site"/torch/bin/protoc*

cp -r "$repo/worker/earshot_worker" "$out/worker/"
find "$out/worker" -name __pycache__ -exec rm -rf {} +
"$python" -m compileall -q "$out/worker"

# Downloaded into the cache (torch skips what's already there), then copied.
echo ">> Beat This! checkpoints"
TORCH_HOME="$cache/torch" PYTHONPATH="$out/worker" PYTHONNOUSERSITE=1 "$python" -c "
from earshot_worker.detectors import CHECKPOINT, SMALL_CHECKPOINT, get_beat_this
for c in (CHECKPOINT, SMALL_CHECKPOINT): get_beat_this(c)"
mkdir -p "$out/models"
cp -R "$cache/torch/." "$out/models/"

# --- ffmpeg and Rubber Band -----------------------------------------------------------

# Copy a Linux program's libraries (all but the system's own) into lib/, and have it
# and them look there.
bundle_libs() {
  local f lib
  for f in "$@"; do
    ldd "$f" | awk '/=> \// {print $3}' | while read -r lib; do
      case "$(basename "$lib")" in
        # glibc's own libraries (all of them: a copy of one from the build machine's
        # glibc breaks on any other system's), and the compiler's runtime.
        libc.so*|libm.so*|libmvec.so*|libdl.so*|libpthread.so*|librt.so*|libresolv.so*|\
        libutil.so*|libanl.so*|libnsl.so*|libnss_*|libthread_db.so*|libBrokenLocale.so*|\
        ld-linux*|libgcc_s.so*|libstdc++.so*) ;;
        *) [ -e "$out/lib/$(basename "$lib")" ] || cp -L "$lib" "$out/lib/" ;;
      esac
    done
    uvx patchelf --set-rpath '$ORIGIN/../lib' "$f"
  done
  for f in "$out"/lib/*.so*; do
    [ -L "$f" ] || uvx patchelf --set-rpath '$ORIGIN' "$f"
  done
}

case "$platform" in
  linux-x86_64)
    echo ">> ffmpeg $FFMPEG_BRANCH"
    name="ffmpeg-n$FFMPEG_BRANCH-latest-linux64-lgpl-shared-$FFMPEG_BRANCH"
    tmp="$(mktemp -d)"
    tar -xJf "$(fetch "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/$name.tar.xz")" -C "$tmp"
    cp "$tmp/$name"/bin/{ffmpeg,ffprobe} "$out/bin/"
    cp -P "$tmp/$name"/lib/*.so.* "$out/lib/"
    cp "$tmp/$name/LICENSE.txt" "$out/licenses/ffmpeg.txt"
    rm -rf "$tmp"

    # Built once and kept in the cache, in a folder named for the version and how it's
    # built, so changing either builds it again.
    options=(-Dfft=builtin -Dresampler=builtin -Ddefault_library=static -Dcmdline=enabled
      -Dtests=disabled -Dvamp=disabled -Dladspa=disabled -Dlv2=disabled -Djni=disabled
      --buildtype=release)
    built="$cache/rubberband-$RUBBERBAND-linux-x86_64-$(echo "${options[*]}" | sha256sum | cut -c1-8)"
    if [ ! -x "$built/rubberband" ]; then
      echo ">> rubberband $RUBBERBAND (building it)"
      tmp="$(mktemp -d)"
      tar -xjf "$(fetch "https://breakfastquay.com/files/releases/rubberband-$RUBBERBAND.tar.bz2")" -C "$tmp"
      (cd "$tmp/rubberband-$RUBBERBAND" &&
        uvx --with ninja meson setup build "${options[@]}" > /dev/null &&
        uvx --with ninja meson compile -C build > /dev/null)
      mkdir -p "$built.part"
      cp "$tmp/rubberband-$RUBBERBAND/build/rubberband" "$tmp/rubberband-$RUBBERBAND/COPYING" "$built.part/"
      rm -rf "$tmp" "$built"
      mv "$built.part" "$built"
    else
      echo ">> rubberband $RUBBERBAND (built before)"
    fi
    cp "$built/rubberband" "$out/bin/"
    cp "$built/COPYING" "$out/licenses/rubberband.txt"
    bundle_libs "$out/bin/rubberband"
    ;;
  macos-arm64)
    echo ">> ffmpeg (martin-riedl.de static builds)"
    for p in ffmpeg ffprobe; do
      unzip -oq "$(fetch "https://ffmpeg.martin-riedl.de/redirect/latest/macos/arm64/release/$p.zip" "$p-macos-arm64.zip")" -d "$out/bin"
    done
    echo ">> rubberband $RUBBERBAND"
    tmp="$(mktemp -d)"
    tar -xjf "$(fetch "https://breakfastquay.com/files/releases/rubberband-$RUBBERBAND-gpl-executable-macos.tar.bz2")" -C "$tmp"
    find "$tmp" -type f -name rubberband -exec cp {} "$out/bin/" \;
    find "$tmp" -type f \( -name COPYING -o -name 'COPYING*' \) -exec cp {} "$out/licenses/rubberband.txt" \; -quit
    rm -rf "$tmp"
    ;;
  windows-x86_64)
    echo ">> ffmpeg $FFMPEG_BRANCH"
    name="ffmpeg-n$FFMPEG_BRANCH-latest-win64-lgpl-shared-$FFMPEG_BRANCH"
    tmp="$(mktemp -d)"
    unzip -q "$(fetch "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/$name.zip")" -d "$tmp"
    cp "$tmp/$name"/bin/{ffmpeg.exe,ffprobe.exe} "$tmp/$name"/bin/*.dll "$out/bin/"
    cp "$tmp/$name/LICENSE.txt" "$out/licenses/ffmpeg.txt"
    rm -rf "$tmp"
    echo ">> rubberband $RUBBERBAND"
    tmp="$(mktemp -d)"
    unzip -q "$(fetch "https://breakfastquay.com/files/releases/rubberband-$RUBBERBAND-gpl-executable-windows.zip")" -d "$tmp"
    find "$tmp" -type f \( -name '*.exe' -o -name '*.dll' \) -exec cp {} "$out/bin/" \;
    find "$tmp" -type f -name 'COPYING*' -exec cp {} "$out/licenses/rubberband.txt" \; -quit
    rm -rf "$tmp"
    ;;
esac
rmdir "$out/lib" 2>/dev/null || true

cat > "$out/licenses/README.txt" <<EOF
earshot itself is MIT licensed (earshot.txt). Source: https://github.com/lewinfox/earshot

Its analysis worker uses mutagen, GPL v2 or later, through cratelib (MIT), to
read and write the tags DJ software keeps in music files. It runs in the
worker's Python, so this folder as a whole is distributed under the GPL's
terms; mutagen's own source is in python/ as it is, and its licence in its
*.dist-info folder. Source: https://github.com/quodlibet/mutagen

earshot runs these programs, shipped with it unchanged, as separate programs:

- Python $PYTHON (python-build-standalone $PYTHON_BUILD), PSF licence (python.txt).
  The packages installed in python/ carry their own licences in their
  *.dist-info folders. Source: https://github.com/astral-sh/python-build-standalone
- FFmpeg $FFMPEG_BRANCH, LGPL (ffmpeg.txt). Source: https://ffmpeg.org/download.html;
  build scripts: https://github.com/BtbN/FFmpeg-Builds (Linux, Windows),
  https://ffmpeg.martin-riedl.de (macOS)
- Rubber Band $RUBBERBAND, GPL v2 or later (rubberband.txt).
  Source: https://breakfastquay.com/files/releases/rubberband-$RUBBERBAND.tar.bz2
EOF

# What uv installs keeps the modes in its cache, whatever the umask.
chmod -R go-w "$out"

echo ">> done: $out ($(du -sh "$out" | cut -f1))"
