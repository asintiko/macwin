#!/usr/bin/env bash
# Installs MacWin.app into /Applications, clears the browser download quarantine
# attribute, re-signs the bundle locally so macOS Gatekeeper never blocks it
# with "MacWin Not Opened / Apple could not verify", and launches MacWin.
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
SRC="$DIR/MacWin.app"
DST="/Applications/MacWin.app"

if [[ ! -d "$SRC" ]]; then
    echo "error: MacWin.app not found next to this installer." >&2
    exit 1
fi

pkill -INT -f "/Applications/MacWin.app/Contents/MacOS/macwin" 2>/dev/null || true
rm -rf "$DST"
ditto "$SRC" "$DST"
xattr -cr "$DST" 2>/dev/null || true
codesign --force --deep --timestamp=none --sign - "$DST"
echo "MacWin installed to $DST and locally signed."
open "$DST"
