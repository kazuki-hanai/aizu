#!/usr/bin/env bash
set -euo pipefail

for iteration in $(seq 1 20); do
  echo "concurrency iteration $iteration"
  cargo test --locked -p aizu-cli --test cli \
    concurrent_process_emit_allocates_every_sequence_once -- --exact
done

for iteration in $(seq 1 50); do
  echo "reconnect iteration $iteration"
  cargo test --locked -p aizu-core \
    remote::tests::exact_replay_is_deduplicated_after_reconnect -- --exact
done

cargo test --locked -p aizu-core \
  desktop::tests::previous_release_desktop_fixture_migrates_to_current_schema -- --exact

cargo audit --version 2>/dev/null | grep -F '0.22.2' \
  || cargo install cargo-audit --version 0.22.2 --locked --force
cargo deny --version 2>/dev/null | grep -F '0.20.2' \
  || cargo install cargo-deny --version 0.20.2 --locked --force
cargo audit
cargo deny check advisories bans licenses sources
corepack enable
pnpm audit --prod --audit-level high
pnpm audit --dev --audit-level high --ignore GHSA-jmr9-qjv8-65gv
