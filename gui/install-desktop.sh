#!/usr/bin/env bash
# Install earshot's icon and desktop entry for this user, so the dock shows the
# icon instead of a cog (Wayland docks find it through the window's app id,
# "earshot", matching earshot.desktop). Run again after moving the checkout.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
exe="${1:-$(cd "$here/.." && pwd)/target/release/earshot-gui}"

for png in "$here"/assets/icons/earshot-*.png; do
    size="$(basename "$png" .png)"; size="${size#earshot-}"
    install -Dm644 "$png" "$data/icons/hicolor/${size}x${size}/apps/earshot.png"
done
install -Dm644 "$here/assets/icons/earshot.svg" "$data/icons/hicolor/scalable/apps/earshot.svg"
sed "s|@EXEC@|$exe|" "$here/assets/earshot.desktop" > "$data/applications/earshot.desktop"
chmod 644 "$data/applications/earshot.desktop"
gtk-update-icon-cache -q "$data/icons/hicolor" 2>/dev/null || true
update-desktop-database -q "$data/applications" 2>/dev/null || true
echo "Installed: $data/applications/earshot.desktop (runs $exe)"
