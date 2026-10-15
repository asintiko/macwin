#!/usr/bin/env bash
# Builds target/MacWin-<version>.dmg: MacWin.app (built and signed by
# bundle.sh), first-launch instructions, and a link to /Applications.
#
#   scripts/macos/dmg.sh             # builds the app first
#   scripts/macos/dmg.sh --no-build  # packs the existing target/MacWin.app
#
# The app is not notarized: on another Mac, Gatekeeper blocks its first
# launch until it is allowed (right-click > Open, or System Settings >
# Privacy & Security > Open Anyway).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
if [[ "${1:-}" != "--no-build" ]]; then
    scripts/macos/bundle.sh
fi
APP="$ROOT/target/MacWin.app"
if [[ ! -d "$APP" ]]; then
    echo "MacWin.app is missing: run scripts/macos/bundle.sh first" >&2
    exit 1
fi
VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist")"
DMG="$ROOT/target/MacWin-$VERSION.dmg"

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
# ditto keeps the bundle's signature and extended attributes intact.
ditto "$APP" "$STAGE/MacWin.app"
cp "$ROOT/scripts/macos/first-launch.txt" "$STAGE/First Launch.txt"
ln -s /Applications "$STAGE/Applications"
rm -f "$DMG"
hdiutil create -volname "MacWin $VERSION" -srcfolder "$STAGE" -fs HFS+ -format UDZO -ov "$DMG" >/dev/null
hdiutil verify "$DMG" >/dev/null
echo "built $DMG"
