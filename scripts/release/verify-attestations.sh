#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "usage: verify-attestations.sh <release-assets-directory> <owner/repository> <source-sha>" >&2
  exit 2
fi

assets_dir=$1
repository=$2
source_sha=$3

[[ -d $assets_dir ]] || { echo "release assets directory does not exist: $assets_dir" >&2; exit 1; }
[[ $repository =~ ^[^/[:space:]]+/[^/[:space:]]+$ ]] \
  || { echo "invalid GitHub repository: $repository" >&2; exit 1; }
[[ $source_sha =~ ^[0-9a-f]{40}$ ]] || { echo "invalid release source SHA" >&2; exit 1; }

shopt -s nullglob
assets=("$assets_dir"/*)
(( ${#assets[@]} > 0 )) || { echo "release assets directory is empty: $assets_dir" >&2; exit 1; }

for asset in "${assets[@]}"; do
  [[ -f $asset && ! -L $asset ]] \
    || { echo "release asset is not a regular file: $asset" >&2; exit 1; }
  gh attestation verify "$asset" --repo "$repository" \
    --signer-workflow "$repository/.github/workflows/release.yml" \
    --source-digest "$source_sha"
done
