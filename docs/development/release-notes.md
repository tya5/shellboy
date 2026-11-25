# Unreleased notes

## Conversation retries (#315)

Conversations now retry strictly recognized untyped Responses SSE errors with
code 429 or 500-599 received before `[DONE]`, even after partial output, using the
existing retry budget and backoff. Partial output is kept for inspection but is
not replayed, unfinished tool calls are not executed, and upstream charges can
be duplicated. The conversation-only SSE exception does not apply to summaries.

**Configuration change:** Remove `response_retry_partial` from every connection
before upgrading. Both `true` and `false` are now rejected as unknown keys, with
no alias. Otherwise eligible conversation and summary failures no longer stop
solely because output was received. `response_max_retries = 0` still disables
automatic retries for the connection.

## Terminal saved notices and preview (#311)

The TUI now takes tool notices from newly observed saved results, rather than a
private feed. A bounded history read may not include every call in a group, so
the TUI no longer reports a group-wide cancellation count from that feed.
Asynchronous errors that were never saved no longer appear as transient TUI
notices (#431). Typed operation refusals and saved recovery remain visible.
The provisional answer clears when saved history advances or the Turn settles;
saved answers and received responses remain available in history. A live answer
can appear before the first saved read and is reconciled when saved state arrives.

TUI のツール通知は、非公開の通知経路ではなく、新たに確認した保存済み結果に
基づきます。上限付きの履歴取得ではグループ内の全呼出しを取得できるとは限らない
ため、旧通知経路によるグループ全体の取消件数は表示しません。保存されなかった
非同期エラーは一時通知に表示しません（#431）。型付きの操作拒否と保存済みの
復旧情報は引き続き表示します。保存済み履歴が進むか Turn が終了すると暫定回答を
消去します。保存済みの回答と受信内容は履歴で引き続き確認できます。最初の履歴取得
より前に届いた暫定回答も表示し、保存済み履歴を取得した時点で照合します。

## Terminal client cutover checklist (#311)

- Live previews show answer text only; saved reasoning stays available. Receive
  age is measured by the owner, including provider keepalives. A preview gap
  clears provisional text and timing; a later partial answer does not imply
  a complete prefix. A superseded history window does not change the preview.
- Recovery bells use newly saved transitions, including on other Threads.
  Opening the client or recovering from an event gap does not replay old bells.
- The DB/GC activity footer and private Help diagnostics are removed. The TUI
  cache contribution to `status --memory` is reported as unknown. Slow-job
  diagnostics remain on the owner.
- Submission reads the saved draft for the exact source revision. If the submit
  acknowledgement is lost, the TUI uses the exact operation ID to reconcile the
  saved receipt, including submissions outside the visible draft page. It does
  not resend the draft. Revisit submission keeps its source binding.

- ライブプレビューには回答本文のみを表示します。保存済みの推論は引き続き
  確認できます。受信からの経過時間にはプロバイダーのキープアライブも反映します。
  イベントに欠落がある場合は暫定本文と受信表示を消去し、後続の断片を完全な回答の
  先頭として扱いません。無効になった履歴ウィンドウはプレビューを変更しません。
- 復旧ベルは、他のスレッドも含め、新たに保存された復旧への遷移に基づきます。
  起動時やイベントの欠落からの復帰時に、過去のベルを鳴らし直しません。
- DB/GC のフッター表示とヘルプの内部診断を削除しました。`status --memory`
  では TUI キャッシュの使用量は不明になります。遅い処理の診断はオーナー側に残ります。
- 送信時は対象リビジョンの保存済み下書きを取得します。応答を受け取れなかった場合は、
  操作 ID で保存済みの結果を照合し、一覧の表示範囲外の送信も確認します。下書きは
  再送しません。Revisit 送信は元の入力との対応を維持します。
