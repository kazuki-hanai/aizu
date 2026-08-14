#!/usr/bin/env bash
set -euo pipefail

if [[ $(uname -s) != Darwin ]]; then
  echo "DMG verification requires macOS" >&2
  exit 1
fi

dmg=${1:-}
if [[ -z $dmg ]]; then
  dmg=$(find target/debug/bundle/dmg -maxdepth 1 -name 'Aizu_*.dmg' -print -quit)
fi
if [[ -z $dmg || ! -f $dmg ]]; then
  echo "Aizu DMG was not found" >&2
  exit 1
fi

hdiutil verify -quiet "$dmg"
mount_point=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/aizu-dmg.XXXXXX")
cleanup() {
  hdiutil detach -quiet "$mount_point" 2>/dev/null || true
  rmdir "$mount_point" 2>/dev/null || true
}
trap cleanup EXIT HUP INT TERM
hdiutil attach -quiet -readonly -nobrowse -mountpoint "$mount_point" "$dmg"

app="$mount_point/Aizu.app"
test -d "$app"
test -f "$mount_point/Applications"
attributes=$(GetFileInfo -a "$mount_point/Applications")
case $attributes in
  *A*) ;;
  *) echo "Applications item is not a Finder alias" >&2; exit 1 ;;
esac
test -s "$mount_point/Applications/..namedfork/rsrc"
test -f "$mount_point/.background/background.png"
test "$(sips -g pixelWidth "$mount_point/.background/background.png" | awk '/pixelWidth:/ {print $2}')" = 660
test "$(sips -g pixelHeight "$mount_point/.background/background.png" | awk '/pixelHeight:/ {print $2}')" = 400
codesign --verify --deep --strict "$app"
test "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$app/Contents/Info.plist")" = dev.aizu.desktop
scripts/verify-audio-resources.sh "$app"
"$app/Contents/Resources/bin/aizu" version --json >/dev/null

echo "verified $dmg"
