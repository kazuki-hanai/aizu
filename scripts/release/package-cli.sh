#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 5 ]]; then
  echo "usage: package-cli.sh <target-triple> <platform> <arch> <version> <output-dir>" >&2
  exit 2
fi

target=$1
platform=$2
arch=$3
version=$4
output_dir=$5
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
binary="$root/target/$target/release/aizu"
archive="aizu-cli_${version}_${platform}-${arch}.tar.gz"
stage=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/aizu-cli.XXXXXX")
cleanup() { rm -rf -- "$stage"; }
trap cleanup EXIT HUP INT TERM

[[ -f $binary && -x $binary ]] || { echo "missing CLI binary: $binary" >&2; exit 1; }
mkdir -p "$output_dir"
install -m 755 "$binary" "$stage/aizu"
TZ=UTC touch -t 198001010000 "$stage/aizu"

if tar --version 2>/dev/null | grep -q 'GNU tar'; then
  COPYFILE_DISABLE=1 tar --format=ustar --owner=0 --group=0 --numeric-owner --sort=name \
    -C "$stage" -cf - aizu | gzip -9n > "$output_dir/$archive"
else
  COPYFILE_DISABLE=1 tar --format ustar --uid 0 --gid 0 --uname root --gname root \
    -C "$stage" -cf - aizu | gzip -9n > "$output_dir/$archive"
fi
printf '%s\n' "$output_dir/$archive"
