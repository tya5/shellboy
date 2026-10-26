# 会話履歴のスクロール

採用状態: 2026-10-24のホイールによる位置飛び報告に対応する実装設計。原文のA18（閲覧anchor・追従停止・新着）とMarkdown追加要求（幅変更時の原文位置維持）を具体化する。保存原文snapshotは変更しない。

## 調査した既存の方式

- [Ratatui Paragraphの公式例](https://ratatui.rs/recipes/widgets/paragraph/): 縦offsetは表示行で指定し、折り返しと文字数を混同しない。現在採用しているParagraphを引き続き使う。
- [Codex viewport設計（固定commit）](https://raw.githubusercontent.com/openai/codex/b093565bfb5b2c016cb157127edb1ad62bfc7a27/codex-rs/tui2/docs/tui_viewport_and_history.md): 履歴を論理cellとして扱い、表示行への投影と末尾追従／固定位置を分離する。入力・状態・履歴の表示領域を分ける設計も参考にする。
- [Codex wrapping実装](https://raw.githubusercontent.com/openai/codex/main/codex-rs/tui/src/wrapping.rs): 折り返し表示と原文offsetの対応、grapheme境界を別管理している。参照時点の実装であり、将来のmainの内容を固定しているわけではない。
- [tui-scrollview実装](https://docs.rs/tui-scrollview/latest/src/tui_scrollview/scroll_view.rs.html): viewport寸法を使い最終full pageで上限をclampする。コンテンツ全体分のBufferを作る方式は、この製品の巨大履歴／有限cacheへそのまま適用しない。

以下はこれらの一次資料と既存の有限取得契約から選択したShellboyの方式であり、参照実装を丸ごと移植するものではない。Markdown解析・描画と折り返しは既存OSSへ委ねる。

## 原因と修正方針

旧実装はwheelごとに原文500文字、Pageごとに2,000文字を加減算していた。表示行の高さ／折り返し／折りたたみと無関係で、短文群では多くのメッセージを飛び越えた。さらにfollow中の移動を古いanchorから開始し得た。派生blockの途中offsetでは表示が変わらず、境界を越えると全体が消える場合もあった。

操作単位を表示行へ統一する。wheelは3表示行、Pageはviewport高−1行（最低1行）。3行は一般的な端末操作に合わせた初期値であり、任意の文字数への換算を行わない。端まで行ったときは最後のfull pageを上限とし、内容が少ない場合も空白側へ過剰に進まない。

## 状態と描画

- 末尾追従では最新の履歴の最後のfull pageを表示する。過去のanchorを移動の基点にしない。
- 閲覧固定では、原文位置を表すnode＋文字offsetと、有限描画windowの原点、その内部の一時的な表示行位置を区別する。wheelでの行移動を原文の文字数へ置き換えない。
- 次の表示行と元node／本文位置との対応を保持する。整形による装飾や原文と直接一致しない派生blockは、対応元blockと一時的な行内位置を用いる。画面行番号を永続的な履歴位置にしない。
- 幅、折りたたみ、表示本文が変われば、原文anchorから行位置を再対応する。行内移動だけでMarkdown全体を再解析しない。リンク領域も同じ表示offsetで計算する。
- 新着は末尾追従中だけ自動追従する。過去を読んでいる間は位置を保ち、新着を識別する。明示的な末尾移動と実末尾到達で追従へ戻る。

## 有限取得とイベント

表示対象は既存の有限cache／取得windowに限定する。投影する隣接範囲は原文合計192KiB／128nodeで制限し、既存の単一取得windowは分割して原文対応を壊さず扱う。表示行位置はusizeで保持し、実描画する行sliceだけをParagraphへ渡すためu16 scroll上限に依存しない。全履歴を全画面Bufferや行配列へ展開しない。行mapもcache予算へ計上する。window外のnode／本文は既存workerへ非同期取得を依頼し、UIでDBやworker完了を待たない。未取得本文のplaceholderを実本文の高さとは扱わず、通過する最初の未取得nodeで読み込む。既知の距離を消費して境界checkpointを進め、残りの行差分を適用することで、小cacheでも過去の本文を繰り返し取得しない。

取得をまたぐ行移動には未消費分を保持し、scroll要求による取得中の連続wheelを反映する。初回Thread読込で基点がまだない場合は入力を後の要求へ持ち越さない。Thread／generation／view revision／selection revisionによる古い応答の拒否を保つ。末尾復帰・Thread切替で旧取得を無効化するときは、その要求と未消費分も取り消す。Thread別の下書き・位置保存を維持する。過去方向も約8192 Unicode文字分を先読みし、512文字ずつの直列取得を避ける。最大32KiB相当の移動なので、既存64KiB本文windowへ元位置との重なりを保つ。読み込み状態は既存footerへ表示し、履歴の高さを変えない。

ホイールは最前面のoverlayを優先し、背面の会話を動かさない。通常画面では履歴の領域を対象とする。選択モードでの端末側選択を妨げない。

## 検証

番号付きの単一長文、短い多数node、日本語／emoji／結合文字と折り返し、コード・通知・折りたたみ、末尾復帰後の上スクロール、新着中の固定、Thread切替、幅変更、小cacheの非同期境界、連続wheel、overlayを確認する。公開回帰は [scrolling](../../tests/scrolling.rs)、[refresh時の位置保持](../../tests/ui_scroll_refresh.rs)、[stream確定時の位置](../../tests/ui_scroll_stream.rs) を参照する。内部stateだけでなく、実端末で確認するときはwheelイベント前後の表示番号も比較する。操作環境・結果・未確認は [検証状況](../validation/acceptance.md) に記録し、セル取得と画像取得・pixel描画・実IMEの確認を区別する。画像取得に失敗した場合、キャプチャによる視覚確認を実施済みとしない。


## 集約した状態通知の末尾と再帰の排除（2026-10-25）

通常会話のmain stack overflowを受け、user＋連続する静かな状態通知2件の合成履歴を独立プロセスで再現した。状態通知群の表示行は先頭nodeをanchorにするため、従来の `last_source.node + 1 == nodes.len()` は真の末尾でもfalseになった。次のcached nodeへ投影を切り替えて同じscrollを再帰呼出しし、同じ集約行へ戻り続けていた。

RowSourceへ表現したnode範囲の排他的終端 `covered_end` を追加し、永続anchorの先頭nodeと分ける。末尾判定と前方向の次投影には範囲終端を使い、本文位置anchorは変更しない。投影切替はnode数に応じた上限を持つ反復にし、方向に沿ってfocusが進まない場合は打ち切る。行数を文字数へ置き換えたり、スタック容量を増やして症状を遅らせたりしない。

非同期window応答後の保留wheel再適用も同じ関数を通るため、操作時だけでなく履歴更新時の条件も回帰に含める。実利用でどの入力が発火したかは未確定だが、保存された会話末尾のnode種別と再現条件は一致した。私的な本文やDBをfixtureへコピーしない。

## 選択経路変更時の自動復旧（2026-10-25）

非追従のrefreshは画面の既存windowを保持するため、受信したtailのrevisionをそのまま
古いordinalへ付け直してはいけない。ページ取得時にCursorExpiredとなった場合は通常の
LoadFailedで止めず、view revision付きのHistoryInvalidatedとしてUIへ返す。
UIは残りの古いスクロール量を捨て、現在のnode ID/文字anchorを持つRebaseHistoryを一度要求する。
DB workerの一つのsnapshotでanchorの現ordinalを引き直し、現在pathのwindowとrevisionを返す。
anchorが選択から外れていれば最新ページ先頭へ復旧し、その旨を表示する。保存履歴の書換え・API・新規送信は不要。
既存の画面cacheは取得成功まで残し、成功後にwindowを置換する。view revisionを進めて
遅い旧window/失敗を拒否する。新規入力による後続スクロールは復旧後のwindowで処理する。

## ゆっくりした下スクロールの途中表示（2026-10-25）

metadataページ境界では、取得中に表示anchorをページ最終行へ移さない。現在の表示位置と行内位置を保持し、未適用のwheel量だけを新しいページ取得後に適用する。従来の最終行checkpointと負の残量の組合せは、下へ3行の入力でも一度大きく下へ移り、応答後に上へ戻る途中表示を発生させた。

通常のnavigation応答は、同じselection revisionかつ重複ordinalのnode IDが全て一致する場合に限り、現在windowと取得windowの連続範囲を結合する。保持済み本文windowを優先し、未キャッシュ本文だけ取得結果で補う。これにより10nodeのlookbehindより高いviewportでも表示元を失わない。既存cache予算・192KiB/128nodeの投影制限・端からの回収は維持する。選択経路のrebaseや不整合な範囲は従来どおり置換し、異なるpathを結合しない。

NodeWindowは既に順序が分かっているnodeの本文更新だけを行う。cacheから外れたnodeを応答到着順で末尾へ追加しない。順序付きmetadataの追加はHistory/HistoryWindowだけが行う。最終位置だけでなく、入力直後・各非同期応答後の描画を完全cacheの参照画面と比較する。

## 読取中の表示用抜粋の一時失敗（2026-10-25）

追加報告に対して、1回の下wheelでP065→P068へ進んだ後、隣接tool結果のraw preview取得が失敗したrefreshでP065へ戻ることを匿名fixtureで再現した。保存nodeの本文・順序・anchorは同じでも、stdout/stderrの抜粋が短い統合fallbackへ変わり、真の末尾のfull-page補正が上向きに働く。最終位置だけでなく各非同期応答後のviewportを検査する。

非追従中は、同じ不変本文windowの再読込だけで保持済みの表示用抜粋を一時的な取得失敗（IoFailed）のfallbackへ置換しない。frontendの表示用fieldで失敗codeを伝え、自由文の部分一致から判定しない。確認済みの表示用コピーを継続表示し、今回のraw再取得失敗は詳細fieldへ反映する。原文・stream種別を捏造したり、rawが現在取得可能だと表示したりしない。本文windowの移動、真に異なる本文・tombstone、確認済みOutputExpired/TargetUnavailableへの更新はこの保持条件の対象外。成功した再描画は採用し、保持したrawコピーも現在の幅で再折返しする。取得成功・失敗で位置が揺れることを防ぎ、有限cache予算は増やさない。

## 受信中の一時項目と保存履歴のID（2026-10-25）

回答完了後の通常文・コードという追加条件で、一時的なattempt IDが正式なpath ordinalへ残る不整合も再現した。追従中にStreamを受け、完了通知より前に追従を止めると、既存の非追従refreshが新しい正式nodeを追加せず、一時項目を正式なordinalのように数えてしまう。表示が上へ動くこと自体とは区別するが、位置計算の前提を壊すため修正する。

一時IDをUI内で明示的に追跡する。非追従時の新たなStreamは新着として扱い、保存履歴cacheへ追加しない。既存output_itemのattempt→node参照を既存履歴読取時に取得し、UI内部の確定通知を履歴pageより先に送る。本文の一致から同一性を推測せず、コア状態・EngineEvent・DB schema・公開protocolは変更しない。

閲覧中の一時項目が確定した場合はnode IDとanchor/focusの参照を引き継ぎ、既存RebaseHistoryで現在の選択経路・ordinalを再取得する。取得完了まで表示済みframeを保持し、旧取得を無効にして新たなスクロール量は復旧後へ適用する。追従時は従来の正式tail置換を用いる。復旧中のtail refreshで旧frameだけを整形し直したり、一時IDをhistory getへ渡したりしない。一時IDの追跡も保持中nodeの範囲へ制限する。
