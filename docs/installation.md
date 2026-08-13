# Installation Details

This guide contains the cautious installation and upgrade procedures that are
too detailed for the main README.

## Development Mac App

The development installer is ad-hoc signed so macOS can associate notification
permission with Aizu's stable bundle identifier. Always drag the app from the
DMG into **Applications**. Do not launch the bundle directly from `target/` if
you use macOS Notifications.

To replace an existing development installation automatically:

```bash
./scripts/install-dev-app.sh
```

The script validates the new bundle and moves the previous development app to a
temporary backup before installing it. Quit Aizu from its menu-bar menu before
replacement. On next launch, Aizu checks the bundled and installed CLI before
opening the local spool. It does not overwrite a symlink, a foreign-owned file,
or an unrelated executable at `~/.local/bin/aizu`.

If the bundled CLI version changes, Aizu keeps the local source read-only until
you explicitly replace the Aizu-managed CLI from the setup screen. This avoids
migrating the shared local spool while an incompatible hook CLI is still
writing to it.

## First CLI Install on an SSH Source

The following procedure commits the binary with a hard link. It fails rather
than replacing anything already present at the target path.

```bash
(
  set -eu
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
)
```

If `ln` fails, inspect the existing target. Never remove or overwrite it until
you know who owns it and how it was installed.

## Upgrade a Source CLI

Use this only when the existing target was installed using the Aizu procedure
above. A successful version response checks compatibility; it is not
cryptographic proof that an unfamiliar file belongs to Aizu.

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

The staging file is in the target directory, so the final rename stays on one
filesystem. A failed build or validation leaves the installed CLI untouched.
After an Aizu or agent update, refresh and review the hooks:

```bash
"$HOME/.local/bin/aizu" integration-install --json
```

Existing spooled events remain on the source and are delivered when the SSH
connection resumes.

## Agent Configuration Safety

`integration-install` reads and validates both Codex and Claude Code JSON files
before changing either one. It preserves unrelated keys and hook handlers and
rejects malformed, oversized, externally linked, or unsafe paths. Aizu
installers are serialized with `~/.aizu/hooks.lock`; editors and agents do not
participate in that lock, so do not edit these configuration files while the
installer runs.

Claude Code's top-level `disableAllHooks` preference is also preserved. When it
is `true`, installation stops without changing either agent. Enable hooks
explicitly in Claude Code only when that is your intent, then rerun
`integration-install --json`.

For a permissions diagnostic, inspect ownership and mode without exposing file
contents:

```bash
ls -ld "$HOME" "$HOME/.codex" "$HOME/.claude"
```

The directories must belong to the current user and must not be writable by
group or others. Fix only the write bits when ownership and paths are already
correct:

```bash
chmod go-w "$HOME/.codex" "$HOME/.claude"
```

Do not apply `chmod` as a workaround for a foreign owner, a dangling symlink,
or a link outside the home directory. Correct the ownership or path explicitly,
then rerun the installer.

## Remote Process Diagnostics

For each connected SSH source, the desktop periodically runs the fixed command
`~/.local/bin/aizu agents --json` through a separate short-lived system SSH
connection. The response contains the supported agent kind and instance count,
not PID, arguments, executable path, working directory, environment, prompt,
or terminal output. Disconnected sources disappear from **Running agents**
until a fresh probe succeeds.

This process list is diagnostic only. Completion and permission notifications
always come from the installed lifecycle hooks and durable source spool.

## Advanced Hook Setup

The default command installs both first-party integrations using the current
CLI's absolute path:

```bash
aizu integration-install --json
```

Use `--agent codex` or `--agent claude-code` to update only one integration.
Use `--aizu-path /absolute/path/to/aizu` only when preparing hooks for a
different installed CLI path. `aizu integration-config` prints a read-only
preview and does not modify either agent configuration.
