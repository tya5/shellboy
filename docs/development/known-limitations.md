# 既知の制限

v0.1.1で残った問題と、v0.1.2向けsourceでの修正・残る制限を区別します。実行済みの範囲は [検証状況](../validation/acceptance.md) に記録します。

## operation receipt予算と期限切れ

v0.1.1では読取り結果も現在instanceのreceiptへ蓄積します。v0.1.2向けsourceは観測のreceiptを作らず、同IDでも再観測します。変更/waitの終端結果は24時間経過後の保守又は圧迫時に結果だけを回収し、期限切れIDを残して同IDの変更再実行を拒否します。pendingと未反映参照は保護します。期限切れは未実行を意味しません。

通常256MiB/outcome8MiB、停止専用固定余白と、期限切れIDのmetadataは同じ予算へ算入します。極端に多い変更や保護対象で満杯なら、明示owner再起動で旧terminalを回収する必要が残ります。`operation get` / `status` / `storage status` はreceiptを作らず、TUIのCtrl+C・`/stop` もreceipt不要です。`status --operation` はv0.1.2だけ非推奨aliasとして残し、次リリースで削除します。

## 索引方式の更新とsemantic検索の対象

v0.1.1ではsplitter identity更新後に旧空間が既定検索から外れることがあります。v0.1.2向けsourceは旧activeを維持し、新規sourceを現行candidateへ索引化します。

明示selectするまでは、新しい履歴は既定のsemantic検索には出ません。select後は旧spaceの履歴が既定検索から外れるため、新旧を同じspaceで検索するには対象を明示したrebuild/backfillが必要で、embedding費用が生じ得ます。keyword検索と原文参照はこの切替の影響を受けません。自動の有料再索引はしません。

candidateの許可済み入力・jobが全て成功するまでselectを拒否します。保存入力がある失敗は `index retry`、原文/入力欠落や中断した準備は、対象を確認して明示rebuildしてください。接続先/modelが違う索引は読めません。旧spaceの元の分割設定が不明な場合は推測で再束縛せず、元の設定の復元が必要です。

## 独立RAGと巨大raw出力

v0.1.1の履歴semantic検索は、範囲なしrawが16MiBを超えると `input_limit` でscope全体が失敗する場合があります。v0.1.2向けsourceではincremental decode/hash検証と最大64K文字のrange referenceを使い、同じwitnessの複数spanは1回のdecodeで検証します。重なるspanごとのhash検証の費用は残ります。改変・retention失効は引き続き失敗として返します。

hybridや複数scopeの検索では、成功した側だけが返る場合があるため `scope_errors` と部分失敗も確認してください。keyword検索、または `shellboy shell read` のcursor付き取得でも原文を辿れます。

## 会話欄の抜粋と受信記録

長いshell出力のカードは取得した表示窓の抜粋です。カードに出る末尾はexecution全体の末尾とは限りません。表示範囲・省略量が不明な場合もあり、F6・原文reader、または `shellboy shell read` のcursorで続きと完了状態を確認してください。

受信本文の会話previewは最新2記録・既存本文予算内に限定します。対応する履歴位置が表示窓外の場合や、大きな成功差分で最終本文と比較できない場合は会話previewを推測表示しません。`/received` のページ付き一覧から監査記録・原文へ到達できます。本文を受信していない失敗記録も一覧には残ります。

## provider互換と監査保存量

会社Windowsの社内proxy、任意providerのreasoning再送、実IME/clipboardの組合せは未検証です。過去の特定proxy/modelの成功を任意providerへ一般化しません。metadataだけのストリーム差分でも受信内容の監査nodeが追加されるため、毎応答で差が出るproxyでは論理保存量が増えます。頻度・DB増分は未測定で、64MiBの受信stream上限はDB増分上限を意味しません。原文の保全は維持しています。
