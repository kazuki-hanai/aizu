# Aizu

Aizu sends Codex and Claude Code completion, question, and permission alerts to
your Mac. Events are stored on the machine where the agent runs, then delivered
locally or over your existing SSH connection. Aizu has no notification relay,
account, or listening port.

## What You Need

| Role | Requirement |
| --- | --- |
| Receiving computer | macOS 12 or later |
| Local agents | Codex or Claude Code on the receiving Mac |
| Remote agents | Linux or macOS reachable through a system SSH alias |

Windows desktop installation is not supported yet.

Install [mise](https://mise.jdx.dev/getting-started.html) before using the
source-build commands below. If every agent runs on the receiving Mac, complete
only **Install on the Receiving Mac**. Continue to **Add an SSH Source** for
each additional machine.

## Install on the Receiving Mac

A signed public release is not available yet. Build the development DMG from
the repository:

```bash
xcode-select --install # Skip if Command Line Tools are already installed.
git clone https://github.com/kazuki-hanai/aizu.git
cd aizu
mise trust
mise install rust node pnpm
mise exec -- pnpm install --frozen-lockfile
./scripts/build-dev-dmg.sh
open target/debug/bundle/dmg/Aizu_*.dmg
```

Drag **Aizu** to **Applications**, then open `/Applications/Aizu.app`.

Complete the first-run screen:

1. Select **Set up** under **Connect Codex and Claude Code**.
2. Review the installed Codex hooks, approve them in Codex, then select
   **Confirm approval** in Aizu. Claude Code does not require this extra step.
3. Choose whether Aizu starts at login and select **Open Aizu**.

Setup installs the bundled CLI at `~/.local/bin/aizu` and merges the Aizu hooks
without deleting unrelated agent settings. Completion notifications require
these hooks; process monitoring alone is diagnostic and cannot determine when
an agent task has completed.

If Claude Code has `disableAllHooks` set to `true`, Aizu leaves that preference
unchanged and setup stops. Enable hooks explicitly in Claude Code, then run
setup again.

### Check the Local Setup

The CLI does not need to be on `PATH` for installed hooks. For manual checks:

```bash
"$HOME/.local/bin/aizu" version --json
"$HOME/.local/bin/aizu" doctor --json
"$HOME/.local/bin/aizu" agents --json
```

## Add an SSH Source

Repeat this section on each Linux or macOS machine where an agent runs.

### 1. Install the CLI on the Source

Clone and build Aizu on the source machine:

```bash
(
  set -eu
  git clone https://github.com/kazuki-hanai/aizu.git
  cd aizu
  mise trust
  mise install rust node
  mise exec -- cargo build --locked --release -p aizu-cli
  install -d -m 700 "$HOME/.local/bin"
  stage="$HOME/.local/bin/.aizu-install-$$"
  trap 'rm -f "$stage"' EXIT HUP INT TERM
  install -m 755 target/release/aizu "$stage"
  "$stage" version --json
  ln "$stage" "$HOME/.local/bin/aizu"
  rm -f "$stage"
  trap - EXIT HUP INT TERM
  "$HOME/.local/bin/aizu" version --json
  "$HOME/.local/bin/aizu" doctor --json
)
```

The hard-link step stops if anything already exists at the target path. Do not
remove or replace that item until you know who owns it. Upgrades use a separate
procedure in [Installation details](docs/installation.md).

### 2. Connect Codex and Claude Code

```bash
"$HOME/.local/bin/aizu" integration-install --json
"$HOME/.local/bin/aizu" agents --json
```

Approve the new hooks in Codex on that source machine. If setup reports that an
agent configuration directory is writable by other users, secure it and retry:

```bash
chmod go-w "$HOME/.codex" "$HOME/.claude"
"$HOME/.local/bin/aizu" integration-install --json
```

### 3. Add the Source to the Mac

Create a normal system SSH alias on the receiving Mac:

```sshconfig
Host remote-host
  HostName 192.0.2.10
  User your-user
  IdentityFile ~/.ssh/id_ed25519
```

Replace the example values, then verify the connection outside Aizu:

```bash
ssh remote-host '$HOME/.local/bin/aizu version --json'
```

In Aizu, open **Sources**, select **+**, enter `remote-host`, then select
**Test connection** and **Add source**.

Aizu uses `/usr/bin/ssh` and the Mac's existing SSH configuration. It does not
copy private keys, store passwords, or disable host-key verification. Remote
events remain in the source SQLite spool while the Mac is disconnected.

## Notifications

- **Aizu Banner** is the default. It needs no macOS notification permission,
  keeps up to three alerts visible, preserves safe line breaks, and closes only
  from its close button or a horizontal click-drag/swipe.
- **macOS Notifications** uses Notification Center. Select it under
  **Settings > Notification style**, then use the test action to request system
  permission.
- **Aizu Pop** is the default sound. Change it or turn sound off under
  **Settings > Notification sound**.
- **Show agent details** is off by default. Enabling it adds a short filtered
  completion or permission excerpt. Aizu still excludes raw commands, full
  prompts, transcripts, secrets, and absolute paths.

Settings are saved immediately. The interface can follow macOS or use Japanese
or English under **Settings > Language**.

## Troubleshooting

### No completion notification

1. Run `"$HOME/.local/bin/aizu" doctor --json` on the machine running the
   agent.
2. Run `"$HOME/.local/bin/aizu" integration-install --json` again after an
   Aizu or agent update.
3. Confirm the hooks in Codex. A configured hook is not active until Codex
   trusts it.
4. If Claude Code has `disableAllHooks` enabled, turn it off intentionally and
   rerun `integration-install --json`; Aizu never overrides it automatically.
5. For SSH sources, use **Test connection** and verify the source is
   **Connected**.

### `an agent configuration path is unsafe`

Do not bypass this check. Verify that `~/.codex` and `~/.claude` belong to the
current user, are not symlinks outside the home directory, and are not writable
by group or others. For the common permissions case:

```bash
chmod go-w "$HOME/.codex" "$HOME/.claude"
```

Then rerun `integration-install --json`. See [Installation details](docs/installation.md)
for ownership, symlink, and upgrade guidance.

### SSH connects but no event arrives

A successful connection test verifies the CLI and protocol only. Trigger a
real Codex or Claude Code completion on the source, then check **Recent
activity** on the Mac. Also verify directly:

```bash
ssh remote-host '$HOME/.local/bin/aizu doctor --json'
```

## Development

[mise](https://mise.jdx.dev/) pins Rust, Node.js, and pnpm.

```bash
mise trust
mise install rust node pnpm
mise exec -- pnpm install --frozen-lockfile
mise run check
```

Common tasks:

```bash
mise run build
mise run cli:smoke
mise exec -- pnpm tauri dev
mise tasks
```

Architecture and protocol details live in [MVP design](docs/mvp-design.md) and
[Bridge protocol](docs/protocol.md). Development and PR rules are in
[AGENTS.md](AGENTS.md). Maintainers can run a no-secret release rehearsal and
prepare protected signed releases using the [release guide](docs/releasing.md).
