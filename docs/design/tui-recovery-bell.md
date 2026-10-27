# 復旧待ちの端末ベル（Issue #12）

利用者の操作が必要になったことを、端末標準のBELで知らせる。既定有効の `ui.bell` をfalseにすると無効。音、点滅、タブの印、マルチプレクサでの転送は端末側の設定に従う。OS通知・OSC通知・正常完了通知は追加しない。

## 通知の根拠と順序

既存のTurn更新CASが `preparing/running/waiting → recovery_wait` を実際に適用した事実だけを使う。DB transactionのcommit後に既存の非永続 `EngineEvent::Changed` の `recovery` フラグをtrueにする。通常のChangedはfalse。既存の準備失敗と通常の復旧の両経路を使い、モデル要求だけに限定しない。保存状態、表、wire、event種別、queue、workerは増やさない。

初期読取・起動・Thread切替で既存の復旧待ちを通知し直さない。同じ状態への重複処理、古いgenerationの遅着、停止済みTurn、自動再試行の待機はtrueを発行しない。明示再開後に同じTurnが再び復旧待ちへ入れば、新しい遷移として通知できる。

frontendは既存の全体broadcast受信でこのフラグを見て、選択Thread/generationの照合より先にUIへ通知する。別会話・子会話にも適用し、会話本文・管理結果・モデル出力の文字列から通知を判定しない。保存済みの表示projectionに検出責務を置かないので、速い再開によって復旧待ちのsnapshotが上書きされても、受信済み遷移通知は失われない。

## 有限な端末出力

UIが所有する通知状態はpending boolと最終送信の単調時刻だけ。Thread別履歴や無限集合を持たない。受信済みの複数通知を1回にまとめ、最終BEL送信から2秒未満の通知はまとめて捨てる。時間経過後に遅延通知を鳴らすtimerは作らない。2秒以降の新しい遷移は再通知できる。

既存terminal loopの描画と描画の間に、同じ `HyperlinkBackend<Stdout>` へ `\x07` を1byte書きflushする。別stdout handle、画面cell、shell、スレッドは使わない。Ratatuiのbuffer・cursor・会話anchor・下書き・選択・overlayを変更せず、ベルのための再描画を要求しない。端末の書込失敗は既存の端末I/O失敗として終了する。

## 保証の境界

通知は既存の有限broadcast（256件）のbest effortであり、永続receiptではない。極端な過負荷によるLagged、プロセス終了、端末がベルを無視する場合の到達保証・再送は行わない。起動済み状態を鳴らさず、コアを増やさないことを優先する。復旧の正本と理由は引き続きDB・会話画面・CLIで確認できる。ベルの欠落が復旧判定や取消し・再試行の安全性へ影響しない。

byte backendによるBEL数・2秒境界・無効化と、UIの下書き/表示状態不変、実際の復旧CAS・generation・commit後通知を合成fixtureで確認する。端末側の音・点滅と、Windows Terminal / WezTerm / tmux等での体感確認は実機検証と区別して記録する。
