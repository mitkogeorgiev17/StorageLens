#!/usr/bin/env bash
# Sign a built StorageLens .app with the project's self-signed certificate
# (#188), so every version shares one designated requirement and macOS keeps
# its permissions (Full Disk Access, ...) across updates. Free: no Apple
# Developer ID. Gatekeeper does not trust the certificate, so a browser
# download still warns once; install.sh avoids that.
#
#   scripts/sign-macos.sh "path/to/StorageLens.app"
#
# The certificate comes from SIGNING_P12_BASE64 + SIGNING_P12_PASSWORD (CI
# secrets) or, locally, ~/.storagelens-signing/. Without either, the app keeps
# its ad-hoc signature and this says so.
set -euo pipefail

app=${1:?usage: sign-macos.sh <app>}
identity="StorageLens Self-Signed"
local_dir="$HOME/.storagelens-signing"
work=$(mktemp -d)
trap 'security delete-keychain "$work/sign.keychain-db" 2>/dev/null || true; rm -rf "$work"' EXIT

if [[ -n "${SIGNING_P12_BASE64:-}" ]]; then
  printf '%s' "$SIGNING_P12_BASE64" | base64 --decode > "$work/cert.p12"
  password=${SIGNING_P12_PASSWORD:?SIGNING_P12_PASSWORD is not set}
elif [[ -f "$local_dir/StorageLens-signing.p12" ]]; then
  cp "$local_dir/StorageLens-signing.p12" "$work/cert.p12"
  password=$(cat "$local_dir/p12-password.txt")
else
  echo "sign-macos: no certificate found; $app stays ad-hoc signed" >&2
  exit 0
fi

keychain="$work/sign.keychain-db"
keychain_password=$(openssl rand -hex 16)
security create-keychain -p "$keychain_password" "$keychain"
security set-keychain-settings "$keychain"
security unlock-keychain -p "$keychain_password" "$keychain"
security import "$work/cert.p12" -k "$keychain" -P "$password" -T /usr/bin/codesign >/dev/null
security set-key-partition-list -S apple-tool:,apple: -s -k "$keychain_password" "$keychain" >/dev/null

codesign --force --deep --sign "$identity" --keychain "$keychain" "$app"
codesign --verify --deep --strict "$app"
echo "sign-macos: signed $app as \"$identity\""
