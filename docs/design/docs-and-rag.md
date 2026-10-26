# Docs / RAG 実装設計

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

独立SQLite schema 1。metaにUUID/schema/generation、sourcesに絶対root/名前/種別、
documentsにsource/key/active_revision/status、revisionsにcontent/extractor hashと
原本/派生provenance、blocksに本文/位置、chunksにUTF-8範囲、embedding spaceに
endpoint/profile/model/dimension/splitter fingerprint、vectorsと処理状態を保存する。
索引の単位はdocument revision。解析済みchunk集合とembeddingの採用はtransactionで
行い、公開版の差替え前に失敗したら旧版を保持する。短いwrite transactionと
プロセス間exclusive writer lock、有限busy timeoutを使う。旧版getは保存本文を読む。
keywordはliteral substringを正確な基準とし、日本語短語にも対応する。
SQLite FTSは候補高速化に使えてもliteral意味を変えない。
UUID付きref/cursorはURL-safe base64のversioned JSON。secretではなく条件と版を検証する。

## 抽出

`src/rag/extract.rs` の公開型:
`Extracted { text: String, blocks: Vec<Block>, extractor: String }`、
`Block { text: String, location: serde_json::Value }`。
`pub fn extract(path: &Path, backend: Option<&Path>, adapter: Option<&Path>)
 -> Result<Extracted, String>`。同期処理はstandalone専用blocking workerから呼ぶ。
backendはXberg v1.3.10を初期対応とし、明示path/版検査、固定argv、config discovery/OCR/
layout/embedding無効、有限timeout/出力/input、child回収を行う。
追加adapterはJSON manifestのexecutable/argvをshellなしで呼び、共通Blockへ変換する。
backend設定は `rag index --extractor PATH`、第三者は `--adapter PATH` で明示する。
原本読み取り前後hashを照合し、処理中更新は採用しない。

## Embedding と共通検索

既存HttpModelClient::embeddingsとindex::split_textを再利用する。
embedding profileをconfigから解決するのはsemantic処理時のみ。
docs文書はcatalogue hashをrevisionとするRAG側cacheへ保持し、history本文は会話DBの
read-only adapterで読む。会話DBへDDLや索引jobを書かない。
履歴readはSQLITE_OPEN_READ_ONLY/query_only、schema完全一致、短いsnapshot。
HTTP待機前にsnapshotを解放し、応答後space/source hashを再確認する。
rawはretention witnessを含めて検証する。immutable=1は使わない。
既存会話schema4はDB UUIDを持たないため、履歴store識別子にcanonical pathのhashを使い、
node ID・版・本文hash・raw witnessと安定したfilesystem identityを併せて束縛する。
Unixはdevice/inode、他のOSは取得可能なfile identity/作成時刻を使う。mtime/長さは使わない。
path自体は参照へ含めない。DBの置換・コピー・VACUUMによるfile identity変更は保守的に失効する。
履歴DBへの新UUID追加migrationは行わない。文書storeは永続UUIDを使う。
scope内で順位を作り、RRF(k=60)で統合。距離はspaceと共に保持する。
partial failureはscope別の失敗を返し、正常scopeの結果を保持する。

## 検証

docs catalogueの整合とページ復元、native CLIのIPC非依存を先に検査する。
RAGは一時store/匿名fixture/偽embedding transportで境界を検査し、最後に実LiteLLMと
固定backendで同じ公開CLIを通す。既定会話storeへの書込み/再索引は行わない。
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

file一件のhash照合→既存版再利用→抽出→再hash→prepare→明示embedding→publishをFileIndexPolicyへ分ける。policyは既存の固定profile/extractor/selection/dry-runだけを保持し、SyncCountsは公開結果に既存のchanged/unchanged/processedを集める。失敗前に増えるchanged/unchangedも従来の時点で加算する。新しいruntime state/予算/Repositoryは作らない。

消失文書sweepとderivedの明示embeddingは別private関数にする。source失敗時も既存参照一覧の読取順を保ち、破壊的sweepだけを止める。fileとderivedに同じ呼出し限定breakerを渡し、順序・失敗上限50・pause/正常reset・成功cache・旧公開版を保持する。HTTP await中に新しいDB transactionを持ち込まない。


## 履歴semantic rawの有限取得（PR2-3）

semanticのtool_raw hitも既存のstream別decoder・64KiB incremental BLOB走査を使い、raw全体をStringへ集めない。採用済みspanのstart/end（抽出Unicode文字座標）全体をincremental SHA-256で検証し、そのspanの先頭最大64K文字だけを取得用referenceのrange/hashへ束縛する。span自体が64K文字を超えても全文hash検証を省かず、locationのindexed_rangeは元の一致範囲、rangeは取得範囲を示す。raw総量が16MiBを超えるという理由だけで正常semantic hitや他の正常結果を失わせない。破損したspan/hash、欠落本文、不正encoding・retention witnessは空本文や成功hitへ変換せず、既存scope failureとして明示する。

raw全体が64K文字以内なら従来の全本文referenceを保持し、小rawのkeyword/semantic hybrid統合を維持する。有限rangeのgetは従来どおり全走査でdecoderとwitnessを検証し、range本文hashとretention変更を再照合する。新しい表・worker・queue・API要求・自動再索引は追加しない。検索の時間は走査raw量とhit span数に比例し、候補上限100件は従来どおり。
