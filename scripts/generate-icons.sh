#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

if [[ -n "${AIZU_NODE:-}" ]]; then
  exec "$AIZU_NODE" scripts/generate-icons.mjs "$@"
fi

if command -v mise >/dev/null 2>&1; then
  exec mise exec -- node scripts/generate-icons.mjs "$@"
fi

exec node scripts/generate-icons.mjs "$@"
