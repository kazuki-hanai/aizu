# ADR 0002: Local notification clicks use bounded terminal activation adapters

- Status: Accepted
- Date: 2026-08-19
- Deciders: repository owner
- Related: `docs/mvp-design.md` §8.2, §13.2, architectural invariants #2, #8, #12

## Context

Aizu notifications identify when Codex or Claude Code has completed or needs input, but the user
must then locate the terminal session manually. Opening the Aizu main window does not help resume
the task. A useful notification action should return to the terminal while supporting common
terminal applications and tmux without introducing arbitrary command execution or leaking cwd,
argv, socket paths, or environment data.

An SSH-delivered event cannot safely identify the original interactive shell on the receiving
Mac. Its hook environment belongs to the source host, and the durable bridge is an independent
non-interactive SSH child. Opening a fresh SSH connection would not return to the same shell.

## Decision

1. The trusted first-party `aizu hook` path captures a `TerminalActivation` descriptor from a
   fixed environment allowlist. Generic `emit` and generic hook inputs cannot set the reserved
   `aizu_terminal_activation` metadata key.
2. The descriptor contains only a fixed terminal enum, a bounded application session/pane ID, and
   an optional tmux socket label plus `%N` pane ID. It cannot represent a command, executable,
   bundle ID, cwd, argv, raw environment value, or absolute tmux socket path.
3. The receiving policy exposes activation only when `source_key == "local"` and the event has
   trusted Codex/Claude Code adapter provenance. Remote SSH notifications, backlog summaries,
   generic events, and manual test notifications remain non-actionable.
4. macOS adapters use only fixed operations:
   - iTerm2 reveal URL for an exact session;
   - WezTerm `cli activate-pane --pane-id` for a numeric pane;
   - tmux `-L <label> select-window/select-pane -t %N` with fixed argv;
   - fixed bundle-ID application focus for Apple Terminal, Ghostty, Warp, Kitty, and VS Code.
   iTerm2 and WezTerm require an exact session/pane identifier and never downgrade to bundle focus.
5. Adapter children have a bounded timeout and Aizu terminates only children it started. No shell
   command string or AppleScript is used.
6. Aizu Banner uses the hidden backend descriptor. The frontend receives only
   `canActivateTerminal` and sends the notification ID back. A normal click or keyboard activation
   returns to the terminal; text selection, vertical movement, and swipe do not. Successful
   activation dismisses an Aizu Banner. Failure leaves it queued.
7. macOS Notification Center installs one app-lifetime response delegate and keeps at most 64
   notification-ID-to-target entries. It does not create a task, future, or waiter per
   notification. A default click atomically consumes one entry and runs the same fixed adapter on
   a background task; dismiss and other actions only remove the entry. New entries evict the
   oldest target when the bound is reached, without affecting notification display. Response
   delegate setup is best-effort: an unavailable bundle identity or native handler disables only
   System notification actions and must not prevent Aizu Banner or the app worker from starting.
   A later System delivery schedules a coalesced, time-gated main-thread retry. An actionable
   notification is not displayed without its response registry; it remains in the durable outbox
   and follows the existing bounded delivery backoff until the handler recovers.
8. Notification activation never opens the Aizu main window. Explicit tray Open and launching a
   second app instance remain the supported ways to show the main window.

## Consequences

- Local iTerm2, WezTerm, and tmux users can return to an exact live target when its identifier is
  still valid. Other supported terminals receive a predictable application-focus fallback.
- iTerm2's reveal URL acknowledges the LaunchServices handoff, not proof that a stale session was
  selected. Aizu never converts that handoff into a less precise iTerm2 bundle-focus action.
- Remote SSH notifications intentionally have no return-to-shell action. This avoids targeting the
  wrong local session and preserves the existing SSH trust model.
- Stale sessions or missing applications can fail without executing attacker-controlled input.
- Aizu Banner actions are persisted in its bounded in-memory queue. Notification Center actions
  use a separate bounded in-memory registry; both registries are lost safely on app exit and no
  activation descriptor crosses the SSH bridge or reaches durable remote storage.

## Alternatives considered

- **Open the Aizu main window:** does not resume the agent and was explicitly rejected by the user.
- **Open a new terminal or SSH session:** can target the wrong host/session and is not equivalent to
  the originating shell.
- **Store cwd, tty path, tmux socket path, or arbitrary activation commands:** creates privacy and
  command-injection boundaries that are unnecessary for the supported adapters.
- **AppleScript automation:** application dictionaries vary, require extra automation permission,
  and encourage interpolated script strings. Fixed URL/argv/bundle adapters are narrower.

## References

- [iTerm2 reveal URL](https://iterm2.com/documentation-command-selection.html)
- [WezTerm `activate-pane`](https://wezterm.org/cli/cli/activate-pane.html)
- [tmux pane and socket identifiers](https://github.com/tmux/tmux/wiki/Advanced-Use)
