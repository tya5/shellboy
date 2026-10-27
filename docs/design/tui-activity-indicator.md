# TUIの実行状態と動き

採用状態: 2026-10-25、ユーザーの「コアを複雑にせず、リアクティブなUI構成を維持する」指示に基づく実装仕様。対象は既存ステータス行。仕様策定後、設計・実装・検証まで進める。[UI設計](ui.md) と [起動と受付](runtime.md) を具体化する。Run/Turnの意味を変更しない。

## 範囲

初版は常に見える画面下部の既存1行へ、マーカーと短い処理段階を表示する。行数を増やさず、経過時間・tool/取消件数・重要なエラー・既存の操作結果は保持する。会話本文・過去メッセージ・スクロール位置は動かさない。履歴のtool行やThread一覧のアニメーションは今回追加しない。保存時点でrunningだった履歴や一覧snapshotだけから、現在も実行中と推定しないためである。詳細は既存`/status`/F8へ進む。

コアの永続状態・公開CLI/protocol・DB/migration・依存・OSスレッド構成を追加しない。Issue #8では受信の事実だけを運ぶ既存in-process通知へ`ModelReceive`を追加する。共有Tokio runtime、専用DB worker、有限blocking worker、メインOSスレッドのUIという構成を維持する。実装は既存transport通知経路、frontendの読取projectionとUI内部の表示状態・描画を使う。

## 表示する段階

Enter直後から既存footerにUI内の送信段階を表示する。既存draft_id/revisionで対象を固定し、backend受付後はmessage_idへ結ぶ。`送信受付中`はUIが送信要求を作った時点、`保存済み・表示待ち`は保存成功通知後で対応message nodeを履歴projectionにまだ確認できない時点を表す。node IDの一致を確認したら送信段階を終える。すでに準備中以降のRun/Turnが動いている場合、保存後の表示待ちは同じ行の補足へ回し、実際の処理段階を主表示に維持する。Run停止/復旧等の注意状態も保持し、同じ行に送信段階を併記する。新しいCore状態や恒久的な送信表は設けない。

反映確認は送信ごとに一方向へ保持する。receipt/source/履歴の到着順の違い、別Thread、古いrevision、下書きの追記で対象を取り違えない。Completed通知がsource通知より先行してもmessage IDの結び付けまで保持する。Completedとsource IDが揃えば、現在の履歴窓外にある場合も送信表示を終了して既存pendingを解放する。これにより実行完了後に無限の表示待ちを残さない。新しい送信を始めた場合も古いCompletedは解放する。保留/失敗/取消では送信の動きを止め、元の入力と保存済み出典を維持する。時間経過だけで未送信扱いや自動再送にせず、表示時計はUIローカル・overlay中は既存の停止規則を使う。

通常診断には既存の有限writerでfrontend受信、処理開始、保存成功、失敗/キュー満杯を固定分類で記録し、draft ID/revision・operation ID・message IDを結ぶ。queuedはfrontendが要求を受け取った事実であり、Enter自体や画面への描画完了を意味しない。本文・添付path・token・任意エラー本文はログへ入れない。

| 表示 | 既存の根拠 | 動き |
|---|---|---|
| 準備中 | 非終端Turnのpreparing | `●`＋段階文字に控えめなshimmer |
| モデル応答待ち | 現Turnのmodel attemptがinflight、対応する受信通知をまだ観測していない | 同上 |
| 応答受信中 | 同じattempt IDの既存Streamを観測し、そのattemptがinflight | 同上 |
| 一時的なエラーのため再試行中（N/M） | Run live・Turn runningで、最新かつ同generationのfailed attemptに保存したautomatic_retryの回数と上限 | 既存の低速pulse。エラー色にしない |
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

1. 既存Changed/Stream/エラー、Thread切替等を契機に表示状態を更新する。保存事実は既存DB worker上のprogress読取へ必要な列を加えて取得する。新しい監視API・周期DB照会・保存eventは作らない。既存の実行中500ms進捗確認は維持するが、アニメーションのために頻度・対象を増やさない。
2. UIへの読取結果はThread/generationと発行世代を照合する。表示段階にはRun/Turn/attempt IDを保持し、古いattemptのStream・切替前の遅い応答・終端後の通知で動作中へ戻さない。同じprojectionは不要な描画を起こさない。
3. 動きはUI内の単調時刻から計算する表示効果だけ。保存値・operation・下書きrevision・履歴revisionを変更しない。表示対象に動きがある間だけ、次フレームの期限でdirtyにする。上限10fpsかつ既存ui.max_fps以下。Runのみのpulseは約2秒周期、shimmerは約1.2秒周期を目安とする。
4. 通常のidle・完了・復旧待ちではアニメーション用の再描画は0。選択/コピー、終了画面、ステータスを覆うoverlay中は時計更新によるdirtyを止める。再表示時は現在の状態・時刻で再開する。入力・スクロール・受信のイベント処理を待たせない。
5. 色・属性だけを変え、レイアウト寸法・文字幅・履歴anchorは固定する。履歴layout cacheを再生成しない。Ratatuiの差分出力を使い、footerのためにMarkdown解析・DB読取・ファイル処理を起こさない。

## 設計と確認条件

frontend内の一時的なActivity projectionと、UI内の小さな表示moduleに責務を分ける。projectionは事実のIDと状態を持ち、別のジョブ状態機械にはしない。UIのStream観測は現在attempt分だけを保持し、新attempt・Thread切替・終端で破棄する。受信通知が読取より先行した場合もIDが一致するまで細分化しない。

匿名fixtureでモデル待ち/受信/ツール/背景/復旧/終端の優先順位、遅い旧通知・Thread切替、同じ状態の無駄なdirty、idle時の描画停止、overlay・選択中の停止、時刻による色の変化と文字/anchor不変を確認する。DB/ファイル処理を遅らせても入力・アニメーションが止まらない境界、既存layout cache再利用も検査する。実PTY/WezTermでは動作中の時間差と終了後idleの出力停止を確認する。標準チェック後にローカルreleaseへ反映し、利用中ownerや既定DBを変更しない。APIを必要としない表示検証を基本とする。

## HTTP受信の空白期間（Issue #8）

モデル要求の応答header、および非空body chunkの受信は既存の有限HTTP→Driver通知を経由し、HTTP層で採取した単調受信時刻を保持し、DriverがThread/attempt IDを付けた非永続`ModelReceive`でfrontendへ渡す。既存の同一32件channel上で型付き内部通知とmodel eventを区別し、providerのJSON値を受信通知へ誤認しない。header/chunkだけの通知はchannel満杯なら捨ててHTTPを待たせないが、通常のmodel eventにも元の受信時刻を付け、待ち合わせの発生時も受信の事実を補完する。Driverは先頭の受信を即通知し、その後の追加通知は最大1回/秒に間引く。長い空白後の受信は即通知される。表示は最終byteより1秒未満古い受信観測になる場合があるが、HTTPのtimeout判定はすべての非空chunkで正確に更新する。新しいtimer・taskは追加しない。SSE comment、keepalive、未完成frameも受信した事実に含める。構造化deltaや文字の生成とは区別し、headerだけで「応答受信中」へ進めず、会話本文やmodel入力へ混ぜない。公開protocol、DB、復旧状態、永続eventは増やさない。新規worker、queue、pollerも追加しない。

frontendは現在選択中Threadのgenerationを付け、UIはThread/generationと保存済みinflight attemptを照合する。UIは受信時刻を1件だけ保持し、古いattemptからの通知を現在の時計へ適用しない。headerが保存projectionに先行した場合は1件の候補として保持し、一致するprojectionだけに適用する。終端、Thread切替、観測不能で破棄する。未選択中・起動前の無通信時間を遡って推測しない。

同attemptの保存済みinflightを画面で観測した時点から30秒以上、受信事実がなければfooterに`受信待ちN秒（画面の観測開始から）`を補足する。最初のheader/byte受信後の空白は`最後の受信からN秒`とする。既存のTurn観測経過とは別であり、API全体timeoutや生成失敗の断定ではない。新しいbyteの通知で補足を解除し、最新時刻から再計測する。古い時刻の遅着で時計を巻き戻さない。

30秒の表示閾値は既存の局所アニメーション時計で更新する。UIでI/O・DB照会・周期要求を行わず、header/chunkごとの通知で履歴revision・本文retention・Markdown layout cacheを無効化しない。hidden overlay中のdirty停止は既存規則を使う。受信がないことだけで復旧待ちや自動再送へ進めない。既存の取消し・最終応答採用順序を維持する。

## 有限の自動再試行（Issue #7）

Issue #8の受信空白の観測と自動再試行の待機は区別する。Driverが既存model_attempt.error_jsonへ保存したautomatic_retry（number、limit、delay_ms等）を、既存Changedから読取projectionへ反映する。最新attemptがfailed、Turnとattemptのgenerationが一致し、Run live・Turn runningの場合だけ再試行待機を表示する。古いattempt、旧generation、復旧待ち、停止中、終端には適用しない。新しいinflight attemptを保存した後は通常のモデル応答待ちへ戻る。

footerは既存の1行・同じpulseと中立的な活動色を使い、保存済みの回数・上限を `一時的なエラーのため再試行中（1/2）` の形で表示する。表示するカウントは既存snapshotへ加えるUI向けprojectionであり、コア状態・永続表・event・worker・queueは増やさない。待機中はIssue #8のattempt別受信時計を破棄し、新attemptの受信を観測してから新たに計測する。完了・復旧後も古い受信通知で待機表示や受信時計を復活させない。終了時は保存済みの終端/復旧表示を優先し、再試行を経て成功したことだけで警告色を残さない。
