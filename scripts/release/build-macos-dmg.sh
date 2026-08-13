#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 4 ]]; then
  echo "usage: build-macos-dmg.sh <app-bundle> <version> <arch> <output-dir>" >&2
  exit 2
fi

app=$1
version=$2
arch=$3
output_dir=$4
[[ -d $app ]] || { echo "missing application bundle: $app" >&2; exit 1; }
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
stage=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/aizu-release-dmg.XXXXXX")
read_write_image="$stage/Aizu-layout.dmg"
payload="$stage/payload"
volume_name="Aizu $version"
mount_point="/Volumes/$volume_name"
attached=0
cleanup() {
  if [[ $attached -eq 1 ]]; then hdiutil detach -quiet "$mount_point" 2>/dev/null || true; fi
  rm -rf -- "$stage"
}
trap cleanup EXIT HUP INT TERM

[[ ! -e $mount_point ]] || { echo "eject the existing '$volume_name' disk image before building" >&2; exit 1; }
mkdir -p "$payload/.background" "$output_dir"
ditto "$app" "$payload/Aizu.app"
sips -s format png "$root/assets/branding/dmg/background.svg" \
  --out "$payload/.background/background.png" >/dev/null
width=$(sips -g pixelWidth "$payload/.background/background.png" | awk '/pixelWidth:/ { print $2 }')
height=$(sips -g pixelHeight "$payload/.background/background.png" | awk '/pixelHeight:/ { print $2 }')
[[ $width == 660 && $height == 400 ]] \
  || { echo "invalid DMG background dimensions: ${width}x${height}" >&2; exit 1; }

hdiutil create -quiet -ov -format UDRW -volname "$volume_name" -srcfolder "$payload" "$read_write_image"
hdiutil attach -quiet -readwrite -noverify -noautoopen -mountpoint "$mount_point" "$read_write_image"
attached=1

finder_ready=0
for _ in $(seq 1 50); do
  if [[ $(osascript -e "tell application \"Finder\" to exists disk \"$volume_name\"") == true ]]; then
    finder_ready=1
    break
  fi
  sleep 0.1
done
[[ $finder_ready -eq 1 ]] || { echo "Finder did not register '$volume_name'" >&2; exit 1; }

osascript <<APPLESCRIPT
tell application "Finder"
  set applicationsFolder to folder "Applications" of startup disk
  tell disk "$volume_name"
    make new alias file at it to applicationsFolder with properties {name:"Applications"}
  end tell
end tell
APPLESCRIPT

applications_icon="$stage/ApplicationsFolderIcon.icns"
applications_resource="$stage/ApplicationsFolderIcon.rsrc"
cp /System/Library/CoreServices/CoreTypes.bundle/Contents/Resources/ApplicationsFolderIcon.icns \
  "$applications_icon"
sips -i "$applications_icon" >/dev/null
DeRez -only icns "$applications_icon" > "$applications_resource"
Rez -append "$applications_resource" -o "$mount_point/Applications"
SetFile -a C "$mount_point/Applications"
SetFile -a V "$mount_point/.background"

osascript <<APPLESCRIPT
tell application "Finder"
  tell disk "$volume_name"
    open
    set current view of container window to icon view
    set toolbar visible of container window to false
    set statusbar visible of container window to false
    set bounds of container window to {180, 180, 840, 580}
    set viewOptions to the icon view options of container window
    set arrangement of viewOptions to not arranged
    set icon size of viewOptions to 112
    set text size of viewOptions to 14
    set background picture of viewOptions to file ".background:background.png"
    set position of item "Aizu.app" to {170, 190}
    set position of item "Applications" to {490, 190}
    update without registering applications
    delay 1
    close
  end tell
end tell
APPLESCRIPT

SetFile -a V "$mount_point/.DS_Store"
sync
hdiutil detach -quiet "$mount_point"
attached=0
hdiutil convert -quiet -ov -format UDZO -imagekey zlib-level=9 \
  -o "$output_dir/Aizu_${version}_${arch}.dmg" "$read_write_image"
hdiutil verify -quiet "$output_dir/Aizu_${version}_${arch}.dmg"
