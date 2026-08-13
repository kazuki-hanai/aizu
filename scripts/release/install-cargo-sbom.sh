#!/usr/bin/env bash
set -euo pipefail

version=0.10.0
expected=4ffe4b49660f4f4331fb5efcf7074a318b10f5f8fd75e42351a7ca32c58c2723

if cargo sbom --version 2>/dev/null | grep -F "cargo-sbom $version" >/dev/null; then
  exit 0
fi

stage=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/cargo-sbom.XXXXXX")
cleanup() { rm -rf -- "$stage"; }
trap cleanup EXIT HUP INT TERM
archive="$stage/cargo-sbom-$version.crate"
curl --fail --location --silent --show-error --user-agent "cargo/${CARGO_VERSION:-1.97.1} (aizu-release)" \
  --output "$archive" "https://crates.io/api/v1/crates/cargo-sbom/$version/download"
actual=$(shasum -a 256 "$archive" | awk '{print $1}')
[[ $actual == "$expected" ]] || { echo "cargo-sbom source checksum mismatch" >&2; exit 1; }
tar -xzf "$archive" -C "$stage"
cargo install --path "$stage/cargo-sbom-$version" --locked --force
cargo sbom --version | grep -F "cargo-sbom $version"
