# 埋め込み索引と許可されたsourceの境界

## Current-only contract (Issue #33, 0.2.0)

Before 1.0.0, release-specific backward-compatibility paths are not part of the contract. The general schema migration runner, `user_version`/ledger validation, and protocol-version checks remain. Earlier sections describing an old-format fallback, deprecated alias, metadata repair, or compatibility shim are historical and superseded by this section; they must not be used to justify a runtime fallback.

本文単位・新規のみ自動・空間別・再構築失敗時に旧索引を維持・原文削除の全space反映という採用契約を具体化する。実装 `src/index/`、検証 `tests/index.rs`。初期索引仕様は非同梱。

`IndexService::new(DbWorker, Arc<Config>)` は接続/model/分割版からprofile keyを作る。URLは接続同一性の入力に含めるが、資格情報はkeyへ含めず表示しない。`with_instance` はreader pinにownerと共通のinstance IDを使う入口。`start` は設定concurrency個の共有Tokio taskを開始し、`IndexWorkers::shutdown` はcancelしてjoinする。UI threadで実行しない。埋め込み未設定なら会話は動き、通常の新規source通知は無課金のno-op。

Controlは新しい最終確定nodeを保存する同じtransaction内で `authorize_new_tx(tx, ids)` を呼び、commit後に `wake()` する。この受付はnode IDだけを `app_meta/index.auto.*` へ保存する短い処理で、会話にraw decodeや分割を待たせない。workerが一件ずつsourceを準備してsegment jobへ変える。再起動しても新規会話の許可範囲が残る。起動時に全nodeや過去Threadを走査して勝手にAPIを呼ばない。直前ownerでrunningだったAPI/準備はfailedへ戻し、既に課金が進んだ可能性を示して明示retryを待つ。

`enqueue_new` は与えられたIDだけを直接準備する補助API。過去分は `preview`、`backfill`、`rebuild` の明示選択（source IDs/Thread IDs/all）だけで対象にする。FeePreviewは候補source数/Unicode文字数/新規segment数/既に存在する処理/取得不能数を返す。token価格やプロバイダーの正確な請求額を推定しない。進行中のqueueを同一source版・body kind・範囲・hashで重複処理しない。

回答・推論・tool結果を混ぜず本文単位でjob化する。assistant_response内も `answer:<item ordinal>` と `reasoning:<item ordinal>` に分ける。reasoningは既知のvisible text/summaryだけで、encrypted_content、画像/file_data、未知opaque項目を埋め込まない。添付実体は自動解析しない。sourceに紐づくattachment_refのパスは別body `attachment_paths` へ入れ、本文中の既知パス/filenameと説明を検索できるようにする。

tool結果の表示用head/tailだけへ検索範囲を狭めない。tool_result→tool_call.operation_id→shell_executionを解決し、terminal collectorの全retained rawを `ShellOutputReader::stream` で64 KiBずつ1 pass decodeする。stdout/stderrのdecoder状態はreaderが独立保持し、出力順の自然言語を `tool_raw` 本文として分割する。UTF-8以外も実行に保存したoutput_encodingを使う。raw全体をRAMへ集めたり、各pageで先頭から再decodeしたりしない。`enqueue_tool_raw` はcollector完了時の明示入口。実際の再実行やshell副作用を起こさない。

分割は約2,000 Unicode文字、段落/改行を優先、overlapなし。sourceのUnicode本文範囲を保持する。rawは64 KiB page＋未確定segment程度の作業bufferで進め、DBへのjob投入は最大128件ごとにyieldする。JSON sourceのparseは有限のblocking source枠へ分離し、DB worker内で巨大なJSON整形を行わない。sourceの参照数・会話数をアプリ独自の実行回数制限へ変換しない。

最初の正常embedding応答でdimensionを確定する。schemaのdimension>0を守るため、未知次元のjobへ架空のdimensionを入れない。未確定時は `app_meta/index.bootstrap.*` に許可済みsegmentを置き、最初の正常応答を採用するtransactionでIndexSpaceとvec0を作り、そのvectorをそのまま採用する。次元調査のために同じ入力を二重課金しない。sqlite-vec 0.1.9の実コード上のvec0上限8,192次元を超える結果は互換性エラーとしてpauseし、切詰めない。生成table名は固定prefix＋Shellboy生成UUIDのhexだけで、利用者文字列をSQL identifierへ埋め込まない。

現schemaのembedding_jobには入力本文列がないため、初版の具体化として `app_meta/index.input.<job-id>` に分割済みjob入力（最大split_chars）、成功後 `index.preview.<chunk-id>` に最大512文字のsnippetを保存する。これは同じSQLite内の派生検索データで、ログや外部ファイルへ原文を複製しない。成功時にjob inputを削除し、失敗時は明示retryのため残す。bootstrap/autoも同じ派生寿命に従う。総論理使用量はindex.*のbytesを全て計上する。この判断はsourceを毎segment先頭から再読込みする二次的I/Oを避け、stateful encodingを含むfull rawの再開可能jobを成立させるため。将来のschema migrationで専用job-input列/表へ移す場合も同じ寿命と削除契約を保つ。

API採用transactionはjob/space状態とnode payload revision/body hash/tombstoneを検査する。tool_rawだけは元rawのretention generation・最終saved sequence・output encodingも検査し、保存済み会話本文/attachment_pathsをrawの期限へ束縛しない。実HTTP前にも共有 `gc::require_capacity_tx` で論理上限を確認する。上限ならqueueをpauseし、停止/GC/既存会話の保存を妨げず、容量回復後の明示retryまで新しい索引用embeddingを開始しない。

vec0 insert、embedding_chunk、job done、snippet、job-input削除は同じtransactionで確定する。DB保存に失敗すればvecだけを残さずrollbackし、同じAPI入力を自動再送しない。GCでjob/bootstrapが消えていれば、その古いinflight結果も不採用にする。node revisionだけでなく元の受付recordがまだrunningかも検査し、metadataが残るraw回収での復活を防ぐ。

`delete_source_vectors_tx` は原文nodeのtombstoneと同transactionで、inactive/candidateを含む全spaceのvec、chunk、job、input、preview、bootstrap、auto許可を削除する。
rawの期限回収/encoding変更は別のraw限定入口で `tool_raw` のvec/chunk/job/input/preview/bootstrapだけを同transactionで無効化し、保存済み会話本文・attachment_paths・source単位のauto許可を残す。
`raw_tombstone.<execution>`（deleting/deleted両方）以後もcanonical sourceは読めるが、新たなtool_rawを準備・復活させない。旧jobのwitnessにrawが一緒に保存されていても、body_kindがcanonicalならnode版/hashだけで照合する。元会話の自動期限削除を索引機構へ追加しない。

APIの明示長さcodeだけsegmentを有限に二分する。unknown provider errorやHTTP status/文面推測では再分割しない。一時障害は設定retry（baseline2回、1秒/4秒、±20% jitter）の範囲。通常Responses生成のretryとは独立。認証/設定/次元不一致は即pause、一時障害がretry尽きで3 job連続失敗なら共通queueをpauseする。正常結果は連続失敗をreset。個別失敗の理由・数はDBへ残し、他sourceを進められる。`retry` は既に許可済みの失敗入力だけを再開し、勝手な過去分追加をしない。

再構築は別candidate group/spaceへ行い、初期投入のbarrierと全job成功を確認してからactiveを切り替える。失敗candidateも旧spaceも自動削除しない。途中の新規確定sourceはdurable auto許可を切替barrierとして使い、旧activeとcandidateの両方へcatch upしてから切り替える。candidateの失敗もstatusへ表示し、通常retryで再開できる。source準備自体に失敗して再構築の対象投入を完了できなかった場合は旧activeを保ち、対象を確認してrebuildを再実行する。過去spaceの明示選択は対応するembedding設定へ戻してから行う。

検索は選択active spaceのconnection/model/dimensionでqueryを一度embeddingし、適格chunkへ先に絞ってvec0 KNNを行う。同nodeをまとめて要求件数（既定10/最大50）へ制限し、Unicode spanとsnippetを返す。候補数はlimit×8から適格件数/4096まで倍増し、node件数が不足する4096到達時だけ適格集合の正確なL2走査へ移る。従来の固定400件という記述は適用しない。候補metadataのRAM/DB占有とfallbackの計算量は[検索と直接関係](search-and-relations.md)に従い、4096を全走査件数やRSSの上限と説明しない。space未作成なら空結果で、検索のために全過去会話を自動indexしない。`search_detailed` は本文kindとspanを持ち、公開CLI互換の `search` はnode ID/抜粋/distanceへ写す。添付の元実体を読んで検索する機構は初版に追加しない。

試験はローカルHTTPのembedding fixture＋実bundled SQLite/sqlite-vecで、新規のみ自動/明示過去分、答えと推論の分離、opaque除外、full rawの中間部分、Unicode分割、auth pause、有限retry、明示長さsplit、失敗再構築と正常切替、late source/raw GC、vecとchunkの障害注入rollback、全space削除、durable新規許可、論理上限を検証する。これは実embedding endpoint/課金/3 OSの合格ではない。

## 管理表示と費用preview

`management_status(IndexArgs) -> Result<serde_json::Value, ApiError>` はTUI用のread-only補足で、公開CLIの `IndexStatus` 型/応答は変更しない。返却は `spaces`（id/profile/model/dimension/state/selected）、`active_space`、表示対象 `selected_space`、`scope`、`coverage`、利用者が分類を判断できる `definitions`。資格情報/接続URLを含めない。明示spaceはそのspaceだけを数え、active/candidate/古いinactiveを合算しない。存在しないspaceは拒否する。引数のscope/Thread/sourceが全部空の場合、管理表示だけを全source観測とし、backfill/rebuildの明示選択要件は変えない。

sourceの重複を除き、現在のpayload revision/body hash、必要ならrawのretention generation/saved sequence/encodingまで一致するjobだけを数える。可視本文ではbody kind/span/本文hashも現在のsourceと照合する。doneは一致するembedding_chunkがある場合だけで、pending/running/failedやsplit親を完了扱いしない。長さerrorで分割された子のdone区間が元segment全体を覆う場合も完了coverageとして扱う。

`coverage` のsource分類は互いに排他的:

- `unavailable_sources`: sourceなし、tombstone、またはpayloadなし。raw回収だけでは保存済みcanonical本文を取得不能扱いにしない。
- `not_indexable_sources`: 現在の既知の可視本文/添付パスが空で、索引対象のretained/未完raw実行もない。opaque/binaryしかないsourceを未索引の有料候補へ数えない。
- `not_indexed_sources`: 対象本文または関連raw実行があるが、選択space内に現在版のdone＋chunkが一つもない。queue済み/失敗もこの意味では未索引。
- `partially_indexed_sources`: 現在版のdone＋chunkが一つ以上あり、既知の可視本文に未coverage部分、または現在版のpending/running/failedがある。
- `fully_indexed_sources`: 索引対象のretained/未完raw実行がなく、現在の既知の全可視本文がdone＋chunkで覆われ、現在版の未処理/失敗がない。
- `coverage_unverified_sources`: 現在版のdone＋chunkがあり、既知の不足/未処理はないが、関連raw実行がある。管理表示はrawをdecodeせず、jobの終端区間だけからfull rawの完了を推測しない。これは部分索引とも完了とも別に表示する。

`eligible_sources` は最後の4分類の和。segment補助件数は `done_segments/pending_segments/failed_segments/stale_segments`。これらはsource数ではない。bootstrap段階のpending/failedはdimension未確定時も観測し、既にactive spaceがある表示はそのspaceのjobだけを数える。sourceごとのread-only観測であり、長い全件走査を一つのDB transactionでロックしない。並行更新中の全source一括snapshotだとは称さず、`definitions.consistency` にも明記する。表示はsource許可、raw reader pin、API呼出しを行わない。

`preview` は既存の候補source数/全文字数/segment総数を維持し、`already_indexed` を選択spaceの現在版done＋matching chunkのcoverageから計算する。pending/failedだけのjobはここへ含めず、長さ分割の完了子は区間unionで認める。APIを呼んだりjobを作ったりして費用を調べない。`segments` と `characters` は候補全体であり、未課金token/確定請求額ではない。full raw previewは既存のpin付きone-pass readerを使い現在内容を確認する。現在のsegment境界を跨ぐ古いraw jobは、両側の本文を集め直して完了と推測せず、`already_indexed` には保守的に含めない。

## 終了時のsource worker待機

`wait_idle()` はworker取消しと受付済み要求のdrain後に呼び、source semaphoreの全permitをasyncで取得/解放する。開始済みの `spawn_blocking` parserがcaller取消しでdetachされても、その実処理が持つpermitを解放するまで待つ。capacityは設定concurrency（内部の最小1）をu32へ検査変換し、変換不能はConfigInvalid。UIメインOS threadでは待たず、Controlの終了猶予future内で行い、強制終了選択を生かす。試験では実blocking closureをdetachし、完了前はwait_idleが完了せず、release後に完了することを確認する。

## 再構築中断の観測

candidateの `index.enqueueing.<profile>.<group>` fenceはsource許可投入が終わるまでactive切替を妨げる。投入中断やsource準備失敗でも勝手に外さず旧activeを保つ。管理JSONの `preparation` にgroup、fence有無/保存countを追加し、通常statusのreasonにも「進行中または中断、準備中断後は明示rebuild再発行、retryだけではfenceを外さない」と表示する。fenceだけから現在実行中と前回中断を区別できるとは称さない。再起動でもこの安全側の停止を維持し、全件自動backfillを再開しない。試験では投入中断相当のfenceを残し、正常vector採用後・retry後・worker再起動後も旧activeが保たれ、通知なくAPIを追加実行しないことを確認する。

## 次の既存Turn境界での索引状態通知

`record_context_observations_tx(tx, thread, at)` をControlの既存Turn境界で呼ぶ。小さい現在状態（active space ID/profile/model/dimension/stateと共通pause有無・reason.kind、ApiError形式ならcodeだけ）を `app_meta/index.observed.<thread-id>` の前回観測と比較し、初回と差分時だけ `context::record` で `context_changed` eventを同transactionへ保存する。payloadはkind=index_state、actor={kind:shellboy}、change={before,after}、observed_at/timezone。target_run_idはNULLの未消費eventで、既存のControl配送が消費確定時のRunを入れる。新Turnの作成/起床や全Thread fanout、全job通知は行わない。pauseの途中履歴全体を通知する契約ではなく、次に観測した現在状態の差分である。provider本文/資格情報/URL/検索原文を含めない。eventと観測markerはrollbackも一緒で、同状態の再観測は追加eventを作らない。

観測markerはThread固有の派生cacheで、source vectorと別寿命。Thread削除時には `DELETE FROM app_meta WHERE key='index.observed.'||thread_id` を同削除transactionで行い、削除Threadへ新しい観測を書かない。物理Thread GCにもこのexact keyの削除を引き継ぐ。markerだけを回収した場合でも次の境界で現在状態を初回通知し直せるため、会話原文は失わない。GCの論理使用量は既存のindex.*集計へ含まれる。観測のために新しいThread IDや原文hashを作り直さない。

## rawの表示encoding変更と索引

`ShellOutputReader::read` で保存encodingを変更するtransactionは、当該shell_executionへ紐づくtool_result nodeを求め、raw限定の削除入口で全spaceのtool_raw vec/chunk/job/bootstrap/input/previewを削除してからencodingを更新する。canonical本文/attachment_paths/auto許可は残す。いずれかが失敗すれば全体rollback。raw bytes・saved_sequence・retention_generationは変更しない。aliasをcanonical encodingへ解決し、同じencodingの再指定では無効化しない。変更後に自動で有料再indexせず、明示のenqueue/backfillで新しいdecodeの索引を作る。ASCIIのように新旧decode本文hashが同じでも、古いjobが削除されるためdedup衝突で再受付を妨げない。inflight旧結果は元job/bootstrap recordの消滅とwitness encoding不一致で不採用にし、古いvectorを復活させない。

## Source削除のmetadata照合（G11）

schema v7ではbootstrap/autoのnode IDへpartial expression indexを持ち、
`delete_source_vectors_tx`は一致するsourceだけを検索する。prefixと`json_valid(value)`を
CREATE INDEXとDELETEで一致させ、`INDEXED BY`でpoint lookupを固定する。他のpreview/inputや
別prefixをJSON解釈しない。旧DBの壊れたJSONはindex対象外で保持し、識別不能なmetadataを
推測で削除しない。既存のindexer診断を迂回して正常化しない。全spaceのvec/chunk/job/input/
preview削除と同transactionなので、source tombstone/retention変更のrollback境界は変えない。

## 埋め込み障害の連続判定（G6）

HTTP 401/402/403と既知の設定・次元不一致は共有queueを即pauseする。402を個別失敗として
次の入力へ進めない。408/429/5xx/transportの有限retryは維持し、尽きたjobを一時障害の
連続失敗として数える。汎用400/protocol等の未知障害は自動retry/再分割せず、同じ原因の
terminal job失敗が設定回数連続すればpauseする。別原因・正常採用・明示長さsplit・取消しで
該当streakは切れる。retry中のattemptを別jobのterminal失敗として加算しない。

Current `index.common_failures.<family>` values are bounded cause/count JSON objects. Schema v11 startup migration removes prior integer resets and other non-current counters once; runtime never parses a legacy fallback. Current valid counters and saved pauses remain. Success, cancellation, split, and explicit retry delete the counter. Cause uses a fixed transient tag or a hash of kind/status/provider code, never request-specific prose. Count saturates at the configured threshold.

If a worker cannot adopt a result because current metadata is malformed, it preserves the provider failure on the claimed input, marks the claim failed, and records an existing family/profile pause reason. Workers stop claiming until explicit retry. This is an uncertain adoption, not a successful embedding or a provider error; the original transaction rolls back. Status exposes the diagnostic and manual retry remains explicit. If storage also rejects recording the pause, no durable diagnostic can be promised; the service cancels its workers rather than spending on more inputs. No new queue, worker, table, or automatic paid repair is introduced.

## G2: raw期限と保存済み会話本文の寿命

原仕様の「会話本文の自動期限削除なし」に従い、raw retentionをtool_resultの検索全体へ適用しない。
旧設計の全body無効化を上記のbody別規則へ変更する。削除の実装は既存helperを共有し、
raw限定の選別を追加する。新表/worker/公開操作は作らず、旧witnessのbody別照合で永続互換を保つ。
raw-onlyかつcanonicalが空のsourceは、保存内容に索引対象がないと報告し、回収済みrawの復元は約束しない。
既に旧版が消したcanonical vectorを勝手に有料再構築しない。必要なら既存preview/backfillを明示実行する。
採用後の回帰では、実raw GC・可視引用/添付パスの残存検索・legacy witness・inflight raw結果の非復活・
全space/rollback・encoding変更・明示原文削除とcoverage/previewの一致を確認する。

## split_charsを含むidentityと旧入力互換（G5）

Historical compatibility behavior is superseded by Issue #33. Only current profile keys and current splitter metadata are accepted. Do not look up a second legacy key, process legacy bootstrap/fences, preserve old-splitter automatic selection, or repair missing query identity during observation. Existing explicit scope authorization and paid-work boundaries remain.

## 論理容量上限とsemantic閲覧（G10）

論理保存量の上限はsource/vector/jobを増やす索引処理を止める。既存spaceを読む明示semantic
検索は、query embeddingを1回生成して保存済みvectorを照会できる。検索からindex.pauseを
作成/解除/上書きせず、容量で停止した索引workerを再開しない。既存の再開は容量回復後の明示retry。
query embeddingの外部費用は閲覧でも発生し得るが、過去資料の再索引や追加source許可には転用しない。
spaceなしでは従来どおり空結果でAPIを呼ばない。新表/worker/公開操作/設定は追加しない。

これは物理disk fullやoperation receipt別予算を無効にするものではない。CLIの読取り結果はreceiptを作らず、同IDでも再観測する。実HTTP/DB失敗・不正filter・取得不能sourceは既存の分類で返す。

## pause/inactiveの遅延応答と明示retry（G8）

source witness・claim・groupが現行でも、HTTP中にspaceがpaused/candidate_paused/inactiveへ
変わった結果はvector採用しない。この場合はsource失効と区別し、既存failed状態と
space_unavailable理由を保存して固定済みinput/Workを保持する。pendingへ自動復帰させず、
同spaceを明示selectし必要ならretryした場合だけ再送する。APIは既に課金済みの可能性があり、
再試行時の重複課金可能性を理由に示す。別spaceへの暗黙再投入、pause解除、新worker/stateはない。

同じ検査をbootstrapから既存spaceへ採用する境界にも適用し、candidateの旧active保護と
全片完了後の一括切替は維持する。source削除・witness変更・古いgroup/claimの失効は従来どおり
入力を破棄し、古い本文の再生成/復活をしない。space都合のfailedは保存済みinputがあるため
UNIQUE dedupと衝突して復旧不能にならず、正常retryも同じjob/spaceを使う。

## 設定モデル変更と明示rebuildの区別（G9）

「モデル切替・旧索引復帰の埋め戻しはユーザーが開始」と「非アクティブ索引には新規追加しない」という採用契約を維持する。設定変更だけでは索引作成も過去分投入も行わない。異なる接続/model/分割policyで許可された新規source（または明示backfill）の最初のembedding成功時に、そのidentityのspaceをactiveとして選び、以前のspaceをinactiveで保存する。最初の要求が失敗した場合は旧spaceの選択とvectorを維持する。検索範囲は選択spaceに限るため、新しいspaceでは旧モデルでのみ索引化された履歴は検索に含まれない。モデル間vectorの混在、過去分の自動再課金、旧索引の自動削除はしない。旧spaceは対応設定へ戻して明示selectすれば再利用できる。

これは全履歴の置換を要求するrebuildとは別の操作である。明示rebuildは従来どおりcandidateを背景で作り、許可された対象の全成功後に切り替え、失敗時は旧索引を維持する。設定変更を暗黙のrebuildへ変える提案は、未許可の過去分費用または候補の恒久滞留を招くため採らない。空間単位の選択と不足分の明示preview/backfill/rebuildという既存操作で扱い、新たなspace状態や自動移行workerは追加しない。

## 索引moduleの責務（R2-1/R2-3）

公開入口は従来の `index::IndexService` / `index::split_text` / `index::delete_source_vectors_tx` などを維持し、通常のRust private moduleへ分割する。`index/mod.rs` は設定・reader・DBを使う許可投入とworker調停、`split` はDB/API非依存のUnicode分割、`coverage` は現在本文と完了区間の読取り照合、`source` は選択対象取得・有限blocking parse・canonical本文抽出、`store` は既存metadata/space/witnessとvector保存・削除、`search` は明示queryと適格集合の検索、`queue` は許可済み固定入力のjob登録を担当する。既存の `claim` / `failure` / `identity` はそれぞれ取得/失敗採用/設定互換性を保持する。RAG storeとは統合しない。

新しいworker・永続型・schema・repository層は追加しない。serviceの依存は実際の調停責務に必要なまま保持する。採用transactionの順序、固定入力/witness再検査、取消後の結果不採用、候補切替barrier、原文/raw寿命、明示的な課金許可の境界を変えず、公開CLI/APIと保存keyは変更しない。

R2-3では既存DB job内の成功採用を `EmbeddingAdoption::adopt_embedding_tx`、失敗採用を `adopt_failure_tx`、起動回復・明示retry/space選択・status/次回時刻の取得も名前付き関数へ分ける。成功・失敗採用と明示retry/space選択ではcallerがtransactionをcommitし、採用helperの既存の早期正常returnでもcommit、不採用errorではrollbackする。起動回復/source claimは従来の各helper内transactionを保持し、起動回復の二つのDB jobとworkerのwake/timeout順序も維持する。`coverage::classify_source` は準備済みsource/jobの純粋な分類だけを行う。

新しい内部型は、DB jobが既存の成功API結果と固定許可を所有し採用methodを持つ `EmbeddingAdoption`（failure key/claim/embedding identity/dimension/vectorの5 field; no legacy adoption flag）と、既存pin付きreaderとcarry/Unicode offset/split_charsを保持する `RawSegments`（4 field）だけである。後者でpreview/投入のpage分割を共有し、投入時だけの既存watermark検査と最大128 jobのDB batchを残す。`Work::pending` は既存のpending初期値を共有する。新しい永続state/queue/cache/workerではない。

公開前のmodule整理時点の記録値（現sourceの再測定ではない）: index入口は2,410行から1,058行、全index moduleは3,028行から3,957行、非空白文字数は92,019から99,016、200文字超の行は46から11。残存する長行はSQL/固定説明文のliteralで、statement-dense closureは分解する。移動のみの先行commitは全体+57行（private moduleのimport/公開path維持と移動signatureのrustfmt展開）で、原提案の+40行目安を17行超える。後続増分は名前付き関数・通常の整形・所有境界の明示であり、行数を目標にminifyしない。`process_key` は191行から127行へ整理し、IndexServiceの9 fieldは変更しない。成功採用DB closureのcaptureは9から1（所有command）、失敗採用は13から7（設定の共有参照を保持）である。公開Rust path・引数は維持する（この段落の規模・field数は整理時点の記録であり、Issue #33で削除した互換処理を保持する根拠にはしない）。これらの旧数値を現sourceの規模・性能の保証として使わない。現在の責務は上記moduleの [実装](../../src/index/mod.rs) と [固定key試験](../../src/index/keys.rs)、動作境界は [index統合試験](../../tests/index.rs)、実行した版と結果は [検証状況](../validation/acceptance.md) で確認する。

## 永続metadata keyの所有（R2-2）

`IndexKeys` は既存profile identityを借用してindexのpoint key・scan pattern・splitter値を生成する内部型とする。新IDや保存版ではない。compactionのpending/inflight/result/summary cacheはcompaction側の `CompactionKeys`、wait.pendingはwait側の `WaitKeys` が生成する。既存実装のkey spellingsとWork/AutoAdmission JSONをgoldenへ固定してから呼出しを移す。公開型・保存文字列・既存SQLのpredicate/順序/件数上限は変えず、SQLのpartial-index条件や範囲走査literalを汎用scanへ置換しない。

JSONの読書きは既存index store内の小さい型付き関数へ集約する。厳格decodeの失敗分類を維持し、current failure counter/claim検証/compaction cacheの意図的な不正JSON無視はcaller側で区別する。既存UPDATEをupsertへ変えず、行ごとのdecode/書込み順序とtransaction境界を保つ。汎用repository・新table・cache・workerは追加しない。

3種類のkey builderはそれぞれprofile/thread/operationを借用する1 fieldだけで、namespace生成method以外の状態を持たない。`IndexKeys` の外部利用は同crateのThread削除時observed keyにも必要なためcrate-privateとし、公開Rust APIへは出さない。index内部10 helperのprofile用 `&str` は `IndexKeys` に置き換える。async DB投入に必要な所有Stringと、既存app_meta全体のpoint key用 `&str` は別責務として残す。

index storeの `decode_json` / `encode_json` / `json_meta` / `set_json_meta` / `update_json_meta` は既存JSON表現とエラー分類を共有する小さい関数だけで、record走査の先読み/全件事前parseは行わない。既存UPDATE statementの重複10箇所を1つに集約するが、消えた入力を新規INSERTしない。compactorのCachedPartは読取りとencodeを所有型へ置き、既存のcache miss許容とraw監査保持を維持する。大きいraw-responseのencodeは従来どおりDB job外で行う。API固有エラー文言を持つauthorization/context解析はその境界を残す。

## Index/RAGの共有境界（R2-4）

履歴のembedding identityはcurrent profile-key validationを共有する。独立RAG資料storeの `rag-unicode-2000-v1` identity・固定split2000は履歴indexの可変split_charsとは別契約であり統合しない。保存hashと課金許可範囲を維持し、明示検索にもcurrent profileと必須query markerの検証を適用する。

`SpaceDescriptor` はstandalone RAGとのcrate内の型付きsnapshot（id/profile/model/dimension/splitter/table）。Current descriptor fields and the required query-profile marker are compared as one bounded snapshot; state/lifetime ownership is not shared.`history::RawWitness` は保存済み4項目の同一性とterminal/retention照合だけを共有する。indexのlive source観測とRAGのterminal限定選択は別query predicateのまま。encoding変更のexecution→複数node逆引きは別cardinalityのため残す。既存reference内の未知field/rangeは引き続きJSONで保持し、厳しい全体再deserializeを導入しない。Canonical body eligibility does not depend on raw-retention metadata; raw witness checks apply only to tool_raw.

`ModelError` は認証/credit・設定・入力長・一時障害・429の独立した事実を共有し、永続indexのretry/pauseとRAG呼出し内のstopは分離する。402を含む認証/credit即停止、indexの429再試行、RAGの429で後続要求停止は既存通り。sqlite-vec dimension上限はstorageの定数、RAG内hit順位は同一comparatorへ整理し、indexの距離昇順とは統合しない。

RAGのProviderBreakerは1回の明示index/searchだけで寿命を終え、永続indexと共有しない。まず重複する失敗加算/即停止判定をprivate helperへ移す。store/API/SQL順序、許可件数、既存成功時reset位置はこの構造変更では維持する。

## 固定request identityの所有（R2-7）

successful embeddingのDB採用に渡すEmbeddingIdentityは既存key/profile/modelを所有し、splitterは既存IndexKeysから導出する。従来のadoption keyと(profile,model,splitter)重複をなくすだけで、claim、witness、group、space compatibility、vector検証、adopt、failure resetの順序は変えない。ensure_spaceはidentity/group/dimension/activationだけを受け取り、新しいidentityや許可をDB内で推測しない。G5の旧key/新key照合、candidate/active切替、保存hashは不変。


## 永続stateのenum採用範囲（R2-8監査）

63種類の状態文字列を一律にstrict enumへ置換する案は採用しない。GCの閉じたStageと既存protocol enumは再利用し、GC表示名もStageに集める。Work.stateはserde defaultが空文字で、旧入力・未知文字列をdecode/再encodeし、既存claim/recovery側で実行可否を判定する契約がある。strict enumのdefaultをpendingへ変更すると未許可入力を起動し、unknown variant拒否は既存管理/回復を新たに失敗させる。unknown保持enumを全永続列へ増やす便益も現状では証明できない。既存byte/default/unknown roundtripとGCの未知stage→mark再構築を維持する。

再評価条件は、個別domainで状態分岐の重複または不正遷移が実証され、未知値/旧default/停止時の保存と再開方針を含む互換codecを同時に検証できること。公開schema/配布migrationを伴わない型変更だけで未知値を消さない。

## CLIの過去分処理と費用確認（G3）

既存TUIのpreview→確認を維持し、CLIには既存IndexService::previewを公開する `index preview` を追加する。引数は既存IndexArgs（scope/Thread/source/space）で、backfillと同じ明示対象選択を要する。返却は既存FeePreviewのsources/segments/characters/already_indexed/unavailableで、embedding要求・job投入・candidate作成・過去分許可を起こさない。元本文/rawの読取りと既存一時pinは許容し、DB全体に一切書かないという保証はしない。価格・token数・最終課金額を捏造しない。

CLIのbackfill/rebuildは、対象を明示する非対話の既存契約を保つ。先行previewや新しい確認tokenを必須にして既存自動化を壊す案、同ユーザーshellの人間／モデルを新しい権限機構で識別する案は採らない。CLI helpとモデル向けガイドで、previewは無課金の候補確認、backfill/rebuildは費用を伴う受付であることと、利用者から依頼されていない全履歴処理をモデルが自発的に始めないことを示す。利用者が依頼した範囲の確認・実行は同じCLIで行える。

previewは長期snapshot/対象予約/費用承認の証明ではなく、その後の履歴追加・GC・設定変更で投入対象や量は変わり得る。機械的な承認の検証は今回のコアの責務に加えない。追加Core公開variantは既存の候補読取りへの薄い入口1個だけで、同じ有限runtime/DB/file workerと一時pin寿命を使い、観測のreceiptは作らず、表・event・worker・依存・常駐状態は追加しない。実装と検証ではpreview前後のjob/許可/candidateとHTTP要求数を照合し、その後の明示backfillが選択範囲だけを受付することを確認する。

## 旧splitterからの更新で検索選択を保つ（PR2-2）

Historical old-splitter selection preservation is superseded by Issue #33. Unsupported legacy profiles fail explicitly; no compatibility candidate, fallback key, or automatic rebinding is created.

## query接続identityと分割policyの分離（PR2-14）

query embeddingは選択spaceの接続名/endpoint/model/次元を使い、索引作成時のsplit_charsをquery互換条件にしない。新spaceの作成transactionで、接続名/endpoint/model/既存algorithmから得た独立fingerprintを `app_meta/index.query_profile.<space-id>` に保存する。各spaceに64文字hash一件、資格情報・URL本文・原文を含めない。既存index.*容量計上対象で、spaceと同じ寿命。spaceを削除する機構を追加する場合は同transactionで回収し、source/raw削除だけでは共有spaceのmarkerを消さない。新table/worker/queueは加えない。

Core and standalone RAG require a saved query-profile marker and verify connection/endpoint/model before HTTP. A split-policy change alone does not reject a compatible query. A missing, malformed, or mismatched marker fails explicitly (`InvalidState` in the owner, with explicit reindexing guidance); readers and selection do not infer or repair it, and no embedding is sent for that rejection.

Explicit reindexing is a separate authorized, potentially paid operation. Missing metadata never authorizes automatic reconstruction, query rebinding, or historical backfill.

RAGの内部SpaceDescriptorは従来6項目にrequired query_profile_keyを加え、query前後でmarkerを含むsnapshot一致を検査する。公開CLI/wireや保存済みspace/jobのhashは変更しない。

## 可視性CTEのコスト観測（PR2-14）

検索時の可視性照合は一query内でnodeごとにcacheするが、異なるsourceが共有する置換の逆向き経路は繰返し辿る。従ってchunk件数だけでなく、distinct source数×到達可能consumer経路の大きさがコストへ影響する。source reverse indexとUNIONによるcycle終了は維持するが、深い共有DAG全体に対する定数時間やlatency上限は保証しない。

実装内の診断試験は、実DbWorker/bundled SQLiteで256個の削除所有sourceを深さ0/8/64の共有summary列へ繋ぎ、live rootあり/削除後の全source判定を実際のsearchable関数で計時する。これはprovider時間・KNN・本文decodeを含まない可視性照合だけの測定であり、全検索latencyや最大規模の合格を意味しない。時間を固定hardgateにせず、計測値を検証記録へ残す。巨大/複雑な実DAGの問題が実証された場合は、root判定の共有化を既存read job内で検討し、永続可視cacheや新workerの導入を先取りしない。

## HTTP停止期限と検索の取消し（Issue #8）

Embeddingの全要求は接続設定 `request_timeout_ms`（既定60,000ms）のHTTP全体期限に従う。期限超過の `ModelError.kind=timeout` は一時障害として既存の有限retryと連続失敗pauseへ渡す。期限切れだけで即時無制限再送せず、既定の許可済入力・attempt・遅延を維持する。

履歴検索のquery embeddingも同じ期限を使い、CLI切断tokenとowner停止tokenで待機中HTTPを中止する。直接API呼出しの `search_filtered` もowner停止に従い、明示caller tokenを必要とする経路は `search_filtered_cancellable` を使う。検索の取消しはindex workerの許可・pause・jobを変更せず、取消し後の再検索は外部課金が再発し得る。DB workerに既に投入した短い照会の巻戻しや、provider側の処理/課金取消しは保証しない。新worker・queue・表を加えない。

## Failure-adoption coverage and local storage-stop diagnosis (#33 review)

The process_one error boundary covers all Err results, not only provider-failure
adoption: capacity_allowed database errors, drain_authorization/decode/write
errors, claim database errors, and missing connection/model errors after claim
also acquire the existing durable worker-error pause. A normal StorageLimit
capacity pause remains its existing successful branch. Preserve a richer pause
already saved by failure adoption rather than replacing its provider evidence.

If the fallback diagnostic pause cannot be saved, stop the existing index
workers and reject query embedding through their existing cancellation token.
One once-initialized pair of typed error codes owned by the existing IndexService
records this storage-diagnostic stop rather than owner shutdown. Search then says index
workers stopped after a storage failure and advises repairing storage and
restarting the owner. If the DB is still readable, status projects paused=true
and this in-memory diagnosis alongside any preserved durable reason; the local
stop is not presented as a durable pause. Normal owner shutdown keeps its
existing owner-stopped message. The bounded record has exactly the service
lifetime and is cleared only by constructing a new service, with no
table, worker, queue, state machine or retry service added.


## Bounded local index-stop evidence (#33 follow-up)

The service-lifetime storage-diagnostic stop uses a once-initialized pair of
ErrorCode values: processing failure and failure to persist its pause. It
replaces the previous boolean, needs no writer/queue/table, retains no raw
provider/SQL message and is published before cancellation. Search includes
those typed codes in its existing error details. Readable status marks paused
and appends the bounded local diagnosis while preserving any existing durable
pause reason verbatim; do not overwrite capacity/provider evidence. Constructing
a new service is still the only reset. This is diagnostic memory, not a new
persistent state or recovery engine.
