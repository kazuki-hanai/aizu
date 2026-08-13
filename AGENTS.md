# AGENTS.md — Aizu 開発ガイド

この文書は、このリポジトリで作業する人間および AI agent に対する共通の開発規約である。
root の本ファイルはリポジトリ全体に適用される。サブディレクトリに、より限定された `AGENTS.md` がある場合は、そのディレクトリ以下では両方を読み、競合時は近い階層の指示を優先する。

本書では次の用語を使う。

- **MUST / 必須**: 例外なく守る。例外が必要なら PR に理由と承認者を記録する。
- **SHOULD / 原則**: 強い理由がない限り守る。逸脱理由を PR に記録する。
- **MAY / 任意**: 状況に応じて選択できる。

プラットフォーム、system、user から与えられた指示は常に本書より優先する。

---

## 1. 最初に読むもの

作業開始前に、最低限次を読む。

1. この `AGENTS.md`
2. 作業対象に近い階層の `AGENTS.md`
3. [`docs/mvp-design.md`](docs/mvp-design.md)
4. 変更対象に関係する protocol、schema、ADR、tests、直近の PR
5. `.handovers/` が存在する場合は、最新の handover

仕様の優先順位は原則として次の通りとする。

1. 現在の task で明示された acceptance criteria
2. 最も近い階層の `AGENTS.md`
3. root `AGENTS.md`
4. versioned protocol / JSON Schema / migration
5. Design Doc / ADR
6. 既存テスト
7. 既存実装

文書と実装が矛盾している場合、都合のよい方を黙って選ばない。矛盾を明示し、正しい仕様へ文書・テスト・実装を同じ PR で揃える。

---

## 2. Session start checklist

各 session の最初に以下を実行する。

1. project root と現在の作業ディレクトリを確認する。
2. `.handovers/` の有無を確認する。
3. handover がある場合、filename timestamp 順で最新の `.md` を読む。
4. handover の **Rejected Approaches** を確認し、理由なく同じ失敗を繰り返さない。
5. **Next Session Priorities** と現在の user request を照合する。
6. `git status --short --branch`、現在 branch、remote、未コミット差分を確認する。
7. user や別 agent の未コミット変更を特定し、上書きしない。
8. 関連 Design Doc、schema、tests を読む。
9. acceptance criteria、変更範囲、必要なテストを短い plan にする。

`.git` がまだ存在しない場合は、その事実を明示する。GitHub PR、CI 成功、merge 完了を装ってはならない。repository の初期化や remote への push は user の意図を確認してから行う。

---

## 3. Project overview

Aizu は、端末で動く AI/LLM agent の完了・質問・許可待ちイベントを、デスクトップ通知へ届けるアプリである。

主実装言語は **Rust**。event model、CLI、SQLite、SSH、protocol、notification policy、Tauri backend は Rust workspace に置く。TypeScript/React は薄い desktop presentation layer に限定し、business logic、trusted state、任意 process execution を持たせない。

MVP の対象は次の通り。

- macOS のメニューバー常駐 desktop app
- ローカル agent の `task.completed`（成功・失敗・キャンセルを `outcome` で区別）
- ローカル agent の `agent.question`
- SSH 接続可能なリモート端末からの同イベント
- 一時切断中の spool と再接続後の再配送

### 3.1 Architectural invariants

以下は単なる実装案ではなく、MVP の不変条件である。変更には Design Doc と ADR の更新、および明示的な承認が必要。

1. **通知イベント用の中央 backend を設けない。**
2. リモート配送には、受信 Mac から開始する既存の SSH 接続を使う。
3. リモート側には単一 `aizu` CLI だけを配置する。
4. リモート側に Aizu 用 daemon、Web server、常時待受 port を置かない。
5. `aizu bridge` は SSH の子としてのみ動く短命 process とする。
6. 切断中の event は source 端末の SQLite spool に保存する。
7. 配送は at-least-once とし、desktop 側で deduplication する。
8. SSH key、password、host verification は system SSH に任せる。
9. Aizu は SSH private key や password を独自保存しない。
10. `StrictHostKeyChecking=no` などで host key verification を無効化しない。
11. terminal output の scraping ではなく、agent hook または明示的 CLI 呼び出しを使う。
12. prompt、response、絶対 path、secret を既定で通知・ログへ載せない。
13. core、event schema、storage、bridge protocol は cross-platform に保つ。
14. GitHub Releases は配布・更新 artifact の置き場であり、通知イベントの relay には使わない。

### 3.2 MVP non-goals

- 独自 account / cloud sync / relay API
- iOS/watchOS app と APNs
- Slack/Discord/Teams integration
- 通知から agent へ回答を返す操作
- Windows/Linux desktop app の正式配布
- agent の会話全文・生成物の収集

non-goal を「ついでに」実装しない。必要なら別 issue と Design Doc 変更を作る。

---

## 4. Intended repository layout

実装は原則として次の責務分割を維持する。

```text
.
├── .github/
│   ├── PULL_REQUEST_TEMPLATE.md
│   └── workflows/
│       ├── ci.yml
│       ├── nightly.yml
│       └── release.yml
├── .handovers/
├── assets/
│   └── branding/
│       ├── README.md
│       ├── app-icon/
│       ├── tray/
│       └── icon-manifest.json
├── apps/
│   └── desktop/
│       ├── src/
│       └── src-tauri/
│           └── icons/
├── crates/
│   ├── aizu-core/
│   └── aizu-cli/
├── docs/
│   ├── adr/
│   ├── schemas/
│   ├── mvp-design.md
│   └── protocol.md
├── tests/
│   ├── e2e/
│   └── fixtures/
├── scripts/
│   ├── generate-icons.sh
│   └── check-icons.sh
├── Cargo.toml
├── Cargo.lock
├── package.json
├── pnpm-lock.yaml
└── rust-toolchain.toml
```

責務:

- `aizu-core`: event model、validation、redaction、SQLite、protocol、cursor、deduplication、notification policy
- `aizu-cli`: `emit`、`hook`、`bridge`、`doctor`、`identity`、`version`
- `apps/desktop`: Rust/Tauri backend が source lifecycle と native integration を所有し、TypeScript/React は tray/settings/history の presentation を担当
- `assets/branding`: canonical app/tray artwork、provenance、generation manifest
- `apps/desktop/src-tauri/icons`: generated package assets。手動編集禁止
- `docs/schemas`: machine-readable schema
- `tests/fixtures`: secret を除去した agent hook payload

business logic を frontend component や Tauri command handler に埋め込まず、可能な限り `aizu-core` へ置く。

---

## 5. Toolchain and local setup

### 5.1 Toolchain policy

- Rust toolchain は `rust-toolchain.toml` で pin する。
- Node.js version は repository の version file で pin する。
- package manager は `pnpm` に統一し、`package.json#packageManager` で version を固定する。
- Rust と frontend の lockfile は commit する。
- lockfile を手作業で編集しない。
- macOS desktop build には Xcode Command Line Tools を使う。
- local/remote SSH test は system OpenSSH client を使う。

manifests や scripts がまだ存在しない段階で、存在しない command を「成功した」と報告してはならない。bootstrap PR で command を実装し、本書と CI を同時に更新する。

### 5.2 Canonical commands

workspace scaffold 後の標準 quality gate は次を基本とする。package scripts が追加されたら、直接の長い command より repository script を優先する。

```bash
# Rust
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features

# Frontend
corepack pnpm install --frozen-lockfile
corepack pnpm lint
corepack pnpm typecheck
corepack pnpm test

# Desktop development
corepack pnpm tauri dev

# Branding assets
./scripts/generate-icons.sh   # source を変更した場合だけ実行
./scripts/check-icons.sh      # generated set と manifest を検証

# Full repository verification; bootstrap 時に script を用意する
./scripts/check.sh
```

新しい platform requirement、environment variable、setup command を導入したら、同じ PR で onboarding docs と CI を更新する。

---

## 6. Development workflow

すべての変更は、原則として以下の流れで進める。

### Step 1: Task を明確にする

- user problem と expected behavior を一文で説明できるようにする。
- in-scope / out-of-scope を分ける。
- acceptance criteria をチェック可能な形で書く。
- security、privacy、compatibility、migration、release への影響を確認する。
- 不明点は repository から調査する。危険な仮定だけを user に確認する。

### Step 2: Design impact を判断する

以下を変更する場合、実装前または同時に Design Doc / protocol / schema / ADR を更新する。

- event schema
- bridge frame、version、delivery semantics
- SQLite schema、migration、retention
- SSH command construction、authentication、trust model
- desktop capability、permission、secret handling
- backend の有無、network topology
- supported OS / architecture
- release、signing、updater
- privacy default

既存の architectural invariant を変える場合は、小さな code change として扱わない。

### Step 3: Branch を作る

`main` へ直接 commit/push しない。最新の `main` から task 専用 branch を作る。

Branch prefix:

- `feat/<short-name>`
- `fix/<short-name>`
- `docs/<short-name>`
- `test/<short-name>`
- `refactor/<short-name>`
- `ci/<short-name>`
- `chore/<short-name>`
- `release/<version>`

例:

```text
feat/local-event-spool
fix/duplicate-notification
docs/ssh-threat-model
ci/macos-e2e
```

branch 名に user name、secret、ticket の機密情報を入れない。

### Step 4: 小さな vertical slice で実装する

- 可能なら schema/model → core → adapter → UI → tests の順で進める。
- bug fix はまず再現 test を追加する。
- 外部 input の validation と failure path を happy path と同時に実装する。
- unrelated refactor や formatting churn を混ぜない。
- generated file、binary、build output を commit しない。
- 例外として `apps/desktop/src-tauri/icons/` の generated branding assets は package input のため commit する。canonical source 変更後に script で生成し、生成物だけを手動編集しない。
- task 中に別問題を見つけたら、現在の fix に必須でなければ issue 化する。

### Step 5: Targeted tests を早く回す

変更した module の最小 test を先に実行し、短い feedback loop を保つ。その後 full quality gate を実行する。

### Step 6: Self-review

push 前に必ず確認する。

```bash
git status --short
git diff --check
git diff
```

確認事項:

- acceptance criteria を満たすか
- unintended file が含まれていないか
- tests が behavior を検証しているか
- log、fixture、screenshot に secret がないか
- error path、timeout、cancellation、retry が安全か
- docs/schema/migration/version 更新漏れがないか
- cross-platform core に macOS 固有 path/API が漏れていないか

### Step 7: Commit

Conventional Commits 形式を使う。

```text
feat(core): add durable event spool
fix(ssh): reject option-like host aliases
test(cli): cover concurrent emit calls
docs: define independent-agent review flow
ci: add macOS desktop build gate
```

Rules:

- 1 commit は論理的に説明可能な単位にする。
- “fix stuff”、“WIP” のような曖昧な final commit message を使わない。
- formatter だけの差分は可能なら機能差分と分ける。
- review 開始後の history rewrite / force-push は原則禁止。
- signed commit を repository setting が要求する場合は従う。

### Step 8: Draft PR を作る

早期共有が有効なら Draft PR を作成してよい。ただし、Ready for review にする前に local checks と self-review を完了する。

### Step 9: CI と independent agent review

実装者とは別の agent に、現在の PR head commit をレビューさせる。詳細は後述する。

### Step 10: 指摘を修正して再検証する

- 各 finding に「修正」「説明して合意」「follow-up issue」のいずれかで応答する。
- blocker/high finding を未解決のまま merge しない。
- code を変更したら relevant tests と CI を再実行する。
- review 対象 SHA が変わったら、independent agent の再レビューを受ける。

### Step 11: Merge

merge checklist を満たしたときだけ squash merge する。

### Step 12: Post-merge

- `main` CI を確認する。
- branch を削除する。
- release/migration/operations follow-up があれば issue を更新する。
- 未完了作業が残る場合は handover を作成する。

---

## 7. Multi-agent collaboration rules

### 7.1 Role separation

最低限、次の二つを分ける。

- **Implementer agent**: code/docs/tests を変更する。
- **Reviewer agent**: diff、仕様、tests、failure path を独立に検証する。

PR に code を commit した agent は、その PR の最終 independent reviewer にはなれない。reviewer が直接 code を変更した場合、その reviewer は contributor とみなし、別の independent reviewer が必要になる。

### 7.2 Work ownership

複数 agent を並行利用する場合:

- subtask を具体的・自己完結的にする。
- write scope を file/directory 単位で分ける。
- 同じ file を複数 agent が同時編集しない。
- blocker ではない side task だけを並列化する。
- 同じ調査を複数 agent に重複させない。
- agent の結果は鵜呑みにせず、integrator が diff と test result を確認する。

### 7.3 Existing work protection

- user や別 agent の未コミット変更を削除・上書きしない。
- `git reset --hard`、`git clean -fdx`、無断 `git checkout -- <file>` を使わない。
- 無断で stash、rebase、amend、force-push しない。
- unrelated diff を「きれいにする」目的で変更しない。
- conflict は内容を理解して解消し、片側を機械的に捨てない。
- repository 外の file を変更しない。

### 7.4 Process and runtime safety

- 任意・未知の PID に対する `kill -9`、`taskkill /F`、`Stop-Process -Force` を禁止する。
- dev server は owning task/session を名前または起動元 terminal から graceful に停止する。
- 自分が動作している shell、container、VM、WSL、service を停止・再起動する可能性がある command は実行しない。
- その操作が必須なら、session が終了すること、復旧手順、再開 command を user に示し、user に実行を委ねる。
- destructive command、credential 操作、外部公開、release publish は明示的な権限と確認を必要とする。

---

## 8. Pull request requirements

### 8.1 PR scope

- 1 PR は 1 つの主目的にする。
- review 可能な大きさを優先する。
- 原則として generated file を除く変更が大きくなりすぎる前に分割する。
- schema migration と利用 code、bug fix と regression test のように、分けると壊れるものは同一 PR に含める。
- mass rename、dependency update、feature change を同じ PR に混ぜない。

### 8.2 PR title

PR title は squash commit として利用できる Conventional Commits 形式にする。

```text
feat(desktop): notify on local agent questions
fix(protocol): preserve cursor across reconnects
docs: add MVP development workflow
```

### 8.3 Required PR body

PR 本文には最低限次を含める。

```markdown
## Summary
- 何を変更したか

## Why
- 解決する問題と user impact

## Scope
- In scope
- Out of scope

## Design
- 主な判断
- Design Doc / ADR / issue link

## Testing
- [ ] 実行した command と結果
- [ ] 追加した regression/failure test
- [ ] 未実行項目と理由

## Security and privacy
- SSH / shell / secret / notification content / permission への影響

## Compatibility and migration
- protocol/schema/DB/OS/CLI compatibility

## UI evidence
- UI 変更時の screenshot または録画
- icon/tray 変更時は app icon の縮小 preview と、menu bar の light/dark/Increase Contrast preview

## Risks and rollback
- 主な failure mode
- rollback 方法

## Independent review
- Required reviewer quorum:
- Reviewer agents:
- Reviewed commit SHA:
- Verdicts:
- Findings resolved:

## Checklist
- [ ] Acceptance criteria を満たした
- [ ] Self-review 済み
- [ ] Docs/schema/tests を更新した
- [ ] Required CI が成功した
- [ ] Required reviewer quorum 全員が現 HEAD を承認した
```

未実行 test を隠さない。「CI で実行予定」だけを local success として書かない。

### 8.4 Draft to ready

Ready for review にする条件:

- 既知の blocker がない
- relevant local tests が成功
- PR body が埋まっている
- Design Doc / schema / migration が同期
- self-review が完了
- secret や accidental artifact がない

---

## 9. Independent agent review process

すべての code、workflow、schema、security-sensitive docs の PR は、実装者とは別 agent にレビューさせる。既定 quorum は 1 だが、user/issue が 3 体などの quorum を指定した場合は、その数の相互に独立した reviewer 全員が current HEAD を確認し、指摘がなくなるまで review → fix → re-review を繰り返す。単純な typo のみの docs PR は repository owner が例外を認められるが、CI は省略しない。

### 9.1 Review request

Implementer は reviewer に次を渡す。

- PR URL または完全 diff
- exact head commit SHA
- user request / issue
- acceptance criteria
- relevant Design Doc / ADR
- changed files
- 実行済み tests
- 特に見てほしい risk

reviewer に結論を誘導してはならない。「問題がないか独立に確認する」と依頼する。

### 9.2 Reviewer responsibilities

Reviewer は次を行う。

1. reviewed SHA が PR HEAD と一致することを確認する。
2. user request と acceptance criteria を読み直す。
3. Design Doc と architectural invariants への適合を確認する。
4. diff だけでなく、呼び出し側・tests・schema・migration を確認する。
5. 可能な範囲で relevant test を自身でも実行する。
6. happy path だけでなく failure、retry、duplicate、concurrency、compatibility を確認する。
7. security/privacy と shell/SSH boundary を確認する。
8. findings を severity 順、file/line 付きで報告する。
9. findings がなければ、確認した範囲と残余 risk を明記する。
10. 最終 verdict を `APPROVE` または `REQUEST_CHANGES` とする。
11. 複数 reviewer quorum の場合、先行 reviewer の結論を前提にせず独立に要件を再導出する。

review report は GitHub の PR review または PR comment として永続化する。session 内の口頭報告だけでは merge gate を満たさない。reviewer が GitHub 上で正式な approval を付けられない場合も、下記 template の report と exact SHA を PR comment に残す。

### 9.3 Severity

- **Blocker**: data loss、credential exposure、RCE、release compromise、主要要件未達、build/test failure
- **High**: 通常利用での誤動作、重複/欠落、migration/compatibility failure、重大な test 欠落
- **Medium**: edge case、保守性、diagnostics、将来障害につながる設計不備
- **Low/Nit**: 可読性、命名、非 blocking な改善

Blocker/High は merge 前に解決必須。Medium を follow-up にする場合は、owner・理由・issue を PR に残す。

### 9.4 Review report template

```markdown
## Independent agent review

- Reviewer: <agent/session identifier>
- Reviewed commit: `<full SHA>`
- Verdict: APPROVE | REQUEST_CHANGES

### Findings
1. [Blocker|High|Medium|Low] `path/to/file:line`
   - Problem:
   - Impact:
   - Suggested fix:

### Verification
- Commands run:
- Results:
- Areas inspected:

### Residual risks
- ...
```

“LGTM” だけの review は承認として扱わない。

### 9.5 Stale review

- review 後に PR HEAD が変わったら承認は stale とする。
- typo のみであっても、reviewed SHA を更新して reviewer に差分確認を依頼する。
- GitHub branch protection では stale approvals dismissal を有効にする。
- implementer は全 conversation を resolve する前に、対応内容を reply する。

CI と independent review は PR 作成後に並行実行してよい。ただし、修正 commit が入った場合は relevant CI と review の両方を current HEAD に対してやり直す。

---

## 10. CI requirements

### 10.1 Pull request CI

`ci.yml` の target jobs:

1. **docs-contract**
   - Markdown/internal links
   - JSON Schema validity
   - Design/protocol examples and size-limit consistency
2. **branding-assets**
   - deterministic app/tray icon generation check
   - dimensions, alpha, sRGB, monochrome template validation
   - source hash/tool version/expected output manifest
   - Tauri default icon fingerprint rejection
3. **rust-quality**
   - `cargo fmt --all --check`
   - clippy with `-D warnings`
   - workspace unit/integration tests
4. **frontend-quality**
   - frozen lockfile install
   - lint
   - typecheck
   - unit tests
5. **ssh-integration-linux**
   - ephemeral localhost `sshd`
   - known_hosts validation
   - real SSH bridge
   - reconnect / missing CLI / protocol mismatch
6. **build-check-macos**
   - unsigned macOS desktop build
   - macOS core/CLI build
7. **build-check-cross-platform**
   - Windows/Linux core/CLI compile matrix
8. **desktop-e2e-macos**
   - `@wdio/tauri-service` test-only embedded WebDriver
   - onboarding、source status、history、backlog UI
9. **security**
   - dependency advisory
   - license policy
   - repository secret scanning

最低限、次を workflow/job の表示名と完全一致する branch protection required checks にする。

- docs-contract
- branding-assets
- rust-quality
- frontend-quality
- ssh-integration-linux
- build-check-macos
- desktop-e2e-macos
- security

`build-check-cross-platform` も原則 green を要求し、必要なら固定名の aggregate check を置く。workflow が未実装の段階では「CI 完了」と記載せず、bootstrap work として明示する。

### 10.2 Nightly CI

`nightly.yml`:

- macOS / Windows / Linux compile matrix
- long-running concurrency tests
- reconnect/chaos tests
- previous release DB fixture からの migration
- dependency/license audit
- unsigned packaging dry run

nightly failure は放置しない。owner と issue を割り当てる。

### 10.3 CI security

- workflow top-level permissions は原則 `contents: read`。
- write permission は job 単位で最小限にする。
- third-party actions は full commit SHA で pin する。
- fork PR に secrets を渡さない。
- untrusted code と release secrets を同じ job で扱わない。
- `pull_request_target` で PR code を checkout/execute しない。
- signing/notarization secrets は protected GitHub Environment に限定する。
- artifact attestation の `id-token: write` / `attestations: write` と Release upload の `contents: write` は、それぞれの protected release job にだけ付与する。
- cache key に lockfile hash と toolchain を含める。
- downloaded tool/artifact は checksum/signature を検証する。
- workflow file 変更は independent review の必須対象とする。
- concurrency group を使い、古い PR run を cancel する。

### 10.4 CI failure handling

- log を読まずに rerun を繰り返さない。
- まず failure を分類し、可能なら local reproduce する。
- code/test/workflow が原因なら修正する。
- external transient と合理的に判断できる場合のみ rerun する。
- flaky test を黙って skip、retry 無制限、assertion 削除で隠さない。
- quarantine が必要なら owner、期限、issue、復旧条件を記録する。
- required check を無効化して merge しない。

### 10.5 Recommended branch protection

`main` には少なくとも次を設定する。

- pull request 経由の変更を必須化
- required status checks を必須化
- merge 前の conversation resolution を必須化
- stale approval の dismissal
- merge queue、または merge 直前の base branch 同期
- force-push と branch deletion の禁止
- administrator bypass の常用禁止

GitHub の approval 数だけで independent agent review を代替しない。逆に agent review の comment だけで、repository が要求する human/code-owner approval を代替しない。両方が設定されている場合は両方を満たす。

---

## 11. Merge policy

### 11.1 Merge checklist

次のすべてを満たすまで merge しない。

- [ ] PR が Draft ではない
- [ ] PR title/body が完全
- [ ] branch が protected base branch と競合していない
- [ ] acceptance criteria を満たす
- [ ] required local/repository tests が成功
- [ ] required GitHub Actions checks が全て green
- [ ] task で要求された independent reviewer quorum 全員が current HEAD SHA を承認
- [ ] Blocker/High finding がゼロ
- [ ] unresolved review conversation がゼロ
- [ ] docs/schema/migration/changelog が同期
- [ ] security/privacy review が必要な変更で完了
- [ ] release/rollback impact が明記

### 11.2 Merge method

- 原則 **Squash and merge**。
- squash commit message は PR title を使い、Conventional Commits に適合させる。
- merge 後は feature branch を削除する。
- `main` への direct push と required checks bypass を禁止する。
- merge queue が利用可能なら、required checks の最終確認に利用する。

### 11.3 Emergency exception

重大障害対応でも、可能な限り PR・test・independent review を維持する。どうしても bypass する場合:

1. repository owner が明示承認する。
2. bypass 理由と risk を記録する。
3. 最小 patch に限定する。
4. 直後に full CI と independent retrospective review を行う。
5. 不足 test/doc を follow-up PR で補う。

AI agent 自身の判断だけで emergency bypass してはならない。

---

## 12. Testing policy

### 12.1 General rules

- behavior change には test を追加・更新する。
- bug fix は修正前に失敗する regression test を原則追加する。
- test は implementation detail より observable behavior を検証する。
- success、failure、boundary、recovery を含める。
- real user home、real spool、real SSH keys、production credentials を使わない。
- temporary directory と isolated DB を使う。
- test 順序に依存しない。
- wall-clock sleep に頼らず、fake clock、bounded timeout、deterministic synchronization を使う。
- secret を含む実 payload を fixture に commit しない。

### 12.2 Required test layers

#### Unit

- event validation / size limits / redaction
- source identity / UUID / pinning / changed identity / duplicate source
- cursor / duplicate suppression
- empty spool / high watermark / cursor rewind
- gap / retention timestamp / clock skew / byte quota / backlog aggregation
- quiet hours / notification policy
- retry / backoff / jitter
- bridge frame parser
- malformed / unknown optional / oversized event and frame
- integer boundaries / duplicate JSON key rejection
- pre-handshake error / startup / heartbeat / stale timeout
- migration

#### CLI integration

- `emit` argument / stdin JSON
- concurrent emit
- stdout/stderr separation
- stable exit code
- `doctor --json`
- agent fixture conversion

#### Local pipeline

`emit -> spool -> ingest -> outbox -> FakeNotifier` を process boundary 付きで検証する。

#### SSH integration

ephemeral `sshd` を使い、system SSH を実際に通す。

- authentication
- known_hosts
- remote bridge
- reconnect
- missing CLI
- protocol mismatch
- stderr categorization

CI 用 `sshd` は test fixture であり、本番 backend ではない。

#### Concurrency

- 複数同時 emit
- bridge read 中の write
- pruning との競合
- SQLite busy timeout
- app crash/restart 後の outbox
- second desktop instance が worker を開始しないこと

#### Desktop E2E

- onboarding
- single-instance behavior
- fake notification permission
- local/remote source state
- reconnect/error
- history
- backlog summary
- settings persistence

#### Native/release smoke

実 Notification Center、notification permission 再設定、signed app、DMG、notarization は release candidate で手動または artifact smoke test を行う。

### 12.3 Coverage goals

- core の重要 branch: 80% 以上を目標
- protocol parser、migration、deduplication: 90% 以上を目標

数値を満たすためだけの価値の低い test は追加しない。security/failure/recovery path の実質的 coverage を優先する。

---

## 13. Rust guidelines

- stable Rust を使い、toolchain を pin する。
- `rustfmt` と clippy `-D warnings` を通す。
- public API、protocol type、security-sensitive function に doc comment を付ける。
- external input path で `unwrap()` / `expect()` / panic を使わない。
- recoverable error は typed error と context 付きで返す。
- user-facing error と internal diagnostic を分離する。
- `unsafe` は原則禁止。必要なら safety invariant、test、ADR、independent security review が必須。
- async runtime thread 上で blocking DB/process I/O を無制限に実行しない。
- child process、read、connect、lock に timeout/cancellation を用意する。
- unbounded channel、unbounded read、無制限 retry を使わない。
- external payload の size、nesting、UTF-8、control character を検証する。
- protocol JSON は duplicate object key と integer overflow/underflow を拒否する。
- platform 固有処理は trait/adapter と `cfg` boundary に閉じ込める。
- test helper 以外で user directory を固定文字列にしない。

---

## 14. TypeScript / React / Tauri guidelines

### TypeScript / React

- strict TypeScript を維持する。
- `any`、unchecked cast、non-null assertion を常用しない。
- backend/IPC input は runtime validation する。
- business rule を React component に実装しない。
- component は loading、empty、error、permission denied、reconnecting state を持つ。
- keyboard operation、focus、label、contrast を考慮する。
- UI text に secret、絶対 path、raw SSH stderr を無加工で表示しない。

### Tauri

- frontend は信頼境界の外として扱う。
- Tauri commands は narrow、typed、validated にする。
- capability/allowlist は最小権限にする。
- frontend から任意 command や任意 shell を実行できる API を公開しない。
- user input を shell command string へ連結しない。
- single-instance plugin と desktop DB lock で ingest/notification worker を 1 instance に限定する。
- updater public key は埋め込めるが、private key は repository/app/artifact に含めない。
- test-only WebDriver server を release build に含めない。
- notification、autostart、updater plugin の permission を必要範囲に限定する。

### Branding and icons

- app icon、macOS tray template icon、interface icon は別 asset として設計し、相互流用しない。
- canonical artwork は `assets/branding/` に置き、license/provenance、generator version、source hash を `icon-manifest.json` に記録する。
- `apps/desktop/src-tauri/icons/` は script 生成物とし、手動編集しない。
- app icon は 1024×1024 square source と vector layers を保持し、corner mask、system shadow、gloss を source に焼き込まない。
- macOS tray icon は black + transparent alpha の template image とし、Tauri `iconAsTemplate` を有効にする。
- tray state は `normal` / `attention` / `paused` / `error` とし、色だけで意味を伝えない。tooltip、status text、accessibility label を併用する。
- icon source を変更したら `scripts/generate-icons.sh` と `scripts/check-icons.sh` を実行し、app/tray preview evidence を PR に添付する。
- Tauri default icon、unapproved placeholder、third-party trademark を release artifact に含めない。
- icon generator、SVG/PNG optimizer を新規導入する場合も §19 の dependency/supply-chain review を行う。

---

## 15. CLI and hook guidelines

- stdout が protocol/JSON 用の command では、人間向け log を stderr に出す。
- bridge stdout は NDJSON protocol 専用とする。
- hook command は短時間で終了し、agent 本体を長時間 block しない。
- `emit` は durable write が完了してから success を返す。
- normalized `task.completed` は成功・失敗・キャンセルを含む terminal event とし、必ず `outcome` を持つ。判別不能時は `unknown`。
- best-effort `hook` と strict `emit` / `hook --strict` の exit semantics を分け、agent 本体を notification failure で壊さない。
- exit code と `--json` output は versioned public interface として扱う。
- shell-specific behavior に依存しない。
- path、host alias、event name、cursor を厳格に検証する。
- user input を `sh -c`、`bash -c`、PowerShell expression へ渡さない。
- hook adapter は unknown optional field を許容し、required data 不足を安全に扱う。
- emit/hook request から `source_id`、sequence、schema version、inserted timestamp を採用せず、trusted spool state/CLI が生成する。
- source identity regeneration は空 spool でのみ安全に行い、event discard を伴う操作は backup と明示確認なしに実行しない。
- prompt/response 全文や environment 全体を event metadata へコピーしない。

---

## 16. Data, schema, and migration rules

### Event/schema

- `schema_version` を必須にする。
- event ID は stable かつ source 内で unique にする。
- timestamp は UTC RFC 3339。
- source `occurred_at` は表示情報として扱い、ordering/cursor/dedup/retention は sequence、desktop `received_at`、spool `inserted_at` に基づける。clock skew だけで event を捨てない。
- remote `display_name` / `urgency` は hint とし、notification の source label と sound/quiet policy は pinned source に紐づく desktop-local setting で決める。
- event/frame の上限 size を enforcement する。
- unknown optional field は forward compatibility のため許容する。
- required field の型変更・削除は breaking change として扱う。
- JSON Schema と Rust type と fixtures を同じ PR で更新する。

### SQLite

- migration は versioned、append-only、transactional にする。
- destructive migration は MVP では禁止。
- schema file を直接編集して migration history を書き換えない。
- app/CLI 共通 migration を exclusive transaction で serialize し、自分より新しい DB schema を検出した binary は変更せず失敗する。
- OS SQLite へ依存せず、WAL-reset 修正を含む bundled SQLite（`3.51.3+` または公式修正版 backport）を使い、CI/`doctor` で runtime version を検証する。
- spool は local filesystem に限定し、WAL が有効にならない配置へ黙って fallback しない。
- WAL + `synchronous=FULL`、bounded busy timeout、directory `0700`、DB/WAL/SHM `0600` を適用する。
- retention は age/count だけでなく byte quota を持ち、high watermark は prune 後も reset しない。
- WAL、busy timeout、concurrent writer behavior を test する。
- migration failure で元 DB を削除・上書きしない。
- app/CLI の concurrent migration と newer-schema refusal を test する。
- previous released DB fixture からの migration test を追加する。
- cursor update、event insert、outbox insert は必要な atomicity を transaction で保証する。

### Bridge protocol

- protocol major version を handshake する。
- frame は bounded UTF-8 NDJSON。
- event payload は最大 64 KiB、wrapper を含む frame は最大 128 KiB とする。
- 正常 stream の最初の frame は `hello`。negotiation/spool open failure の terminal `error` のみ first frame になり得る。
- sequence は source ごとに単調増加。
- empty spool/high watermark/gap、cursor ahead、source identity pinning/change を test する。
- unknown major version では event を消費しない。
- stdout に debug text を混ぜない。
- reconnect は at-least-once を前提にし、receiver が idempotent であることを test する。
- protocol change は `docs/protocol.md`、fixtures、compatibility test を更新する。

---

## 17. SSH and process security

- `/usr/bin/ssh` など system SSH client を使う。
- `~/.ssh/config`、ssh-agent、known_hosts、ProxyJump を再利用する。
- private key/password を Aizu DB や config に保存しない。
- host key を自動承認しない。
- changed host key を通常の reconnect error として隠さない。
- password prompt で background process を永久に待たせない。
- background 接続は `BatchMode=yes`、`StrictHostKeyChecking=yes`、bounded connect/server-alive timeout を command line で固定する。
- host alias の先頭 `-` を拒否し、option injection を防ぐ。
- local SSH options は fixed allowlist から構築する。
- OpenSSH が remote command を remote shell へ渡すことを前提にする。MVP の remote CLI path は `$HOME/.local/bin/aizu` 固定、remote command は固定 template とし、任意 path/command を UI から受け取らない。
- TTY/stdin、port/X11/agent forwarding、`LocalCommand` を command line で無効化し、user config の `RemoteCommand` 競合を preflight で検出する。
- cursor/version は数値 type から生成する。
- child process の stdout/stderr と frame size を bounded にする。
- Aizu が起動した child process だけを追跡し、graceful に終了する。
- arbitrary PID list を force kill しない。
- SSH error は authentication、host-key、network、remote-command、protocol に分類する。

security boundary を弱めて「接続しやすくする」変更は merge しない。

---

## 18. Privacy, logging, and diagnostics

### Privacy by default

- notification は generic title/body を既定にする。
- prompt/question summary は opt-in。
- path は basename のみを原則とする。
- raw response、environment、token、key、credential を収集しない。
- lock screen に表示される前提で notification content を設計する。
- central telemetry は MVP では送信しない。

### Logging

通常 log に含めてよいもの:

- timestamp
- log level
- component
- source ID の短い hash
- event ID
- error category
- retry count

通常 log に含めないもの:

- notification title/body
- prompt/response
- username/home path
- host address の全文
- SSH key path の全文
- environment variable
- credential/token

diagnostic export は明示操作時だけ作り、追加 redaction を行う。fixture、screenshot、CI log も同じ privacy rule に従う。

---

## 19. Dependencies and supply-chain policy

新しい dependency/action を追加する PR は次を説明する。

- 解決する問題
- standard library / existing dependency で代替できない理由
- maintenance 状況
- license
- security/advisory 状況
- binary size / startup / permission への影響

Rules:

- 不要な dependency を追加しない。
- lockfile を更新して commit する。
- git dependency は commit SHA を pin する。
- CI action は tag ではなく full SHA を pin する。
- release build 中に未検証 script を `curl | sh` で実行しない。
- downloaded binary は checksum/signature を検証する。
- dependency update と feature change は原則別 PR。

---

## 20. Documentation and decision records

### Docs that must stay synchronized

- `docs/mvp-design.md`: product scope と architecture
- `docs/protocol.md`: bridge wire contract
- `docs/schemas/*.json`: machine-readable event contract
- `docs/adr/*.md`: durable technical decisions
- `assets/branding/*` と `apps/desktop/src-tauri/icons/*`: canonical/generated brand assets
- user setup / troubleshooting docs
- release notes / changelog
- `AGENTS.md`: development process

### ADR required changes

次は ADR を作成する。

- central backend または external relay の導入
- SSH 以外の default transport
- storage engine の変更
- event/protocol の breaking change
- framework/runtime の変更
- secret/identity/trust model の変更
- telemetry の導入
- destructive migration

ADR は最低限 Context、Decision、Alternatives、Consequences、Security/Privacy、Migration/Rollback を含める。

code comment は「何をしているか」より、非自明な「なぜ」を説明する。古くなった comment を残さない。

---

## 21. Release and CD

Aizu における deploy は server deployment ではなく、signed desktop/CLI artifact の公開を指す。

### 21.1 Release prerequisites

- SemVer の protected `vX.Y.Z` tag
- version が Cargo、Tauri、frontend で一致
- tag commit が protected `main` に含まれる
- PR CI と release CI が green
- protected GitHub Environment の承認

### 21.2 Release pipeline

1. full tests を再実行
2. target architecture 向け app/CLI build
3. Developer ID で code sign
4. Apple notarization
5. signature/notarization と app/tray icon bundle verification
6. updater artifact signing
7. checksum と SBOM 生成
8. app/CLI/checksum/SBOM の artifact attestation/provenance 生成。利用できない repository plan では dedicated release key で checksum/SBOM を署名
9. draft GitHub Release へ upload
10. clean runner で checksum/signature/attestation を verify して smoke test
11. human approval 後に publish

### 21.3 Release secrets

次は protected environment secret とし、fork PR、log、artifact、repository に露出させない。

- Apple signing certificate/password
- Apple Team ID
- App Store Connect API key
- updater private key/password

### 21.4 Rollback

- 公開済み artifact を同じ version で差し替えない。
- bad release は latest から外す。
- fix は新しい patch version として出す。
- destructive/downgrade-incompatible migration を避ける。
- previous release からの forward migration test を必須にする。

GitHub Releases は static updater/artifact hosting にのみ使い、通知 event を保存・中継しない。

---

## 22. Definition of Ready

実装開始前に以下が揃っていることが望ましい。

- [ ] user problem が明確
- [ ] acceptance criteria が検証可能
- [ ] in-scope / out-of-scope が明確
- [ ] relevant design/schema/protocol が特定済み
- [ ] security/privacy/compatibility impact を評価済み
- [ ] dependency/credential/platform blocker を把握済み
- [ ] test strategy が決まっている
- [ ] 複数 agent 利用時の write scope が分離されている

---

## 23. Definition of Done

task は次のすべてを満たすまで完了ではない。

- [ ] acceptance criteria を満たす
- [ ] implementation が architectural invariants に適合
- [ ] happy/failure/recovery path の tests がある
- [ ] targeted tests と full quality gates が成功
- [ ] `git diff --check` が成功
- [ ] docs/schema/migration/version が同期
- [ ] icon/UI変更時に branding source、generated assets、manifest、light/dark preview が同期
- [ ] secret、PII、raw model content の漏洩がない
- [ ] cross-platform impact を確認
- [ ] PR body と evidence が完全
- [ ] task で要求された independent reviewer quorum 全員が current HEAD SHA を承認
- [ ] required CI が全て green
- [ ] review conversation が解決
- [ ] squash merge 後の `main` CI が green
- [ ] 残作業を issue/handover に記録

---

## 24. Session handover

`.handovers/` が存在する場合、各 session の開始時に最新 handover を読む。

次の場合は session 終了前に handover を作成する。

- work が未完了
- 次 session へ重要な判断や調査結果を引き継ぐ
- rejected approach がある
- migration/protocol/release/security の注意点がある
- user が handover を依頼した

可能なら `handover` skill を使う。保存先:

```text
.handovers/YYYY-MM-DD_HHmm.md
```

Template:

```markdown
---
tool: codex
date: "YYYY-MM-DDTHH:mm:ss"
session_id: "<session-id-if-available>"
---

# Session Handover

## Session Summary

## Work Done

## Decisions Made

## Rejected Approaches

## Files Modified

## Current State

## Unresolved Issues

## Next Session Priorities

## Technical Notes
```

特に **Rejected Approaches** には、試した内容だけでなく、失敗理由、error、関連 file/line を書く。handover に secret を書かない。

---

## 25. Prohibited shortcuts

以下は禁止する。

- required test/CI/review を省略して merge
- implementer 自身だけの「別人格」レビュー
- stale SHA に対する承認の流用
- failing/flaky test の無断 skip
- security check の無効化
- SSH host verification の無効化
- arbitrary shell execution API
- terminal output scraping を正式 integration として追加
- raw prompt/response の log/fixture 保存
- destructive DB migration
- public release artifact の同一 version 差し替え
- secret の repository、PR、CI log、artifact への記載
- user/別 agent の未コミット変更の破棄
- direct push to `main`
- 無断 force-push、hard reset、clean

速度を理由に品質・security・review traceability を落とさない。

---

## 26. Quick PR workflow

通常の変更は次の最短ループで進める。

```text
latest handover / design / git status を確認
  ↓
task と acceptance criteria を確定
  ↓
feature branch 作成
  ↓
test + implementation + docs
  ↓
targeted tests → full local checks → self-review
  ↓
Draft PR / PR 作成
  ↓
GitHub Actions
  ↓
task で要求された数の別 agent が current HEAD SHA を独立レビュー
  ↓
指摘修正 → tests/CI → 再レビュー
  ↓
required CI green + reviewer quorum 全員 APPROVE
  ↓
Squash and merge
  ↓
main CI 確認・branch 削除・必要なら handover
```

この順序を基本とし、**「PR を作る → 別 agent がレビューする → CI が通る → merge する」ことを完了条件の一部として扱う。**

### 26.1 Canonical commands for the PR loop

上のループを実際に実行するときの標準コマンドを示す。GitHub CLI (`gh`) を前提とする。これらは repository が git 初期化済みで GitHub remote に push できる状態を前提とし、未初期化・未 push・CI 未実装の段階では **実行結果を捏造しない**（§2, §5.1, §10.1）。

```bash
# 1) 最新 main から feature branch を作る
git switch main && git pull --ff-only
git switch -c feat/<short-name>

# 2) 変更を段階的に commit する
git add -p
git commit -m "feat(core): add durable event spool"

# 3) branch を push する
git push -u origin feat/<short-name>

# 4) PR を作る（まず Draft、準備できたら Ready へ）
gh pr create --draft --fill --base main
gh pr ready            # self-review と local checks 完了後

# 5) CI を確認する（"CI 通ったら" の判定はここで行う）
gh pr checks --watch   # 全 required check が pass するまで待つ
gh run watch <run-id>  # 個別 workflow run を追跡する場合

# 6) 別 agent のレビューを PR 上に永続化する（§9）
#    reviewer が current HEAD SHA を確認したうえで実行する
gh pr comment <pr> --body-file review-report.md  # §9.4 の report（常に残す）
# reviewer が PR author と別 GitHub identity の場合だけ:
gh pr review <pr> --approve                       # または --request-changes

# 7) merge checklist（§11.1）を満たしたら squash merge する
gh pr merge <pr> --squash --delete-branch

# 8) post-merge
git switch main && git pull --ff-only
```

コマンド利用時の必須条件:

- **CI 通過だけでは merge しない。** required CI が green かつ指定された reviewer quorum 全員が current HEAD SHA を `APPROVE` した両方が揃って初めて step 7 に進む（§9, §11.1）。
- 修正 commit を push したら reviewed SHA が変わるため、step 5 の CI と step 6 のレビューを current HEAD に対してやり直す（§9.5）。
- reviewer agent が PR author と同じ GitHub identity/credential を共有する場合、GitHub は self-approval を正式 approval として扱えない。その場合は exact SHA と verdict を §9.4 形式の PR comment に残し、`gh pr review --approve` を実行したと偽らない。
- `gh pr review --approve` を実装者自身による self-review で代替しない。reviewer は実装者とは別 agent とする（§7.1）。
- repository が human/code-owner approval も要求する場合は、agent review comment と GitHub approval の両方を満たす（§10.5）。
- `--delete-branch` 以外で `main` を直接書き換える操作（direct push、force-push、required checks bypass）を使わない（§11.2, §25）。
