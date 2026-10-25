# TUIの実行状態と動き

採用状態: 2026-10-25、ユーザーの「コアを複雑にせず、リアクティブなUI構成を維持する」指示に基づく実装仕様。対象は既存ステータス行。仕様策定後、設計・実装・検証まで進める。[UI設計](ui.md) と [起動と受付](runtime.md) を具体化する。Run/Turnの意味を変更しない。

## 範囲

初版は常に見える画面下部の既存1行へ、マーカーと短い処理段階を表示する。行数を増やさず、経過時間・tool/取消件数・重要なエラー・既存の操作結果は保持する。会話本文・過去メッセージ・スクロール位置は動かさない。履歴のtool行やThread一覧のアニメーションは今回追加しない。保存時点でrunningだった履歴や一覧snapshotだけから、現在も実行中と推定しないためである。詳細は既存`/status`/F8へ進む。

コアの状態・EngineEvent・公開CLI/protocol・DB/migration・依存・OSスレッド構成を追加しない。共有Tokio runtime、専用DB worker、有限blocking worker、メインOSスレッドのUIという構成を維持する。実装はfrontendの読取projectionとUI内部の表示状態・描画に限定する。

## 表示する段階

| 表示 | 既存の根拠 | 動き |
|---|---|---|
| 準備中 | 非終端Turnのpreparing | `●`＋段階文字に控えめなshimmer |
| モデル応答待ち | 現Turnのmodel attemptがinflight、対応する受信通知をまだ観測していない | 同上 |
| 応答受信中 | 同じattempt IDの既存Streamを観測し、そのattemptがinflight | 同上 |
| ツール処理中 | 現Turnの保存済みtool groupがrunning、モデル要求のinflightではない | 同上 |
| 処理中 | 非終端Turnで、上の内訳を確定できない | 同上 |
| 待機中 | Turnのwaiting | `●`の低速pulse。待機対象は推測しない |
| 回答済み背景処理中 | Turn completed・Run live・所有shell又は子Runが非終端 | `◌`の低速pulse |
| 実行継続中 | Run liveだがTurnは終端、上の背景資源を確認できない | `◌`の低速pulse |
| 停止処理中 | Run stopping | `◌`の低速pulse |
| 復旧待ち／異常終了／中断済み／完了 | 保存済みの対応状態 | 静止。注意状態は色と文言でも区別 |
| 入力できます | 実行中のRun/Turnがない | 静止 |

Run停止・異常・Turn復旧待ちを受信表示より優先する。モデル要求とbackground shellが並行する場合、Turnの主段階はモデル側、背景作業の詳細は`/status`で確認する。「思考中」「添付を解析中」「子の完了待ち」「圧縮中」等の細分化は今回行わない。Streamのreasoning flagから内部の思考内容や進捗を推定せず、受信した事実だけを表す。受信していないことは、providerがまだ生成していない証明ではない。

## リアクティブな更新

1. 既存Changed/Stream/エラー、Thread切替等を契機に表示状態を更新する。保存事実は既存DB worker上のprogress読取へ必要な列を加えて取得する。新しい監視API・周期DB照会・イベントは作らない。既存の実行中500ms進捗確認は維持するが、アニメーションのために頻度・対象を増やさない。
2. UIへの読取結果はThread/generationと発行世代を照合する。表示段階にはRun/Turn/attempt IDを保持し、古いattemptのStream・切替前の遅い応答・終端後の通知で動作中へ戻さない。同じprojectionは不要な描画を起こさない。
3. 動きはUI内の単調時刻から計算する表示効果だけ。保存値・operation・下書きrevision・履歴revisionを変更しない。表示対象に動きがある間だけ、次フレームの期限でdirtyにする。上限10fpsかつ既存ui.max_fps以下。Runのみのpulseは約2秒周期、shimmerは約1.2秒周期を目安とする。
4. 通常のidle・完了・復旧待ちではアニメーション用の再描画は0。選択/コピー、終了画面、ステータスを覆うoverlay中は時計更新によるdirtyを止める。再表示時は現在の状態・時刻で再開する。入力・スクロール・受信のイベント処理を待たせない。
5. 色・属性だけを変え、レイアウト寸法・文字幅・履歴anchorは固定する。履歴layout cacheを再生成しない。Ratatuiの差分出力を使い、footerのためにMarkdown解析・DB読取・ファイル処理を起こさない。

## 設計と確認条件

frontend内の一時的なActivity projectionと、UI内の小さな表示moduleに責務を分ける。projectionは事実のIDと状態を持ち、別のジョブ状態機械にはしない。UIのStream観測は現在attempt分だけを保持し、新attempt・Thread切替・終端で破棄する。受信通知が読取より先行した場合もIDが一致するまで細分化しない。

匿名fixtureでモデル待ち/受信/ツール/背景/復旧/終端の優先順位、遅い旧通知・Thread切替、同じ状態の無駄なdirty、idle時の描画停止、overlay・選択中の停止、時刻による色の変化と文字/anchor不変を確認する。DB/ファイル処理を遅らせても入力・アニメーションが止まらない境界、既存layout cache再利用も検査する。実PTY/WezTermでは動作中の時間差と終了後idleの出力停止を確認する。標準チェック後にローカルreleaseへ反映し、利用中ownerや既定DBを変更しない。APIを必要としない表示検証を基本とする。
