#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT

copy_fixture() {
  rm -rf "$fixture/repo"
  mkdir -p "$fixture/repo/apps/desktop/src-tauri" "$fixture/repo/assets" "$fixture/repo/scripts"
  cp -R "$repo_root/apps/desktop/src-tauri/icons" "$fixture/repo/apps/desktop/src-tauri/icons"
  cp -R "$repo_root/assets/branding" "$fixture/repo/assets/branding"
  cp "$repo_root/scripts/check-icons.sh" \
    "$repo_root/scripts/generate-icons.mjs" \
    "$repo_root/scripts/inspect-icons.mjs" \
    "$repo_root/scripts/verify-agent-assets.mjs" \
    "$fixture/repo/scripts/"
}

expect_rejected() {
  local description="$1"
  if AIZU_NODE="$(command -v node)" "$fixture/repo/scripts/check-icons.sh" >/dev/null 2>&1; then
    echo "branding mutation unexpectedly passed: $description" >&2
    exit 1
  fi
}

copy_fixture
printf '\n' >> "$fixture/repo/assets/branding/agents/openai/OAI_OpenAI-Blossom_Black.svg"
expect_rejected "modified official agent asset"

copy_fixture
sed -i.bak 's/#eef2f0/#ffffff/' "$fixture/repo/assets/branding/dmg/background.svg"
rm "$fixture/repo/assets/branding/dmg/background.svg.bak"
expect_rejected "modified DMG background artwork"

copy_fixture
sed -i.bak 's/width="660"/width="6600"/' "$fixture/repo/assets/branding/dmg/background.svg"
rm "$fixture/repo/assets/branding/dmg/background.svg.bak"
expect_rejected "invalid DMG background dimensions"

echo "branding mutation tests passed"
