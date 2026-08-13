# Aizu

Durable desktop notifications for terminal AI agents, without a central
notification backend.

## Installation

Aizu currently ships as a source-built development application. A Developer
ID-signed and notarized GitHub Release is intentionally unavailable until the
release branding and signing keys are approved. Building the desktop app
requires macOS 12 or later and Xcode Command Line Tools. The development
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

Remote sources require a platform-compatible `aizu` binary installed as
`~/.local/bin/aizu` on the remote host, plus a working alias in the receiving
Mac's `~/.ssh/config`. Build the CLI on that host with the pinned Rust toolchain:

```bash
mise trust
mise install rust
mise exec -- cargo build --locked --release -p aizu-cli
install -d -m 700 "$HOME/.local/bin"
install -m 755 target/release/aizu "$HOME/.local/bin/aizu"
```

Configure Codex and Claude Code hooks on the remote host using the commands in
the [Agent hooks](#agent-hooks) section, then add its SSH config alias in Aizu.
Aizu invokes the system `/usr/bin/ssh`; it does not store private keys or
passwords and does not disable host-key verification.

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
