#!/bin/sh
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
source_app="$root/target/debug/bundle/macos/Aizu.app"
stage=$(mktemp -d "${TMPDIR:-/tmp}/aizu-dmg.XXXXXX")
read_write_image="$stage/Aizu-layout.dmg"
attached=0

cleanup() {
  if [ "$attached" -eq 1 ]; then
    /usr/bin/hdiutil detach -quiet "$mount_point" || true
  fi
  rm -rf -- "$stage"
}
trap cleanup EXIT HUP INT TERM

cd "$root"
if command -v mise >/dev/null 2>&1; then
  RUSTUP_TOOLCHAIN=1.97.1 mise exec -- pnpm --filter @aizu/desktop exec tauri build --debug --bundles app
else
  RUSTUP_TOOLCHAIN=1.97.1 pnpm --filter @aizu/desktop exec tauri build --debug --bundles app
fi

version=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$source_app/Contents/Info.plist")
identifier=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$source_app/Contents/Info.plist")
if [ "$identifier" != "dev.aizu.desktop" ]; then
  printf '%s\n' "refusing to package an unexpected bundle identifier" >&2
  exit 1
fi

payload="$stage/payload"
mkdir -p "$payload"
/usr/bin/ditto "$source_app" "$payload/Aizu.app"
/usr/bin/codesign --force --deep --sign - "$payload/Aizu.app"
/usr/bin/codesign --verify --deep --strict "$payload/Aizu.app"
mkdir -p "$payload/.background"
/usr/bin/sips -s format png "$root/assets/branding/dmg/background.svg" \
  --out "$payload/.background/background.png" >/dev/null
background_width=$(/usr/bin/sips -g pixelWidth "$payload/.background/background.png" \
  | /usr/bin/awk '/pixelWidth:/ { print $2 }')
background_height=$(/usr/bin/sips -g pixelHeight "$payload/.background/background.png" \
  | /usr/bin/awk '/pixelHeight:/ { print $2 }')
if [ "$background_width" != "660" ] || [ "$background_height" != "400" ]; then
  printf '%s\n' "refusing DMG background ${background_width}x${background_height}; expected 660x400" >&2
  exit 1
fi

architecture=$(uname -m)
output_directory="$root/target/debug/bundle/dmg"
output="$output_directory/Aizu_${version}_${architecture}.dmg"
mkdir -p "$output_directory"
volume_name="Aizu $version"
mount_point="/Volumes/$volume_name"
if [ -e "$mount_point" ]; then
  printf '%s\n' "eject the existing '$volume_name' disk image before building" >&2
  exit 1
fi

# Finder persists a disk image window's presentation in .DS_Store. Create a
# writable image first so the installer opens as a compact drag-to-install
# window instead of an unstyled directory.
/usr/bin/hdiutil create -quiet -ov -format UDRW -volname "$volume_name" \
  -srcfolder "$payload" "$read_write_image"
/usr/bin/hdiutil attach -quiet -readwrite -noverify -noautoopen \
  -mountpoint "$mount_point" "$read_write_image"
attached=1

finder_ready=0
attempt=0
while [ "$attempt" -lt 50 ]; do
  if [ "$(/usr/bin/osascript -e "tell application \"Finder\" to exists disk \"$volume_name\"")" = true ]; then
    finder_ready=1
    break
  fi
  attempt=$((attempt + 1))
  sleep 0.1
done
if [ "$finder_ready" -ne 1 ]; then
  printf '%s\n' "Finder did not register '$volume_name'" >&2
  exit 1
fi

/usr/bin/osascript <<APPLESCRIPT
tell application "Finder"
  set applicationsFolder to folder "Applications" of startup disk
  tell disk "$volume_name"
    make new alias file at it to applicationsFolder with properties {name:"Applications"}
  end tell
end tell
APPLESCRIPT

# Finder does not consistently render the target icon for aliases on a
# read-only disk image. Preserve the system Applications folder artwork in the
# alias resource fork so the drag target is never an empty placeholder.
applications_icon="$stage/ApplicationsFolderIcon.icns"
applications_resource="$stage/ApplicationsFolderIcon.rsrc"
/bin/cp /System/Library/CoreServices/CoreTypes.bundle/Contents/Resources/ApplicationsFolderIcon.icns \
  "$applications_icon"
/usr/bin/sips -i "$applications_icon" >/dev/null
/usr/bin/DeRez -only icns "$applications_icon" > "$applications_resource"
/usr/bin/Rez -append "$applications_resource" -o "$mount_point/Applications"
/usr/bin/SetFile -a C "$mount_point/Applications"
/usr/bin/SetFile -a V "$mount_point/.background"

/usr/bin/osascript <<APPLESCRIPT
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

/usr/bin/SetFile -a V "$mount_point/.DS_Store"
/bin/sync
/usr/bin/hdiutil detach -quiet "$mount_point"
attached=0
/usr/bin/hdiutil convert -quiet -ov -format UDZO -imagekey zlib-level=9 \
  -o "$output" "$read_write_image"
/usr/bin/hdiutil verify -quiet "$output"

printf '%s\n' "$output"
