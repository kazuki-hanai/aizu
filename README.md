# Aizu

Durable desktop notifications for terminal AI agents, without a central
notification backend.

## Installation

Aizu currently ships from source. Choose the installation that matches the
machine's role:

| Machine | Install | Supported in the MVP |
| --- | --- | --- |
| Receiving Mac | Aizu desktop app and bundled CLI | macOS 12 or later |
| SSH source | `aizu` CLI and agent hooks | Linux or macOS |
| Windows | None | Core/CLI compilation is checked, but Windows is not an installation target yet |

The desktop notification receiver is macOS-only. A Linux machine such as a
mini-PC runs only the CLI: it stores agent events in its local SQLite spool and
the Mac retrieves them through system SSH. No daemon, web server, or listening
port is installed on the source machine.

### Receiving Mac: desktop app

A Developer ID-signed and notarized GitHub Release is intentionally unavailable
until the release branding and signing keys are approved. Building the desktop
app requires macOS 12 or later and Xcode Command Line Tools. The development
installer applies an ad-hoc signature so macOS can associate notification
permission with Aizu's stable bundle identifier.

```bash
xcode-select --install # omit this if the tools are already installed
mise trust
mise install rust node pnpm
mise exec -- pnpm install --frozen-lockfile
./scripts/build-dev-dmg.sh
open target/debug/bundle/dmg/Aizu_*.dmg
```

Drag **Aizu** onto **Applications** in the opened disk image, then launch
`/Applications/Aizu.app`. Do not launch the bundle directly from `target/`:
macOS notification permission is tied to the installed, signed application
identity. For automated local replacement, `./scripts/install-dev-app.sh` uses
the same validation and moves an existing Aizu development bundle to a temporary
backup before installing the new one.

The bundle includes the matching `aizu` CLI. On first launch:

1. Select **Allow** when Aizu requests notification permission.
2. Select **Set up** under **Connect Codex and Claude Code**. Aizu atomically
   installs the bundled CLI at `~/.local/bin/aizu` and merges the required
   lifecycle hooks into both agent configuration files without deleting
   existing hooks.
3. Review the installed command hooks in Codex and select **Confirm approval**
   in Aizu after approving them. Claude Code does not use this separate Codex
   trust prompt.
4. Choose whether Aizu should launch at login, then select **Open Aizu**.

The generated hooks use the absolute CLI path, so adding `~/.local/bin` to
`PATH` is optional. It is useful for manual diagnostics:

```bash
export PATH="$HOME/.local/bin:$PATH"
aizu version --json
aizu doctor --json
aizu agents --json
```

To upgrade a source build, quit Aizu from its menu-bar menu, rebuild or replace
`Aizu.app`, and launch it again. If the bundled CLI version changed, Aizu keeps
the local spool read-only until you explicitly replace the Aizu-managed CLI
from the setup screen. It never overwrites a symlink, a file owned by another
user, or an unrelated binary at the install path.

### Linux or macOS SSH source: CLI only

Run these commands on every remote source. They install no desktop app and no
background service. The repository must already be cloned on that machine:

```bash
cd /path/to/aizu
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
```

The hard-link commit intentionally fails if anything already occupies the
target path and never follows a destination symlink. Do not remove or overwrite
that item until you have established who owns it.

Generate and merge the first-party hooks on that same source machine:

```bash
"$HOME/.local/bin/aizu" integration-config \
  --agent codex \
  --aizu-path "$HOME/.local/bin/aizu"
"$HOME/.local/bin/aizu" integration-config \
  --agent claude-code \
  --aizu-path "$HOME/.local/bin/aizu"
```

The commands print configuration fragments; merge them into
`~/.codex/hooks.json` and `~/.claude/settings.json` without removing unrelated
hooks. Codex requires explicit hook approval on that machine. Verify the source
before configuring the receiving Mac:

```bash
"$HOME/.local/bin/aizu" agents --json
```

On the receiving Mac, add a normal system SSH alias to `~/.ssh/config`, verify
it outside Aizu, then use **Sources > + > Test connection > Add source**:

```sshconfig
Host mini-pc
  HostName 192.0.2.10
  User your-user
  IdentityFile ~/.ssh/id_ed25519
```

```bash
ssh mini-pc '$HOME/.local/bin/aizu version --json'
```

Replace the example address, user, and key with the existing SSH configuration
for the source. Aizu invokes the receiving Mac's system `/usr/bin/ssh`; it does
not copy or store private keys or passwords and never disables host-key
verification.

### Upgrading a source CLI

Pull the desired Aizu revision on the Linux or macOS source, rebuild, and
replace only the Aizu-managed binary:

```bash
(
  set -eu
  cd /path/to/aizu
  git pull --ff-only
  mise exec -- cargo build --locked --release -p aizu-cli
  target="$HOME/.local/bin/aizu"
  test -f "$target" && test ! -L "$target"
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
  validate_report "$target"
  stage="$HOME/.local/bin/.aizu-update-$$"
  trap 'rm -f "$stage"' EXIT HUP INT TERM
  install -m 755 target/release/aizu "$stage"
  validate_report "$stage"
  mv -f "$stage" "$target"
  trap - EXIT HUP INT TERM
  validate_report "$target"
)
```

Continue only if you installed the existing file using these Aizu instructions
and both reports contain a valid compatibility record such as
`{"application":"0.1.0","protocol":1,"event_schema":1,"database_schema":1,...}`.
The `application` field is the Aizu CLI version. A successful version response
checks compatibility; it is not cryptographic proof of ownership, so do not use
this update procedure for an unfamiliar file. The staging file is created in
the same directory so the final rename stays on one filesystem; a failed build
or validation leaves the installed CLI untouched. Re-run both
`integration-config` commands after an Aizu or agent update and review the
resulting fragments before merging them. Existing spooled events remain on the
source and are delivered after the SSH connection resumes.

The **Running agents** list includes both this Mac and connected SSH sources.
For a remote source, the desktop periodically runs the fixed
`~/.local/bin/aizu agents --json` diagnostic over a separate short-lived SSH
connection. The response contains only the supported agent kind and instance
count; it contains no PID, command arguments, path, environment, prompt, or
terminal output. A source that disconnects is removed from the running list
until a fresh probe succeeds.

For local SSH integration checks, use the existing system SSH alias `mini-pc`
as the real remote fixture. Install the current `aizu` CLI on that host before
testing, verify `ssh mini-pc '$HOME/.local/bin/aizu version --json'`, and run a
Codex or Claude Code hook event there. A successful UI connection test alone is
not sufficient: the event must traverse `aizu bridge` and appear in the Mac
desktop history under the `Mini PC` source.

## Development setup

[mise](https://mise.jdx.dev/) is the source of truth for the Rust, Node.js, and
pnpm toolchain versions.

```bash
mise trust
mise install rust node pnpm
mise run check
```

Useful tasks:

```bash
mise tasks
mise run build
mise run cli:smoke
mise run ci
```

Run an individual command inside the pinned environment with:

```bash
mise exec -- cargo test --workspace --all-features --locked
```

Start the desktop app in development mode:

```bash
mise exec -- pnpm install --frozen-lockfile
mise exec -- pnpm tauri dev
```

## Agent hooks

Codex and Claude Code are first-party integrations. The CLI converts their
lifecycle hook JSON from stdin into the same durable local spool. Generate a
configuration fragment with the absolute installed CLI path, then merge it
into `~/.codex/hooks.json` or `~/.claude/settings.json` without removing
existing hooks:

```bash
aizu integration-config --agent codex --aizu-path "$HOME/.local/bin/aizu"
aizu integration-config --agent claude-code --aizu-path "$HOME/.local/bin/aizu"
```

Codex requires reviewing and trusting a new or changed command hook in its
hook UI before it will run. Aizu monitors Codex and Claude Code process
presence for diagnostics only; completion and permission notifications come
from verified lifecycle hooks, never terminal output scraping.

To include a short completion message or the explicit question/permission
description in an alert, open **Settings > Advanced** and enable **Show agent
details**. This is off by default because notifications can appear on the lock
screen. Aizu limits the excerpt, rejects credential/path-like content, and does
not copy raw commands, full prompts, transcripts, or tool input objects.

The desktop interface can follow the macOS language or be set explicitly to
Japanese or English under **Settings > Language**. Changes are persisted and
applied immediately. Agent messages, SSH source labels, and event excerpts stay
in their original language.
