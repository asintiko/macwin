#!/usr/bin/env bash
# Builds target/macwin-<version>-windows-x64.zip: macwin.exe (command line),
# macwin-tray.exe (notification area), install.cmd / install.ps1,
# uninstall.ps1, README.txt and the license notices.
#
# From a Mac or Linux this cross-builds with mingw-w64 (brew install
# mingw-w64); on Windows set MACWIN_WINDOWS_TARGET=x86_64-pc-windows-msvc.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
TARGET="${MACWIN_WINDOWS_TARGET:-x86_64-pc-windows-gnu}"

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
if [[ "$TARGET" == *-windows-msvc ]]; then
    # The package must not require a separate Visual C++ redistributable.
    export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C target-feature=+crt-static"
fi
cargo build --release --locked -p macwin-agent --target "$TARGET"

NAME="macwin-$VERSION-windows-x64"
ZIP="$ROOT/target/$NAME.zip"
TMP_ZIP="$ZIP.tmp.$$"
STAGE="$(mktemp -d)"
rm -f "$TMP_ZIP"
trap 'rm -rf "$STAGE"; rm -f "$TMP_ZIP"' EXIT
mkdir "$STAGE/$NAME"
cp "target/$TARGET/release/macwin.exe" "target/$TARGET/release/macwin-tray.exe" \
    scripts/windows/install.cmd scripts/windows/install.ps1 scripts/windows/uninstall.ps1 \
    scripts/windows/README.txt THIRD_PARTY_NOTICES "$STAGE/$NAME/"
cd "$STAGE"
if command -v zip >/dev/null; then
    zip -qr "$TMP_ZIP" "$NAME"
elif command -v python3 >/dev/null; then
    python3 -m zipfile -c "$TMP_ZIP" "$NAME"
else
    # Windows: Git Bash has no zip; its Python is called python.
    python -m zipfile -c "$TMP_ZIP" "$NAME"
fi
mv -f "$TMP_ZIP" "$ZIP"
echo "built $ZIP"
