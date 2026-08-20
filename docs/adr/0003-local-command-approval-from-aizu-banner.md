# ADR 0003: Local command approvals use an ephemeral one-shot broker

- Status: Superseded by [ADR 0004](0004-terminal-owned-command-approval.md)
- Date: 2026-08-20
- Deciders: repository owner
- Related: `docs/mvp-design.md` §4, §13.5, architectural invariants #11, #12

## Context

Codex and Claude Code both invoke Aizu's synchronous `PermissionRequest` hook before showing their
normal terminal approval UI. Aizu currently persists only a privacy-safe `agent.question` event and
returns no decision, so users must locate the originating terminal to approve an otherwise clear
command request. Aizu Banner can show more content than Notification Center and remains visible,
but putting the raw command in the durable spool, history, notification outbox, logs, or the SSH
event stream would violate the existing privacy boundary.

## Decision

1. A first-party local `PermissionRequest` for the canonical `Bash` tool containing an exact
   `tool_input.command` may also open a versioned Unix-domain-socket request to the running desktop
   app. The existing sanitized `agent.question` event is still persisted independently.
2. The local request contains only a random request ID, fixed agent kind, bounded tool label, and
   exact bounded command. It is held in memory only. It is never written to SQLite, settings,
   history, logs, Notification Center, or the SSH bridge.
   After the approval banner is actually presented, the sanitized event stores only an
   Aizu-generated boolean marker. Notification policy trusts that marker only for a first-party
   local event and suppresses its duplicate passive notification while retaining history.
3. The socket lives in Aizu's private `0700` state directory and is `0600`. Symlink and non-socket
   nodes are rejected. The protocol accepts one bounded strict-JSON frame in each direction.
4. At most one approval waits at a time. The desktop waits 45 seconds and the generated
   `PermissionRequest` hook has a 50-second outer timeout. Broker absence, disabled preferences,
   invalid/oversized input, another pending request, banner dismissal, timeout, or app shutdown
   returns no decision so the agent's normal approval UI remains authoritative.
5. Aizu Banner shows the complete command in a selectable, scrollable code region with `Deny`,
   `Allow once`, and `Choose in terminal`. The terminal action returns no decision immediately,
   ending Aizu's synchronous hook so the agent can show its standard approval prompt. A request
   becomes actionable only after the native banner window reports a
   successful show and the banner WebView acknowledges that the exact command and controls were
   rendered; terminal fallback remains available if that acknowledgement fails. A decision is
   atomically consumed once. A close or swipe also means fallback, not deny.
6. The CLI prints only the agent's structured `allow` or `deny` hook response. Aizu never executes
   the command and does not support “always allow”, permission mutation, or free-form answers.
7. The feature is enabled by default and can be disabled immediately in Settings. Approval requests
   always use Aizu Banner even when normal notifications use Notification Center, because a direct
   allow action requires the complete command to remain visible.
8. This ADR covers local hooks only. Remote approval requires a separately versioned, bidirectional
   SSH bridge extension. Until then, remote requests continue to use the source terminal's normal
   approval UI.
9. Banner data and actions are restricted to the banner WebView by caller-label checks. The main
   and banner WebViews may listen for backend events, but frontend capability grants do not include
   event emit or emit-to permissions, preventing a second WebView from forging the command shown
   for a real approval ID.

## Consequences

- Users can approve common local shell commands without locating the terminal while retaining the
  agent's built-in prompt as an explicit fallback. The synchronous hook contract does not expose
  both decision surfaces at once: choosing the terminal path ends the notification-side request
  before the agent renders its prompt.
- Raw commands exist briefly in the hook process, local socket buffers, desktop memory, and the
  banner WebView. They do not become durable notification content.
- Unsupported permission tools continue unchanged. This avoids fabricating an incomplete approval
  view for structured actions that cannot be represented exactly as one command.
- Native Notification Center remains display-only for command approval; it cannot safely expose a
  direct allow action with truncated content.

## Alternatives considered

- **Execute the command in Aizu:** rejected because the agent, not Aizu, owns tool execution and
  policy. It would create an arbitrary shell-execution API in the desktop app.
- **Persist the raw request with the event:** rejected because command text can include credentials,
  paths, prompts, or user data and would cross history/retention/SSH boundaries.
- **Treat closing the banner as deny:** rejected because UI dismissal is ambiguous and could alter
  agent behavior unexpectedly. Fallback preserves the terminal confirmation.
- **Use Notification Center action buttons:** rejected for direct allow because macOS can truncate
  the command and the user cannot verify the exact request.

## References

- [Codex Hooks](https://learn.chatgpt.com/docs/hooks)
- [Claude Code Hooks](https://code.claude.com/docs/en/hooks)
