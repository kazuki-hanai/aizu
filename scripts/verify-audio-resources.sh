#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: verify-audio-resources.sh <app-bundle>" >&2
  exit 2
fi

app=$1
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
resources="$app/Contents/Resources"
[[ -d $resources ]] || { echo "app resources are missing: $resources" >&2; exit 1; }

for asset in aizu-pop.wav aizu-chime.wav aizu-pulse.wav aizu-bloom.wav; do
  canonical="$root/assets/audio/$asset"
  bundled="$resources/$asset"
  [[ -f $canonical ]] || { echo "canonical notification sound is missing: $canonical" >&2; exit 1; }
  [[ -f $bundled ]] || { echo "bundled notification sound is missing: $bundled" >&2; exit 1; }
  cmp "$canonical" "$bundled"
done

echo "verified bundled Aizu notification sounds"
