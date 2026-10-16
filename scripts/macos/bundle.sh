#!/usr/bin/env bash
# Builds target/MacWin.app around the release `macwin` binary and signs it.
#
# macOS ties privacy permissions (Accessibility, Input Monitoring, ...) to the
# code signature, so a STABLE identity keeps them across rebuilds:
#   MACWIN_SIGN_IDENTITY="Developer ID Application: Name (TEAMID)" scripts/macos/bundle.sh
# A "Developer ID Application" identity signs for distribution: Hardened
# Runtime and a secure timestamp, ready for notarization (dmg.sh). Without an
# identity, the first "Apple Development" identity in the keychain is used,
# and as a last resort an ad-hoc signature (permissions then reset on every
# rebuild).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
APP="$ROOT/target/MacWin.app"

# rust-toolchain.toml pins 1.96.0; use `stable` when it is that version and
# the pinned toolchain is not installed (never auto-install).
if [[ -z "${RUSTUP_TOOLCHAIN:-}" ]] && command -v rustup >/dev/null; then
    if ! rustup toolchain list | grep -q '^1\.96\.0'; then
        if rustup run stable rustc --version 2>/dev/null | grep -q '^rustc 1\.96\.0 '; then
            export RUSTUP_TOOLCHAIN=stable
        fi
    fi
fi

VERSION="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)"
cargo build --locked --release -p macwin-agent

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/macwin "$APP/Contents/MacOS/macwin"
cp "$ROOT/assets/AppIcon.icns" "$APP/Contents/Resources/AppIcon.icns"
cp "$ROOT/THIRD_PARTY_NOTICES" "$APP/Contents/Resources/THIRD_PARTY_NOTICES"
sed "s/@VERSION@/$VERSION/g" scripts/macos/Info.plist.in >"$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist" >/dev/null

IDENTITY="${MACWIN_SIGN_IDENTITY:-}"
if [[ -z "$IDENTITY" ]]; then
    IDENTITY="$(security find-identity -v -p codesigning | sed -n 's/.*"\(Apple Development: [^"]*\)".*/\1/p' | head -1)"
fi
if [[ -z "$IDENTITY" ]]; then
    echo "WARNING: no signing identity found; signing ad-hoc." >&2
    echo "         macOS will forget the granted permissions after every rebuild." >&2
    IDENTITY="-"
fi

case "$IDENTITY" in
-)
    # Ad-hoc signature without Hardened Runtime avoids extra Gatekeeper blocks
    # on unnotarized builds in macOS 15+.
    codesign --force --timestamp=none --sign - "$APP"
    ;;
"Developer ID Application:"*)
    # Notarization needs the Hardened Runtime and a secure timestamp.
    codesign --force --options runtime --timestamp --sign "$IDENTITY" "$APP"
    ;;
*)
    codesign --force --options runtime --timestamp=none --sign "$IDENTITY" "$APP"
    ;;
esac
codesign --verify --strict --verbose=2 "$APP"
echo "built $APP (version $VERSION, signed with: $IDENTITY)"
