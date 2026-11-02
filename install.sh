#!/usr/bin/env bash
# Install or update StorageLens on macOS (#188):
#
#   curl -fsSL https://raw.githubusercontent.com/mitkogeorgiev17/StorageLens/main/install.sh | bash
#
# Downloads the latest release, replaces /Applications/StorageLens.app, and
# opens it. A download made here (not by a browser) carries no quarantine flag,
# so macOS opens the app without the "unidentified developer" warning.
set -euo pipefail

repo="mitkogeorgiev17/StorageLens"
url="https://github.com/$repo/releases/latest/download/StorageLens.dmg"
target="/Applications/StorageLens.app"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "StorageLens: this installer is for macOS. Windows is coming soon." >&2
  exit 1
fi

work=$(mktemp -d)
mount="$work/mount"
cleanup() {
  hdiutil detach "$mount" -quiet 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT

echo "Downloading StorageLens…"
if ! curl -fsSL --retry 3 -o "$work/StorageLens.dmg" "$url"; then
  # While the repository is private, only a signed-in GitHub CLI can fetch it.
  if command -v gh >/dev/null 2>&1; then
    gh release download --repo "$repo" --pattern StorageLens.dmg --dir "$work" --clobber
  else
    echo "Could not download $url" >&2
    exit 1
  fi
fi

hdiutil attach -nobrowse -readonly -quiet -mountpoint "$mount" "$work/StorageLens.dmg"

if pgrep -xq StorageLens || pgrep -fq "$target/Contents/MacOS/"; then
  echo "Quitting the running StorageLens…"
  osascript -e 'tell application "StorageLens" to quit' >/dev/null 2>&1 || true
  sleep 2
fi

echo "Installing to ${target}…"
rm -rf "$target"
ditto "$mount/StorageLens.app" "$target"
xattr -dr com.apple.quarantine "$target" 2>/dev/null || true

version=$(/usr/libexec/PlistBuddy -c "Print CFBundleShortVersionString" "$target/Contents/Info.plist")
echo "StorageLens $version is installed. Opening it…"
open "$target"
