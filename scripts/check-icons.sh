#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

run_node() {
  if [[ -n "${AIZU_NODE:-}" ]]; then
    "$AIZU_NODE" "$@"
    return
  fi
  if command -v mise >/dev/null 2>&1; then
    mise exec -- node "$@"
  else
    node "$@"
  fi
}

run_node scripts/generate-icons.mjs --check
run_node scripts/inspect-icons.mjs
run_node scripts/verify-agent-assets.mjs

dmg_background="assets/branding/dmg/background.svg"
if command -v xmllint >/dev/null 2>&1; then
  xmllint --noout "$dmg_background"
else
  python3 -c 'import sys, xml.etree.ElementTree as ET; ET.parse(sys.argv[1])' "$dmg_background"
fi
