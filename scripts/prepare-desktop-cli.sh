#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
destination="$repo_root/apps/desktop/src-tauri/resources/bin/aizu"

cd "$repo_root"
build_args=(--locked --release -p aizu-cli)
binary="$repo_root/target/release/aizu"
if [[ -n ${AIZU_BUNDLED_CLI_TARGET:-} ]]; then
  build_args+=(--target "$AIZU_BUNDLED_CLI_TARGET")
  binary="$repo_root/target/$AIZU_BUNDLED_CLI_TARGET/release/aizu"
fi
cargo build "${build_args[@]}"
install -d -m 700 "$(dirname "$destination")"
install -m 755 "$binary" "$destination"
