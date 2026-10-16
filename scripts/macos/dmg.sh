#!/usr/bin/env bash
# Builds target/MacWin-<version>-macos-arm64.dmg: MacWin.app (built and signed
# by bundle.sh), the installer ("Install MacWin.command", the same script as
# install-macos.sh), first-launch instructions, and a link to /Applications.
#
#   scripts/macos/dmg.sh             # builds the app first
#   scripts/macos/dmg.sh --no-build  # packs the existing target/MacWin.app
#
# With a "Developer ID Application" identity in MACWIN_SIGN_IDENTITY the dmg
# is signed and, given credentials, notarized and stapled, so it opens on any
# Mac without a Gatekeeper prompt. Notary credentials, either of:
#   MACWIN_NOTARY_PROFILE   a `xcrun notarytool store-credentials` profile
#   APPLE_ID, APPLE_TEAM_ID, APPLE_APP_PASSWORD (an app-specific password)
# Test builds (ad-hoc signature) are not notarized: install them with
# install-macos.sh, which does not trigger Gatekeeper.
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
DMG="$ROOT/target/MacWin-$VERSION-macos-arm64.dmg"
IDENTITY="${MACWIN_SIGN_IDENTITY:-}"

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
# ditto keeps the bundle's signature and extended attributes intact.
ditto "$APP" "$STAGE/MacWin.app"
cp "$ROOT/scripts/macos/install-macos.sh" "$STAGE/Install MacWin.command"
chmod +x "$STAGE/Install MacWin.command"
cp "$ROOT/scripts/macos/first-launch.txt" "$STAGE/First Launch.txt"
ln -s /Applications "$STAGE/Applications"
rm -f "$ROOT"/target/MacWin-*.dmg
hdiutil create -volname "MacWin $VERSION" -srcfolder "$STAGE" -fs HFS+ -format UDZO -ov "$DMG" >/dev/null

case "$IDENTITY" in
"" | -) ;;
"Developer ID Application:"*)
    codesign --force --timestamp --sign "$IDENTITY" "$DMG"
    notary=()
    if [[ -n "${MACWIN_NOTARY_PROFILE:-}" ]]; then
        notary=(--keychain-profile "$MACWIN_NOTARY_PROFILE")
    elif [[ -n "${APPLE_ID:-}" && -n "${APPLE_TEAM_ID:-}" && -n "${APPLE_APP_PASSWORD:-}" ]]; then
        notary=(--apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_PASSWORD")
    fi
    if ((${#notary[@]})); then
        echo "notarizing $DMG (this takes a few minutes)"
        out="$(xcrun notarytool submit "$DMG" "${notary[@]}" --wait --timeout 45m --output-format json)"
        echo "$out"
        if ! grep -q '"status" *: *"Accepted"' <<<"$out"; then
            id="$(sed -n 's/.*"id" *: *"\([^"]*\)".*/\1/p' <<<"$out" | head -1)"
            [[ -n "$id" ]] && xcrun notarytool log "$id" "${notary[@]}" || true
            echo "notarization failed" >&2
            exit 1
        fi
        xcrun stapler staple "$DMG"
        xcrun stapler validate "$DMG"
        spctl --assess --type open --context context:primary-signature --verbose=2 "$DMG"
    else
        echo "WARNING: no notary credentials; the dmg is signed but not notarized." >&2
    fi
    ;;
*)
    codesign --force --timestamp=none --sign "$IDENTITY" "$DMG"
    ;;
esac
hdiutil verify "$DMG" >/dev/null
echo "built $DMG"
