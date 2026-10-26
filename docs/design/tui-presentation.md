# TUIの構造化された表示設計

2026-10-24。採用状態: 今回の依頼に基づく表示方針・実装設計。表示/画面を実装し、種類別表示・原文/詳細・OSS Markdown・操作フォームを接続した。検証条件と実画面レビューの履歴は非同梱の検証記録に保存しており、本書の契約そのものと区別する。

基準は [既存UI設計](ui.md) と本書の表示・操作契約。由来となったMarkdown追加案とTUI/CLI対応資料は非同梱。今回のユーザー条件は「Claude/Codexのようにシンプルに使える」「情報量を落とさない」「構造化されたデータを構造的に表示する」。他製品の画面をそのまま複製する設計ではない。

## 1. 目標と不変条件

通常の会話画面は依頼・回答・今の作業が読み取れる量に整える。種類と関係を残した表示モデルを作り、必要な詳細へ少ない操作で進める。情報の削除、固定文字数での無表示切捨て、JSONの丸投げを可読性改善とはしない。

1. 保存DAG、APIに渡すitem、原文、raw出力、Run/Turn/所有/受付を変更しない。整形・グループ化は表示だけ。
2. 常に表示する情報、折りたたむ詳細、原文への入口を種類ごとに定義する。折りたたみは内容・件数・状態が分かり、元へ到達できることを条件とする。
3. 意味のある差、失敗、復旧待ち、未保存、未完了を装飾だけで隠さない。色・アイコンに加え状態名を表示する。
4. 各表示要素は元node/item/operation/execution等に結び付く。長いIDを全行へ繰り返さず、選択と詳細で完全IDを確認・コピーできる。
5. 正常回答、受付、tool終了、Turn完了、Run終了は別に表す。観測のtimeout/取消を対象の失敗や停止に変換しない。
6. UIでDB/file/network/解析worker完了を待たない。既存cache予算、遅着拒否、原文anchorと入力領域を守る。

## 2. 一つの画面と三段階の閲覧

常時はヘッダー、会話、入力欄、短い状態/操作案内だけを置く。複数の常設dashboardや毎行の罫線は作らない。発話ごとの「あなた」「Shellboy」という専用行も置かず、本文左のマーカーの形と色で種類を示す。色が無効でも形で区別し、継続行は本文開始位置へ揃える。実行一覧・リンク・診断等は対象から開く一時的な詳細画面へまとめる。

| 段階 | 見せるもの | 全情報への道 |
|---|---|---|
| 通常表示 | 発話本文、実行の目的/コマンド、状態と結果、変更点 | 項目の選択から詳細へ |
| 構造化詳細 | field/value、関係、時刻、対象/出典、個別call/result、警告・失敗 | 完全ID、本文page、全出力・前後・出典・URL一覧へ |
| 原文・診断 | 保存本文、tool原文、raw page、取得可能な診断値 | page/cursor/encoding指定、コピー、参照移動 |

通常表示から詳細まで同じ選択対象を維持する。詳細を閉じると元nodeの元位置へ戻り、下書きは変わらない。原文画面もページ取得し、全履歴/全rawをRAMへ展開しない。JSON形式そのものが必要な診断では明示的な原文閲覧/コピーを提供するが、通常本文や一般詳細の既定形式にはしない。秘密・返却されない内部推論・暗号化opaqueを公開する入口にはしない。

通常画面のイメージ（表示上の文言例。保存本文ではない）:

```text
Shellboy · 調査メモ                            接続 / モデル

› parserの状態遷移を調べ、問題があれば説明してください。

実行中  shell · rg 'state' src/parser.rs
  出力あり · 2秒 · 詳細

完了  子エージェント「停止境界の確認」
  検証3件成功 · 回答 / 所有処理 / 詳細

● 調査結果
  状態遷移は……

状態変更 3件 · 接続変更 / 指示更新 / Turn開始     詳細

┌ 下書き ─────────────────────────────────────────┐
│                                                 │
└─────────────────────────────────────────────────┘
回答済み・背景処理1件                  ヘルプ / 詳細
```

順序は履歴の因果順を守る。図の配置のために実際の通知を並べ替えない。狭い画面では横並びのfieldを縦へ移し、入力・状態領域を維持する。

## 3. 種類ごとの通常表示と詳細

| 表示種別 | 通常表示 | 構造化詳細・原文 |
|---|---|---|
| User message | 依頼原文、添付の種別/件数、必要な受付状態。plain | message/Run/Turnの割当、適用/保留/取消、原文、添付選択、編集分岐 |
| Assistant自然言語 | 発話本文を基本Markdown。途中発話と確定回答を識別 | node/Turn/phase等の意味があるmetadata、前後・原文・copy |
| Reasoning | 推論テキストの存在と分量、折りたたみ | 返却された可視テキストだけ展開してMarkdown。元の位置へ対応 |
| Tool call | tool名、コマンド、foreground/background、受付/実行状態。call単位 | 引数をfieldとして表示、callとresultの対応、executionへの参照。引数JSONを回答文へ混ぜない |
| Tool result / shell | 成否/終了理由、exit code、不完全出力、経過、短いplain抜粋 | stdout/stderr原文page、raw量/保存状態/encoding、stdin/write量/EOF、継続cursor。全出力への入口を表示 |
| 子Run・途中報告 | 子の目的、Turn回答とRun寿命、必要な進捗/失敗 | 親子/所有参照、回答原文、実行一覧、retry/stop、報告の送受信先 |
| 通常のcontext event | 「接続変更」「指示更新」「Turn開始」等の人間向けラベルと重要差分 | 変更前後、発生時刻/操作者、対象、元event/node。hash・全UUID・timezoneを毎行に出さない |
| 失敗・復旧待ち | 段階・理由・対象、保留中であること、次に取れる操作 | provider/HTTP/code/hint、元operationと対象、再試行の影響。正本DB障害は一般API失敗と分ける |
| Summary | 要約ラベル、基本Markdown本文、対象範囲/出典件数 | 原文出典、前後、置換経路、圧縮の受付/採用/失敗 |
| Spill | 原文参照化のラベル、対象と取得可能範囲 | 保存された原文/全出力、期限/回収状況。引用抜粋を全文扱いしない |
| 添付 | file/image、名前、選択/除外/欠落、更新あり | 完全path、kind/managed、容量、送信時の観測、再読込/除外。実体は必要時workerで準備 |
| 再起動・中断metadata | 前回処理が中断/結果未確認であること、件数 | 旧Run/子/実行/未処理message/wait条件の全参照。preview外もpageで取得 |
| tombstone / 未対応node | 回収理由、または「未対応の項目: 種別」 | 取得できる参照・metadata・原文。未知JSONを空の正常回答や巨大な会話本文へ化けさせない |

単一のassistant_response nodeに複数itemがある場合、自然言語・reasoning・function_callを別blockへ投影する。tool groupのcall/result対応は順序を保ち、共通group表示から個別callを開ける。roleだけを見て全体へMarkdownを適用しない。

Tool抜粋を折りたたむ場合は全文量と省略中の範囲を明示する。途中のerrorが通常抜粋外でも、保存された構造化error/statusを通常表示から確認できる。本文の特定語を見つけただけで成功/失敗を推測しない。

## 4. 通知の整理と情報の保存

同じ対象の連続する通常context通知は、一つの表示groupにできる。groupは所属event IDと元順序を持ち、「状態変更3件」のように件数と種類を示す。展開すると各変更前後を同じ順序で読む。別の発話・tool・失敗をまたいだ結合や、同じ文言だからというevent削除はしない。

「接続 A → B」「指示ファイル更新」「索引pause」等、利用者の判断が変わる情報は通常表示にも短く残す。run/turnの定型開始通知はまとまりの中へ置く。失敗・取消・復旧待ち・保存障害・出力不完全は、通常の成功通知groupへ埋めない。

非同期進捗の同一表示項目は最新状態へ更新できるが、確定した履歴eventやtool結果を書き換えない。「更新集約」「同じ履歴を消す」「状態変化を隠す」を区別する。開いた詳細の対象が変更された場合はrevisionと更新表示で知らせる。

## 5. 表示モデルと責務

```text
保存node + provider item + runtime snapshot
    ↓ bounded workerで種類・field・元範囲を抽出
NodePresentation
    ├─ identity: node/item/call/execution/operationとrevision
    ├─ blocks: prose / user text / reasoning / tool call / result / event / ...
    ├─ fields: 種別ごとの意味・値・状態・関連参照
    ├─ source spans: 原文node + Unicode文字位置（rawはbyte位置）
    └─ links/actions: 完全URL、参照、利用可能な共通操作
    ↓ viewport範囲・幅・style・開閉状態でlayout
Ratatui lines/cells + source map + link regions
```

`UiNode.role + text` のみでは足りない。保存種別とitem区分を保持したpresentation enum/構造体をUI境界へ渡す。任意ValueをUI描画で再帰stringifyしない。`display_payload` / `history::text_of_payload` は原文取得・copy・既存anchorの互換経路として区別し、表示のために抽出規則を一括変更しない。

view modelへDBやControlの可変内部状態を露出せず、ID/revision付きのimmutable snapshotと利用可能操作を渡す。別UIが使えるcore操作・状態と、標準TUI固有のMarkdown/セルlayoutを分ける。既存非公開DB照会が必要な表示情報は、共有snapshot契約として切り出す対象に列挙する。単にUI moduleを別crateへ動かす作業で代替しない。

原文offsetの根拠がない構造化fieldを本文offsetへ捏造して割り当てない。fieldは安定したitem/field keyで選択し、本文ブロックは元node/item/原文位置へ対応させる。再起動・既存view_stateとの互換は保存node＋本文offsetを基準にし、fieldの一時フォーカスは表示状態として扱う。

## 6. Markdown、リンク、copy

- prose/summary/可視reasoningだけをA01範囲でMarkdown化。user/draft、コマンド、stdout/stderr、JSONを含むtool原文はplain。
- 見出し・リスト・引用・強調・inline/fenced code・基本表・HTTP(S)リンクを扱う。コードは区別し、任意のハイライト能力を初版必須へしない。
- 狭い表はcell折返し又は行ごとの「列名: 値」に縮退し、列/値を捨てない。未対応記法も読めるtextで残す。
- URLは完全値をlink metadataに保持し、下線等で識別。ラベル短縮・折返しはリンク先/コピー値へ適用しない。現在メッセージからリンク一覧と完全URL確認/コピーへ到達できる。
- OSC 8対応は採用Ratatui版の差分描画と統合し、制御列を通常Stringの幅へ混ぜない。未対応端末にはリンク一覧を代替として残す。
- shell由来OSC 8はlabel/targetへ解析するか完全URL表示へ縮退。外部制御列をそのまま端末へ透過させない。raw bytesは保持する。
- URL認識だけではネットワークへアクセスしない。開くのは明示操作で、起動成否を観測できない場合は「起動成功」と断定しない。
- copyは表示装飾・折返し・省略記号を含む画面文字列ではなく、原文又は完全なfield値を使う。長い原文は現在の限定取得と明示的なpage継続を守る。

## 7. 操作の一貫性とTUI内入口

通常は入力フォーカスを維持する。履歴項目を選ぶ操作から詳細へ進み、Escで元の位置と下書きへ戻れる。マウスを必須にせず、選択・開閉・参照移動・copy・操作対象指定をキーボードで完結する。F2の既存端末選択/copy、送信/改行、中断、終了を転用しない。キーとslashの追加候補はhelpと衝突を確認して実装前に固定する。

各画面は一つの対象選択と構造化表示を共有し、以下のControl操作へ接続する。汎用JSON入力欄やCLIコマンド実行窓を通常の利用入口にしない。

| TUI入口 | 到達する操作・必要な情報 |
|---|---|
| Thread一覧/詳細 | list/get/create/fork/set/delete-preview/delete。対象・descendants、動作中の停止条件、未保存new画面の差 |
| 依頼/受付詳細 | message send/get、operation get。本文・割当・適用状態、pending、受付結果と実行結果 |
| Run/Turn/復旧詳細 | run get/retry/stop、turn get。現在以外の対象も選択、所有関係と理由、retry主体 |
| 実行一覧/詳細 | shell get/read/write/eof/stop。execution、encoding、出力量、stdin/EOF/部分write、対象停止 |
| 待機/通知 | typed conditions、複数any、timeout、観測取消、matched/snapshot。自動status表示とは別の明示wait |
| 履歴/原文/検索 | history list/get/search。任意node、出典/前後、page/encoding、query/limit |
| 添付管理 | add/list/remove、下書き/保存選択、kind、完全path、明示再読込 |
| 圧縮 | compact。安全境界待ち・採用/失敗・原文参照、復旧待ちを勝手に解除しない |
| 索引管理 | status/backfill/retry/rebuild。scope/space/thread/source、費用preview、既存成功と失敗範囲 |
| 保存管理 | storage status/collect。カテゴリ、論理/物理/回収下限/保護量、回収結果 |

状態が不適切な操作は理由と利用条件を表示し、既存Controlが最終検査する。同じ対象/引数/呼出元条件ではCLIと同じ意味にする。UIからCLI子プロセスやDB直接書込みを行わない。保持・コピー・展開のような表示操作だけでRun/所有処理を停止しない。

## 8. cache、stream、anchor

意味抽出/Markdown解析/重いlayoutはUI外の有限worker。原文revision/item版、解析/style版、幅、開閉状態、link対応方式等でcache有効性を決める。新しいcacheを同じ既存予算と別枠に暗黙増設しない。構造metadata/source map/link文字列とcapacityも計上し、表示窓外は再取得できる。

streamは未完のblockを暫定表示でき、確定後に同原文を整形し直す。未完Markdownを毎delta全文再解析せず、有限tail/変更区間と更新集約を使う。前のTurn/選択/幅に対する遅いworker結果はgeneration/revisionで破棄する。既に選択している過去本文を新着tailへ置き換えない。

スクロール・折りたたみ・表縮退・幅変更は、元node/item/原文位置から対応セルを再計算する。装飾位置にいる場合は直近の元blockへ対応させ、見た目の行番号を永続anchorにしない。折りたたんだ本文にanchorがあればそのplaceholderへ対応させ、展開時に元位置を復元する。

## 9. 実装単位と確認条件

| 単位 | 変更候補 | 完了に必要な証拠 |
|---|---|---|
| 表示モデル | frontend投影、UiNode/更新型、各読み込みとstream採用 | 全読込み経路で種別/参照/原文位置を保持。旧保存view_stateとcopyが不変 |
| 基本レンダラー | 種別ごとのblock、通知group、詳細/原文画面 | 代表全種別の通常/詳細/回収済み/未知、エラーが埋まらない、JSON dumpなし |
| Markdown/URL | renderer選定、解析worker、source/link map、copy/OSC 8/代替一覧 | A01§7の全ケース、plain toolを誤解釈しない、完全URLとcopy一致 |
| 操作導線 | 共通詳細/対象選択、未対応TUI入口 | 全35操作の画面入口と条件を照合。キーだけで対象選択→結果確認まで到達 |
| 資源/復元 | 既存cache、window/page、遅着拒否、anchor | 小cache/幅変更/stream/Thread切替/長文/保留中waitでも入力継続と原文位置復元 |
| 視覚確認 | 専用data-dirの実WezTerm等、合成fixture、スクリーンショット | 通常会話/ツール/子/通知/失敗/要約/表/URL/狭幅を実画面で読み、手順と限界を記録 |

型・状態の自動試験に加え、実画面を撮って可読性を確認する。大量context通知の後でも依頼・回答・入力・状態が読み取れること、折りたたみから全情報へ辿れることを確認する。見た目だけのmockを、実際の履歴読込み・stream・原文page・操作受付の試験に代用しない。

実装前に閉じるものは、キー/詳細の最小操作、採用rendererと版、元範囲の具体型、OSC 8と差分描画、既存予算への計上。ライブラリ化のためのcrate再編やRAG実装は、この表示改善の前提にしない。


## 10. 実装着手時の判断記録

- 初期実装はOSS `pulldown-cmark = 0.13.5` による解析と独自Markdown rendererだった。原文offset/link metadataが必要という理由だけで既存表示rendererを使わなかった判断を、依頼者のOSS利用指示により見直す。`cargo info` と公式sourceで実在する `tui-markdown = 0.3.12` を確認し、表示rendererとして採用・固定した。初期のminimal設定では任意syntax highlightを無効にしていたが、コード表示を改善する追加指示により既存 `highlight-code` featureを有効にする。独自highlighterを作らず、OSS内のsyntectとansi-to-tui、共有lazyの組込みBase16OceanDarkを使う。テーマfileの読取りや外部取得は追加しない。依存するratatui-core 0.1は固定Ratatui 0.30.2と互換。`StyleSheet::heading_marker` 等で表示方針を指定し、`Options::table_width` へ実表示幅を渡す。通常のMarkdown解析・構文表示を独自に再実装しない。pulldown側は原文offset/完全URLの薄いmetadata adapterとして残し、rendererの生成linesと別管理する。幅に応じたOSS変換は有限frontend workerで行い、UIは生成済みlinesを描く。描画linesのheap/capacityも保持量とmetadata64KiB上限へ計上する。上限超過時は幅付きのomitted状態を保持し、元block本文のplain表示へ戻す。未準備のNoneと区別し、毎描画でworkerへ再依頼するloopを作らない。保存形式/API入力は変えない。[renderer公式API](https://docs.rs/tui-markdown/0.3.12/tui_markdown/)・[Options](https://docs.rs/tui-markdown/0.3.12/tui_markdown/struct.Options.html)・[parser公式資料](https://docs.rs/pulldown-cmark/0.13.5/pulldown_cmark/)を参照。
- コード領域はOSSが生成するfenceの位置にだけ薄いstyle adapterを適用し、言語caption・控えめな背景・余白を付ける。全周枠や独自のMarkdown字句解析は追加しない。一時markerは元本文と衝突しない値をworker内だけで使い、保存本文・Cellへ出さない。未知言語やOSS highlight/conversionで表示本文が欠ける場合は元コードのplain表示へ戻し、情報を失わない。stdout/stderr/combinedはPresentationBlock.streamで区別し、本文先頭へ文字タイトルを混ぜない。実driverの既存merged outputはCombinedのままとし、stderrを推測しない。
- Ratatui 0.30.2の[公式hyperlink例](https://ratatui.rs/examples/apps/hyperlink/)はescapeをCell symbolへ埋める幅回避を含む。日本語・隣接セル・差分描画を保つためそのhackは採用せず、完全URLと現在セル位置をBackend境界で対応させる。リンクの変更/解除で文字が同じ場合も古いtargetを消せることを回帰対象にする。
- 待機取消はTUIからの観測tokenをControlへ渡し、対象を停止しない。既存owned request workerと受付/結果保存を維持した `request_cancellable` を追加し、独自のDB経路やCLI子プロセスで迂回しない。
- 視覚確認には `examples/visual_fixture.rs` の使い捨て保存履歴と `scripts/capture-tui.py` の専用WezTermを使う。モデル出力を画像へ描いたmockではなく、実renderer・DB読み込み・端末・入力・スクリーンショットを通す。合成データの見た目確認を実provider能力や実行所有の検証へ一般化しない。利用者のownerを切り替えない。
- 最初の実画面で発話ごとのrole名行が冗長だったため、依頼者の指示に従ってマーカーへ置き換える。種類の意味が必要なツール・結果・状態変更の短い説明は残す。発話ラベルを消すことと本文・状態情報を省略することを混同しない。


## 11. 実装した入口と表示の境界

F6で履歴項目とfield、Oで原文page、F7で現在項目の完全URL、F8で全36共通操作の型付きフォームへ進む。通常の会話はinlineマーカー、ツール呼出しは操作マーカー、出力はタイトルを繰り返さないガイド付きブロックで区別する。stderrは型が分かる場合だけ赤いガイドとし、本文の単語からstreamや成否を推測しない。

保存tool resultが統合出力だけを持つ場合、frontendが既存のpin/retention generation付きraw readerで表示用の先頭抜粋を補完する。各stream最大8KiBかつ既存tool_excerpt_bytes由来の上限、履歴page全体に最大1秒（設定のfile timeoutが短ければその値）の時間枠を適用する。識別不能・期限切れ等は理由とともに統合保存抜粋へ戻し、既に明示されたstream型は保持する。canonical本文・モデルへ返すitem・保存payloadを変更しない。部分表示と完全出力を混同しない。

実装/試験の対応は tui-operations.md（非同梱の検証記録）、レビューの手順・画像・制約は tui-visual.md（非同梱の検証記録） を参照。旧初版の性能値を追加描画機能の新しい測定結果とは扱わない。

## 保存済み実行情報と削除影響の追加

F8末尾index 34に「Thread · 削除影響」を追加し全35操作へ到達可能とする。読み取りのpreviewに削除確認を課さず、結果から実削除が保証されたとは表示しない。阻害Run・共有や出典・添付参照・更新時刻を構造化fieldとして表示し、容量未見積もりの理由を残す。既存getのcontextとoperation照会のtargets/downstreamも構造化表示し、受付完了と後続完了、未知値とゼロを区別する。[採用設計](observability-and-deletion.md)。

## 2026-10-25 表示用の要点・原値の分離（X3/UX5.5）

状態変更の既知kindは人向けの名前と保存済みの重要差分へ投影する。定型開始通知の件数は同じ連続groupにまとめ、設定/指示/索引の変化や失敗理由をまとめの外へ落とさない。未知kindは未知の変更として詳細へ誘導し、既知の状態や原因へ推定変換しない。回答・依頼・推論・状態・要約は異なる記号を使う。

詳細fieldは元のJSON pointerと完全値を選択/copyの正本として保持し、optionalな表示値と人向けlabelを別に持つ。日時の解析と端末のlocal timezoneへの変換はpresentation workerで行い、表示値をcopy元へ逆流させない。「要点 / 実行 / 出力 / 参照 / その他」で既知fieldをまとめ、nullが単なる未提供を表す理由などだけを省く。未確定の終了コードなど意味のあるnullは説明して残す。原文と全fieldの明示的な取得は維持する。

要約は保存されたsummary_format/source_node_ids/part参照を根拠に出典件数と原文導線を示す。shellboy-summary-v1の生成layoutを扱う互換decoderは、part数・header順・footerのsource IDsがmetadataと厳密に一致する場合だけ生成footerを通常本文から外す。単なる文字列の出現でUUID列を削らない。曖昧なlayoutや未知formatは本文を変えず、元の内容を確認できるfallbackを残す。表示本文は元のpart本文rangeへ対応し、partial windowでも保存文字offsetを保つ。置換件数/範囲が記録から確定しない場合は作らない。

Thread抜粋は最新の保存nodeを無条件に採らず、採用経路上の依頼・回答だけを候補にし、可視推論とtool/eventを除外した本文から選ぶ。中断した依頼の案内は、実在するpending revisionに結び付いた/restore番号を先に短く示す。管理結果は人向け操作名と「すべての詳細を見る」を入口にし、受付・完了・現在の復旧対象を混同しない。Coreのschema、lifecycle、event、workerは増やさない。

起動時に保存される汎用のprevious_instance_interrupted理由は、過去の復旧待ちを証明しない。現在Turnがrecovery_waitであるか、保存理由がtyped ApiErrorである場合だけ復旧理由の専用blockを投影する。実際のrecovery_waitで理由の形式が未知なら段階不明を残す。中断snapshotに記録された状態や外部結果不確実性は、その保存記録の説明として扱う。tool結果のreason/errorがJSON nullなら理由の文字列へ変換せず、nonnull errorへfallbackする。

/retryは現在の復旧対象へ既存RunRetryを送る会話操作として扱い、受付成功はstatusへ示して通常会話を覆わない。F8から明示的に行う同じ管理操作は引き続き構造化結果を開く。入力revisionと非同期結果の相関、拒否時の下書き復元は共通のCommandOutcomeを使う。

復旧再開の履歴には、その保存eventのRun/Turnと観測日時以前に永続化されたmodel_attemptのerrorだけを読取投影する。「このTurnの再開前に記録されたモデル失敗件数」と直近の保存理由であり、復旧待ち回数・全種別の失敗回数や現在の復旧対象とは扱わない。後の失敗や別Turnは混ぜない。履歴原文は変更せず、投影fieldは/saved_model_failures下のDB由来参照として明示する。直近Attemptの完全ID・完全理由と、一致するincomplete_response原文が保存されていればそのnode参照を詳細に残す。理由bodyが128KiBを越える/不明schemaの場合は理由の捏造や切断した値のcopyをせず参照を残す。GCで元の失敗記録がなくなっている場合は補完しない。この投影は現在のactivity状態や操作targetを更新しない。

### Tool観測の鮮度と保持中viewport

保存tool結果のstatus/reasonは返却時の事実、current_stateは既存DBから読んだ時点の観測として分離する。Changedからの既存tail再取得を契機に、非follow表示の保持nodeにもcall ID単位でtool metadataだけを反映する。tail50件外の保持toolは表示近傍最大128nodeを同じ有限read workerへ照会し、各viewに未完了要求1件と再照会dirty bitだけを持つ。新しい定期照会/Core eventは増やさない。本文・Unicode offset・anchor・原文は交換しない。要求revision/Thread世代を照合し、navigation/Thread切替/受付拒否/読取失敗で待機を解消する。

旧保存reasonの英文は互換decoder一箇所でToolReasonKindへ分類し、UIの因果分岐はenumで行う。既存raw reasonは保持し、未知文言は既知の取消原因へ推測しない。Coreのtool result schemaや保存理由を変更するものではない。

### 管理画面の表示要求と副作用の分離（U14）

UIが管理操作を起動する時点で、表示epochと起点draft/viewの識別子を付ける。frontendの処理完了結果・失敗status・index費用preview・管理結果からの参照操作まで相関を維持し、UIでepochと起点を照合してから適用する。Escで管理画面全体を閉じる、会話を切り替える、別の操作入口を開く時に表示要求を失効させる。既にUIキューに届いた旧結果も新画面へ再適用しない。副作用やreceipt/CommandOutcomeは失効させず、実施結果の照会は既存CLI操作に残す。

管理結果の詳細pageは同じ表示sessionに属し、新しい表示epochを作らない。詳細から一覧へのEscではpending pointerを解除し、遅いpageだけを適用しない。これにより並行する同一index previewの有効結果まで捨てない。管理画面を開いていないslash操作の結果待ちは、overlayがない会話画面のEscでも表示を失効させられる。閉じる際には結果field/index preview/実行待ち確認をまとめて解除し、次のフォームへ過去のfieldが混ざらない。

### 操作一覧とフォームの境界（PR1-15/X9）

OpKindと全操作一覧は同じRust variant宣言から生成し、フォームの型付きbuilderは通常のRustのままとする。入力欄の名前が未定義/欠落している場合はResultの失敗にし、存在する空欄（継承や未指定）と区別する。失敗時は入力を保持して送信しない。全操作の回帰は単にbuild成功を見るだけでなく、独立したwire command名と異なるThread/Run/Turn/Message/Node/Execution IDの引数対応を照合する。

F8は既存activityの確定したphaseから現在の会話で使う候補を先頭に置き、続けて共通操作と全操作を同じtyped IDで表示する。これは受付可能性の保証ではなく、最終検証は既存Coreへ残す。文字入力で人向け名/CLI名/対応slashを絞り込み、Escは絞込み解除→一覧を閉じる。フォームからのEscは入力を送らず一覧へ戻る。原文や識別子を元に明示的に開いたフォームの対象は保持し、通常F8へ過去管理fieldを混ぜない。少数fieldのフォームには既存popup::compactを使い、長い本文/多数fieldは共通scroll枠へ収める。絞込みは128文字までで、候補のないEnterは送信しない。新しいActivityだけをThread世代/epochで受け取り、次のフォームの候補へ反映する。開いて編集しているfieldは置き換えない。関連slashの案内は同一副作用を保証するaliasではなく、添付では下書き操作と保存操作を区別する。


### 外部Thread設定の変更通知（PR1-20）

Thread設定のcommit後に既存Changedを通知する。通知先はmutationの内部effectで保持し、receipt replayでは再生成しない。既存wake/quiet_wakeはdriver開始も伴うため使わず、通知専用の対象一覧から既存changedを呼ぶ。新しいCore状態・event・worker・schemaは追加しない。値が同じ指定も既存のrevision更新契約を維持するため通知するが、Run開始/復旧再開は行わない。保存失敗はeffectを公開しない。

frontendはChangedを受けた可視Threadについて、既存の有限read workerでThreadDetailsも再取得する。履歴/Activityの更新と同じdirty/coalescing経路に置き、Thread世代を照合する既存UiUpdate::Threadへdraftなしで投影する。下書きや編集中の操作対象を交換しない。これは保存済み設定の再観測で、モデル内部状態や実行への適用時点を推測する表示ではない。


### 通常画面の補助色と見出し（X4/X5）

補足・状態・一覧見出し・本文の前景はUI内のsemantic roleへ集約する。補助の読める条件をANSI8/固定Gray/Whiteへ依存させず、端末既定前景を使う。階層は既存の記号・文言・余白・強調で保ち、色を無効化しても情報種別を失わない。syntax highlight/強調色の全面的なテーマ最適性を保証する変更ではない。暗色/明色の実テーマ受入は別途実端末で確認する。

見出しは状態→題名→モデル→cwdの順で表示する。状態は既存Activityの保存観測、未観測は確認中とし、題名に幅の半分を超えて与えない。幅不足では後方項目から縮退し、省略箇所へ…を置く。cwdは末尾優先で、完全値はF8のThread詳細field/詳細copyで保持する。接続名はモデル表示を押し出さないため通常見出しから外し、モデル一覧/Thread詳細に残す。初回受付も既存dirty-threadの世代付きThreadDetails再取得から題名を更新し、依頼本文からUIだけで別の名前を生成しない。


### 初回観測と実際の変更差分（X3実画面追補）

既知context eventのbefore:nullは初回観測として扱う。索引の全設定が明示nullかつpaused:falseの初回は「索引の状態 · 未設定」、指示ファイルの存在が明示boolの初回は「指示ファイル確認 · あり/なし」とする。初回の設定済み索引は既知の非null値と停止理由を旧値への矢印なしで示す。未知/欠落schemaから不在や未設定を推測しない。

通常の既知field差分では欠落とnullを同じ未設定として比較し、「未設定→未設定」を作らない。旧nonnullからnullへの実変更は残す。AGENTSのpath/hash、索引の全field、未知field、完全IDは原文・構造化詳細・copyで保持し、保存eventやsource範囲を改変しない。これは通常会話の見出しの投影だけで、通知自体を消したりCoreで重複除去したりする変更ではない。


### 回答の通常詳細と保存構造（X3実画面追補）

既知assistant messageのfield一覧は回答の状態・本文・参照を中心にする。実schemaでmessage/assistantと確認できたitemだけについて、単一items/contentの件数、既知のrole/type、top-level group_id:nullとreasoning_epoch:0を通常一覧から除く。複数回答/本文は元の番号を残し、意味ある0、非既定世代、未知field/null、既知schemaでないitemは保持する。UI内の一覧投影だけで、保存payloadとPresentationFieldのkey/value、本文原文O、完全値copy、source対象は変更しない。Vは要点と保持中の内部項目の切替で、省いた既定値も元のfield値で確認/copyできる。O/HistoryGetはcanonical textでありpayload JSON全体ではない。既存metadata上限と省略表示を維持し、未取得の全payload構造への到達を新たに保証しない。本文を表示ラベルから再構成したり、未知配列の構造を推測して畳んだりしない。


実端末追補: 初回の詳細回帰はlegacy items形式だけを使い、実保存のoutput配列へ通常投影が適用されないことを見逃した。canonical output、既知response/output、legacy items/itemをそれぞれ実際の配列件数とmessage/assistant型で確認する。異なるcollectionが併存する曖昧なschemaは元の項目一覧へ戻す。実DB→frontend→F6/Enter/Vを通る回帰を追加し、単体投影だけを実画面成立の根拠としない。

### 下書きの投影と入力操作（X8/U2隣接）

下書きの実表示とcursorは同じgrapheme単位の文字折返しを使う。履歴/MarkdownのOSS word wrapは変更しない。fold/添付のtyped rangeから表示chipを投影し、短い貼付け番号と量を強調する。入力高は投影後の表示行数+枠、最大8行かつ画面高の1/4（最小操作域は3行）を上限とし、実viewportにも収める。全文Buffer/全折返し行を保持せず、row/cursor計数と可視最大6行の生成に限る。計測値や実端末完了をここから推定しない。

Ctrl+Uは現在の下書き本文・fold・local添付参照・edit文脈を消去し、revisionを進める。既受付snapshotと外部ファイルには触れず、画像参照の解放は既存DraftAttachmentsへ渡す。送信/改行へCtrl+Uを設定した場合は設定を優先する。保存待ちや遅いreceipt/入力履歴が古い本文を復活させない既存revision境界を維持する。

枠上の主要案内はconfigured送信キーを使い、観測済Activityが処理中なら追加指示と安全な区切り、復旧待ちなら新しい指示による再開、停止中なら保留送信を案内する。内部処理stageや即時適用を推測しない。Ctrl+C/B、Ctrl+Uと貼付け操作は下枠へ分け、狭幅では主案内を優先する。

### shellの終了codeと観測表示（X1/X4.3実画面追補）

非0終了は色に依存せず✗、0が既知なら✓、終了code不明は中立記号とする。呼出しと結果は同じ
番号/command/現在のexit codeで照合する。ToolPresentationのUI-only optional current_exit_code
一つを既存current_stateと同じ有限DB照会から投影し、保存exit_codeへ上書きしない。
呼出しに保存終了codeがない場合、紐づく結果のexitedは「結果あり」とし、返却時code不明という
別の事実を合成しない。見出しと色は共通の失敗判定に従う。背景返却後の終了、または返却時と現在の不一致は返却時も
併記し、未知の現在codeへ保存codeを流用しない。読取失敗は現在状態未確認と明示して観測codeを
消去する。PR17の世代/要求revision失効と同じ寿命に揃え、古い値を現在として残さない。
追加値はtool metadata一件につき固定サイズ一つで、core state/schema/event/workerは増やさない。

### Turnの観測経過と単調時計（F11）

既存の分秒/日本語書式は維持し、Turnのwall timestamp差は経過時間として使わない。
S6のshell elapsed_msはその実行の単調実測としてtool見出しに表示し続ける。API待ちが欠け、
背景実行が重なるため、shell値の合計をTurn全体の時間へ置き換えない。保存日時は変更しない。

UIは既存Activity観測のRun/Turn IDと、同じDB読取完了時のInstantを使い、確認できた活動期間だけを
一つの小さいlocal clockへ積算する。返値は未知と既知の0を区別し、「観測経過2分13秒」と表示する。
選択前/再起動前の時間や、観測を失った間を補完しない。既存選択時のActivityViewリセットに従う。
遅れて届く観測時点は直近Instantより前へ戻さず、停止/再開をまたいでも二重計数しない。
DB由来turn_in_progressがfalseになる復旧待ち/終端では停止し、同じTurnの明示再試行では既知分から
再開する。Run背景活動だけで完了済みTurnの時計を進めない。古いepoch/Thread世代は時計も更新しない。

追加はUI読取snapshotのderived bool/Instantと、既存view内の既知Duration/直近観測Instant/稼働boolのみ。
読取失敗は既存の有限worker結果からUIへ観測失効を伝え、時計を停止する。新Core状態・表・列・event・
worker・timerは追加しない。既存UI tickで計数を進め、描画には確定済みの計数を使う。遅延した観測で表示秒を遡らせず、
履歴/anchorを変更しない。

### 詳細値の省略判定（R5-7、X7）

詳細fieldの文字列中に省略案内が含まれるかどうかで完全値の取得を分岐しない。UI用PresentationFieldに既定falseのvalue_omittedを置き、値を1024文字へ切る場合と深さ上限で値を置換した場合にだけtrueとする。labelの短縮と値の短縮は別であり、利用者が案内と同じ文字列を入力しても完全値copyを妨げない。管理結果のCはtrueのfieldだけ既存の詳細readerへ進め、通常fieldは保持している値をcopyする。

既存serdeにfieldがない場合はfalseで読む。PresentationFieldは表示用の投影であり、不変nodeやCLIの要求型/保存表は変更しない。固定サイズのflagを既存metadata予算へ計上する。旧版の短縮済み投影を文字列から推測復元せず、新しい投影で実際の省略を判定する。本文窓とmetadata上限の注記を分離する作業は同じX7で別途追跡し、このflagだけで本文全体の省略判定が完了したとはしない。

X7の注記は表示投影内の固定サイズ3値へ分ける。metadata上限への到達、実際に表示から除いた本文/出力のUnicode文字数、raw等の全表示範囲が未確認であることを別に保持する。旧details_omittedはserde互換の集約値として残すが、新しい注記はその一値から本文省略を推測しない。本文の取得窓はUiNodeの既存base/total座標で範囲を示す。変換出力は切った位置から省略文字数を数え、raw未回収分の量は未確認と表示する。metadataだけの上限到達は本文抜粋という注記を出さない。

F6のOはcanonical本文であり、全metadata/rawが必ず得られる入口とは称さない。metadata上限の案内は保持中の項目数とF6詳細・A操作への入口を示す。保存本文/source/copy対象と有限cache予算は維持し、新しいcore型・表・event・workerは追加しない。3値のサイズは既存NodePresentationのretained_bytesへ含まれる。

実画面ではraw出力の内側の抜粋文・表示行省略・取得窓・文字省略・metadata上限が重なったため、metadataだけの上限はF6のfield画面へ置く。通常の本文にはmetadataを理由に注記を足さない。本文の取得範囲と既知の省略文字数/未知raw範囲は短い一つの注記へまとめ、変換出力の本文へ案内文字列を混ぜない。表示行の折畳みはその場所に省略行数を示す。旧集約値しかない場合もF6で不確実性を伝え、本文省略と推測しない。

### 詳細一覧の長文プレビュー（UX5.8）

F6のfield一覧は長文をそのまま可変高itemにすると、残りの枠へ入らない項目をOSS Listが描画せず大きな空白が出る。一覧は共通popup Itemのプレビューを使い、各fieldは先頭と末尾を含む最大4表示行と省略記号で示す。小さい枠では既存の枠高制限をさらに適用する。fieldの保持値・value_omittedは変更しない。Cは従来どおり完全値/既存原文要求へ分岐し、O/Aの入口も保つ。リスト上のプレビュー省略と、元metadata値自体の省略を混同しない。新しいreader mode・core状態・workerは追加しない。

### リンク印と領域の共通化（R5-6、F13.2/F13.3）

URLは既存terminal_links::valid_link_targetを唯一の判定として、投影時とOSC 8送出直前の双方から呼ぶ。http(s)/host/制御文字拒否の契約は維持する。private RGB markerの符号化/復号と隣接cellのURL領域結合もLinkRegionの所有moduleへ寄せる。会話のoffscreen fgとリンク一覧の一時underlineから同じcollectorへ渡し、対象URLの順・実widgetの折返し/可視範囲/選択offsetを保つ。markerゼロは未割当として扱う。

リンク一覧は実List描画後のcellからhit領域を取得するが、private markerはbackendへ渡す前に対象cellから除く。新しい描画engine/worker/cacheは増やさない。色styleとクリック対象は別情報であり、取り除くのは印用underline色だけ。既存の完全URL copy・同ラベル別URL・装飾ラベル・折返しURL・wide cell/スクロールの回帰で維持を確認する。

F3のtool heading/command/outputは、全文URLがまだ一つのspan内にある段階で同じlink metadataを付け、その後に既存hanging_wrap/折畳みを適用する。可視の断片はどの行でも同じ完全URLを指し、省略した行や字下げ記号へ架空のclick領域を足さない。F7の完全URL一覧/原文/copyは維持する。Markdownの同ラベル問題F2は別の識別修正として扱い、この順序変更だけでは解決済みとしない。

### 表示色の責務（X4、UX6.1）

UIの前景・背景・強調色は `ui::theme` の意味別roleから取得する。本文/補助/強調はterminal既定前景を維持し、注意、警告、区分見出し、置換履歴、tool、発言者marker、選択面を区別する。同じ色値でも意味が違うroleは別に保つ。画面側で色値を直接選ばない。表示workerのMarkdown StyleSheetも同じUI paletteを使うが、OSSのsyntax highlight themeは再実装しない。コード面は固定背景と明るい前景の組であり、端末の既定前景だけへ置き換えない。Markdown内のリンクは本文前景と下線、独立URL行は青いlink roleという文脈差を保つ。

Activityのpulse/shimmerも同じpalette内で色を計算する。リンク位置を運ぶ不可視RGB markerは表示色ではないため、このpaletteの責務に混ぜない。色なし表示は既存CrosstermのNO_COLOR処理に従い、終了記号・番号・文言・選択記号・見出しを保持する。色設定のためにUI OS threadで環境や設定を再読しない。新しいCore状態、永続設定、workerは追加しない。

この集約は既存色値と描画意味を保つ構造整理である。暗色/明色/色なしの過去の実画面証拠はその版の確認とし、この変更後の確認とは区別する。特定の256色がすべてのterminalテーマで最適とは仮定しない。

明色の実画面確認で、highlightのないcode本文がterminal既定の暗い前景を暗いcode面へ継承することが分かった。code bodyのline既定前景と、明示前景なし/ResetのspanにはCODE_TEXTを補う。OSSが付けた具体的なsyntax色は維持し、原文・折返し・行数は変えない。これはpalette集約とは別のcontrast修正であり、明色/暗色の基底Styleからの実描画回帰と再撮影で確認する。

B12の実撮影で、明背景のMarkdown見出し/表header/引用/名前付きリンクのcyan/greenが淡いと分かった。既存OSS StyleSheetの公開hookで本文系前景をterminal Resetへ揃え、見出しと表headerは太字、リンクは下線、引用はOSS prefixを維持する。語義は色だけへ依存させない。paired code surface/syntax色、非表示link marker、URL/copy/原文範囲、OSS geometryを変えず、theme自動検出や新rendererは追加しない。これは当該Markdown本文の明暗確認に限る追加是正であり、警告色等の全面的なcontrast保証ではない。

### Markdownリンクの同一性（F2、R5-6.5）

表示labelの一致でURLを推測しない。既存pulldown-cmarkでOSS tui-markdown 0.3.12と同じoptionsのlink eventを読み、既存の有限URL metadataへdestinationで対応付ける。OSS StyleSheet::linkの呼出し順へprivate markerを付け、装飾やtable/折返しを通じて同じspanに保持する。Markdown本文・原文範囲・URL一覧/copy・OSS layout/highlightは変えない。表示時にnode内markerを既存画面targetへ一度だけ変換し、通常の裸URL検出は既に識別されたspan（未取得を含む）を再解釈しない。

呼出しmapは最大256組（既存128リンクの開始/終了分）とinline code最大256件、link文脈stack最大128件、counter/mapはrender workerの一回限りのArcで、Core状態・worker・公開DTOは追加しない。無効/metadata予算外のURLもeventの順序には数え、クリックは無効にする。OSSの単一active destinationはimage内のlink等で上書きされるため、単純な開始数×2ではなく実際のstart/take動作を追う。callback総数不一致時は全semantic markerを無効化し、誤URLを推測しない。inline codeはOSSが通常のinline styleを継承しないため、code callbackにも同じlink文脈を付ける。通常コードの裸URL検出は維持し、予算外codeは推測せず無効とする。OSS版更新ではparser options/StyleSheet clone共有/全link・code callback順と、重複・装飾・参照・table・image内link後の通常linkを再検証する。永続原文・保存link metadataは変更しない。

### 入力編集と小さい閲覧操作の境界（R4-5）

UiStateは入力受付revision、byte上限、fold番号採番、添付解放action、送信/復元の相関を所有する。本文の挿入/削除、grapheme移動、fold/添付offset調整はDraftSnapshotの純メモリ操作へまとめる。Composer全体や非同期受付を新controllerへ移さない。ModelPickerは局所key Outcomeを返し、UiStateが閉じる/再取得/コピー/モデル変更の既存actionへ変換する。閲覧用の上下/Pageキーだけをpopupの小さい純helperへ共通化し、各一覧のselectionは統合しない。

折りたたみ済み貼付けは表示上の一項目である。cursorの左右移動はその内部へ入らず境界間を移動し、Home/End/上下で内部へ到達する場合も移動方向の境界へ揃える。境界でのBackspace/Deleteは当該表示項目を一括削除し、見えない文字だけを編集してfoldが解除される挙動を避ける。展開中は既存の文字編集を維持する。原文・送信snapshot・revision/拒否回復の寿命は維持し、UI I/Oを増やさない。

入力高は既存の投影後本文/実幅計測、処理段階別案内も既存input::hintsを継続する。描画は見出し/会話/入力/状態の小さいhelperに分け、link座標/公開draw入口と既存履歴cache責務は維持する。readerのpage量は現在のterminal寸法と同じpopup幾何から決め、直前に描いた別popupの値を使わない。確認文の必要行数も共通popup幅から計算し、footerを本文scrollへ巻き込まない。

page計算用の既存popup_page一値は、UI-onlyの現在viewport Rect一値へ置き換える。初期値は未知の0×0とし、初期terminal frameの既存メモリ値、Resize event、実frameで更新する。未知時は1行へ縮退し、OSへ寸法を追加照会しない。各readerとModelPickerは自身のcontext/footerと共通幾何から操作時にpage量を求める。確認は同じ本文・scrollbar用1列も除いた同じ実幅で必要行数と操作時pageを求め、少数行はcontext/既定N/Esc footerを収める最小高へ、大きい本文は実viewport内のreaderへ縮退する。保存対象・Core状態・timerは増やさない。

確認とモデル選択の固定footerは、戻る/確定の主要操作を先頭行へ、上下/Pageや再取得等を補助行へ置く。35列でも「戻る」「実行/変更」をキー名と一緒に読めるようにし、短い確認の高さ計算には共通footerの行数も含める。最小画面では共通frameが主要行から残す。確認の既定は従来どおり戻るで、Enterを実行へ割り当てない。

### ThreadViewの履歴所有と変更境界（R4-1、U7/U13/U16.1）

ThreadViewは一つのThread世代の履歴窓、source anchor、viewport内位置、仮stream nodeを所有する。Thread ID/世代/必要なview revisionの照合、履歴・本文窓・stream確定の採用、scrollとsource照合はその所有者へ寄せる。UiStateはviewの選択、更新の宛先選択、全viewを横断する予算と有限reader要求の配送を保持する。layout cacheと予算は既存の一つを借用で渡し、別cacheや万能controllerは追加しない。

同値の移動と挙動修正を分ける。250msの保存ループはSaveDraftのキュー受付成功時にpublic view_mutでsave_retryを解除するため、保険revisionが本文未変更でも進む（U7）。StorageSaved更新handlerのprivate accessorとは別の経路であり、保存受付とdraft/status編集でも同じlayoutを再利用する回帰を置く。public view_mutの保険revisionと履歴更新の一律増分は、有限node変更入口を作ってから外す。可変node collectionを公開したままrevisionを省略せず、外部fixtureは既存UiUpdateによって履歴を作る。本文/source範囲/役割/投影が変わる入口がrevisionを進め、draft/status/anchorのみの変更では進めない。source本文を毎回全走査/cloneして同値判定する設計は採らない。

StreamCommittedとHistoryInvalidatedは宛先Threadと世代のviewへ適用し、古い世代を拒否する。背景の仮ID/anchorも保存済みnode IDへ確定するが、現在画面のanchorや管理結果を変更しない。backgroundの再取得要求は同じ宛先の世代/revisionを保持する。画面選択とRun/Turn状態は別であり、この整理で作業を再開しない。

layout keyの固定view_revision/offset/followはsource投影の識別に使っていないため取り除く。実際に必要なThread/view、履歴/選択revision、projection node ID、展開対象、実幅/高さを保持する。anchor内の行位置は描画済みsource行から計算し、anchor移動だけで同一source投影を破棄しない。独立したsource行・幅・本文更新・Thread世代・予算回帰で確認する。streamの全文clone量、queue lag、負荷下の公平性や性能の測定はF4で別に追跡し、責務移動だけで完了と扱わない。



### 生成表示の集約と原文ページの準備（F1/F4/F10）

Changedは保存済みメタデータの再照会要求であり、進行中attemptのsource bufferを無効化する通知ではない。ThreadViewのfollow履歴更新も未確定streamを単に消さず、output_itemの確定対応が到着するまで保持する。仮attempt IDを選択経路のordinalとみなさない既存境界を維持する。確定・中断済みattemptの退役はDBの一次状態を照合して行い、新しいattemptを古い照会結果で消さない。

既存EngineEvent::Streamのstream_char_offsetはattempt内の暫定delta列に対するUnicode文字位置であり、永続canonical本文のsource offsetではない。driverの既存reasoning/text/refusalフィルタと同じ順序だけを数え、未知eventは位置を進めない。新しい業務状態・永続field・event種は足さない。

stream deltaの受付と画面向けsnapshotの準備を分け、途中表示は既存max_fps以下へ集約する。UI向けキュー満杯時にdelta経路から空きをawaitせず、未送信の最新表示を有限に保持する。整形は既存presentation worker枠を使い、Core状態・workerを増やさない。snapshotの本文窓は既存64KiB/Unicode境界、表示投影は既存192KiB/128node予算を維持する。同じattemptの同じsource範囲・総文字数を繰り返し受けた場合は、本文の全走査による同値判定をせず再layoutを避ける。

broadcast欠落時に次deltaを新しい先頭と偽らない。表示済みprefixと既知offsetを保持し、不確かな続きは一時停止して保存済み出力で回復する。確定結果や受付通知は途中表示と別の信頼できる経路を維持する。準備済みframeは既存DB workerのindexed attempt照合と非blocking UI enqueueを同じclosure内で行い、terminal保存の前後と順序を揃える。整形はDB closureへ持ち込まず、full/closedでは所有frameを返してbusy loopや再cloneをしない。新規pending frameがある間だけcadenceを有効にし、idle pollerを増やさない。負荷下でdelta数、snapshot/copy量、layout回数、queue欠落・本文保持、入力応答を同条件のbaseline/afterで測り、構造整理を性能の受入証拠としない。

表示adapterは現在/直前attemptの最大2窓を所有する。各窓のsource本文は64KiB以下（UTF-8境界）で、reasoning範囲と既存metadata予算を別に数える。選択変更は旧jobのtokenを取消し、窓と未送信frameを破棄する。作成からUI dequeue ackまでのjobは全体で1件だけとし、ackは各frame固有、旧世代としてUIが拒否しても必ず返す。ackは描画完了や永続化の証拠ではない。終了時は既存read jobの取消・回収責任へ含める。Fullだけなら次cadenceで同じ所有frameを再試行し、stream ackが来ない通常通知だけの混雑でも永久停止しない。明示的な端末・履歴取消後のqueue内closureもtokenを再照合する。 再選択はUIが保持する最新2 source窓だけを戻し、同じgateで状態を確認する。一度だけの照会結果を信じず、後のChangedもその窓をdirtyにして検証するため、再選択直後の中断に追加deltaがなくても退役できる。未読の空bodyは生成本文として配信せず、次deltaの一次位置又はterminal状態を待つ。gateはinflightに加えRun live/Turn running、attempt generationとselection revisionの一致を確認する。遅延した取消・選択変更を文字列理由から推測しない。

生成途中の位置は、StreamCommittedのID対応だけでは確定本文位置にならない。pausedで仮本文を見ている場合はそのframeをattempt IDのまま保持し、boundedな本文窓だけをUI-local RebaseStreamへ渡す。旧本文をcanonical nodeへrenameしない。既存node readerでcanonical本文の先頭64KiB窓を取得し、既存presentation workerで照合する。この予算はJSON envelopeやopaque fieldのbytesではなく可視本文窓のbytesであり、大きいopaque metadataだけを理由に短い本文の照合を拒まない。canonical全本文が窓内と分かり、全体一致又は一意な範囲一致があり、reasoning/proseの対応範囲が一致する場合だけUnicode位置を写す。反復・本文差・区切り差・窓外・原文不明は推測せずcanonical先頭へrebaseし、理由をstatusに示す。全体一致なら反復本文でも同じ位置を保持する。一意の範囲探索は重なりを含む二度目の一致も検出する。照合はUIでは行わない。

RebaseStreamの本文copyは完了時の一窓だけで、通常deltaごとのcopyへ戻さない。ThreadViewは未解決のattempt/canonical ID対応を最大1組保持し、受付拒否・reader失敗後も次のスクロールで同じ有限要求を再作成できる。成功したHistoryWindowで対応と仮本文を解放する。 理由通知もThread/generation/view revisionと返却anchorを照合し、readerによる選択経路外への移動やユーザーのfollow復帰を古い照合理由で上書きしない。followへ戻った後の確定・本文cache破棄でも未解決ID対応を解放する。通常のselection token/世代/view revisionを使い、古い照会は現在画面へ反映しない。terminal照合時にoutput_itemの保存済み対応が既にあれば既存StreamCommittedを使い、未対応の中断だけをStreamSettledで退役させる。

同じ履歴再取得による全再layoutは、最終enrich後のworker生成projection witnessで抑制する。node IDだけでは、同じ保存nodeが参照する実行状態/raw出力やrecovery投影の変化を識別できず、selection revision/generationは本文窓の同値性でもない。witnessは非永続・serde対象外の32byte値とし、source窓/位置/総文字数、role/envelope、全表示field/block/link、omission情報に加え、renderedの幅/省略状態と暫定stream種別を含める。既存SHA-256へ借用値を逐次serializeし、JSON全文の一時copyを作らない。公開source hash/provenanceとしては使わない。生成不能は最適化を使わず保守的に更新する。

幅はOSS Markdownのcode/table行そのものを変えるためworker投影の識別に必要であり、viewportの最終折返し/高さを識別する既存layout keyとは責務が異なる。UIは同じnode範囲・selection revision/ordinalと同じwitnessだけを比較し、本文を再走査しない。source/window/metadata/stream確定が異なる場合は再投影する。paused previewの継続やtoolの後更新でprojectionを変更した場合はwitnessを無効化する。意味の同じtool factsだけの反復はrevisionを増やさず、状態変化と観測不能への変化は継続して反映する。

原文ページは取得後に既存presentation workerでsanitizeと表示用tab展開を一度だけ行い、rawのHistoryBodyと整形済みLineを一ページだけ保持する。描画は借用し、幅変更時のParagraph折返しだけを行う。原文のcopy、source ID/offset、前後・出典・cursor・tombstone表示はrawの一次情報を保持する。UIメインOSスレッドで64KiB本文のsanitizeを繰り返さない。既存64KiB取得上限から増やさず、次page/対象変更で古いページを置き換える。


## コードblockのcopy境界（F8）

コードblock番号と字下げ除去は固定版pulldown-cmarkのCodeBlockイベント順に従い、表示と同じoptionsを使う。独自fence走査を廃止し、表示側が既に抽出するcode本文をcopyにも共用する。list/quote内fence・indented code・CRLF・EOF終端を同じparserで扱い、表示用装飾やタブ展開をコピー値へ逆流させない。完結した原文ではCommonMarkのEOF終端も有効なblockとして扱う。

取得pageが途中で切れている場合、そのpage末まで続く未完blockを完全なcopyと称さない。parserのsource範囲と最終本文位置を保持して、既に閉じたblock又はpageより前で終わったblockだけを許容し、境界が不明なblockは原文のpage取得へ案内する。本文取得64KiB予算は増やさず、全rawの暗黙読込やUIメインへのparser処理移動は行わない。この追補は表示/copyの解釈の整合であり、保存本文や公開CLIの変更ではない。

## 未完の端末制御列（F9）

制御列として完全に認識できる範囲だけを表示から除く。OSCの終端BEL/STがない、改行又は別のESCで壊れている場合と、CSIがparameter/intermediate/finalの順序を満たさない場合は、導入部ESC]/ESC[だけを落とし、残るpayloadを通常の安全な本文として再処理する。欠けた終端を探して後続本文全体を隠さない。完全なOSC 8だけを専用link metadataへ変換し、未完payload内のURLは通常のbare URLとして元のsource位置で扱う。

入れ子ESC・改行を越えて探索しないため、未完導入部を繰返す入力でも走査を線形に保つ。raw/copy/保存本文とsource offsetは変えず、外部の制御byteを端末へ透過させない。既存の有限本文/リンク予算を使い、新worker・状態・依存は増やさない。

## 表示上のtab stop（F7）

タブは表示用Lineの可視行先頭を0として4列ごとのtab stopへ展開し、Unicodeの端末幅とspanを跨ぐ列位置を使う。原文・コピー・source offset・Markdown解析入力は変更しない。OSSコード本文はgutter/paddingを足す前、plain/toolはwrap前、共通readerは表示用borrowed Lineで整形する。同じ純helperを用い、style/URL markerは維持する。tabのない行は早期returnし、最大4倍の表示用byte増分も既存のlayout/metadata予算へ数える。

独自Markdown rendererや新cache/workerは追加しない。reader原文の毎frame整形はF10で別途cache化を検証し、この変更だけで性能指摘を完了扱いにしない。



## 投影の責務分割（R5-5）

projectはkind分類、既存history::payload::visitによる有限窓の抽出、event/tool envelopeの表示、予算確定の順をつなぐ。局所Walkは独自payload walkerではなくsource窓の投影状態として通常private moduleへ移し、schema順序/role/ordinalの走査は既存の共通visitorだけに残す。原文範囲・summary parts・opaque入力・partial window・fallback・field順を変えない。

同条件のevent prefix除去/JSON parseは共有し、trim＋object/array限定の詳細parseとは区別する。eventの停止全般とtoolのattentionは適用する状態集合が異なるため、そのpolicyを混ぜず同じsuffix/値判定だけを共有する。現在のenvelope内fields_of_valueは既に一度で、event/attention/詳細へその結果を渡す。外側payloadのfieldは別のpointer scopeなので、見かけの重複として削除しない。上限の入場判定は有限性のため抽出中に残し、最終予算の確定を一か所へまとめる。

新しい永続state/cache/worker/依存は追加しない。単なる移動を先行commitに分け、各phaseの抽出・重複削減も同値性を確認する。関数行数だけを目的としたminifyや一回転送wrapperは作らない。

投影phaseの実装では、`WindowProjection`が既存visitorのleafを窓へ切り出し、`projection::envelopes`が一回のfield抽出結果をevent/call/resultの各policyへ渡す。`projection::limits`は保持metadataの上限を所有する。Markdown tokenはenvelope field/outputの追加確保より前に制限し、最終field/link/decorate後の予算確定と区別する。この順序を最後の一括検査へ変えると途中の保持量が増えるため、入場時と確定時の二段階は残す。各phaseは既存の所有値を移動し、全文複製や別payload走査を増やさない。

## 小画面と入力イベントの再描画（U9）

最小サイズ未満でも警告を一度描いた時点でdirtyを解除し、旧画面のリンク領域を破棄する。次のresize/意味のある入力/worker更新が再描画を要求する。マウス移動・ボタン・対象外wheel・key release・focus通知は表示を変えないためdirtyを立てない。履歴領域内の有効wheel、Paste、押下key、寸法が変わったresizeは従来通り反映する。すでに立ったdirtyは無視イベントで消さない。新しいpollerや状態機械は追加しない。

## B12の残存UX監査（X11/X12/X15/X19/X20/X21、UX6.2–6.4）

原レビューの各子条件を採用し、公開ID・原文・CLI語彙を変えず表示だけで補う。X15は既存OSS code surfaceの言語captionを右端へ寄せ、「言語」と明記して色なしでも本文と区別する。syntax highlighting、code原文/copy/source rangeは既存parserを維持する。X11は共通popupの二段footerを使い、狭幅では補助keyから省きEnter/F1/再取得の意味を残す。既存headerはphaseを先頭にし、履歴/入力の最低領域と主要statusの順を実frameで確認する。

X19の空会話では設定済み送信/改行keyと作業場所を既存UI状態から表示する。AGENTS.mdは現在UIへ確認済み観測が渡らないため存在を推測せず「未確認」と明示する。UIでstatせず、不要なworker/Core観測を増やさない。X20は選択中のheaderを「表示固定 · F2/Esc解除」に変え、背景更新を適用しても端末frameを固定する既存責務とanchorを維持する。X21はキー/説明と補足を別の型付き表示行にし、十分な幅では説明列を揃え、狭幅ではキーの次行へ説明を移して全文を読めるようにする。

UX6.2は既存sourceを持つ回答/推論/ツール/結果/状態/要約のmarkerと状態語で色に依存せず識別する。呼出し/resultで共有する✓/✗等は同じ対象の状態を示すもので、別状態に同記号を割り当てない。UX6.3の本文100幅制限は既不採用を維持し、既存2列gutterとcode/table情報保持、長文の実画面で評価する。UX6.4/X12は通常画面では「会話」「作業」「応答処理」等の人向け語彙を使い、内部Run/Turn/stateはID詳細・原文・CLI名に残す。エラーの保全/次操作と短縮IDの完全copyは既存receipt/detail/rawの責務を再利用する。F1は全体help、会話一覧内は選択会話の「会話情報」と案内して文脈差を明示する。

X12のモデル失敗表示は、同世代incomplete attemptのtyped ModelErrorと保存理由の完全一致を既存workerが確認できた場合だけ、UI projection.messageをmessage部分へする。kind/status/provider codeや保存JSONは変更しない。不一致の任意文字列からprefixを推測して除去せず、モデル名/CLI名/原文・外部理由の引用は維持する。共通field labelの日本語化もkey/value/完全copyを変えない。

選択modeの固定bannerは最小画面でpopupの上枠に隠れないよう、selection ownerの最終描画としてheaderに出す。幅8では「表示固定」を省略せず、解除keyはstatus先頭に置く。既存popupの操作/reader状態やsource anchorは変更せず、復帰時は通常frameへ戻す。bannerは端末既定前景と太字を使い、明暗themeで固定warning色だけに依存しない。

### 本文・詳細の対象と表示契約（B11 F13/X10/X16–18/X22）

通常表示の末尾追従中に F6/F7 を開いた場合は、保持している最新nodeを初期対象にする。追従を止めて閲覧している間は、その閲覧anchorのnodeを使う。保持範囲から外れたanchorは最新の保持nodeへ縮退する。詳細内で選んだ対象は O（原文）、C（選択した完全値）、F6（一覧へ戻る）で交換しない。詳細の閲覧自体は通常viewportや追従状態を変更しない。

URLの妥当性は `terminal_links::valid_link_target`、セル内の非表示リンク印は同moduleのencode/region decoderが正本（既存 a2c59f6/27a4e77）。本文・詳細・OSC8 transportの各層が別のURL許可規則や印のビット演算を持たない。完全URLと原文は保持し、表示幅による省略でコピー対象を変えず、F7のコピー対象は保存したURLと一致する。

X16の引用記号と名前付きリンクの宛先併記は、pinned `tui-markdown 0.3.12` の標準表示として維持する調整不採用。引用の `>` は未処理のMarkdownではなくOSSが挿入する引用prefixであり、リンクの `label (destination)` と共に現公開StyleSheetには省略hookがない。出力文字列を一律削除すると、同じ文字を含む本文/コード、入れ子引用、表の列幅やリンクの対応を壊すため採らない。独自rendererや依存更新をこの修正へ混ぜない。将来OSSに引用prefix/宛先表示の正式な設定が追加された時点で、原文・F7/OSC8 target・表幅を維持したまま再評価する。

回答と要約のMarkdownは同じOSS renderer・同じ実幅で生成する。種類ごとの別table rendererは置かない。回答の `●` は最初の本文行に置き、推論は `▸ 推論` と字下げで従属させる。推論の開閉は表示投影だけであり、元のsource範囲を保つ。要約は `◇ 要約`、出典数、F6→Oの原文導線で通常回答から区別する。既知の `shellboy-summary-v1` だけは検証済み `source_node_ids` の件数を置換node数として表示できる。元のThread上のordinalやTurn数は保存されていないため推測表示しない。F6詳細の出典IDおよび原文ページの前/後/出典参照から、`shellboy history get --node <ID>` で元nodeへ到達できる（CLIの実オプションはhelpに従う）。未知/legacy要約へ置換範囲を捏造しない。

添付chipのdecodeと型変換はfrontendの一つのhelperへ集約し、kindは既存 `AttachmentKind::parse` を使う。通常Sendはcwd Unicode検査の後、SendEditedはその前にdecodeする既存の失敗優先順を維持する。chip順序・UTF-8位置検証、画像lease、receipt/immutable sourceの確定境界は変更しない。


### 正常終了後の保存文脈への到達（X13）

C5の停止前snapshot/永続中断nodeを新たな状態やUI内cacheへ複製しない。既存のInterrupted投影で終了理由・対象件数・不確実性・自動再実行しない事実を表示する。F6一覧にはそのnodeの既存presentation.title（作業/応答処理/コマンド/子/待機観測/終了時の記録）を使い、一律「中断情報」へ丸めない。見出しがないlegacy nodeだけ既存の汎用ラベルへ縮退する。選択対象・原文読取り・完全値copyは通常の詳細と同じ経路を維持する。snapshotのprevious/planned状態や `os_processes_inspected=false` を、再起動時にOS結果を再確認した事実へ読み替えない。通常Ctrl+Qの追加確認画面は採用しない。
## 非active下書きと更新batchの責任境界（U6/U8）

250ms autosaveはactive画面を保存済みとみなす時計ではなく、変更・未受付・失敗したUI所有下書きを再試行する機会とする。Thread切替時に旧画面の保存責任を残し、受付前拒否とworker失敗は対象Threadの最新snapshotを再試行対象へ戻す。保存責任は既存の保持viewごとの軽量署名（draft ID/revision、cursor、anchor、follow）で識別し、全本文を定期copyしない。署名が変わった対象だけを既存action queueへ渡し、通常queueとfrontend既存32件save channel/64Thread coalesceの上限は増やさない。 保存待ちIDがある時だけ既存250ms機会を利用し、表示clockだけでは保存責任を作らない。非activeの遅い失敗は旧snapshotを復元せず最新署名の再試行だけを要求し、遅い成功は入力を編集しない。 新規選択viewの下書き読込未完了はview内のUI-only flag一つで区別し、未編集の空placeholderを保存して既存下書きを隠さない。読込応答でflagを解放し、応答前でも利用者が編集した本文は保存対象に含める。

QuitはUI入力/IPC受付をfreezeした後、既存reserved shutdown envelopeで各保持Threadの最新保存対象を運ぶ。既存view数を超えてsnapshotを積み増すキューは作らず、通常queueに受付済みの送信/保存を先にdrainし、最後のsnapshotをcoalesceして既存save workerを閉じる。本文copyは変更時の保存要求と終了時に限定し、終了snapshotを拒否した重複Quitで置き換えない。新DB状態・Core event・worker・常駐pollerは追加しない。実際の保存失敗と強制終了による未保存を成功とは扱わない。

通知は最大32件又は4msの短い有限batchで適用し、各batch間で入力機会を設ける。1件の既存有限投影の適用自体は途中でpreemptせず、最初の1件は常に進める。source/presentation/viewport保持量が変わる通知だけをtrim対象とし、実UI pumpはbatch末に一度だけ全viewの予算を確定する。公開の単体applyは従来どおり返却前に予算を守る。status/receipt等、本文cacheを増やさない通知で全view×全nodeを反復しない。更新batchの件数/時間上限は入力公平性の上限であり、確定通知を捨てる経路ではない。pending streamだけのcadenceと停止/取消の優先経路は維持する。


## 表示補助の互換範囲（R5-8）

JSON Pointerはsegmentのescape/unescapeだけを共通化し、元の置換順と未知escapeをliteralとして残す挙動を維持する。path分割・配列/値参照は既存caller/serde_jsonの責務とし、独自path parserは増やさない。行からsource候補を得る既存装飾除去とconversational判定も同じ入力条件を共有するだけで、新しいmarker分類は行わない。projection生成時の未使用height引数を除いても、viewport/layout keyの高さ依存とsource anchorは変えない。

UiNode.roleは公開serde形とUnknown/legacyの表示fallbackに残るため削除しない。保存された既知kindを優先し、未知kindの原文・空placeholder・従来fixtureが可視である互換を回帰で保つ。productionの現分類だけを根拠に未知入力への分岐を到達不能とは扱わない。

## frontendの局所所有（R5-3）

保存queue/workerはDraftSaver、途中表示のsource/準備/ackは既存stream adapter、画像leaseはDraftImages、管理結果のrequest世代/保持値/待機取消はManagement、選択中Thread/draft/観測世代/token/進捗受理はSelectionへ分ける。これらを束ねる万能sessionは作らない。epoch一致の進捗受理と非active receipt回収は区別し、管理結果の参照先を現在の会話へ流用しない。画像の採用・revision・選択添付の解放条件、送信中lease、終了時の解放順は既存のままとする。

refreshのquery/project/deliverは既存HistoryReaderを利用し、有限read slot/進捗slotを保持する小さい入力snapshotとして切り出す。受付前の拒否先と各操作の結果相関を平板なdispatchに残し、120行という数値だけを満たすために全action共通controllerや追加workerを導入しない。構造の効果は所有変数・受理重複・入力条件と有限枠の回帰で確認し、短文化を挙動/性能改善の証拠と混同しない。

## 結果通知を受けた続行の表示（X14）

driverは継続入力へ取り込む通知を、既存event IDを持つShellboy event envelopeとしてcanonical経路に保存する。人向け投影はこのIDを持つ既知のshell終了・子の回答/終了/途中報告を「結果/回答/報告を受けて続行」と表示し、単なる「状態変更」だけで内容を隠さない。新しいCore状態・event種・永続field・DB照会は追加しない。

表示が示すのはShellboyが当該通知を継続入力へ取り込んだことであり、新しいTurnを起動した唯一の原因、モデルによる観測完了、回答成功、副作用の再実行を保証するものではない。既に進行中のTurnへ届いた結果も同じ意味で扱う。event IDがない古い/合成envelope、未知のkind、通知でないcontext観測では因果を推測せず、既存の事実表示を保つ。event ID・元payload・通知対象はF6原文/詳細で確認できる。開始/待機/復旧/停止の現在状態は既存activity投影が担当する。

復旧再開eventの過去失敗の抜粋でも、typed ModelError.messageを使用する。完全fieldは既存のModelError全体の表示値を維持し、利用者の理由自体に含まれる `provider:` 等の文字列を推測で除去しない。保存済みJSON・原文参照・IDは変更しない。内部のTurnという名称は人向け表示で「応答処理」とする。

## 失敗したモデル要求の受信本文

モデル要求の失敗で、表示済みの有料本文を確定回答へ偽装せず、同時に画面から消さない。`incomplete_response` は既存どおり監査用nodeとし、Threadの採用path・モデル再送manifestへ追加しない。`model_attempt.error_json` と同じ保存bodyを持つ監査nodeのIDを原文の正本にする。UIの未確定表示と原文取得はこのIDへ結ぶ。

streamの終了時、保存済み未確定sourceがある場合は、既存の暫定本文窓を「未確定の受信本文・再送対象外」として保持する。保存sourceのない取消・未受信は従来どおり暫定表示を閉じる。後続の履歴更新、Thread再選択、アプリ再起動では、保存済みbodyから最新2件を既存64KiB本文窓へ投影する。UIの本文cache予算とcanonical ordinalは増やさず、未確定項目を採用pathの件数へ混ぜない。最新2件の表示だけを古い失敗への唯一の到達手段にはせず、有限pageの未確定受信一覧から総件数・source ID・本文原文へ到達できるようにする。

失敗本文はchannelごとに非空の `partial_reasoning` / `partial_text` を優先し、それがなければ `raw_response.output`、さらに `completed_items` の平文推論/message/refusalを表示する。代替候補を連結して同じ回答を重複させず、tool引数・未知metadata・encrypted reasoningを会話本文へ混ぜない。保存原文を変更せず、表示窓・省略と未確定であることを明記する。表示済み推論と最終回答の文字位置が一致するとは推測しない。

取得・投影は既存DB/presentation workerで行う。新しいCore操作、業務event、永続表、独立workerは追加しない。失敗中の保持、原文へのsource一致、再選択/再起動での復元、古い失敗の発見、通常成功時の二重表示抑止、採用path/再送への非混入を合成データで確認する。

成功時の `model_stream_observation` も同じ読取り専用の受信一覧へ含め、失敗とは分けて「ストリーム差分・最終確定済み」と表示する。最終応答は通常の採用履歴にあり、差分記録を再送しない。channelごとにpartial_reasoning/partial_textを優先し、失敗時はfinal→done、成功時差分はdone→finalを代替候補とする。途中内容を最終内容で上書きせず、暗号化推論は表示しない。

一覧は `/received [page]`、1page32件、総件数・前後page・分類・Turn・保存時刻を既存管理popupへ表示し、Enterで実source IDの既存原文readerへ進む。表示一覧は現在のDBを読むものでsnapshot予約ではない。最新previewは2件、それぞれ最大64KiB窓。保存envelopeのRust側materializeは1MiBまで、大きいenvelopeはSQLiteから各partial channel先頭16k Unicode字を取得し、省略を明示する。既存body cacheを超えて保持せず、UI OS threadでDB/JSON/Markdown処理をしない。

原文APIは `incomplete_response` / `model_stream_observation` の2kindだけ保存payload_json bytesをUTF-8 textとして既存cursorで返す。本文previewの文字位置を監査JSONのbyte位置とみなさず、一般payload visitor・検索本文・モデル再送は変更しない。保存fieldがない古い記録へ本文や推論を捏造しない。

metadataだけの成功差分と、tool/encryptedだけで表示本文がない成功差分は通常previewから除外する。最終確定本文は通常nodeで読めるため重複表示を避けるもので、保存・一覧総件数・原文からは除かない。実際に回答/平文推論が異なる成功記録と失敗本文は表示する。

成功監査はStreamSettled時の即時保持対象にせず、保存previewの分類後にだけ表示する。metadata-only記録の瞬間的な重複表示を避ける。`delta_differences` が非空の成功記録はmetadata-only抑止の対象外とする。


## PR2 UI補正（12 / 13 / 16 / 17 / 18）

通常入力とrecall待ち後の遅延Enterは同じ管理action scope付与を通す。元下書きへ戻ったslashを含め、Esc・Thread切替後に古い管理結果がpopupを再表示しない。別overlayへ移った間の遅延Enterは実行せず、入力を残す。

StreamCommittedは保存nodeとの対応通知であり、canonical本文取得の完了ではない。既存仮本文は同世代の保存nodeを実際に取り込むまで保持し、UIの32件/4ms更新境界や読込失敗で有料の受信本文を消さない。保存nodeへのID付替え・本文一致推測・新しいコア状態は導入しない。

plain/toolの同一URL全出現へ同じ完全targetを結び付ける。semantic Markdownの既存targetを表示文字から上書きしない。構造化errorはmessage/code等の有限要約を通常表示し、未知構造は詳細への入口を示す。元JSONはF6原文・fieldsに保持する。

中断summaryが明示参照する詳細nodeは同じ表示範囲内で一つの要約へまとめ、全memberの原文・F6入口を残す。保存関係が確認できない詳細やpage境界の単独詳細は隠さない。terminalなshell状態は単に終了/停止等と表示し、OS生存の観測を意味する「現在」を付けない。呼出しと結果の見出しは目的と結果へ分担する。コードのtext/plaintext/未指定言語は無駄なcaption行を作らず、具体言語は短い言語名を示す。索引の内部state名は人向け状態へ写し、通常のactive遷移を管理情報の長文へ膨らませない。

UI-localの確定待ち対応表は、保持中のprovisional nodeごとに最大1件のattempt→保存nodeだけを持つ。複数commitの到着順に依存して前の対応を失わない。既存本文cacheの予算へID capacity/entry overheadを算入し、canonical取り込み・node eviction・view破棄で解放する。保存済み本文やDAGの代替ではなく、新しいworker/queue/永続表は作らない。同一executionと確認できる隣接call-only node/resultだけを表示groupにし、全memberのRowSource範囲と元callの完全URLを維持する。mixed prose、離れた結果、本文未取得の結果では呼出しを独立表示する。元call/node・F6原文を残す。

canonical IDだけを持つ本文未取得のnodeでは仮本文を置き換えない。本文を伴うHistory/NodeWindow、または真の0文字本文が届いて初めて対応を解消する。

中断groupはInterrupted summaryの保存kind（previous_instance_interrupted/app_shutdown）と、既知sectionの正規/change/.../detail_node_ids/index参照を検証する。別kindのtool JSONや似たfield名ではまとめない。NodeWindowは非同期中に元slotがevictされ得るため、既存slotへ本文を取り込めた場合だけ確定待ち仮本文を解放する。消えたslotを古い窓応答で追加せず、次の採用履歴更新まで仮本文を保つ。
