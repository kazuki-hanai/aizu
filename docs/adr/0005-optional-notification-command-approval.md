# ADR 0005: Notification command approval is an explicit opt-in

- Status: Accepted
- Date: 2026-08-21
- Deciders: repository owner
- Supersedes: [ADR 0004](0004-terminal-owned-command-approval.md)
- Related: `docs/mvp-design.md` §4, §13.5, architectural invariants #11, #12

## Context

The terminal is the least surprising default place to answer Codex and Claude Code permission
requests. Some users nevertheless want to review an exact local shell command and choose a one-time
decision from Aizu Banner. A synchronous agent hook can use either Aizu or the terminal as the
active decision surface for a request, but cannot safely make both surfaces actionable at once.

## Decision

1. **Show command approval buttons** is persisted in Settings and defaults to off. Settings schema
   version 2 resets the former default-on value to off so an upgrade never exposes command text or
   changes the decision surface without a fresh opt-in. Settings schema version 3 adds **Show
   approval in the center**, defaults it to on, and preserves every version 2 approval opt-in.
   Version 5 adds the secondary-display selection and preserves every version 4 display choice.
2. Generated local first-party `PermissionRequest` hooks are synchronous with a 50-second outer
   timeout. When the setting is off, the private desktop broker immediately returns `unavailable`
   with `presented: false`; the CLI returns no decision and the agent continues to its standard
   terminal prompt.
3. When the setting is on, one canonical local `Bash` request may show a large Aizu approval dialog
   containing the exact bounded command and only `Deny` and `Allow once`. Aizu uses the shared
   banner-display preference: the macOS primary display by default, the first connected secondary
   display, the focused-window display, or the pointer display. The dialog is centered by default;
   the separate centering setting moves it to that display's top right without removing its
   approval controls. A missing secondary display falls back to the primary display. It is
   always on top, requests focus with a one-shot informational platform-attention fallback,
   temporarily takes visual priority over queued passive banners, and cannot be closed or swiped
   away. The command becomes actionable only after native window presentation and frontend render
   acknowledgement. The decision is consumed once.
4. Timing out, stopping Aizu, disabling the setting, losing the broker, or failing presentation
   returns no decision. The agent then shows its terminal prompt. There is no separate `Choose in
   terminal` button, and the absence of a decision never means deny.
5. The raw command remains ephemeral in the hook process, private socket buffers, desktop memory,
   and banner WebView. It is never written to the spool, settings, desktop database, history,
   notification outbox, logs, SSH bridge, or Notification Center. Banner data and approval commands
   are restricted to the banner WebView and frontend event emission remains disallowed.
6. Aizu returns only the agent's structured one-time allow or deny response. It never executes the
   command, edits permission rules, or offers permanent approval.
7. Notification Center and remote SSH requests remain passive. Remote decisions stay in the source
   terminal because the current bridge is intentionally one-way.

## Consequences

- Existing and new users retain the terminal approval flow until they opt in.
- Opted-in local requests briefly show `Running PermissionRequest hook` while Aizu owns the decision
  surface. The centered dialog stays visible until an explicit choice or the bounded broker timeout;
  timeout and other failure paths hand the request back to the terminal without a decision.
- Passive banners remain queued while the approval dialog is visible and return to the normal
  top-right stack after the request is resolved.
- Changing the centering setting repositions an existing approval without replaying notification
  sound or changing its one-shot decision state.
- The setup merger replaces recognized five-second background Aizu permission hooks with the
  synchronous form while preserving unrelated and lookalike hooks.
- Two simultaneously actionable approval surfaces remain out of scope without an agent-owned,
  request-addressable approval API.

## Alternatives considered

- **Keep notification approval enabled by default:** rejected because it unexpectedly delays and
  replaces the familiar terminal prompt.
- **Show both notification and terminal controls concurrently:** rejected because background-hook
  output cannot later decide the request, and terminal input injection is unsafe.
- **Keep a `Choose in terminal` button:** rejected because the approval dialog is intentionally a
  focused two-choice surface; bounded failure and timeout paths already preserve terminal fallback.
- **Persist raw approval requests:** rejected because commands can contain credentials, paths, and
  user data.

## References

- [Codex Hooks](https://learn.chatgpt.com/docs/hooks)
- [Claude Code Hooks](https://code.claude.com/docs/en/hooks)
