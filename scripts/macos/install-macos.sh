#!/usr/bin/env bash
# Installs or updates MacWin on this Mac, without Gatekeeper prompts.
#
#   curl -fsSL https://github.com/djjdjdkdjdvdvd/macwin/releases/latest/download/install-macos.sh | bash
#
# Run from the dmg ("Install MacWin.command") it installs the MacWin.app next
# to it; otherwise it downloads the dmg of the latest release with curl, which
# does not mark it as downloaded from the internet. Then it:
# - quits a running MacWin and waits for it to stop sharing,
# - copies MacWin.app to /Applications and clears the quarantine attribute,
# - signs a test build (ad-hoc) for this Mac; a Developer ID build keeps its
#   notarized signature,
# - resets the Accessibility and Input Monitoring entries when the signature
#   changed: macOS keeps a stale entry that looks granted but no longer
#   works, so MacWin asks again,
# - opens MacWin.
#
# MACWIN_REPO=owner/repo or MACWIN_DMG_URL=... choose another download.
set -euo pipefail

REPO="${MACWIN_REPO:-djjdjdkdjdvdvd/macwin}"
URL="${MACWIN_DMG_URL:-https://github.com/$REPO/releases/latest/download/MacWin-macos-arm64.dmg}"
BUNDLE_ID="io.macwin.MacWin"
DST="/Applications/MacWin.app"
EXE="$DST/Contents/MacOS/macwin"

say() { printf '%s\n' "$*"; }
die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

[[ "$(uname -s)" == "Darwin" ]] || die "this installer is for macOS"
[[ "$(uname -m)" == "arm64" ]] || die "MacWin needs a Mac with Apple Silicon (M1 or later)"
major="$(sw_vers -productVersion | cut -d. -f1)"
((major >= 15)) || die "MacWin needs macOS 15 or later (this Mac has $(sw_vers -productVersion))"

MNT=""
TMP=""
cleanup() {
    if [[ -n "$MNT" ]]; then
        hdiutil detach "$MNT" -quiet 2>/dev/null || true
        rmdir "$MNT" 2>/dev/null || true
    fi
    if [[ -n "$TMP" ]]; then
        rm -rf "$TMP"
    fi
}
trap cleanup EXIT

# The app next to this script (run from the dmg), or a download.
SRC=""
here="${BASH_SOURCE[0]:-}"
if [[ -n "$here" && -f "$here" ]]; then
    dir="$(cd "$(dirname "$here")" && pwd)"
    if [[ -d "$dir/MacWin.app" ]]; then
        SRC="$dir/MacWin.app"
    fi
fi
if [[ -z "$SRC" ]]; then
    TMP="$(mktemp -d /tmp/macwin-install.XXXXXX)"
    say "Downloading $URL"
    curl -fL --progress-bar "$URL" -o "$TMP/MacWin.dmg" || die "cannot download $URL"
    MNT="$TMP/mnt"
    mkdir "$MNT"
    hdiutil attach "$TMP/MacWin.dmg" -mountpoint "$MNT" -nobrowse -readonly -quiet ||
        die "cannot open the downloaded dmg"
    SRC="$MNT/MacWin.app"
    [[ -d "$SRC" ]] || die "the dmg has no MacWin.app"
fi

cdhash() {
    codesign -dvvv "$1" 2>&1 | sed -n 's/^CDHash=//p' | head -1
}
developer_id() {
    codesign -dvv "$1" 2>&1 | grep -q '^Authority=Developer ID Application:'
}

old_hash=""
if [[ -d "$DST" ]]; then
    old_hash="$(cdhash "$DST" || true)"
fi

# Quit a running MacWin the normal way, so it stops sharing and releases the
# keyboard and mouse; force it only if it does not stop.
if pgrep -f "$EXE" >/dev/null 2>&1; then
    say "Quitting the running MacWin…"
    osascript -e "tell application id \"$BUNDLE_ID\" to quit" >/dev/null 2>&1 || true
    for _ in $(seq 1 50); do
        pgrep -f "$EXE" >/dev/null 2>&1 || break
        sleep 0.2
    done
    if pgrep -f "$EXE" >/dev/null 2>&1; then
        pkill -TERM -f "$EXE" 2>/dev/null || true
        sleep 1
        pkill -KILL -f "$EXE" 2>/dev/null || true
    fi
fi

rm -rf "$DST"
ditto "$SRC" "$DST"
xattr -dr com.apple.quarantine "$DST" 2>/dev/null || true
if developer_id "$DST"; then
    say "Signed by a Developer ID and notarized: keeping the signature."
else
    # A test build: sign it for this Mac.
    codesign --force --timestamp=none --sign - "$DST"
fi
codesign --verify --strict "$DST" || die "the installed app fails its signature check"

new_hash="$(cdhash "$DST" || true)"
if [[ -n "$old_hash" && "$old_hash" != "$new_hash" ]]; then
    tccutil reset Accessibility "$BUNDLE_ID" >/dev/null 2>&1 || true
    tccutil reset ListenEvent "$BUNDLE_ID" >/dev/null 2>&1 || true
    say "The signature changed: macOS will ask for Accessibility and Input Monitoring again."
fi

say "MacWin is installed in $DST. Opening it…"
open "$DST"
