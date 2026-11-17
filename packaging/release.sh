#!/usr/bin/env bash
# Tag main at the app's version and push the tag, which starts the release workflow
# (.github/workflows/release.yml): builds for every platform and a GitHub release.
#
#   packaging/release.sh        (make release)
#
# The version is gui/Cargo.toml's, so the tag and the app's own version agree: bump it
# (through a PR) before releasing. Checks first that main is checked out, clean and the
# same as origin's, and that the tag is new; then asks before pushing.
set -euo pipefail

cd "$(dirname "$0")/.."

fail() {
  echo "release: $*" >&2
  exit 1
}

version=$(sed -n 's/^version = "\(.*\)"/\1/p' gui/Cargo.toml | head -1)
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "gui/Cargo.toml's version isn't x.y.z: '$version'"
tag="v$version"

[[ "$(git branch --show-current)" == main ]] || fail "check out main first"
[[ -z "$(git status --porcelain)" ]] || fail "main has uncommitted changes"
git fetch --quiet --tags origin
[[ "$(git rev-parse HEAD)" == "$(git rev-parse origin/main)" ]] ||
  fail "main isn't the same as origin/main (pull or push first)"
if git rev-parse -q --verify "refs/tags/$tag" > /dev/null; then
  fail "$tag already exists: bump the version in gui/Cargo.toml"
fi

last=$(git describe --tags --abbrev=0 2> /dev/null || true)
echo "Release $tag from main at $(git log -1 --format='%h %s')"
[[ -n "$last" ]] && echo "Since $last: $(git rev-list --count "$last"..HEAD) commits"
read -r -p "Tag and push $tag? [y/N] " answer
[[ "$answer" == [yY] ]] || fail "nothing done"

git tag -a "$tag" -m "earshot $version"
git push origin "$tag"
echo "Pushed $tag: the release workflow is building it (GitHub > Actions > release)."
