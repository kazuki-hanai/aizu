#!/usr/bin/env bash
set -euo pipefail

if [[ $(uname -s) != Linux ]]; then
  echo "SSH integration fixture requires Linux" >&2
  exit 1
fi
if [[ ${GITHUB_ACTIONS:-false} != true || -z ${RUNNER_TEMP:-} ]]; then
  echo "SSH integration fixture requires an ephemeral GitHub Actions runner" >&2
  exit 1
fi

temp=$RUNNER_TEMP
key="$temp/aizu-ci-key"
host_key="$temp/sshd-host-key"
config="$temp/sshd-config"
known_hosts="$temp/known_hosts"
pid_file="$temp/sshd.pid"
log="$temp/sshd.log"
desktop_db="$temp/desktop-ssh.sqlite"

stop_sshd() {
  if [[ -f $pid_file ]] && sudo kill -0 "$(cat "$pid_file")" 2>/dev/null; then
    sudo kill -TERM "$(cat "$pid_file")"
  fi
}
cleanup() {
  status=$?
  if [[ $status -ne 0 && -f $log ]]; then sudo cat "$log"; fi
  stop_sshd
  exit "$status"
}
trap cleanup EXIT

install -d -m 700 "$HOME/.ssh"
if ! id aizu-ci >/dev/null 2>&1; then
  sudo useradd --create-home --shell /bin/bash aizu-ci
fi
sudo passwd --delete aizu-ci
sudo install -d -o aizu-ci -g aizu-ci -m 700 \
  /home/aizu-ci/.local/bin /home/aizu-ci/.local/state /home/aizu-ci/.ssh
sudo install -o aizu-ci -g aizu-ci -m 755 target/debug/aizu /home/aizu-ci/.local/bin/aizu
ssh-keygen -q -t ed25519 -N '' -f "$key"
sudo install -o aizu-ci -g aizu-ci -m 600 "$key.pub" /home/aizu-ci/.ssh/authorized_keys
ssh-keygen -q -t ed25519 -N '' -f "$host_key"
printf '%s\n' \
  'Port 22222' \
  'ListenAddress 127.0.0.1' \
  "HostKey $host_key" \
  'AuthorizedKeysFile .ssh/authorized_keys' \
  'PubkeyAuthentication yes' \
  'PasswordAuthentication no' \
  'KbdInteractiveAuthentication no' \
  'UsePAM no' \
  'AllowUsers aizu-ci' \
  'AllowTcpForwarding no' \
  'X11Forwarding no' \
  "PidFile $pid_file" > "$config"
sudo mkdir -p /run/sshd

start_sshd() {
  sudo /usr/sbin/sshd -f "$config" -E "$log"
}
start_sshd
ssh-keyscan -p 22222 127.0.0.1 > "$known_hosts" 2>/dev/null
install -m 600 "$known_hosts" "$HOME/.ssh/known_hosts"
printf '%s\n' \
  'Host aizu-ci' \
  '  HostName 127.0.0.1' \
  '  Port 22222' \
  '  User aizu-ci' \
  "  IdentityFile $key" > "$HOME/.ssh/config"
chmod 600 "$HOME/.ssh/config"

remote() {
  ssh -T -n -o BatchMode=yes -o StrictHostKeyChecking=yes \
    -o ClearAllForwardings=yes aizu-ci "$@"
}
remote 'HOME=/home/aizu-ci XDG_STATE_HOME=/home/aizu-ci/.local/state exec /home/aizu-ci/.local/bin/aizu emit task.completed --title "SSH integration completed" --outcome succeeded --json' >/dev/null
output=$(remote 'HOME=/home/aizu-ci XDG_STATE_HOME=/home/aizu-ci/.local/state exec /home/aizu-ci/.local/bin/aizu bridge --protocol 1 --after 0')
printf '%s\n' "$output" | node -e '
  let input = "";
  process.stdin.on("data", chunk => input += chunk);
  process.stdin.on("end", () => {
    const frames = input.trim().split("\n").map(line => JSON.parse(line));
    if (frames[0]?.type !== "hello" || frames[0]?.protocol_version !== 1) process.exit(1);
    if (frames[1]?.type !== "event" || frames[1]?.event?.kind !== "task.completed") process.exit(1);
  });'

mismatch_status=0
mismatch=$(remote 'HOME=/home/aizu-ci XDG_STATE_HOME=/home/aizu-ci/.local/state exec /home/aizu-ci/.local/bin/aizu bridge --protocol 999 --after 0') || mismatch_status=$?
test "$mismatch_status" -eq 2
printf '%s\n' "$mismatch" | node -e '
  let input = "";
  process.stdin.on("data", chunk => input += chunk);
  process.stdin.on("end", () => {
    const frame = JSON.parse(input.trim());
    if (frame.type !== "error" || frame.code !== "incompatible_protocol") process.exit(1);
  });'

sudo mv /home/aizu-ci/.local/bin/aizu /home/aizu-ci/.local/bin/aizu.disabled
if remote 'HOME=/home/aizu-ci XDG_STATE_HOME=/home/aizu-ci/.local/state exec /home/aizu-ci/.local/bin/aizu bridge --protocol 1 --after 0' >"$temp/missing-cli.stdout" 2>"$temp/missing-cli.stderr"; then
  echo "missing remote CLI unexpectedly succeeded" >&2
  exit 1
fi
sudo mv /home/aizu-ci/.local/bin/aizu.disabled /home/aizu-ci/.local/bin/aizu

export AIZU_REAL_SSH_ALIAS=aizu-ci AIZU_REAL_SSH_DB="$desktop_db"
AIZU_REAL_SSH_PHASE=initial cargo test --locked -p aizu-core --test ssh_process \
  unix::real_ssh_bridge_reconnects_from_cursor_and_deduplicates_replay -- --exact
stop_sshd
for _ in $(seq 1 50); do
  if ! sudo kill -0 "$(cat "$pid_file")" 2>/dev/null; then break; fi
  sleep 0.1
done
start_sshd
remote 'HOME=/home/aizu-ci XDG_STATE_HOME=/home/aizu-ci/.local/state exec /home/aizu-ci/.local/bin/aizu emit task.completed --title "SSH reconnect completed" --outcome succeeded --json' >/dev/null
AIZU_REAL_SSH_PHASE=resume cargo test --locked -p aizu-core --test ssh_process \
  unix::real_ssh_bridge_reconnects_from_cursor_and_deduplicates_replay -- --exact
