#!/bin/sh
set -eu

usage() {
  printf '%s\n' "usage: $0 [--upgrade]" >&2
}

mode=install
case $# in
  0) ;;
  1)
    if [ "$1" != "--upgrade" ]; then
      usage
      exit 64
    fi
    mode=upgrade
    ;;
  *)
    usage
    exit 64
    ;;
esac

if ! command -v mise >/dev/null 2>&1; then
  printf '%s\n' "mise is required; install it from https://mise.jdx.dev/getting-started.html" >&2
  exit 1
fi

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

mise trust
mise install rust node
mise exec -- cargo build --locked --release -p aizu-cli

built="$root/target/release/aizu"
target_dir="$HOME/.local/bin"
target="$target_dir/aizu"

validate_report() {
  "$1" version --json | mise exec -- node -e '
    let input = "";
    process.stdin.on("data", chunk => input += chunk);
    process.stdin.on("end", () => {
      const report = JSON.parse(input);
      const version = /^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/u;
      if (!version.test(report.application)
        || !Number.isInteger(report.protocol)
        || !Number.isInteger(report.event_schema)
        || !Number.isInteger(report.database_schema)
        || typeof report.sqlite !== "string") process.exit(1);
    });'
}

if [ ! -f "$built" ] || [ ! -x "$built" ]; then
  printf '%s\n' "build did not produce an executable Aizu CLI" >&2
  exit 1
fi
validate_report "$built"

install -d -m 700 "$target_dir"

if [ -e "$target" ] || [ -L "$target" ]; then
  if [ ! -f "$target" ] || [ -L "$target" ]; then
    printf '%s\n' "refusing to replace a symlink or non-file at $target" >&2
    exit 1
  fi

  if cmp -s "$built" "$target"; then
    validate_report "$target"
    printf '%s\n' "Aizu CLI is already installed at $target" >&2
    "$target" doctor --json
    exit 0
  fi

  if [ "$mode" != upgrade ]; then
    printf '%s\n' "refusing to replace the existing $target; rerun with --upgrade after verifying it belongs to Aizu" >&2
    exit 1
  fi

  validate_report "$target"
  stage="$target_dir/.aizu-update-$$"
  trap 'rm -f "$stage"' EXIT HUP INT TERM
  install -m 755 "$built" "$stage"
  validate_report "$stage"
  mv -f "$stage" "$target"
  trap - EXIT HUP INT TERM
else
  stage="$target_dir/.aizu-install-$$"
  trap 'rm -f "$stage"' EXIT HUP INT TERM
  install -m 755 "$built" "$stage"
  validate_report "$stage"
  ln "$stage" "$target"
  rm -f "$stage"
  trap - EXIT HUP INT TERM
fi

"$target" version --json
"$target" doctor --json
