# Docs と独立 RAG の採用仕様

状態: 採用、2026-10-25。実装前に固定するローカル仕様。
由来となったRAG・利用パターンの初期提案は非同梱。採用する操作・範囲を本書に示す。ユーザー指定により名称は `docs`、
`skill` は将来の一般的なスキル対応に予約する。

## 入口

`shellboy docs list [--parent ID] [--limit N] [--cursor C]`、
`docs get --id ID [--section ID] [--limit-bytes N] [--cursor C]`、
`docs search --query TEXT [--parent ID] [--limit N] [--cursor C]` を提供する。
同梱ガイドはバイナリと同じ版で、cwd、設定、owner、API、文書DBに依存しない。
一覧は親子関係を持ち、安定ID `shellboy/shell`、`shellboy/delegation` 等で参照する。
Markdown 本文、見出し、内部リンクを取得できる。IDと配置パスは分離する。
モデルへの常時提示は一覧・検索の短い案内のみ。全文は必要時に読む。

Search defaults to keyword over the English bundled catalogue. Matching remains Unicode-aware, case-insensitive literal substring search, including Japanese literals where present; queries are not translated. See the [documentation language policy](documentation-language.md).
Embedding 不在時にも使える。semantic/hybrid は `rag search` で明示選択する。
一覧は既定50、最大200、検索は既定10、最大100件。
本文は既定16KiB、最大64KiBのUTF-8境界付きページ。
cursor は版・条件・offset に束縛し、別条件や更新後の再利用を拒否する。

## 独立 RAG

`shellboy rag [--db PATH] init`、`source add PATH --name NAME`、`source list`、
`source remove --source ID`、`index [--source ID] [--dry-run] [--mode keyword|semantic]`、
`status`、`inspect --id ID`、`ingest --id ID --source-file PATH --kind KIND
 (--text-file PATH|--text-stdin) [--metadata-file PATH]`、
`search --query TEXT [--scope S ...] [--mode keyword|semantic|hybrid] [--limit N]`、
`get --ref REF [--limit-bytes N] [--cursor C]` を提供する。

owner/生成モデル起動は不要。DBは既定データディレクトリ直下の `rag.db`、
会話DBと別のschema/UUIDを持つ。未初期化の読み取りでDBを作らない。
パス解決は既存CLI同様 `--data-dir` / `SHELLBOY_DATA_DIR` / OS既定の順で、
`--config` はembedding接続用。TOML data_dirはRAGの保存先や履歴scopeを切り替えない。
初期化は明示的 `init`、他のmutationは未初期化をエラーにする。
登録対象は単一ファイルまたはフォルダ。登録・dry-run はAPIを呼ばない。
変更なしは再抽出/embeddingを省略。内容hashで判定しmtimeのみは変更扱いにしない。
フォルダ全走査成功時だけ消失を確定し、アクセス失敗で既存索引を消さない。
登録解除は管理索引のみを対象にし、原本を変更・削除しない。
既存版は更新失敗で壊さず、旧版が公開中である旨を明示する。
embeddingの成功分を永続化して中断・失敗後に再利用する。

原本はUTF-8 text/Markdown、文字層PDF、DOCX/PPTX/XLSXを対象とする。
PDF/Office はOSS backendを使うadapterを同梱する。backend実行ファイルは
明示設定し、自動取得/黙示切替はしない。OCR、旧Office、マクロ、完全な版面再現は
今回の対象外。第三者のJSON extractorも同じblock契約で利用可能にする。
位置情報はbackendが示した範囲のみ。元ファイル位置と抽出本文offsetを区別する。

外部生成済み解説/要約/抽出本文のingestを正式提供する。元の参照/hashと生成本文hashを
区別し、producer等は任意の出典情報とする。LLM生成は内部では行わない。
派生本文は通常ファイル同期で上書きしない。

## 共通検索と原文

scope は `docs`、`history`、`history:THREAD`、`source:ID`（登録名も解決可）。省略時は
docs と全登録資料、履歴は明示opt-in。keyword が既定。
semantic/hybrid は既存embedding接続設定を共用する。未設定・接続失敗・
異なるendpoint/model/次元の混在は明示エラー。無断keyword fallbackはしない。
異なるspaceのdistanceは直接比較せずRRF(k=60)で融合し、同一参照を重複統合する。
ヒットはscope、原文参照、版/hash、種類、位置、native provenance、距離とspaceを持つ。
履歴ノードの存在と採用経路所属を混同しない。

get は検索時の保存版を返し、現在ファイルへのすり替えをしない。
過去版を保持し、解除/GC等で取得不能なら `revision_unavailable`。
参照は保存先識別子/種類/ID/版/hashを持つopaque値で、任意pathは開かない。
文書storeは永続UUID、UUIDのない既存会話storeはpath hash・filesystem identity・node witnessで識別する。
history は明示read-only SQLite adapterでlive/offline共通。owner起動、復旧、migration、
writer fallbackはしない。短いsnapshotでschema/space/source witnessを検査し、API中は
transactionを保持しない。SQLiteのWAL/SHM調整はあり得るが論理変更はしない。

各scopeは個別snapshotであり全scope一括snapshotとは称さない。
一部失敗は部分結果とscope別エラー、終了コード非0で返す。空結果と障害を区別する。
stdout は既存CLIと同じ単一JSON。standalone用envelopeは `protocol_version`,
`ok`, `result`, `error` を持ち、部分失敗ではresultとerrorを併記する。
ownerのoperation receiptへ登録せず、`--operation-id`併用は拒否する。

## 完了条件

1. repo外cwd、owner/API/設定無しで英語catalogueの階層閲覧とkeyword検索。Unicode本文は引き続きliteral検索・UTF-8境界を検証。
2. 新規モデルが短い入口からガイドを自ら読み、shell/子への依頼を正しいIDで実行。
3. text/PDF/DOCX/PPTX/XLSXおよび第三者adapterから検索→同版get。
4. 差分同期、消失、読取失敗、版保持、派生ingest、dry-runとAPI呼出し省略。
5. 実LiteLLM embeddingで索引とsemantic/hybrid、明示scopeと部分失敗。
6. live/offline履歴参照、schema不一致、space変更、GC/更新後の参照失効、
   owner正常終了との並行動作。秘密・実会話は検証記録へ含めない。
7. cursorの条件/版束縛、UTF-8境界、入力/出力上限、並行writer busy、
   timeout/異常終了/巨大出力、標準チェック。

段階実装中は各項目の未完了を明記し、docsだけの完了を全体完了としない。

## 同梱ガイドの明示semantic索引（G14）

既定scopeはdocs＋全登録資料、historyは明示opt-in、keyword既定という採用仕様を維持する。一方、検索から同梱ガイドの資料embeddingを暗黙に行う実装は採らない。`rag init`の後、`rag index --source docs --dry-run --mode semantic`で同梱ガイドの件数・UTF-8 byte数・分割件数と資料embeddingに外部費用が発生し得る旨を確認し、`rag index --source docs --mode semantic`で明示的に準備する。予約済みdocs（内部ID @docs）はindex入口だけの特別対象で、既定のindexは従来の登録資料/派生資料を処理しdocsを追加しない。docsには外部extractor/adapterを使わない。

semantic/hybrid検索は既存RAG DBをread-onlyで開き、現在の同梱ガイド版と選択embedding profileの準備が不足する場合は、init/indexの次操作を示す。検索からDB作成、writer lock、guide本文準備、資料embedding、publishを行わない。明示semantic queryのquery embeddingだけは外部API費用が発生し得る。未初期化DBでもdocs keywordとdocs getは従来どおりowner/embedding/DBなしで動く。hybridでは不足scopeのエラーと既に得たkeyword結果を併せて返す。未索引を空結果や暗黙fallback成功とは扱わない。

明示docs indexは従来のrevision/hash/embedding cacheを再利用し、途中失敗では成功済みcacheと旧公開版を保持する。同じindexの明示再実行で不足分だけ再開する。旧実装が検索時に作った互換cacheも使用可能で、不要な再課金をしない。binary更新後のguide版不一致/追加/除外は検索前に検知し、明示indexまで資料embeddingをしない。schema/worker/設定は増やさず、会話DBとは独立した既存RAG storeを使う。

既定scopeの登録資料には、明示登録されたfile sourceと、公開済みingest本文のあるderived sourceを含める。初期化だけで作る空の内部derived容器は未登録資料と同じく既定scopeへ展開しない。これによりdocs準備済み/外部資料未登録のsemantic検索を、空容器の未索引エラーで失敗させない。空文字の明示scopeは引き続きscope_invalidであり、scope省略とは区別する。

## Thread-scoped keyword history (Issue #34)

The explicit `history:THREAD` scope shares the current-path plus transitive
completed replacement-source membership of semantic `history search --thread`.
Summary, spill, and omit sources are included, without extending to unrelated
parent/fork history. Keyword mode needs no embedding or selected semantic index.
Hits distinguish `thread_scope.on_current_path`; merged matches from several
explicit Threads retain `thread_scopes`. `provenance.current_thread_paths`
remains a current-path-only projection. The same SQL scope helper is used for
ranking, candidate filtering and snapshot revalidation. A deleted/missing Thread
is a scope failure, not an implicit all-history search.

### Overlapping retrieval scopes and deleted Threads

Repeated/global/Thread/fork selectors over the same reference are not independent
rank evidence. RRF keeps the best rank once per reference and method across those
selectors, while retaining all matching scope labels. Keyword and semantic remain
independent contributions. A selected Thread deleted during query embedding is a
`thread_not_found` scope failure after the read snapshot is reopened, rather than
a retryable index-change diagnostic.
