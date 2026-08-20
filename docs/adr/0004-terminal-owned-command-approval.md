# ADR 0004: Agent terminals own command approval

- Status: Accepted
- Date: 2026-08-21
- Deciders: repository owner
- Supersedes: [ADR 0003](0003-local-command-approval-from-aizu-banner.md)
- Related: `docs/mvp-design.md` §4, §13.5, architectural invariants #11, #12

## Context

Codex and Claude Code invoke `PermissionRequest` when their normal approval UI is about to appear.
A synchronous Aizu hook delays that UI while waiting for a notification-side decision. This leaves
the terminal displaying a hook wait instead of its familiar approval controls and forces users to
choose which decision surface becomes active.

Both agents support background command hooks. A background hook can record or notify, but it cannot
allow, deny, or otherwise control the request. That contract fits Aizu's notification role and keeps
the agent's native permission model authoritative.

## Decision

1. Generated first-party `PermissionRequest` command hooks use `async: true` and a five-second
   process timeout. Completion hooks remain synchronous and bounded as before.
2. The CLI parses and persists the privacy-safe `agent.question` event, writes no decision to
   stdout, and does not contact the local approval broker. The agent therefore presents its normal
   approval UI without waiting for Aizu.
3. Permission notifications are passive. They contain only the filtered adapter excerpt and may
   return the user to a verified local terminal session. Aizu Banner and Notification Center expose
   no allow, deny, permanent-grant, or terminal-routing decision buttons.
4. Raw commands remain excluded from the spool, desktop database, history, notification outbox,
   logs, and SSH bridge. Aizu never executes the command or injects input into a terminal.
5. For compatibility with an older installed CLI, the desktop keeps the private local socket long
   enough to return an immediate `unavailable` response with `presented: false`. It never displays
   the legacy approval banner. Re-running agent setup replaces known blocking 50-second Aizu hooks
   with the background form while preserving unrelated hooks.
6. The persisted `command_approvals_enabled` preference field remains readable for settings-schema
   compatibility but is no longer presented or acted upon. It can be removed only with an explicit
   settings migration.
7. Remote SSH permission requests remain passive notifications on the receiving Mac. The source
   terminal remains the only decision surface.

## Consequences

- The standard terminal approval controls appear by default instead of `Running PermissionRequest
  hook` waiting on Aizu.
- Notification delivery cannot delay, approve, or deny a tool request. A notification may arrive
  slightly before or after the terminal prompt because the hook runs independently.
- Direct approval from Aizu is removed. Supporting two simultaneously actionable decision surfaces
  would require an agent-owned, request-addressable approval API rather than terminal input
  injection or a completed background hook.
- Older Aizu CLIs fail open to the agent's normal prompt during the transition instead of waiting
  for the former 45-second broker deadline.

## Alternatives considered

- **Keep `Choose in terminal`:** rejected because routing is not a user decision that should occupy
  the notification. The terminal should be ready without an extra click.
- **Keep notification allow/deny while also showing the terminal prompt:** rejected because once a
  background hook returns, its later output cannot control the pending request. Injecting keys into
  arbitrary terminal applications would be unsafe and unreliable.
- **Keep the synchronous hook but shorten its timeout:** rejected because even a short artificial
  delay replaces the agent's standard permission UI with hook progress.

## References

- [Codex Hooks](https://learn.chatgpt.com/docs/hooks)
- [Claude Code Hooks](https://code.claude.com/docs/en/hooks)
