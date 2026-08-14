#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: verify-macos-dmg.sh <dmg> <arm64|x86_64>" >&2
  exit 2
fi

dmg=$1
expected_arch=$2
[[ -f $dmg ]] || { echo "missing DMG: $dmg" >&2; exit 1; }
[[ $expected_arch == arm64 || $expected_arch == x86_64 ]] \
  || { echo "unsupported macOS architecture: $expected_arch" >&2; exit 2; }

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
mount_point=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/aizu-release-verify.XXXXXX")
cleanup() {
  hdiutil detach -quiet "$mount_point" 2>/dev/null || true
  rmdir "$mount_point" 2>/dev/null || true
}
trap cleanup EXIT HUP INT TERM

hdiutil verify -quiet "$dmg"
hdiutil attach -quiet -readonly -nobrowse -mountpoint "$mount_point" "$dmg"
app="$mount_point/Aizu.app"
[[ -d $app ]] || { echo "Aizu.app is missing from the DMG" >&2; exit 1; }
[[ -f $mount_point/Applications ]] || { echo "Applications alias is missing" >&2; exit 1; }
case $(GetFileInfo -a "$mount_point/Applications") in
  *A*) ;;
  *) echo "Applications item is not a Finder alias" >&2; exit 1 ;;
esac
[[ -s $mount_point/Applications/..namedfork/rsrc ]] \
  || { echo "Applications alias icon resource is missing" >&2; exit 1; }
background="$mount_point/.background/background.png"
[[ -f $background ]] || { echo "DMG background is missing" >&2; exit 1; }
[[ $(sips -g pixelWidth "$background" | awk '/pixelWidth:/ { print $2 }') == 660 ]]
[[ $(sips -g pixelHeight "$background" | awk '/pixelHeight:/ { print $2 }') == 400 ]]
codesign --verify --deep --strict "$app"
[[ $(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$app/Contents/Info.plist") == dev.aizu.desktop ]]
"$root/scripts/verify-audio-resources.sh" "$app"
[[ $(lipo -archs "$app/Contents/Resources/bin/aizu") == "$expected_arch" ]]

echo "verified $dmg"
