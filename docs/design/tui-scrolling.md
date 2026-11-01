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

## スクロール状態のフラグと書込み箇所の棚卸し（#118 第1段、2026-11-01）

リファクタ前の基準として、`origin/main` `a1b3d086` の本番コード（`#[cfg(test)]` を除く）で、会話履歴のスクロール状態を書き換える箇所を数えた。数え方は `src/ui.rs`（試験module手前）、`src/ui/view.rs`（試験を除く）、`history_interaction.rs`、`overlay.rs` の代入と状態遷移メソッド呼出しの行で、読取り・試験の準備・スレッド一覧や modelの別の `loading` は含めない。issueの当初の数（約90）とは範囲が異なるため、各段の測定は同じ数え方で比較する。

| 状態 | 持ち主 | 本番の書込み箇所 |
| --- | --- | --- |
| `follow` | ThreadView | 8 |
| `loading` | ThreadView | 22 |
| `scroll.request` | VisualScroll | 11 |
| `scroll.pending` | VisualScroll | 14 |
| `history_rebase_pending` | ThreadView | 11 |
| `committed_preview` | ThreadView | 6 |
| `scroll.initialized` | VisualScroll | 4 |
| `unseen` | ThreadView | 8 |
| `moved_during_read` | VisualScroll | 6 |
| `anchor`（model側） | ThreadView | 11 |
| `scroll.row` / `scroll.within` | VisualScroll | 8 / 8 |
| `cancel_navigation()` 呼出し | - | 5 |
| `rebase_navigation()` 呼出し | - | 5 |

書込み箇所は `view.rs` の `scroll`/`viewport_row`/各応答（`on_history`、`on_history_window`、`on_node_window`、`on_load_failed`）/`cancel_navigation`、`ui.rs` の `action_rejected`・Send・reasoning展開、`history_interaction.rs` の `select_history_item`、`overlay.rs` のCtrl+Endに集中する。行番号は `grep -n` で再現でき、各段で同じ数え方を使う。

構造の事実（変更前）: `scroll()` は `src/ui/view.rs:527` から約395行で、読取り・早期returnの分岐は、読取り中の同方向/逆方向、アンカー位置の本文窓、キャッシュ内の隙間、前後ページ、metadataのみの行の5系統に分かれる。「本当の末尾」判定（`covered_end` と選択ordinal合計と最後のnodeの文字範囲の3項）は `viewport_row` と `scroll` の2か所に重複する。`viewport_row` は問い合わせでありながら row/origin/width/height/revision/anchor/initialized を更新し、render・scroll・Tab選択から呼ばれる。

固定した試験は `src/ui/scroll_characterization_tests.rs`（flagの組を `snap()` で比較し、実際の `UiState` の入力・応答経路を通す）に置く。期待値は現在の振る舞いであり、設計上の正解ではない。疑わしいものは試験内に `// characterization: current behaviour, see #118` を付けた。PTYの手動確認（#117の3件）は自動試験にしていない。

今回の固定で分かった、現状の振る舞いのうち疑わしいもの（修正はしていない）: (1) 追従中の何も動かないwheel下でも `scroll()` がmodel anchorを現在の先頭source行へ書き、`view_revision` を進める。(2) Tabによる履歴項目選択は `scroll()` を通さず follow/anchor/within/row を直接書き、平文だけでも追従を外す。(3) 読取り中の逆方向入力は、保留した移動量を捨てるが、その逆方向入力自体は再生されない。(4) 読取り失敗後の `pending` は残り、次の明示スクロールで新しい入力に加算されて再生される。

## Navigation ownership migration (#118, stage 2)

This section is the adopted implementation plan. Stage-1 characterization is a
baseline, not the intended behaviour. Do not combine structural movement with a
behaviour fix in one commit. Existing cache/read budgets, generation and request
revision fences, selected-path ordering and source-offset anchors remain intact.
Navigation performs no I/O: ThreadView supplies bounded layouts/source windows
and returns the existing UiAction values to the frontend.

### Owned state and transitions

One `Navigation` owner replaces the fields split across ThreadView and
VisualScroll. It owns the source anchor, follow/new-output mode, request revision,
read lifecycle (including provisional-to-saved rebasing), deferred row movement,
and the projection checkpoint. Geometry/cache identity is derived state, not a
second navigation authority. No deref/alias layer will retain writable copies on
ThreadView. Source windows, provisional ID correspondence and tool observation
stay on ThreadView; they are not another scroll state machine.

| Region / state | Event | Transition / effect |
| --- | --- | --- |
| Following | Up / PageUp | Pause at visible source position, consume display rows |
| Following | Down at true tail | Remain following; first preserve the characterized revision side effect |
| Reading / ReadingUnseen | Stream / saved tail refresh | Retain source/frame; mark unseen |
| Reading | Down reaching true tail / explicit Ctrl+End | Follow, clear unseen; explicit End cancels owned read |
| Either follow mode | ViewState | Adopt supplied source anchor/mode, clear unseen |
| Idle / presentation hydration | Scroll needing source | Own one bounded body/metadata read and unconsumed row delta |
| Awaiting body / metadata | Same-direction scroll | Accumulate delta, issue no duplicate read |
| Awaiting body / metadata | Reverse scroll | First preserve current cancellation/replay semantics; fix separately |
| Awaiting read | Matching reply | Adopt window only under existing generation/revision/path fences; replay delta |
| Awaiting read | Unrelated / stale reply | Do not release the owned read or change its frame |
| Awaiting read | Failure / action rejection | Release ownership; first preserve current deferred-delta semantics |
| Provisional anchor | Commit / invalidation | Rebase source correspondence, retain frame and deferred movement |
| Rebasing | Tail refresh | Retain frame; no provisional ID passed to history.get |
| Any | Resize / projection revision | Reproject source checkpoint; first preserve render-time settlement |
| Any | Cancel / replacement view | Fence old replies, discard owned movement as currently specified |

Follow and unseen are initially independent, including the rare following+unseen
combination possible during rebasing. Structural migration must represent it,
not silently normalize it. Read ownership must distinguish hydration with no
navigation ticket from an owned navigation read, and a delivered ticket from a
still-loading ticket until the existing reply dispatcher consumes it. These
are orthogonal regions of one Navigation owner, not new workers or queues.

### Commit boundaries and gates

1. Record this design and reproducible baseline counts before moving code.
2. Move ownership and navigation algorithms into a dedicated module. Keep all
   19 snap/viewport expectations unchanged; only fixture access paths may move.
3. Centralize transitions, replace independently writable flags with explicit
   states, share true-tail detection and split scroll orchestration into named
   bounded steps. This remains structural: preserve all six marked cases.
4. After structural migration passes, fix each marked case in its own commit,
   updating only its expectations and adding regression coverage as needed:
   tail-down revision side effect; reverse while awaiting; failed-read replay;
   Tab focus entering history; render-time checkpoint writes; render-time
   true-tail clamping. A fix must name its intended invariant in this design.

Each structural boundary must pass the 19 characterizations and focused scrolling
regressions. Behaviour commits also run those gates with the one deliberately
changed expectation. Branch CI uses its default Linux/macOS jobs; do not request
Windows-only validation for this platform-independent change. A green structural
checkpoint may be handed off as partial delivery, explicitly listing remaining
migration/fixes rather than claiming stage 2 complete.

### Measurement rules

Record source-write sites (assignments, mutable take and transition invocations),
scroll function physical lines and stored navigation fields before/after each
checkpoint. Exclude test fixture writes and unrelated UI loading states. Report
projection-cache fields separately; moving them to another struct is not a field
reduction. The existing stage-1 table remains the historical reference. Retain
source references with measurements and do not use timing as an acceptance gate.
If the complete migration does not reduce write sites, scroll size and stored
fields, revisit the representation rather than count only relocated wrappers.

Baseline at `2e58321d` (before ownership movement): `scroll()` 395 physical
lines (the UiState forwarding wrapper is separately 7); 20 stored fields =
7 on ThreadView (anchor, follow, unseen, loading, rebase flag, committed-preview
pair and view_revision) + 13 on VisualScroll. Aggregate wrapper fields are not
counted twice. `scripts/dev/scroll-metrics.py` emits the source sites and distinguishes
mutable takes and visual-anchor writes that the original assignment-only table
did not count. Its before/after results, not the historical table's narrower
categories, are the consistent automated comparison.

The representation-only state boundary uses FollowMode's four existing
combinations and ReadState's Idle / Hydrating / Awaiting(ticket) /
Delivered(ticket). Delivered retains request identity until the existing UI
reply dispatcher consumes it; loading=false must not mean no owned ticket.
Both enums preserve every old combination. The stored-field count is now 18,
not 20 (7 Navigation members + 12 VisualScroll members minus the aggregate
scroll member). Independent source/presentation identities are not collapsed.
Characterization19, navigation-state2 and view-unit41 pass without changing any
of the six marked expectations. This remains structural, not a bug-fix commit.
