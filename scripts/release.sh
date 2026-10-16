#!/usr/bin/env bash
# CI: publishes the GitHub release of tag $1 from the packages in folder $2
# (the CI artifacts). The files get stable names, so the installers can
# always fetch releases/latest/download/<name>:
#   MacWin-macos-arm64.dmg, MacWin-windows-x64.exe, MacWin-windows-x64.zip,
#   install-macos.sh, install-windows.ps1 and SHA256SUMS.txt.
# Needs `gh` with GH_TOKEN allowed to write the repository's contents.
set -euo pipefail
TAG="${1:?usage: release.sh TAG DIST}"
DIST="${2:?usage: release.sh TAG DIST}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$(mktemp -d)"
trap 'rm -rf "$OUT"' EXIT

one() {
    # $1 is a glob on purpose.
    # shellcheck disable=SC2206
    local found=("$DIST"/$1)
    if [[ ${#found[@]} -ne 1 || ! -f "${found[0]}" ]]; then
        echo "expected one $1 in $DIST" >&2
        exit 1
    fi
    cp "${found[0]}" "$OUT/$2"
}
one 'MacWin-*-macos-arm64.dmg' MacWin-macos-arm64.dmg
one 'MacWin-*-windows-x64.exe' MacWin-windows-x64.exe
one 'macwin-*-windows-x64.zip' MacWin-windows-x64.zip
cp "$ROOT/scripts/macos/install-macos.sh" "$ROOT/scripts/windows/install-windows.ps1" "$OUT/"
(cd "$OUT" && sha256sum -- * >SHA256SUMS.txt)

REPO="${GITHUB_REPOSITORY:?}"
cat >"$OUT/notes.md" <<NOTES
MacWin ${TAG#v}: one keyboard, mouse and clipboard for a Mac and a Windows PC.

**Mac** (macOS 15+, Apple Silicon) — in Terminal:

\`\`\`sh
curl -fsSL https://github.com/$REPO/releases/latest/download/install-macos.sh | bash
\`\`\`

**Windows** (x64) — in PowerShell:

\`\`\`powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://github.com/$REPO/releases/latest/download/install-windows.ps1 | iex"
\`\`\`

Or download \`MacWin-macos-arm64.dmg\` / \`MacWin-windows-x64.zip\` (\`MacWin-windows-x64.exe\` runs without installing).
Then open MacWin: choose **Primary** on the Mac, place the PC next to its screens, choose **Secondary** on the PC and enter its code on the Mac.
Checksums: \`SHA256SUMS.txt\`. Built by CI from commit ${GITHUB_SHA:-unknown}.
NOTES

assets=()
for f in "$OUT"/*; do
    [[ "$(basename "$f")" == notes.md ]] || assets+=("$f")
done
gh release create "$TAG" "${assets[@]}" --repo "$REPO" --verify-tag --latest \
    --title "MacWin ${TAG#v}" --notes-file "$OUT/notes.md"
