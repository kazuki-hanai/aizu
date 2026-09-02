#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
mkdir -p "$root/target"
fixture=$(mktemp -d "$root/target/install-cli-test.XXXXXX")
trap 'rm -rf "$fixture"' EXIT HUP INT TERM

mkdir -p "$fixture/repo/scripts" "$fixture/repo/target/release" "$fixture/tools"
cp "$root/scripts/install-cli.sh" "$fixture/repo/scripts/install-cli.sh"

cat >"$fixture/tools/mise" <<'FAKE_MISE'
#!/bin/sh
set -eu
case "${1:-}" in
  trust|install)
    exit 0
    ;;
  exec)
    shift
    if [ "${1:-}" = "--" ]; then shift; fi
    if [ "${1:-}" = "cargo" ]; then exit 0; fi
    exec "$@"
    ;;
  *)
    exit 64
    ;;
esac
FAKE_MISE

cat >"$fixture/repo/target/release/aizu" <<'NEW_AIZU'
#!/bin/sh
set -eu
case "${1:-}" in
  version)
    printf '%s\n' '{"application":"1.2.3","protocol":1,"event_schema":1,"database_schema":1,"sqlite":"3.50.0"}'
    ;;
  doctor)
    printf '%s\n' '{"healthy":true}'
    ;;
  *)
    exit 64
    ;;
esac
NEW_AIZU

chmod 755 "$fixture/tools/mise" "$fixture/repo/scripts/install-cli.sh" \
  "$fixture/repo/target/release/aizu"
installer="$fixture/repo/scripts/install-cli.sh"
test_path="$fixture/tools:$PATH"

HOME="$fixture/first-home" PATH="$test_path" "$installer" >/dev/null
cmp "$fixture/repo/target/release/aizu" "$fixture/first-home/.local/bin/aizu"
HOME="$fixture/first-home" PATH="$test_path" "$installer" >/dev/null

mkdir -p "$fixture/foreign-home/.local/bin"
printf '%s\n' 'not Aizu' >"$fixture/foreign-home/.local/bin/aizu"
if HOME="$fixture/foreign-home" PATH="$test_path" "$installer" >/dev/null 2>"$fixture/foreign-error"; then
  printf '%s\n' "installer replaced an existing foreign file" >&2
  exit 1
fi
grep -F "rerun with --upgrade" "$fixture/foreign-error" >/dev/null

mkdir -p "$fixture/upgrade-home/.local/bin"
cat >"$fixture/upgrade-home/.local/bin/aizu" <<'OLD_AIZU'
#!/bin/sh
set -eu
case "${1:-}" in
  version)
    printf '%s\n' '{"application":"1.2.2","protocol":1,"event_schema":1,"database_schema":1,"sqlite":"3.50.0"}'
    ;;
  *)
    exit 64
    ;;
esac
OLD_AIZU
chmod 755 "$fixture/upgrade-home/.local/bin/aizu"
HOME="$fixture/upgrade-home" PATH="$test_path" "$installer" --upgrade >/dev/null
cmp "$fixture/repo/target/release/aizu" "$fixture/upgrade-home/.local/bin/aizu"

mkdir -p "$fixture/nonzero-home/.local/bin"
cat >"$fixture/nonzero-home/.local/bin/aizu" <<'NONZERO_AIZU'
#!/bin/sh
set -eu
if [ "${1:-}" = version ]; then
  printf '%s\n' '{"application":"1.2.2","protocol":1,"event_schema":1,"database_schema":1,"sqlite":"3.50.0"}'
  exit 1
fi
exit 64
NONZERO_AIZU
chmod 755 "$fixture/nonzero-home/.local/bin/aizu"
cp "$fixture/nonzero-home/.local/bin/aizu" "$fixture/nonzero-original"
if HOME="$fixture/nonzero-home" PATH="$test_path" "$installer" --upgrade >/dev/null 2>"$fixture/nonzero-error"; then
  printf '%s\n' "installer accepted a version command that exited unsuccessfully" >&2
  exit 1
fi
cmp "$fixture/nonzero-original" "$fixture/nonzero-home/.local/bin/aizu"

mkdir -p "$fixture/symlink-home/.local/bin"
ln -s "$fixture/repo/target/release/aizu" "$fixture/symlink-home/.local/bin/aizu"
if HOME="$fixture/symlink-home" PATH="$test_path" "$installer" >/dev/null 2>"$fixture/symlink-error"; then
  printf '%s\n' "installer replaced a symlink" >&2
  exit 1
fi
grep -F "refusing to replace a symlink" "$fixture/symlink-error" >/dev/null

printf '%s\n' "validated install, idempotency, upgrade, failed-version handling, and unsafe-target rejection"
