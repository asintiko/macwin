#!/usr/bin/env bash
# One-step macOS installer for MacWin:
# - Uses an already downloaded ~/Downloads/MacWin*.dmg if present, or downloads
#   the latest release DMG with curl (which does not set com.apple.quarantine).
# - Strips browser quarantine attributes, installs MacWin.app into /Applications,
#   re-signs it locally for this Mac, and opens MacWin.app.
set -euo pipefail

URL="${MACWIN_DMG_URL:-https://github.com/kojikor/macwin/releases/download/v0.1.0-test.1/MacWin-0.1.0-macos-arm64.dmg}"
MNT="$(mktemp -d /tmp/macwin-mnt.XXXXXX)"
TMP_DMG=""

cleanup() {
    hdiutil detach "$MNT" -quiet 2>/dev/null || true
    rmdir "$MNT" 2>/dev/null || true
    if [[ -n "$TMP_DMG" && -f "$TMP_DMG" ]]; then
        rm -f "$TMP_DMG"
    fi
}
trap cleanup EXIT

DMG="$(ls -t "$HOME"/Downloads/MacWin*.dmg 2>/dev/null | head -1 || true)"
if [[ -n "$DMG" && -f "$DMG" ]]; then
    echo "Using downloaded DMG: $DMG"
    xattr -cr "$DMG" 2>/dev/null || true
else
    TMP_DMG="$(mktemp /tmp/MacWin.XXXXXX.dmg)"
    echo "Downloading $URL ..."
    curl -fL "$URL" -o "$TMP_DMG"
    DMG="$TMP_DMG"
fi

hdiutil attach "$DMG" -mountpoint "$MNT" -nobrowse -readonly -quiet
pkill -INT -f "/Applications/MacWin.app/Contents/MacOS/macwin" 2>/dev/null || true
rm -rf /Applications/MacWin.app
ditto "$MNT/MacWin.app" /Applications/MacWin.app
xattr -cr /Applications/MacWin.app 2>/dev/null || true
codesign --force --deep --timestamp=none --sign - /Applications/MacWin.app
echo "MacWin installed to /Applications/MacWin.app and launched."
open /Applications/MacWin.app
