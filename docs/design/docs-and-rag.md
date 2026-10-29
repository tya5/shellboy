# Docs / RAG 実装設計

## Current-only contract (Issue #33, 0.2.0)

Before 1.0.0, release-specific backward-compatibility paths are not part of the contract. The general schema migration runner, `user_version`/ledger validation, and protocol-version checks remain. Earlier sections describing an old-format fallback, deprecated alias, metadata repair, or compatibility shim are historical and superseded by this section; they must not be used to justify a runtime fallback.

基準: [採用仕様](docs-and-rag-spec.md)。2026-10-25、実装前の設計。

## 分離と呼出し

`src/docs.rs` は `include_str!` のcatalogue、親子関係、Markdown見出し/リンク、
content hash、keyword検索、版付きcursorを担当する。設定や保存先を読まない。
公開入口は `DocsCommand` (clap Subcommand) と
`pub fn execute(command: DocsCommand) -> Result<serde_json::Value, String>`。
RAGとの連携に `pub fn documents() -> Vec<Document>` を提供し、Documentは
`id, parent_id, title, summary, text` の公開Stringフィールドを持つ。
ガイドは全件公開し、categoriesは一覧用。意味検索はRAG側に委ねる。

docsのlist/get/searchは続きがある場合だけ非空のnext_cursorを返し、末尾および空結果ではkeyを省略する（P11、CLI契約§6）。cursorの版・条件束縛とUTF-8境界は維持する。共通のローカルpage投影に集め、owner/設定/DB/APIへの依存や公開型を増やさない。

`src/rag/mod.rs` は standalone `RagOptions` (clap Args、内部subcommand)、
`pub async fn execute(options: RagOptions, config: Option<PathBuf>,
data_dir: Option<PathBuf>, cwd: &Path) -> serde_json::Value` を公開する。
envelopeのokで終了コードを決める。エラーの本文へ秘密やHTTP response全文を出さない。
source/store/search/history/extract等は通常のRust moduleで分割する。

`main.rs` はclap parse直後に Docs/Rag を判別し、owner IPC解決より前に処理する。
ownerコマンドの公開型や35操作は変更しない。`into_command`はローカルコマンドを
`None`として返す。global operation-idとの併用はローカル入口で拒否する。
driverのsystem promptには `shellboy docs list` と `shellboy docs search --query ...` の
短い案内を追加する。パス依存や全文埋込は追加しない。

## 文書store

独立SQLite schema 3。Readers and dry-run require the current schema; only the writer owns schema preparation.metaにUUID/schema/generation、sourcesに絶対root/名前/種別、
documentsにsource/key/active_revision/status、revisionsにcontent/extractor hashと
原本/派生provenance、blocksに本文/位置、chunksにUTF-8範囲、embedding spaceに
endpoint/profile/model/dimension/splitter fingerprint、vectorsと処理状態を保存する。
索引の単位はdocument revision。解析済みchunk集合とembeddingの採用はtransactionで
行い、公開版の差替え前に失敗したら旧版を保持する。短いwrite transactionと
プロセス間exclusive writer lock、有限busy timeoutを使う。旧版getは保存本文を読む。
keywordはliteral substringを正確な基準とし、日本語短語にも対応する。
SQLite FTSは候補高速化に使えてもliteral意味を変えない。
UUID付きref/cursorはURL-safe base64のversioned JSON。secretではなく条件と版を検証する。

## UTF-8入力と外部変換

`src/rag/extract.rs`はUTF-8 text/Markdownの有限読取りのみを行う。
`Extracted { text, blocks, extractor }` / `Block { text, location }`の保存用構造を保持し、
`extract(path: &Path) -> Result<Extracted,String>`とする。PDF/Office/画像の変換・モデル解釈は外部で行い、原本を指定したingestで出典を結ぶ。詳細は末尾のIssue #24採用節。

## Embedding と共通検索

既存HttpModelClient::embeddingsとindex::split_textを再利用する。
embedding profileをconfigから解決するのはsemantic処理時のみ。
docs文書はcatalogue hashをrevisionとするRAG側cacheへ保持し、history本文は会話DBの
read-only adapterで読む。会話DBへDDLや索引jobを書かない。
履歴readはSQLITE_OPEN_READ_ONLY/query_only、schema完全一致、短いsnapshot。
HTTP待機前にsnapshotを解放し、応答後space/source hashを再確認する。
rawはretention witnessを含めて検証する。immutable=1は使わない。
会話DBは永続DB UUIDを持たないため、履歴store識別子にcanonical pathのhashを使い、
node ID・版・本文hash・raw witnessと安定したfilesystem identityを併せて束縛する。
Unixはdevice/inode、他のOSは取得可能なfile identity/作成時刻を使う。mtime/長さは使わない。
path自体は参照へ含めない。DBの置換・コピー・VACUUMによるfile identity変更は保守的に失効する。
履歴DBへの新UUID追加migrationは行わない。文書storeは永続UUIDを使う。
scope内で順位を作り、RRF(k=60)で統合。距離はspaceと共に保持する。
partial failureはscope別の失敗を返し、正常scopeの結果を保持する。

## 検証

docs catalogueの整合とページ復元、native CLIのIPC非依存を先に検査する。
RAGは一時store/匿名fixture/偽embedding transportで境界を検査し、最後に実LiteLLMと
外部変換後のingestで同じ公開CLIを通す。既定会話storeへの書込み/再索引は行わない。
上流資料の原文を変更せず、実装で確定した詳細は本設計とguidesへ反映する。

## 履歴keywordのraw走査と有限reference（G13）

historyのkeyword検索は会話DBのread-only connectionを1本だけ開き、開始時のnode rowid watermarkまでIDを32件ずつ取得する。本文は一度に1nodeだけ読み、各nodeの処理後にsnapshotを解放する。後続batchで消えた本文は取得不能として飛ばす。全履歴を固定snapshotにした保証はせず、既存どおり各hitのnode revision/hash、raw retention generation/saved sequence/encodingをreferenceへ固定し、get時に再検証する。GCで原文がなくなったreferenceは空本文へ変換せず失効する。

確定shell rawはoutput_chunkのsequence順にread-only incremental BLOB I/Oで64KiBずつ読む。既存shell出力のstream別encoding_rs decoderとEOF flushを再利用し、全rawをStringへ集めない。未知encodingまたは回収済みrawはそのbodyだけを対象外とし、canonical本文や他nodeの結果を失わせない。DB破損等の予期しない失敗まで成功として隠さない。検索は64K Unicode文字の窓にquery長（lowercase後文字数）のoverlapを付け、byte page/文字窓をまたぐliteral一致も拾う。raw内の順位は最も一致した有限窓のcountとし、全raw件数の正確な合計とは説明しない。候補は全体で100件まで保持する。queryの既存16KiB上限と空/空白拒否は変えない。

rawのkeyword hitは一致窓のstart/end（抽出Unicode文字座標、最大64K文字）をlocationおよびraw witnessへ含め、その窓のhashをreferenceへ束縛する。`rag get`は同じstream decodeから範囲だけを取り出し、既存のreference-bound cursorでページ化する。検索結果から末尾の一致も取得でき、全文raw取得の16MiB制限を引き上げない。範囲を持たない旧referenceの全文取得規則は従来どおり。有限範囲のgetでもdecoder状態を推測せず原rawを先頭から検証・走査するため、処理時間はraw量に比例する。追加DB/索引/job/cache/embedding API呼出しは発生しない。

巨大な単一chunkへSQL substrを繰返す方式はSQLite内部の全BLOB実体化を避ける保証がないため採らない。既存rusqlite固定版の`blob` featureだけを有効にし、SQLite既存APIでrowidを開いて有限bufferへ読む。追加crate、SQLite版変更、schema変更はない。小rawの全文が窓内に収まり従来の取得上限内なら旧referenceをそのまま使い、semantic/keywordの同一referenceによるhybrid統合を維持する。


## RAG呼出し内のprovider停止（R2-4追補）

ProviderBreakerはsearchの各明示scope、indexの各file/derived revision処理に共通化する。認証/credit/429は即停止、その他embedding失敗3連続で後続処理を止める。途中のscope/revision正常完了（保存済みembedding再利用を含む）は失敗countを0へ戻す。従来fileのみresetしていた差をsearch/derivedへ揃え、交互の正常/失敗を累積3件として止めない。非embeddingエラーはprovider失敗countを増やさず、既存通り保持する。停止した同一呼出しを自動再開せず、利用者の明示再実行だけで新予算を開始する。

変更により進めるのは当初から明示指定/既定展開された有限scope/revisionだけで、新source許可・自動index・自動追加API要求は作らない。失敗結果は引き続きpartial failure、成功結果/既存公開版/cacheを保持する。永続indexの同原因判定・retry回数・group停止とは寿命/予算が異なるためbreaker自体は共有しない。

## 履歴readerと検索本文の所有（R2-7）

HistoryDbは有限read-only connectionとそのstore identityを所有する。keywordはreader生成時に一度、semanticはAPI応答後の再open時に一度identityを求め、hitごとのcanonicalize/statを除く。API前のspace観測はidentity不要のconnectionだけで行う。getは従来通りreferenceのidentity一致をschema検査より先に確認してからreaderを構成する。長時間の全履歴snapshotを増やさず、keywordのnodeごとのsnapshot解放、semanticのAPI前解放/再open、refのinode束縛を維持する。

HistoryBodyはbody kind/実本文/raw witnessを一組として所有し、make_hitへ渡す。raw keywordは有限range本文、semanticも下記PR2-3の有限取得規則に従う。取得unitのhashと索引span全体のhash検証を混同しない。canonical body列挙はrawをdecodeしない。旧include_raw引数は全呼出しがfalseであったため削除し、既存raw streaming/寿命/引用範囲の責務を各readerに残す。公開HistoryBody protocol型とは別のprivate adapter型で、保存ref形式は変えない。

## 同期処理の段分離（R2-8）

file一件のhash照合→既存版再利用→抽出→再hash→prepare→明示embedding→publishをFileIndexPolicyへ分ける。policyは固定profile/dry-runだけを保持し、SyncCountsは公開結果に既存のchanged/unchanged/processedを集める。失敗前に増えるchanged/unchangedも従来の時点で加算する。新しいruntime state/予算/Repositoryは作らない。

消失文書sweepとderivedの明示embeddingは別private関数にする。source失敗時も既存参照一覧の読取順を保ち、破壊的sweepだけを止める。fileとderivedに同じ呼出し限定breakerを渡し、順序・失敗上限50・pause/正常reset・成功cache・旧公開版を保持する。HTTP await中に新しいDB transactionを持ち込まない。


## 履歴semantic rawの有限取得（PR2-3）

semanticのtool_raw hitも既存のstream別decoder・64KiB incremental BLOB走査を使い、raw全体をStringへ集めない。採用済みspanのstart/end（抽出Unicode文字座標）全体をincremental SHA-256で検証し、そのspanの先頭最大64K文字だけを取得用referenceのrange/hashへ束縛する。span自体が64K文字を超えても全文hash検証を省かず、locationのindexed_rangeは元の一致範囲、rangeは取得範囲を示す。raw総量が16MiBを超えるという理由だけで正常semantic hitや他の正常結果を失わせない。破損したspan/hash、欠落本文、不正encoding・retention witnessは空本文や成功hitへ変換せず、既存scope failureとして明示する。

raw全体が64K文字以内なら従来の全本文referenceを保持し、小rawのkeyword/semantic hybrid統合を維持する。有限rangeのgetは従来どおり全走査でdecoderとwitnessを検証し、range本文hashとretention変更を再照合する。新しい表・worker・queue・API要求・自動再索引は追加しない。semantic検索では全結果のraw spanを同一retention witness（execution/generation/saved_sequence/encoding）で束ね、witnessごとに1回だけraw全量をdecodeする。spanの入力順・順位は維持し、ページ内のUnicode文字/byte境界を共有して各spanとの交差だけをSHA-256へ渡す。異なるwitnessを混同せず、重なるspanのhashは各々を検証する。時間は異なるwitnessのraw総量と検証対象span文字総量に依存し、同じrawの再decodeをspanごとに繰り返さない。作業中は既存64KiB byte pageとその文字境界表、spanごとのdigest/最大64K文字、witness別の小raw全文最大64K文字だけを保持する。候補は既存KNN上限内で、追加の永続cacheは作らない。検索の外向け候補上限100件は従来どおり。


## 履歴query identityとsplit設定（PR2-14）

semantic queryは選択spaceに保存された接続名・modelを使い、現在のembedding既定値やsplit_charsで検索済みcoverageを切り替えない。新規spaceは生成時に独立したquery fingerprint（接続名・endpoint・model・既存splitter系統）を既存app_metaのspace別keyへ保存する。markerがあればRAGとcore検索は同じquery identity照合を使い、split_charsだけの変更を読取拒否の理由にしない。endpoint/modelの変更は拒否し、異なるvector空間で検索しない。markerの補完・削除はownerの索引責務で、read-only RAGは保存DBへ書かない。

The saved query-profile marker is required. Missing or invalid metadata fails explicitly with reindexing guidance before query HTTP; neither owner search/select nor read-only RAG infers, falls back, or repairs identity. Reindexing requires explicit scope and cost authorization.

## Embeddingの停止期限（Issue #8）

独立RAGのembeddingは履歴indexと同じHTTP clientの接続別 `request_timeout_ms`（既定60,000ms）へ統一する。RAGだけの90秒外側timeoutは置かない。期限超過は `embedding_failed`（内部causeはtimeout）として、HTTP clientが生成した設定key・stage・経過時間だけを診断へ残し、現在の明示処理を終了し、後続batchを送らない。永続indexのretry/pause政策はRAGへ持ち込まず、失敗revisionの旧公開版保全を維持する。

## 走査対象・検索準備・状態の追補（Issue #20〜#22）

独立RAGのwriterが旧schema 1/2から3へ短いatomic migrationを行い、schema 1のsourcesに除外globのJSON列、旧版に下記hot query索引を加える。旧sourceの既定は空集合で、会話DB/schemaには触れない。Readers and dry-run reject noncurrent schema; they do not project missing exclusions as empty.source addの複数 `--exclude` を永続化し、無指定の再登録では維持、非空指定で置換、`--clear-excludes` で明示解除する。pattern数64・各1024bytes以内。構文とマッチは固定版globsetを利用し、gitignoreや否定規則の独自解釈は加えない。pathはsource相対のslashへ正規化し、slashなしpatternはbasenameにも適用、directory一致で配下を走査しない。

dot entryと現在使うRAG DB・sidecarを既定対象外とする。unsupported形式は失敗ではなくskippedで、拡張子別内訳と上限付き対象/除外例を返す。対象外の旧active資料は完全な走査成功時にremovedへ移す。A complete traversal establishes the selected key set before reading or extracting any document. A per-file read/extraction failure retains that file’s old active revision as stale, but does not block removal of other unseen documents. Incomplete traversal or input limits skip all file updates and deletion detection for that source; provider-breaker deferred files also suppress deletion detection for that execution. This distinguishes a known unreadable file from an unknown directory listing.新規文書の失敗はfailed、成功版を残す失敗はstaleとする。dry-runは外部API・DB書込みをせず、semantic分割量と課金可能性を示す。

statusはsourceごとのlast_scan・有効revisionの最大created_at（last_changed_at）・現在のembedding profileに欠けるchunk数（semantic_missing）と、上限付きの失敗資料/key/理由を既存行から投影する。未知のprofileで0や成功時刻を捏造しない。embedding完了時刻の列、結果保存表、operation receipt、会話Coreとの連携を増やさない。長時間index結果は既存shell executionのwait/readで回収する。

hybridは既にkeywordとsemanticを明示要求しているため、keywordに成功し、設定・identityを検証したうえでsemanticの未作成/不足だけが確認できた場合は `ok:true` と `degraded:true` を返す。scope_statusと集約warningsで使用した手段・不足量・対象source付きnextを示す。Degradation reasons appear only in warnings; scope_errors contains actual failures, without duplicated warning entries.設定不正・認証・通信・DB・identity不一致等の本当のエラーはpartial_failureと非0終了を維持する。明示semantic検索はkeywordへ切り替えない。CLIはJSONのままとし、別の人間向け出力modeを作らない。

## 外部変換からの登録（Issue #24、0.1.6で採用）

RAGはUTF-8 text/Markdownと登録・出典管理へ絞る。PDF/Office固有backend、`--extractor`/`--adapter`、manifest・subprocess supervisorと選択fingerprintは削除し、互換経路を作らない。外部の変換ツールはモデルがshellで選び、変換本文は`ingest --kind extracted`、モデル解釈は`llm-explanation`/`summary`として原本と関連付ける。ツールの追加導入は利用環境の管理方針を利用者へ確認する。RAG自身は変換ツール・マルチモーダルモデルを取得/起動しない。

原本は固定64KiB bufferで逐次hashし、regular fileだけを許可する。原本の16MiB上限は廃止し、本文16MiB・metadata64KiBの上限は保持する。保存するhashは取り込み時の観測で、生成入力との一致や更新中ファイルのsnapshotを証明しない。原本の移動/削除後も保存済み派生本文と観測出典を取得でき、自動再検証/自動削除は行わない。

既存のPDF/Officeのfile-source revisionは次の完全同期でunsupported/removedとなるため、必要な資料は外部変換後にingestし直す。#20の失敗/対象外の区別を先に適用する。既存UTF-8 revisionは本文hashとtext extractor名で再利用し、旧extractor選択に由来する差は新しい互換機構を設けず通常再索引で同期する。新しいCore操作・表・worker・queue・event・依存は作らない。

### 独立RAGのhot query索引（Issue #26）

`embeddings(space_id,vec_rowid)`はMAX行IDの割当てと削除、`chunks(hash)`は孤立embedding回収のNOT EXISTSに使用する。EXPLAINと同一fixture比較で旧走査を確認した2本だけを追加し、新しい表は作らない。新規schema 3と既存schema 1/2のwriter移行で同じ索引を作り、移行は単一transactionでschema番号まで確定する。readonly/dry-runは旧schemaを変更しない。会話DB migrationとは独立する。


### Reopened Issue #20: source identity and exclusion boundaries

Glob matching is case-sensitive on every OS, uses `/` separators, and applies to descendants of a directory source, never to the registered directory root itself. An explicitly registered file remains eligible for glob exclusion. This prevents a root basename match from silently removing its entire index.

Windows source keys have used slash separators since v0.1.6. There is no legacy-key lookup or identity migration. On the first complete synchronization, old backslash-key documents leave the active index as removed and current slash keys receive new document IDs; recreating the database is unnecessary. Old keys and IDs do not identify the replacement documents for `inspect --id`. POSIX backslashes remain literal filename characters. This supersedes the initial compatibility proposal and corrects the later database-recreation guidance.

A registered root replaced by a symlink or a non-regular, non-directory entry (including a Unix socket or FIFO) is `source_unavailable`, even when an exclusion glob would match its basename. It is an incomplete traversal: all updates and removal detection for that source are skipped, and existing active revisions are retained as stale. Descendant symlinks remain skipped entries. This protects a source moved behind a symlink without following an unregistered target and prevents special files from being read or treated as empty sources. The source stores its current path, not an original filesystem-kind identity: replacement of a directory with an ordinary file is accepted as a new complete selection, which can remove the previous directory documents. Detecting that historical transition would require an additional persisted identity contract; no schema or inferred identity heuristic is added here.

A supported file that fails on every synchronization keeps the overall index result at `partial_failure` (`ok:false`, nonzero CLI exit) on every attempt, even when unrelated changes and removals are applied successfully. Callers must inspect the result counts and individual failures rather than treating that exit as a rollback.

Existing-store `rag init` takes the ordinary writer lock and applies the isolated schema migration without registering sources or starting indexing/embedding. Read-only old-schema errors recommend this non-content-mutating operation; dry-run remains read-only.

## Thread-scoped history retrieval (Issue #34)

`rag search --scope history:THREAD` applies the same current-path plus transitive completed-replacement-source scope as `history search --thread THREAD`, including summary, spill, and omit sources. Both adapters and semantic result revalidation share one SQL scope helper; no second membership rule, index, table, or worker is introduced. Filtering precedes keyword candidate collection and semantic ranking. A missing or deleted Thread is a scope failure. Keyword mode needs no embedding configuration and never starts indexing.

Scoped history hits expose `thread_scope` with `thread_id` and `on_current_path`; false identifies a retained replacement source outside the current path. Existing `provenance.current_thread_paths` remains explicitly current-path-only. If several explicit Thread scopes return the same reference, RRF merges the reference and retains all memberships in `thread_scopes`; a single membership uses `thread_scope`. Global `history` remains opt-in and does not infer the caller's Thread. The existing finite per-node snapshots and revision witnesses remain; Thread scope is rechecked in each node snapshot, including after a previously collected candidate batch. Membership is not frozen across an entire keyword scan.


### Explicit upgrade result (#33 follow-up)

`rag init` returns `schema_version: 3` and nullable `migrated_from_schema` in its
existing result. The latter is the pre-upgrade version only when this call
actually upgraded an existing store; fresh creation and reopening a current
store return null. Existing-source identities/counts remain unchanged and no
indexing or model API is implied. The completion hint reports readiness before
optional material-registration instructions.
