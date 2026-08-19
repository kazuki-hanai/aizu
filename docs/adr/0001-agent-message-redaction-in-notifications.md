# ADR 0001: Agent メッセージは既定表示し、sensitive token はマスクする

- Status: Accepted
- Date: 2026-08-19
- Deciders: repository owner
- Related: `docs/mvp-design.md` §8.3, architectural invariant #12

## Context

通知に「AI からのメッセージがちゃんと出ないケースがある」という報告があった。原因は 2 つの設計判断だった。

1. `agent_details_enabled` の既定が `false` で、多くの通知が generic テンプレート（"Task finished on <source>" など）のみを表示していた。
2. 有効時でも `safe_agent_excerpt` が、private path や credential marker を 1 つでも含むメッセージ全体を `None` にして破棄していた。結果として通知は静かに generic テンプレートへフォールバックし、agent メッセージが消えていた。

architectural invariant #12（prompt/response/絶対 path/secret を既定で通知へ載せない）との整合を保ちつつ、「全通知で agent メッセージを見たい」という要求を満たす必要がある。

## Decision

1. `agent_details_enabled` の既定を `true` にする（core policy / desktop `Preferences` / frontend contract / persisted settings の serde default）。first-party adapter（`codex-v1` / `claude-code-v1`）のメッセージは既定で通知と activity に表示する。pre-versioned settings に旧既定 `false` が保存済みの場合は、settings schema version 1 への一回限りの migration で `true` にする。version 1 以後に user が明示した `false` は保持する。
2. `safe_agent_excerpt` を「破棄」から「in-place redaction」に変更する。
   - private path token → `[path]`
   - credential value token（known provider token、JWT/high-entropy token）、secret `key=value`、Authorization header、credential-bearing URL → `[redacted]`
   - `Bearer` / `Basic` / `Token` / `Blob` / `Base64` / `Encoded` の直後は、値の語形・長さ・後続文に関係なく明示的な sensitive-value context として `[redacted]`。普通語の false positive より credential 漏えい防止を優先し、値以外のメッセージは保持する。
   - multiline private key block 全体 → `[redacted private key]`
   - 残りのメッセージは保持し、必ず表示する。
   - 表示不能な non-whitespace control character を含む値のみ、従来通り excerpt 全体を破棄する。
3. 非 first-party / remote spoof 対策として、trusted adapter 以外の `body` は引き続き通知へ出さない（generic テンプレートのまま）。

## Consequences

- すべての first-party 通知に agent メッセージが表示され、報告された「出ないケース」が解消する。
- 実際の secret / 絶対 path はマスクされ、Notification Center やロック画面へ生の credential が残らない。
- invariant #12 は「既定で secret を載せない」を維持する（redaction を強制）が、「adapter 要約は opt-in」という以前の既定は本 ADR で上書きされる。
- 影響テスト: `aizu-core` の `adapter` / `notification` / `pipeline`（adapter → source spool → desktop history → notification の adversarial corpus を含む）、desktop `model` / `store` migration、frontend `App.test.tsx` を新挙動へ更新した。

## Alternatives considered

- **生メッセージをそのまま表示**: 「全部出す」に最も忠実だが、API key や SSH 秘密鍵が永続的な通知履歴へ漏れる回帰（Blocker）となるため却下。
- **opt-in のまま維持し redaction だけ追加**: 既定オフのため「全通知で出す」を満たさず却下。
- **trusted adapter ゲートの撤廃**: compromised remote が任意本文を通知へ注入できるため却下。
