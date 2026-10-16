#!/usr/bin/env bash
# CI only: imports the Developer ID Application certificate into a temporary
# keychain and exports MACWIN_SIGN_IDENTITY for the following steps.
#   MACOS_CERTIFICATE_P12       the .p12 (certificate and key), base64
#   MACOS_CERTIFICATE_PASSWORD  its password
set -euo pipefail
: "${MACOS_CERTIFICATE_P12:?}" "${MACOS_CERTIFICATE_PASSWORD:?}" "${RUNNER_TEMP:?}" "${GITHUB_ENV:?}"

KEYCHAIN="$RUNNER_TEMP/macwin-signing.keychain-db"
P12="$RUNNER_TEMP/macwin-signing.p12"
PASS="$(openssl rand -hex 24)"
trap 'rm -f "$P12"' EXIT

printf '%s' "$MACOS_CERTIFICATE_P12" | base64 --decode >"$P12"
security create-keychain -p "$PASS" "$KEYCHAIN"
security set-keychain-settings -lut 21600 "$KEYCHAIN"
security unlock-keychain -p "$PASS" "$KEYCHAIN"
security import "$P12" -k "$KEYCHAIN" -P "$MACOS_CERTIFICATE_PASSWORD" -f pkcs12 \
    -T /usr/bin/codesign -T /usr/bin/security
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$PASS" "$KEYCHAIN" >/dev/null
# shellcheck disable=SC2046 # the existing keychains, one word each
security list-keychains -d user -s "$KEYCHAIN" $(security list-keychains -d user | tr -d '"')

IDENTITY="$(security find-identity -v -p codesigning "$KEYCHAIN" |
    sed -n 's/.*"\(Developer ID Application: [^"]*\)".*/\1/p' | head -1)"
if [[ -z "$IDENTITY" ]]; then
    echo "the certificate has no Developer ID Application identity" >&2
    exit 1
fi
echo "MACWIN_SIGN_IDENTITY=$IDENTITY" >>"$GITHUB_ENV"
echo "signing with: $IDENTITY"
