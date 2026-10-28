# 既知の制限

v0.1.1で残った問題と、v0.1.2での修正・残る制限を区別します。実行済みの範囲は [検証状況](../validation/acceptance.md) に記録します。

## HTTP停滞と期限（Issue #8）

公開済みv0.1.2ではembeddingの接続後の期限がなく、上流が接続を保って止まると索引・検索が長く残る場合があります。未リリースsourceは接続別のHTTP期限を実装し、設定・挙動は [設定ガイド](../guides/configuration.md#http-timeouts) を参照します。会話の無受信600秒は、high effortの実proxy要求がHTTP400で拒否されたため実測による確定値ではなく、利用者が変えられる暫定既定値です。

受信経過は画面で現在のattemptを観測した範囲だけです。UI通知を最大1回/秒へ制限するため、最終byteからの時間には1秒未満の観測差があり、上流の生成停止を断定しません。HTTPの期限自体は全chunkで更新します。未リリースsourceではIssue #7の未受信一時失敗だけに有限retryを追加しています。受信済みの失敗やtimeoutは自動再送せず、上流の重複課金は防げません。[復旧ガイド](../guides/recovery-and-storage.md#automatic-retries-for-temporary-model-communication-failures)を参照してください。

## operation receipt予算と期限切れ

v0.1.1では読取り結果も現在instanceのreceiptへ蓄積します。v0.1.2は観測のreceiptを作らず、同IDでも再観測します。変更/waitの終端結果は24時間経過後の保守又は圧迫時に結果だけを回収し、期限切れIDを残して同IDの変更再実行を拒否します。pendingと未反映参照は保護します。期限切れは未実行を意味しません。

通常256MiB/outcome8MiB、停止専用固定余白と、期限切れIDのmetadataは同じ予算へ算入します。極端に多い変更や保護対象で満杯なら、明示owner再起動で旧terminalを回収する必要が残ります。`operation get` / `status` / `storage status` はreceiptを作らず、TUIのCtrl+C・`/stop` もreceipt不要です。未リリース0.2.0 sourceでは `status --operation` を削除しました。受付の照会には `operation get --operation ID` を使用してください。

目安として、1件0.7〜0.9KiBのID metadataを新IDで1秒に1回保存すると仮定すれば、1日約60〜80MiB、数日で通常予算を圧迫します。これはmetadata長・予約・保護量を省いた計算例で、現環境の実測値や稼働日数の保証ではありません。waitの既定30秒待機では増加速度が下がります。既に満たされた条件へのwaitを高速反復せず、現在値の確認にはreceipt不要のgetを使い、実際の使用量はstatusのreceipt_budgetを確認してください。

## Current index identities (#33)

Only current profile keys and saved query-profile markers are accepted. Missing,
malformed or endpoint/model-mismatched markers fail before query embedding; no
legacy-key lookup, marker inference or automatic paid rebuilding is provided.
Rebuild explicitly with the intended configuration when required. Keyword and raw
retrieval remain available independently. Candidate activation still requires
successful authorized work. Pre-1.0 releases do not promise old index compatibility.

## 独立RAGと巨大raw出力

v0.1.1の履歴semantic検索は、範囲なしrawが16MiBを超えると `input_limit` でscope全体が失敗する場合があります。v0.1.2ではincremental decode/hash検証と最大64K文字のrange referenceを使い、同じwitnessの複数spanは1回のdecodeで検証します。重なるspanごとのhash検証の費用は残ります。改変・retention失効は引き続き失敗として返します。

hybridや複数scopeの検索では、成功した側だけが返る場合があるため `scope_errors` と部分失敗も確認してください。keyword検索、または `shellboy shell read` のcursor付き取得でも原文を辿れます。

## 会話欄の抜粋と受信記録

長いshell出力のカードは取得した表示窓の抜粋です。カードに出る末尾はexecution全体の末尾とは限りません。表示範囲・省略量が不明な場合もあり、F6・原文reader、または `shellboy shell read` のcursorで続きと完了状態を確認してください。

受信本文の会話previewは最新2記録・既存本文予算内に限定します。対応する履歴位置が表示窓外の場合や、大きな成功差分で最終本文と比較できない場合は会話previewを推測表示しません。`/received` のページ付き一覧から監査記録・原文へ到達できます。本文を受信していない失敗記録も一覧には残ります。

## provider互換と監査保存量

会社Windowsの社内proxy、任意providerのreasoning再送、実IME/clipboardの組合せは未検証です。過去の特定proxy/modelの成功を任意providerへ一般化しません。metadataだけのストリーム差分でも受信内容の監査nodeが追加されるため、毎応答で差が出るproxyでは論理保存量が増えます。頻度・DB増分は未測定で、64MiBの受信stream上限はDB増分上限を意味しません。原文の保全は維持しています。

## Live selection and exceptional UI saturation (#30)

A queued thread switch can miss a transient prefix while Control still filters
for the previous thread. Gap marking and canonical terminal-history refresh are
retained; switching is not a promise to replay every live delta.

The shared UI outbox preserves result order and finite memory. Issue #32 adds
480 worker slots plus 32 main slots within the existing critical capacity 512,
and a bounded main receipt continuation. Priority Cancel/Quit remain polled under
full UI backpressure; terminal-marker retries also poll explicit ForceQuit.
Current integrated validation is recorded in [acceptance](../validation/acceptance.md).
The guarantee uses the real TUI's separate priority mailbox. A plain embedded
FIFO action adapter cannot bypass normal actions queued ahead of a control.

On explicit force, ForceShutdown is a best-effort UI marker: a stalled/full queue
can suppress that marker while the frontend returns a forced shutdown error and
nonzero exit. Normal result ordering and lossless delivery are not relaxed by
this exception. Forced termination can abandon undelivered presentation and
unfinished cleanup; it is never reported as a successful graceful shutdown.

## Earlier interruption payloads (#33)

Pre-0.2 interruption nodes using `legacy_pending_operations_with_unknown_targets` are not converted or read through a compatibility shim. Their unknown-target count and related interruption details may no longer be grouped in the summary. Saved nodes remain available as original text through F6.
