# 履歴の保存と採用経路

採用設計。フォーク、DAG、原文参照、履歴選択と公開読取り契約を具体化する。由来となった初期履歴・CLI仕様は非同梱。

`node` はIDを再利用しない追記履歴、`edge` は会話接続、`thread_path` はThreadが採用する正確な順序である。分岐DAGをcursorから逆探索して経路を推測しない。`append_node` は呼出し元transaction内でnode・前cursorからのedge・path・cursor・selection revision・GC root revisionを一括更新する。既存IDへの上書き/置換は行わない。会話と出典は別関係に置く。

モデル応答は `assistant_response` nodeのJSONに元のoutput配列とmetadataを保持する。個別tool resultは `function_call_output` を持つ後続nodeにする。`group_id` があれば保存されたtool_group/tool_call/tool_resultと対応させる。forkは指定nodeまでのprefixを共有し、未対応callが一つでも残るprefixは拒否する。これにより全call/全resultを分断しない。前の正常な境界へ黙って丸めない。副作用と実行中processは引き継がず、親Runも停止しない。

forkはcwd/AGENTS参照/profile/model/effort/reasoning境界と採用replacement参照、選択中経路に由来するattachment参照をコピーする。node本体は共有し、attachment実体はコピーしない。相対cwdは親cwd基準で字句的に絶対化する。任意cwdだけが指定された場合もAGENTS.md参照は親から継承する。明示agents指定は下記#51契約に従って上書きする。元Threadの後続追加・採用変更はforkへ反映されない。fork自体のparent_thread_idは系譜でありRun所有辺を作らない。

`select_path` は圧縮採用などのための低水準transaction関数。expected selection revisionをCASし、node存在・重複なし・DAG循環なしを検査し、必要な新edgeを追加してpath/cursorを差替える。summary→既存suffixは正当なDAG接続として受け入れる。replacement/source作成と全group境界検証は呼出し元の同transactionで行う。

公開listは最大200件、抜粋512 Unicode文字。cursorはscope/thread/selection revision/初回watermark/次ordinalに結び、採用経路変更で失効する。本文getは最大64 KiBのUTF-8文字境界で分割する。limitが次の一文字のbyte数より小さければinvalid_argumentとし、無進捗cursorを返さない。本文cursorはnode/payload revision/encodingに結び、encoding変更は専用エラー、payload tombstone化はcursor_expiredとなる。rawの再デコードはshell出力取得側の責務で、JSONの会話本文に任意encoding変換を装わない。

自然言語本文は既知のtext/content/output/summaryから抽出し、encrypted_content/ID/未知拡張を自然言語として取得・索引化しない。元JSONはDBに保持し、モデルreplayで使う。`get` は本文ページと置換出典・会話の前後IDを返す。payload欠落は空の成功本文だけで隠さずtombstone_reasonを返す。参照一覧は件数上限を超えれば明示input_limitとし無断truncateしない。

GCは別の実装段階。会話・推論・要約の期限削除は導入しない。Thread削除後も共有pathと有効なreplacement sourceは到達性rootとして保護する必要があり、単なる「親Thread削除＝node削除」にはしない。本モジュールはroot revisionを更新し、GCが別laneで無検証deleteすることを認めない。

## 検索本文の正本（R2-5）

自然言語本文/role/Unicode offsetは既存 `history::payload::visit` が正本で、text_of_payload・presentation・frontendは引き続きこれを使う。重複walkerを追加しない。indexとRAGが共有する検索用body labels（answer/reasoningの元output ordinalを含む）と既知添付pathの抽出は `history::search_bodies` へ移す。自然言語抽出・検索対象kind/opaque除外・添付path metadataは意味が異なるため、一律に全JSONを歩くwalkerへ統合しない。raw本文取得/寿命とattachment_ref表のpath取得は各reader側に残し、UIのBlockKindはhistoryへ持ち込まない。保存原文・本文文字列・span・課金対象は変更しない。

## 本文readerの行decode（R2-6）

history get・index source・standalone RAG nodeは、既にpayloadを読むreaderとしてcrate内NodeRowを共有する。named fieldはkind/payload/revision/body_hash/deleted_reasonで、共通decoderの列対応を一箇所に固定する。history getのprojectionは未使用body_hashをNULLにし、従来読まなかったhashの不正型で失敗しない。各readerのNULL/tombstone判定、revision cast、本文size limit、JSON parse、エラー公開は各境界に残す。witnessのpayload存在判定や2〜3列の参照queryに本文取得を持ち込まない。DDL・保存bytesは変更しない。

## 局所helperの整理（R2-8）

history listのfilter fingerprint、cursor束縛/範囲検証、row→snippetをprivate関数へ分ける。validation→Thread/count→fingerprint→cursor→SQL→since→limit→本文decode→execution/provenanceの順序、filter JSON bytes、512文字抜粋、watermark/selection revision/state変更による失効は維持する。本文page終端は固定toolchainのstr::floor_char_boundaryを用い、開始位置検証・無進捗invalid_argumentは従来通り。

SHA-256のlowercase hexだけはcrate内digest関数を共有する。node JSON、summary keyのserde bytes、embedding/profile/文書cacheの独自framingとincremental hashは各domainで生成し、hash対象の意味を統合しない。新しい保存形式/依存/workerはない。

## Selection revisionの生存条件（R1-4）

scalar revision読取はThreadScope::Live/Anyを呼出し元に明示させ、同じ2つのSELECTだけを共有する。Liveはdeleted_at IS NULL、Anyは削除済みを含み、存在しない行のQueryReturnedNoRowsとOptionalExtensionを選ぶ位置は従来通り保持する。履歴/添付のCASはLive、既に割当済みsource/edit fork/driver snapshotの観測は既存通りAnyとする。cwd等を同時に読むcompound queryを分解してDB jobやlookupを増やさず、削除競合のvalidationを弱めない。

## TUI履歴readerの責務（R5-2）

DB workerは生存Threadのselection revision、最大50件のnamed row、採用経路と保存済みattempt対応を取得する。原文JSONのdecode/共通visitor/有限本文窓/Markdown投影は既存の有限presentation workerへ残し、RawNodeが所有する入力からUI nodeを一度に作る。位置指定は先頭からのUnicode文字位置と末尾からの位置を区別し、rebase時のanchorは投影結果のtotal_charsでclampする。新たな全文cloneや独自payload walkerは追加しない。

HistoryReaderは既存Control・更新sender・描画幅の借用をまとめ、query・project・enrich・deliverを分ける。成功/失敗の宛先は同じThread/世代/必要なview revisionへ束縛し、stream確定通知を本文窓より先に届ける。ページqueryとanchor周辺queryの異なる順序/範囲は保持し、共通の件数・row decode・生存revisionだけを共有する。pinは原文readerの既存所有を使い、表示のために別pin表・worker・queue・FrontendSessionを増やさない。

削除Threadの条件は既に各page入口がlive_thread_revisionを使う版を基準に照合する。場所の整理とは別に、通常page・cursor続頁・rebase・指定窓で削除済みと存在しないThreadを拒否する回帰の証拠を確認し、古いレビューのSQLとの差だけで新しい挙動修正を捏造しない。保存形式・公開CLI/UiUpdate契約は維持する。

R5-2の配送先はpage用のReplyとview revisionを持つWindowReplyを区別し、成功と失敗で同じ識別値を使う。DBで返すordinal/nodeとpage metadataは名前付きの一回限りの値として投影workerへ移動する。selection revisionは既存ThreadScope::Liveのscalar getterを使う。本文を読む列の異なるUI行へ未使用のhash/revision列を追加せず、rawのpin寿命は既存shell readerに残す。明示node_idの本文窓は従来通り個別nodeの可用性で判定し、Threadの採用経路とは区別する。

## Explicit instruction references (#51)

The [per-Thread instruction reference contract](agents-reference.md) supersedes
previous fixed-reference wording: explicit create/set/fork selection is allowed,
while cwd changes alone never rediscover instructions and same-Turn retry
continues using its durable snapshot.

### Persistence-only Rust fork helpers

`History::fork` and `history::fork_thread` only inherit the saved instruction
reference. They reject any explicit `agents: Some(...)`, including an empty
string, with invalid_argument before writes, directing callers to Control
thread.fork. These DB-only helpers do not acquire file workers or invent an
actor. Their returned agents_path is the actual inherited path. Product
Control/CLI/TUI fork supports explicit references through validation, CAS and
audit before invoking the primitive with an inherited/default argument.
