as published at tag b644b3c (Core sections only), not main

# Shellboy Core v0.14.0

- Core: `v0.13.1` → `v0.14.0`（`898259c5` に版番号の更新だけを足した commit）

## 日本語

### Core

- **質問と返信:** 返事を求めるには `message ask --thread ID` または `message ask --to NAME` を使います。宛先を省略すると `human` です。`message list --awaiting` はページ送りに対応し、`--owed-by`、`--asked-by`、`--to` のいずれか一つで絞れます。`thread list --awaiting NAME` も任意の名前を受け付けます。送り手は配達後も `message cancel` で未回答の依頼を取り下げられます。（#317）
- **待機と ID:** 明示的な `shellboy wait` は、無関係な変更のたびに全件を再確認せず、条件に関係する変更で再確認します。対応する CLI の Thread、メッセージ、Run、shell 実行 ID には、8〜35 文字の一意な前方一致を使えます。（#351、#325）
- **予定:** `schedule add / set / enable / disable / remove / list` で、Thread に属する shell 実行を `--cron`、`--at`、`--in` で予約できます。予定は owner の再起動後も残り、Thread と一緒に削除されます。一回限りの予定は、実行した shell の終了処理で予定表から取り除かれます。同梱ガイドの検索でも予定の節を見つけられます。（#281、#343、#342）
- **会話の復旧:** 部分出力があっても、再試行できる失敗は既存の再試行枠内で再試行します。Responses のストリームでは、`[DONE]` 前に受けた所定の 429／5xx エラーも対象です。利用上限で `recovery_wait` になった Run は、再開時刻が記録されているなど条件を満たす場合、その時刻以降に一回だけ自動で再開を試みます。`thread rewind --run RUN_ID` は、対象の `recovery_wait` Run の停止と巻き戻しを一操作で行います。（#315、#321、#288）
- **自動要約:** 通常の会話と共通のモデル要求経路を使い、再試行条件を満たす失敗は要約用の枠で再試行します。要約の段階と失敗は、通常の会話試行とは別に要約側の記録で確認できます。`thread get` は自動要約が実行中かを `summarizing` で返します。（#328、#331、#341）
- **読み取り:** 大きな Thread でも `thread get` のコンテキスト読み取りは上限付きになりました。Thread の正確な使用量合計は `usage get` で読みます。`attachment list` は `--limit` と `--cursor` に対応し、`thread list --order created` で作成順のページを読めます。Thread 一覧には `created_at` が加わりました。（#333、#346、#327、#338）
- **クライアント連携:** ライブ応答の試行が終わったときに、その結果と保存位置に対応する情報をクライアントに伝えます。保存済み送信の履歴項目に操作 ID、`events.wait` に設定変更のヒント、IPC の `Welcome` に実行中の owner が自己申告するソフトウェア版を追加しました。`Welcome` 前に拒否された場合、その版は分かりません。（#337、#339、#340、#294）
- **API スキーマ:** `attachment.remove`、`credentials.set`、`credentials.clear`、`shell.stop` の成功応答に型を宣言しました。既存の応答の形は変えていません。（#332）
- **修正:** Windows などで IPC の最終応答が閉じる前に届かない場合を修正し、Unix の shell は確認済みの終了結果を回収確認の失敗で上書きしなくなりました。メモリ状態の読み取りには、通常の観察要求と別の IPC 受付枠を設けました。同梱ガイドも最新の Run、返信、設定コマンドに合わせました。（#326、#330、#349、#347、#353）

### アップグレード

- **確認の範囲:** macOS は実機で確認しました。Windows と Linux は CI と候補の検査だけで、実機ではまだ確認していません。
- **先にデータディレクトリをバックアップしてください。** Core の `MIGRATION_NAMES` は 49 件から 56 件へ増えました（0050〜0056）。v0.13.1 は schema 56 の DB を開けません。
- **IPC client は protocol 11 に対応した版を使ってください。** Core の protocol は 5 → 11 です。
- `connections.<name>.response_retry_partial` は削除されました。値にかかわらず、すべての接続から削除してください。`response_max_retries = 0` で自動再試行を無効にできます。利用上限からの自動再開だけを無効にするには、`[recovery]` の `auto_retry_usage_limit = false` を使います（既定 `true`）。
- `message send --expect-reply` は廃止されました。返事を求める場合は `message ask` に変更してください。`thread list --awaiting` の名前指定は拡張であり、削除ではありません。
- API 利用者向け: `thread get` の `awaiting_replies` 配列は `pending_replies.owed_by` / `.asked_by` の件数に置き換わりました。個別の依頼は `message list --awaiting` で取得します。Thread 一覧の `awaiting_human_count`、`awaiting_human_count_exact`、`awaiting_human_oldest_at` は対応する `awaiting_named_*` に変わりました。Thread 詳細の `usage_totals` は `usage get` で取得してください。

## English

### Core

- **Questions and replies:** Use `message ask --thread ID` or `message ask --to NAME` to request a reply. The default recipient is `human`. `message list --awaiting` is paged and takes at most one of `--owed-by`, `--asked-by`, or `--to`. `thread list --awaiting NAME` also accepts any name. Senders can withdraw an unanswered request with `message cancel` even after delivery. (#317)
- **Waits and IDs:** Explicit `shellboy wait` conditions are rechecked on relevant changes instead of rechecking every wait on unrelated changes. Supported CLI arguments for Thread, message, Run, and shell-execution IDs accept a unique prefix of 8–35 characters. (#351, #325)
- **Schedules:** `schedule add / set / enable / disable / remove / list` manages Thread-owned shell commands scheduled with `--cron`, `--at`, or `--in`. Schedules survive owner restarts and are removed with their Thread. A one-shot schedule is removed from the schedule list when its shell execution settles. The bundled guides also make schedules searchable. (#281, #343, #342)
- **Conversation recovery:** Eligible failures still retry within the existing budget after partial output, including recognized Responses stream errors (429 or 5xx) before `[DONE]`. A Run in `recovery_wait` due to a usage limit can automatically try to resume once, after its recorded reset time, when eligible. `thread rewind --run RUN_ID` stops and rewinds the specified `recovery_wait` Run in one operation. (#315, #321, #288)
- **Automatic summaries:** Summaries use the same model request path as conversations; eligible failures retry within the summary budget. Summary stages and failures are recorded separately from normal conversation attempts. `thread get` reports active automatic summarization through `summarizing`. (#328, #331, #341)
- **Reads:** `thread get` bounds the context reads it performs even for large Threads. Use `usage get` for exact Thread usage totals. `attachment list` supports `--limit` and `--cursor`; `thread list --order created` pages in creation order. Thread lists include `created_at`. (#333, #346, #327, #338)
- **Client integration:** Clients receive the outcome of a live response attempt when it ends, with information for aligning it to saved history. Saved send items include operation IDs, `events.wait` carries configuration-change hints, and IPC `Welcome` reports the running owner's self-reported software version. The version is unavailable when the connection is refused before `Welcome`. (#337, #339, #340, #294)
- **API schema:** Success results for `attachment.remove`, `credentials.set`, `credentials.clear`, and `shell.stop` are now typed in the published schema. Their existing response shapes are unchanged. (#332)
- **Fixes:** Fixed cases where the final IPC response was lost before close, including on Windows. A known Unix shell exit is no longer overwritten by a reap-confirmation failure. The memory-status read has its own IPC admission slot, separate from ordinary observations. Bundled guides now match current Run, reply, and configuration commands. (#326, #330, #349, #347, #353)

### Upgrade notes

- **Verification:** macOS was checked on a real device. Windows and Linux passed CI and the candidate checks only; they have not been checked on a real device yet.
- **Back up the data directory first.** Core's `MIGRATION_NAMES` grows from 49 to 56 entries (0050–0056). v0.13.1 cannot open a schema-56 database.
- **Use IPC clients built for protocol 11.** Core protocol changes from 5 to 11.
- Remove `connections.<name>.response_retry_partial` from every connection, regardless of its value. `response_max_retries = 0` still disables automatic retries. To turn off only automatic recovery from usage limits, set `auto_retry_usage_limit = false` under `[recovery]` (default `true`).
- `message send --expect-reply` was removed. Switch reply-requesting sends to `message ask`. Named recipients for `thread list --awaiting` are an extension, not a removal.
- API clients: `thread get` replaces its `awaiting_replies` array with `pending_replies.owed_by` and `.asked_by` counts. Read individual duties with `message list --awaiting`. Thread list fields `awaiting_human_count`, `awaiting_human_count_exact`, and `awaiting_human_oldest_at` become the corresponding `awaiting_named_*` fields. Read Thread detail's former `usage_totals` with `usage get`.
