#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
destination="$repo_root/apps/desktop/src-tauri/resources/bin/aizu"

cd "$repo_root"
cargo build --locked --release -p aizu-cli
install -d -m 700 "$(dirname "$destination")"
install -m 755 "$repo_root/target/release/aizu" "$destination"
