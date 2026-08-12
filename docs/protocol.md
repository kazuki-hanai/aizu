# Aizu Bridge Protocol

- **Status:** Draft
- **Protocol version:** 1
- **Last updated:** 2026-08-12
- **Source of truth:** This file is the wire contract for the `aizu bridge` stream. It is derived from [`mvp-design.md`](mvp-design.md) §10 and MUST stay synchronized with it and with [`schemas/event-v1.schema.json`](schemas/event-v1.schema.json) (see AGENTS.md §16, §20).

## 1. Overview

The bridge protocol transports normalized events from a source host (local or remote) to the Aizu desktop app.

- Transport は SSH の標準出力に流す UTF-8 NDJSON（1 行 1 JSON object = 1 frame）。
- ネットワークポート、独自 TLS、常駐サーバーは使用しない。
- `aizu bridge` は SSH の子として動く短命プロセスであり、独立サーバーではない。
- stdout は protocol 専用。人間向けログ・診断は stderr に出す。

## 2. Versioning

- protocol version は整数の major version で表す。現行は `1`。
- desktop は起動時に `--protocol <major>` を渡し、CLI は `hello` frame で `protocol_version` を返す。
- desktop が扱えない major version を受け取った場合、event を消費せず接続を停止し、CLI/app の更新を案内する。
- 後方互換な追加は optional field / optional frame として行い、major version を上げない。
- receiver は既知 frame 内の未知 optional field を無視できなければならない。`hello` 完了後の未知 `type` frame も bounded parser で読み飛ばす（forward compatibility）。
- `hello` 前に許容する frame は `hello` または terminal `error` だけであり、未知 `type` を無視して handshake を続けてはならない。
- remote shell startup file が stdout へ banner/text を出した場合も任意行として読み飛ばさず pre-handshake protocol violation とする。UI は shell startup を non-interactive 時に silent にするよう案内する。
- protocol v1 が運ぶ normalized event は `schema_version: 1` とする。event の必須 field/type/kind を互換性なく変更する場合は protocol major も更新する。

## 3. Invocation

desktop は system SSH client を使い、host/options は local process arguments として分離して起動する。

```text
/usr/bin/ssh -T -n \
  -o BatchMode=yes \
  -o StrictHostKeyChecking=yes \
  -o ConnectTimeout=10 \
  -o ConnectionAttempts=1 \
  -o ServerAliveInterval=15 \
  -o ServerAliveCountMax=3 \
  -o ClearAllForwardings=yes \
  -o ForwardAgent=no \
  -o ForwardX11=no \
  -o PermitLocalCommand=no \
  <validated-host-alias> \
  'exec "$HOME/.local/bin/aizu" bridge --protocol 1 --after <sequence> [--follow]'
```

- `--protocol <major>`: desktop がサポートする protocol major version。
- `--after <sequence>`: この source について最後に確定処理した `sequence`。初回は `0`。
- `--follow`: 既存分を送出後もストリームを維持し、新規イベントを push し続ける。
- `<host-alias>` は local `ssh` の独立 argument とし、先頭 `-`、NUL、改行を拒否する。
- `<sequence>` と `<major>` は数値型から構築する。
- `sequence` / `--after` は `0..=9223372036854775807`、event sequence は `1..=9223372036854775807` に制限する。
- OpenSSH は host 以降の command arguments を space 区切りで再構成して remote shell へ渡すため、local argument 分離だけでは remote shell injection を防げない。
- MVP の remote CLI path は `$HOME/.local/bin/aizu` 固定とし、remote command は上記の固定 template から生成する。UI から任意 path/command を受け取らない。
- `BatchMode=yes` と `StrictHostKeyChecking=yes` により password/passphrase/unknown-host prompt を出さずに失敗する。unknown host の確認・登録は通常の Terminal で事前に行う。
- `ssh -G <host-alias>` の preflight で user config の `RemoteCommand` 競合を検出する。TTY/stdin、forwarding、agent/X11 forwarding、`LocalCommand` は command-line option で無効化する。

## 4. Frames

各 frame は 1 行の JSON object で、必ず `type` を持つ。normalized event payload は最大 64 KiB (65536 bytes)、envelope を含む frame line は末尾 LF を除いて最大 128 KiB (131072 bytes)。

### 4.1 `hello`

正常ストリームの最初の frame。必ず 1 回だけ、他の frame より前に送る。protocol negotiation または spool open に失敗した場合だけ、§4.5 の terminal `error` が最初かつ唯一の frame になり得る。

SSH child 起動から 20 秒以内に `hello` または terminal `error` を受信できない場合、desktop は startup timeout として child を graceful に終了する。

```json
{"type":"hello","protocol_version":1,"source_id":"7a4881c7-c667-47dc-b544-f98a46ab17ca","oldest_sequence":1,"latest_sequence":140}
```

全 event が prune 済みだが過去に sequence 140 まで割り当てた spool:

```json
{"type":"hello","protocol_version":1,"source_id":"7a4881c7-c667-47dc-b544-f98a46ab17ca","oldest_sequence":null,"latest_sequence":140}
```

| field | type | 説明 |
|---|---|---|
| `type` | string | `"hello"` |
| `protocol_version` | integer | source が話す protocol major version |
| `source_id` | string (UUID) | source spool の永続 ID |
| `oldest_sequence` | integer or null | spool に現存する最小 `sequence`。retained event が無ければ `null` |
| `latest_sequence` | integer | 過去に割り当てた最大 `sequence` (high watermark)。新規 spool は `0`、全 event prune 後も reset しない |

desktop は `protocol_version` と `source_id` を event/cursor 処理前に検証する。

- 初回成功時に SSH source 設定へ `source_id` を pin する。
- 以後 mismatch した場合は、old cursor を適用せず source を停止する。ユーザーが明示的に “Replace source” を選んだ場合だけ新規 source として cursor を `0` へ reset する。
- 別設定が同じ `source_id` を返した場合は二重登録として active にしない。
- 要求した `--after` が `latest_sequence` より大きい場合は DB rollback/source rewind として terminal error にする。自動 cursor reset はしない。
- retained event があり、`--after < oldest_sequence - 1` の場合は §4.4 の `gap` を送る。
- retained event が無くても `--after < latest_sequence` なら、全欠落範囲を示す §4.4 の `gap` を送る。

### 4.2 `event`

1 件の normalized event を運ぶ。

```json
{"type":"event","sequence":124,"event":{"schema_version":1,"id":"0198a012-3456-7abc-8def-0123456789ab","kind":"agent.question","occurred_at":"2026-08-12T12:34:56.789Z","source":{"source_id":"7a4881c7-c667-47dc-b544-f98a46ab17ca","display_name":"build-server","agent":"generic"},"title":"Agent is waiting for input","body":"Approval is required to continue.","urgency":"high"}}
```

| field | type | 説明 |
|---|---|---|
| `type` | string | `"event"` |
| `sequence` | integer | source 内で単調増加する序数 |
| `event` | object | [`event-v1.schema.json`](schemas/event-v1.schema.json) に適合する normalized event |

- `sequence` は source ごとに厳密に増加し、同じ stream では直前 event より大きくなければならない。desktop は transaction commit 後に cursor を進める。
- `event.source.source_id` は `hello` の `source_id` と一致しなければならない。
- `event.schema_version` は protocol v1 では `1` でなければならない。
- notification の source label は pinned `source_id` に対応する desktop-local label を使い、remote-provided `event.source.display_name` を authoritative な表示名として信用しない。
- `(source_id, sequence)` または `(source_id, event.id)` の重複は通知せず idempotently 無視する。同じ key で payload が異なる場合は protocol/data-integrity error とする。

### 4.3 `heartbeat`

無通信時のハング検出用。event/heartbeat を 15 秒送っていない場合に送る。

```json
{"type":"heartbeat","sent_at":"2026-08-12T12:35:30Z"}
```

45 秒間 frame が届かない場合、desktop は stream を stale と判断し、SSH child を graceful に終了して §6 の再接続を行う。

### 4.4 `gap`

要求された `--after` より後で、既に prune 済みのため送れない範囲があることを示す。

```json
{"type":"gap","requested_after":20,"oldest_sequence":80,"lost_through_sequence":79}
```

retained event が 1 件も無く、high watermark まで全て欠落している場合:

```json
{"type":"gap","requested_after":20,"oldest_sequence":null,"lost_through_sequence":140}
```

| field | type | 説明 |
|---|---|---|
| `requested_after` | integer | 欠落範囲直前の cursor。初回 gap では desktop の `--after`、mid-stream gap では最後に送出済みの sequence |
| `oldest_sequence` | integer or null | 最初に取得可能な retained sequence。retained event が無ければ `null` |
| `lost_through_sequence` | integer | 取得不能になった最後の sequence |

desktop は `requested_after + 1` から `lost_through_sequence` の欠落を履歴に警告として記録し、同一 transaction で cursor を `lost_through_sequence` へ進める。これにより再接続ごとに同じ gap を再処理しない。`oldest_sequence` が integer の場合はそこから event 処理を継続し、`null` の場合は high watermark まで欠落した状態で follow を継続する。

`gap` は hello 直後だけでなく、bridge 読み取り中に concurrent maintenance が未送信 event を prune した場合も event 間に現れ得る。bridge は event sequence の jump を暗黙に飛ばさず、その範囲を必ず `gap` で明示する。receiver は `gap` の無い sequence jump を protocol violation として停止する。

### 4.5 `error`

source 側で継続不能な状態を通知する。

```json
{"type":"error","code":"incompatible_protocol","message":"unsupported protocol major version"}
```

| `code` | 意味 |
|---|---|
| `incompatible_protocol` | desktop の要求 major version を source が扱えない |
| `spool_unavailable` | spool を開けない・読めない |
| `cursor_ahead` | requested cursor が source high watermark より大きく、DB rollback/source rewind が疑われる |
| `invalid_request` | protocol/cursor argument が不正 |
| `internal` | その他の source 内部エラー |

`message` は user-safe な固定文言とし、最大 512 Unicode scalar values。path、username、SQL、secret を含めない。詳細診断は redacted stderr へ出す。

desktop は `error` を error category として分類し、必要に応じて再接続または更新案内を行う。`incompatible_protocol`、`cursor_ahead`、`invalid_request` は自動 retry しない。

## 5. Delivery semantics

配送は **at-least-once + deduplication**。
この保証は source retention window 内に限る。retention/quota により未取得 event が prune された場合、bridge は再送できず §4.4 の `gap` で欠落を明示する。

desktop は 1 件の `event` を受信するたびに、次を単一 transaction で行う。

1. `(source_id, sequence)` と `(source_id, event.id)` を unique key として event を insert（同一 payload の重複は無害に無視）。
2. その source の cursor を `sequence` へ更新。
3. notification outbox に `pending` として追加。

別 worker が outbox を OS 通知へ送り、成功後 `delivered` にする。permission/policy により送らない event は理由付き `suppressed`、一時失敗は bounded retry、恒久失敗は `failed_terminal` とし、無限 retry しない。クラッシュ位置により OS 通知が再送され得るため、可能な範囲で event `id` 由来の安定した notification identifier を使い、重複表示を抑制する。

receiver は idempotent でなければならない。再接続で同一 `sequence` を再受信しても、二重通知・二重保存をしてはならない。

同じ unique key で異なる payload を受け取った場合は、source corruption/protocol violation として source を停止し、既存 event を上書きしない。

## 6. Reconnection

- 起動直後は即時接続。
- 失敗後は 1, 2, 5, 10, 30, 60 秒の上限付き指数バックオフ。
- jitter を加え、複数 source の同時再接続を避ける。
- sleep/wake と network change では backoff を解除して即時再試行。
- ユーザーの "Reconnect now" でも即時再試行。
- 再接続時は保存済み cursor を `--after` に渡し、cursor 以後だけを取得する。
- source が disable/削除された時、app 終了時、または non-retriable security/protocol error 時は pending retry と child process を cancel する。
- desktop は常に `--follow` を使うため、terminal `error` の無い EOF/child exit は切断として分類し再接続する。将来 one-shot mode を使う場合だけ、`--follow` 無しの backlog 完了 EOF を正常終了とする。
- network/refused/reset/timeout/unexpected EOF は automatic retry。authentication、unknown/changed host key、missing CLI、SSH config conflict、incompatible protocol/database、cursor ahead、source identity mismatch は user-action-required として auto retry を pause する。

## 7. Limits and safety

- event payload は最大 64 KiB、frame line は最大 128 KiB。receiver は LF 到着前から bounded buffer で上限を enforcement し、optional CRLF も 1 delimiter として扱う。
- JSON nesting、UTF-8、required field type、timestamp、control character、integer range を receiver 側で制限し、同一 object 内の duplicate key は曖昧性を避けるため拒否する。
- bridge は backlog を全件 memory に載せず、短い read transaction と bounded page で sequence 順に読む。stdout backpressure 時は block してよいが、未送信 event は spool に残し、再接続で再送できるようにする。
- stdout に protocol 以外の出力を混ぜない。
- 各 frame は LF まで書いた直後に stdout を flush し、stdio buffering で event/heartbeat を遅延させない。
- child process の stdout/stderr は bounded に読む。
- desktop は自分が起動した SSH child のみを追跡し、graceful に終了する。
- event 本文に secret・全文 prompt・全文 response・生の環境変数を入れない（[`event-v1.schema.json`](schemas/event-v1.schema.json) の制約と §privacy に従う）。

## 8. Compatibility test expectations

- 正常時の `hello` → (`gap` | `event` | `heartbeat`)* → (terminal `error`)? と、pre-handshake terminal `error` の順序を検証する。
- `hello` 後の未知 `type` frame は無視し、`hello` 前の未知 `type` は拒否することを検証する。
- event に未知 optional field があっても受理できることを検証する。
- 64 KiB event を wrapper に入れた frame が許容され、128 KiB 超 frame は delimiter 前に拒否されることを検証する。
- retained event がある prune と、全 event prune 後の empty spool の両方で `gap` と high watermark を検証する。
- bridge 読み取りと concurrent prune の競合で、暗黙の sequence jump ではなく mid-stream `gap` が送られることを検証する。
- cursor ahead、source ID mismatch、同一 source の二重登録を検証する。
- 再接続後に cursor 以後のみを再取得し、重複通知しないことを検証する。
- gap warning と cursor advance が atomic で、再接続後に同じ gap warning を重複作成しないことを検証する。
- 同一 key/同一 payload は idempotent、同一 key/異なる payload は error になることを検証する。
- 15 秒 heartbeat と 45 秒 stale timeout を fake clock で検証する。
- 20 秒 startup timeout、integer boundary、duplicate JSON key rejection を検証する。
- remote shell startup stdout pollution が pre-handshake error になることを検証する。
- bounded-page backlog、stdout backpressure、`--follow` 中の unexpected EOF/reconnect を検証する。
- protocol major mismatch で event を消費せず停止することを検証する。

protocol を変更する PR は、本ファイル・[`schemas/event-v1.schema.json`](schemas/event-v1.schema.json)・fixtures・compatibility test を同一 PR で更新する。
