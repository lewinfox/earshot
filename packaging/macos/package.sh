#!/usr/bin/env bash
# Turn a macOS release folder (packaging/bundle.sh) into earshot.app, and that into a
# disk image to drag it to Applications from.
#
#   packaging/macos/package.sh app <release folder> <out dir>   # makes <out dir>/earshot.app
#   packaging/macos/package.sh dmg <earshot.app> <out.dmg>
#
# The app: earshot in Contents/MacOS, the rest of the folder in Contents/Resources
# (core/worker_env.rs looks there). Every program and library in it is signed with the
# hardened runtime, as notarization requires: with MACOS_SIGN_IDENTITY (a "Developer
# ID Application" certificate in the keychain) if set, else ad hoc, so an unsigned
# build still runs under the same rules and the smoke test catches what they break.
#
# The disk image is signed too, and notarized and stapled when NOTARY_KEY (the path
# of an App Store Connect API key, .p8), NOTARY_KEY_ID and NOTARY_ISSUER are set.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
identity="${MACOS_SIGN_IDENTITY:--}"

# A real identity gets a secure timestamp (notarization needs one); ad hoc can't.
sign() {
  local flags=(--force --options runtime --sign "$identity")
  [[ "$identity" == - ]] || flags+=(--timestamp)
  codesign "${flags[@]}" "$@"
}

make_app() {
  local src=${1:?release folder} out=${2:?out dir}
  local version app
  version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo/gui/Cargo.toml" | head -1)
  app="$out/earshot.app"
  rm -rf "$app"
  mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
  cp "$src/earshot" "$app/Contents/MacOS/"
  for f in "$src"/*; do
    [[ "$(basename "$f")" == earshot ]] || cp -R "$f" "$app/Contents/Resources/"
  done

  echo ">> icon"
  local set
  set="$(mktemp -d)/earshot.iconset"
  mkdir -p "$set"
  local icons="$repo/gui/assets/icons"
  for s in 16 32 128 256 512; do
    cp "$icons/earshot-$s.png" "$set/icon_${s}x${s}.png"
    [[ $s == 512 ]] || cp "$icons/earshot-$((s * 2)).png" "$set/icon_${s}x${s}@2x.png"
  done
  iconutil -c icns "$set" -o "$app/Contents/Resources/earshot.icns"

  # LSMinimumSystemVersion: the worker's numpy and scipy are their macOS 14 builds.
  cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>earshot</string>
    <key>CFBundleDisplayName</key><string>earshot</string>
    <key>CFBundleIdentifier</key><string>io.github.lewinfox.earshot</string>
    <key>CFBundleExecutable</key><string>earshot</string>
    <key>CFBundleIconFile</key><string>earshot</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>LSMinimumSystemVersion</key><string>14.0</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.music</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSRemovableVolumesUsageDescription</key>
    <string>earshot reads and writes DJ libraries on USB sticks.</string>
</dict>
</plist>
EOF

  echo ">> sign ($([[ "$identity" == - ]] && echo "ad hoc" || echo "$identity"))"
  # Every Mach-O file in Resources: programs get the bundled entitlements, libraries
  # don't need them. Then earshot, then the app around them.
  local path desc exes=() libs=()
  while IFS='|' read -r path desc; do
    case "$desc" in
      *Mach-O*executable*) exes+=("$path") ;;
      *Mach-O*) libs+=("$path") ;;
    esac
  done < <(find "$app/Contents/Resources" -type f -print0 | xargs -0 file -N -F '|')
  echo "   ${#exes[@]} programs, ${#libs[@]} libraries"
  export -f sign
  export identity entitlements="$here/bundled.entitlements"
  ((${#libs[@]})) && printf '%s\0' "${libs[@]}" |
    xargs -0 -n 16 -P 8 bash -c 'sign "$@"' _
  ((${#exes[@]})) && printf '%s\0' "${exes[@]}" |
    xargs -0 -n 16 -P 8 bash -c 'sign --entitlements "$entitlements" "$@"' _
  sign "$app/Contents/MacOS/earshot"
  sign "$app"
  codesign --verify --strict --deep "$app"
  echo ">> done: $app ($(du -sh "$app" | cut -f1))"
}

make_dmg() {
  local app=${1:?earshot.app} dmg=${2:?out.dmg}
  # Nothing may have changed inside it since it was signed (the smoke test).
  codesign --verify --strict --deep "$app"
  local stage
  stage="$(mktemp -d)"
  # ditto keeps the signatures, symlinks and extended attributes as they are.
  ditto "$app" "$stage/earshot.app"
  ln -s /Applications "$stage/Applications"
  rm -f "$dmg"
  # hdiutil sometimes fails with "Resource busy" on CI machines: try again.
  local i
  for i in 1 2 3 4 5; do
    hdiutil create -volname earshot -srcfolder "$stage" -ov -format UDZO "$dmg" && break
    [[ $i == 5 ]] && exit 1
    sleep 10
  done
  rm -rf "$stage"

  if [[ "$identity" != - ]]; then
    codesign --force --timestamp --sign "$identity" "$dmg"
  fi
  if [[ -n "${NOTARY_KEY:-}" ]]; then
    echo ">> notarize"
    xcrun notarytool submit "$dmg" --key "$NOTARY_KEY" --key-id "$NOTARY_KEY_ID" \
      --issuer "$NOTARY_ISSUER" --wait --timeout 1h --output-format json \
      > "$dmg.notary.json" || true
    cat "$dmg.notary.json"
    if ! grep -q '"status" *: *"Accepted"' "$dmg.notary.json"; then
      id=$(sed -n 's/.*"id" *: *"\([^"]*\)".*/\1/p' "$dmg.notary.json" | head -1)
      [[ -n "$id" ]] && xcrun notarytool log "$id" --key "$NOTARY_KEY" \
        --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER" || true
      exit 1
    fi
    rm -f "$dmg.notary.json"
    xcrun stapler staple "$dmg"
    spctl --assess --type open --context context:primary-signature -v "$dmg"
  fi
  echo ">> done: $dmg ($(du -sh "$dmg" | cut -f1))"
}

case "${1:-}" in
  app) make_app "${@:2}" ;;
  dmg) make_dmg "${@:2}" ;;
  *) echo "usage: $0 app <release folder> <out dir> | dmg <earshot.app> <out.dmg>" >&2; exit 1 ;;
esac
