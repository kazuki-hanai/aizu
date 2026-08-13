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
