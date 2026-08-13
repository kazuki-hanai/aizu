#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
source_app=${1:-"$root/target/debug/bundle/macos/Aizu.app"}
target_app=${2:-"/Applications/Aizu.app"}
expected_identifier=dev.aizu.desktop

bundle_identifier() {
  /usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$1/Contents/Info.plist" 2>/dev/null
}

if [ ! -d "$source_app" ] || [ "$(bundle_identifier "$source_app")" != "$expected_identifier" ]; then
  printf '%s\n' "refusing to install an invalid Aizu bundle" >&2
  exit 1
fi

target_parent=$(dirname -- "$target_app")
stage="$target_parent/.Aizu.install-$$.app"
backup="${TMPDIR:-/tmp}/Aizu.preinstall-$$.app"

cleanup() {
  if [ -d "$stage" ]; then
    rm -rf -- "$stage"
  fi
}
trap cleanup EXIT HUP INT TERM

/usr/bin/ditto "$source_app" "$stage"
/usr/bin/codesign --force --deep --sign - "$stage"
/usr/bin/codesign --verify --deep --strict "$stage"

if [ -e "$target_app" ]; then
  if [ ! -d "$target_app" ] || [ "$(bundle_identifier "$target_app")" != "$expected_identifier" ]; then
    printf '%s\n' "refusing to replace a non-Aizu application at $target_app" >&2
    exit 1
  fi
  mv -- "$target_app" "$backup"
  printf '%s\n' "previous Aizu bundle moved to $backup"
fi

mv -- "$stage" "$target_app"
/usr/bin/codesign --verify --deep --strict "$target_app"
printf '%s\n' "installed $target_app"
