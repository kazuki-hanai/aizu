#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "usage: build-macos-targets.sh <rehearsal|publish> <version> <output-dir>" >&2
  exit 2
fi

mode=$1
version=$2
output_dir=$3
[[ $mode == rehearsal || $mode == publish ]] || { echo "invalid release mode: $mode" >&2; exit 2; }
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$root"
mkdir -p "$output_dir"

for spec in aarch64-apple-darwin:aarch64:arm64 x86_64-apple-darwin:x64:x86_64; do
  target=${spec%%:*}
  remainder=${spec#*:}
  arch=${remainder%%:*}
  mach_arch=${spec##*:}
  export AIZU_BUNDLED_CLI_TARGET=$target

  cargo build --locked --release -p aizu-cli --target "$target"
  scripts/release/package-cli.sh "$target" macos "$arch" "$version" "$output_dir"
  if [[ $mode == publish ]]; then
    pnpm --filter @aizu/desktop exec tauri build --ci --target "$target" \
      --bundles app --config src-tauri/tauri.release.conf.json
  else
    pnpm --filter @aizu/desktop exec tauri build --ci --no-sign --target "$target" --bundles app
  fi

  app="target/$target/release/bundle/macos/Aizu.app"
  [[ -d $app ]] || { echo "missing app bundle for $target" >&2; exit 1; }
  [[ $(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$app/Contents/Info.plist") == "$version" ]]
  [[ $(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$app/Contents/Info.plist") == dev.aizu.desktop ]]
  scripts/verify-audio-resources.sh "$app"
  [[ $(lipo -archs "$app/Contents/Resources/bin/aizu") == "$mach_arch" ]]

  if [[ $mode == publish ]]; then
    codesign --verify --deep --strict "$app"
    codesign -dv --verbose=4 "$app" 2>&1 | grep -F 'Authority=Developer ID Application:'
    xcrun stapler validate "$app"
    spctl --assess --type execute --verbose=4 "$app"
    bundle="target/$target/release/bundle"
    updater=$(find "$bundle" -type f -name '*.app.tar.gz' -print -quit)
    updater_signature=$(find "$bundle" -type f -name '*.app.tar.gz.sig' -print -quit)
    [[ -n $updater && -n $updater_signature ]]
    [[ $(find "$bundle" -type f -name '*.app.tar.gz' | wc -l | tr -d ' ') == 1 ]]
    [[ $(find "$bundle" -type f -name '*.app.tar.gz.sig' | wc -l | tr -d ' ') == 1 ]]
    cp "$updater" "$output_dir/Aizu_${version}_${arch}.app.tar.gz"
    cp "$updater_signature" "$output_dir/Aizu_${version}_${arch}.app.tar.gz.sig"
    dmg="$output_dir/Aizu_${version}_${arch}.dmg"
    scripts/release/build-macos-dmg.sh "$app" "$version" "$arch" "$output_dir"
    codesign --force --sign "$APPLE_SIGNING_IDENTITY" --timestamp "$dmg"
    xcrun notarytool submit "$dmg" --key "$APPLE_API_KEY_PATH" \
      --key-id "$APPLE_API_KEY" --issuer "$APPLE_API_ISSUER" --wait
    xcrun stapler staple "$dmg"
    xcrun stapler validate "$dmg"
    spctl --assess --type open --context context:primary-signature --verbose=4 \
      "$dmg"
    scripts/release/verify-macos-dmg.sh "$dmg" "$mach_arch"
  else
    codesign --force --deep --sign - "$app"
    codesign --verify --deep --strict "$app"
    scripts/release/build-macos-dmg.sh "$app" "$version" "$arch" "$output_dir"
    scripts/release/verify-macos-dmg.sh "$output_dir/Aizu_${version}_${arch}.dmg" "$mach_arch"
  fi
done
