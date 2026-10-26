# 既知の制限

v0.1.1で残った問題と、v0.1.2向けsourceでの修正・残る制限を区別します。実行済みの範囲は [検証状況](../validation/acceptance.md) に記録します。

## 停止とoperation receipt予算

v0.1.1では通常receiptが満杯になるとCLIの `run stop` / `shell stop` も `storage_limit` になり得ます。v0.1.2向けsourceは停止専用の固定余白を追加しています。通常256MiB/outcome8MiBの上限、同ID再送・異要求conflictの契約は維持します。

停止自身の結果で余白も埋まるため、停止を無制限には受け付けません。TUIの現在の会話でCtrl+Cまたは `/stop` を使う経路はreceiptを作りません。`shellboy status` で保存状態と予算を確認し、作業を通常終了してownerを明示的に再起動すると旧receiptが回収されます。停止受付とOS processの終了を混同しないでください。

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
