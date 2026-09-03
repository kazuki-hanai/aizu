# ADR 0006: Answer Claude Code multiple-choice questions from Aizu

- Status: Accepted
- Date: 2026-09-03
- Deciders: repository owner
- Related: [ADR 0005](0005-optional-notification-command-approval.md), `docs/mvp-design.md` §4, §13.5, architectural invariants #11, #12

## Context

ADR 0005 lets an opted-in user answer a `Bash` command or Claude Code `WebFetch`
`PermissionRequest` with a one-shot `Deny` / `Allow once` decision. Claude Code
can also block on the `AskUserQuestion` tool, which presents one question with a
small set of labelled options (a "3-choice" prompt). Today Aizu shows only a
passive notification for that state, so the user must return to the terminal to
choose. Users want to choose the option directly from Aizu.

`AskUserQuestion` differs from an approval in two ways. First, the decision is a
selection among agent-supplied options, not a binary allow/deny. Second, the
`PermissionRequest` hook cannot return a chosen option; only a `PreToolUse` hook
matching `AskUserQuestion` can return an `updatedInput` that auto-answers the
question. Claude Code documents this specialized `updatedInput` as the original
`questions` array plus an `answers` object that maps each question text to the
selected option label. Aizu still fails safe to the terminal for malformed,
unsupported, or changed agent payloads.

Displaying the question and its options, and returning the selected option to the
agent, extends the trust boundary beyond ADR 0005: it shows agent-authored prompt
text and injects a structured answer back into the agent. This is a deliberate,
scoped change to the MVP non-goal in `docs/mvp-design.md` §4.

## Decision

1. A new preference **Answer questions in Aizu** (`question_answers_enabled`) is
   persisted in Settings and defaults to off. It is independent of
   `command_approvals_enabled`. The generated hook is installed during normal
   agent setup, but while the preference is off the broker immediately returns
   unavailable and the current terminal-answer behaviour is unchanged.
2. Aizu installs a synchronous first-party `PreToolUse` hook whose matcher is
   exactly `AskUserQuestion`. The hook has a bounded outer timeout shorter than
   Claude Code's own `AskUserQuestion` timeout so a missed answer always falls
   back to the terminal prompt rather than failing the tool.
3. The hook extracts only the first question's bounded `question`, optional
   `header`, `multiSelect` flag, and its ordered option `label`/`description`
   pairs. `session_id`, `transcript_path`, `cwd`, and all other hook context are
   discarded before the private broker request is built. Sizes and counts are
   bounded and control characters are rejected, matching the ADR 0005 limits.
4. The desktop broker shows a large Aizu dialog that reuses the ADR 0005
   presentation rules (separate `approval_display`, centering, always-on-top,
   one-shot consumption, temporarily hiding passive banners, no close/swipe). The
   dialog renders the bounded question and one button per option. `multiSelect`
   questions are out of scope for this ADR and fall back to the terminal.
5. On an explicit selection, the CLI returns the agent's structured answer for the
   chosen option via the `PreToolUse` `updatedInput` contract. If the selected
   option cannot be mapped to a well-formed answer, the CLI returns no
   `updatedInput` and the agent shows its terminal question. Aizu never invents an
   answer, never selects on the user's behalf, and never returns free-form text.
6. Timing out, stopping Aizu, disabling the setting, losing the broker, a busy
   broker, or a presentation failure returns no answer and hands the question back
   to the terminal. Absence of an answer never means a particular option.
7. The question text, option labels, and option descriptions remain ephemeral in
   the hook process, private socket buffers, desktop memory, and the banner
   WebView. They are never written to the spool, desktop database, history,
   notification outbox, settings, logs, the SSH bridge, or Notification Center.
8. Remote SSH and Notification Center questions stay passive. The one-way bridge
   cannot carry an answer back to a source terminal, so answering is local only.
9. The local approval protocol version is incremented. A desktop app only treats a
   bundled/managed CLI as current when its reported approval protocol matches, so
   an older CLI that cannot answer questions is surfaced as an update, never used
   as if it could answer.

## Consequences

- Opted-in users can answer a single-select `AskUserQuestion` from Aizu; every
  failure path preserves the terminal prompt, so the feature cannot strand or fail
  a question.
- Aizu now installs a `PreToolUse` hook when the setting is on. The setup merger
  adds and removes only its own generated `AskUserQuestion` handler and preserves
  unrelated and lookalike `PreToolUse` hooks.
- The feature depends on Claude Code's documented `PreToolUse` `updatedInput`
  auto-answer contract. If a future Claude Code release changes it, the fail-safe
  path degrades to the existing terminal prompt without breaking the tool.
- `multiSelect` questions and non-first questions remain terminal-answered for now.

## Alternatives considered

- **Answer via the `PermissionRequest` hook:** rejected because allowing the tool
  only lets it run and re-prompt in the terminal; it cannot carry the selection.
- **Enable by default:** rejected because it shows agent prompt text and injects an
  answer, which needs an explicit opt-in like ADR 0005.
- **Support `multiSelect` now:** deferred because a bounded multi-toggle plus a
  confirm step is a larger UI and answer-shape change; single-select ships first.
- **Free-form answers from the notification:** rejected; it remains a non-goal.

## References

- [Claude Code Hooks](https://code.claude.com/docs/en/hooks)
