# 保持・到達性GCと容量表示

採用設計。保持と到達性・GC確定境界を具体化する。会話・回答・推論・要約を期限で削除しない。由来となった初期保持仕様と引継ぎ資料は非同梱。

生存Threadの選択pathだけでなく、そのThreadのTurnに属する未採用/過去node、Threadのmessage receipt、採用replacement出力も保持rootとする。削除Thread由来のnodeは、別の生存fork pathまたは有効なreplacement_sourceから必要なものだけ保持する。会話edgeの全outgoing先を保持対象にしない。これによりfork共有prefixの後ろにある削除元専用suffixを誤って残さない。出典はreplacement出力→sourceの関係を再帰展開し、旧summaryを含む多段出典を保護する。

`gc_job` とDB接続内TEMP work tableでroot走査/出典mark/sweepを短いbatchへ分ける。marksetをアプリRAMへ全件常駐させない。接続終了でTEMPが消えたら、保存済みjobはmarkから再開する。job開始時のnode rowid上限で対象を固定し、root revisionが変わった場合は既markを保守的に残す。未markの破壊候補は現在のrootと逆方向replacement出典到達性を同transactionで再検証し、runtime pinも照合する。後発nodeはこのjobの対象へ含めず、後発の削除jobも完了扱いにしない。sweepはnode自体/edge/出典関係を残してpayload tombstone化し、理由と本文revisionを更新する。duplicated output_item本文も同transactionでtombstone化し、全IndexSpaceのvec/chunk/job/入力snippetを索引serviceのhelperで削除する。

復旧待ちの `incomplete_response` は選択pathへ追加しない監査nodeだが、生存ThreadのTurn所属により保持rootになる。このINSERTと同じtransactionで `root_revision` を更新し、Roots走査後の追加も次の破壊batch前に現在到達性の再検証へ反映させる。再送対象への混入や、全nodeを無条件に保持する対処はしない。公開型・表・event・worker・依存の追加は不要。

rawは確定済みshellのpipe原文だけを、既定7日/合計1 GiBの両方で回収する。生存shellと `runtime_pin(resource_kind='shell_output',resource_id=execution ID)` は回収しない。最初のbatchで `app_meta.raw_tombstone.<execution ID>` をdeletingへ保存し、retention generationを増加し、その実行のtool_raw索引だけを全spaceから同transactionで削除し、保存済みの会話引用/添付パスの索引は残す。その後に原文chunkを有限batchで消す。途中crash後もmarkerがあるrawを新たにread/indexしない。完了時markerをdeletedへ進める。metadataと履歴中の既存引用を会話期限削除にすり替えない。

managed画像のdeleting→unlink→deletedとpin/active検査は共有AttachmentServiceへ委ねる。外部ファイルは対象にしない。期限/容量判定でpinned分を無理に削除せず、回収不能bytesを別に返す。

容量は論理live bytesと物理DB/WAL/managed/log bytes、DB再利用可能page bytesを分離する。DELETE直後にOS空き容量が増えると説明せず、自動VACUUMを実行しない（明示コマンドは[末尾の節](#明示的なstorage-vacuum-238)）。物理属性の取得は有限file worker上で実施し、UI/main OS threadから同期stat/走査を呼ばない。logical limit超過は新しい副作用をstorage_limitで保留するための判定として公開し、停止/終端保存/GCは許す。保存済みspaceのsemantic閲覧はquery embeddingだけを生成して許可し、索引pauseを変更しない。索引用の新規embedding停止と閲覧を区別する（[G10](index.md#論理容量上限とsemantic閲覧g10)）。

logical bytesの採用定義は保存本文（nodeと監査outputの重複を含む）、snapshot/draft/attempt/event/receipt本文、pipe原文、managed raw_size、vec float32要素、`app_meta` の `index.*` 入力/preview/bootstrap/auto payloadの合計。SQLiteのB-tree/索引page overheadとは別の値とする。`reclaimable_bytes` はrawから確実に回収できる下限であり、会話GCやmanaged候補の未確定見積りを加算しない。詳細応答の `reclaimable_is_lower_bound` をtrueとし、quotaはこの見積りではなくlogical live合計で判定する。索引workerも同じ `require_capacity_tx` を利用する。

raw候補metadataは128実行ずつ取得し、chunkは128行かつ1 MiB以内のtransactionに分ける（単独の既存oversize chunkだけは進捗のため許容）。root/出典/sweepは各128件。mark後のruntime pin追加を含め、各破壊候補を現在のpinを含むrootと逆方向の再帰出典で同transaction内に再検証する。sweep cursorは戻さず、継続するpin追加で進捗を失わない。root revisionだけでは捉えられない一時読取り保護をこの検査で補う。

確定境界は [GC実装](../../src/gc/history.rs) と [到達性・容量・競合回帰](../../tests/gc.rs) を参照する。実行版・環境・測定と未実施は [検証状況](../validation/acceptance.md) に記録する。全量データでのGC中入力遅延・physical filesystemの回収速度は局所回帰から達成としない。

成功済み要約partの `summary.part.*` 本文もlogical history bytesへ計上する。cacheは保持rootを追加しない。部分成功は参照元本文が有効な間は再利用用に保存し、明示削除GC後に一つでもsourceがtombstone/不存在になれば、そのexact入力版へ一致できないcacheを128件ずつ回収する。共有prefixが残っていても、一緒に生成された削除branch本文の派生cacheを永久保持しない。

`sources` が無い・空・不正JSONの `summary.part.*` は元をたどれないcacheとして、同じ走査で古いものとして回収する（一度きりの移行は設けない。同じ入力のやり直しで作り直される）。統合段のpartは入力partの`sources`の順序つき和集合を保存するため、元会話の削除で回収される。

要約要求のusage記録 `app_meta summary.usage.*`（#133）もlogical history bytesへ計上する。会話attemptのusageと同じく通算の事実として保持し、回収しない。

未完了waitの `app_meta wait.pending.*` も保存byte数をlogical historyへ加算する。成立・取消しで解放し、前instance分はstartupで履歴metadata nodeへ移すため、以後は通常のnode payload accountingと到達可能性に従う。


## W7の進捗是正

root変更のたびに全markをやり直す方式は、継続書込みでsweepへ進めなくなるため採らない。
この進捗条件は [GC回帰](../../tests/gc.rs) でroot変更・後発node・pin保護と組み合わせて確認する。
過去の10k/100k制御測定は当時の独立node中心の条件であり、現sourceの再測定や巨大出典DAGの最悪時間保証には流用しない。
Threadごとのcounterへ単純分割せず、既存global revisionとTEMP markを使う。
job開始時のnode rowid上限で対象を固定し、後から増えたnodeをそのjobのsweepへ入れない。
revision変更後は既markを保守的に残し、未mark候補は破壊transaction内で現在のrootと逆方向の
replacement出典到達性を再検証する方式を採る。新たなfork/Turn/pin/sourceを除外しない。
旧rootを過剰保持した場合は次のjobで再判定し、安全性を優先する。

root判定SQLを共有し、同じSQLの毎node prepareを避ける局所整理を挙動変更と分ける。
新しい永続表・worker・Thread counterは追加せず、既存cursorにhorizonと再検証要否を記録する。
有効なTEMP sessionとcursorがあるjobはstart_historyの再呼出しでも進捗を保持する。旧cursor（上限記録なし）/TEMP消失は従来どおり安全にresetする。job.root_revisionは開始時の基準として維持し、その時点までの削除jobだけを同時完了できる。Root/Provenance/Sweepの保守的markはjob終了で破棄し、後発の明示削除jobで除去済みrootを再判定する。

## 容量集計の本文読取り除去（G4）

原レビューG4は本文の反復読取りとDB worker占有の是正であり、O(1)集計の追加を要求しない。
容量の定義を変えず、schema v7の狭いexpression/covering indexだけをSUMする。
TEXTは`octet_length`、rawは`length(BLOB)`、managedは既存`raw_size`を使い、明示の
`INDEXED BY`で本文table走査への退行を防ぐ。nodeの既存`payload_bytes`は旧rowと実UTF-8
本文長の一致を保証するDDLがないため、集計では実本文由来のindexを採る。旧rowを補正・改変しない。
vec量は従来どおり保存dimension×4をchunkごとに加算し、vector本文の取得を追加しない。
receipt予算counterの固定metadata overhead/予約量はlogical historyへ流用しない。

`quota_counter`の再利用も検討したが、全tableの増減・状態変更・削除・rollbackを担当する
数十個のtriggerと初期化責任を増やすため不採用。派生indexはSQLiteが原文のINSERT/UPDATE/
DELETE・tombstoneと同transactionで保守し、rollback/crash/reopenで別cacheとずれない。
集計は残存row数に対してO(N)だが、本文byte数に比例する反復読取りをしない。
index構築は旧DBのmigration時に一度必要で、永続サイズと更新コストも増える。上限は原文row数に
比例するscalar index（本文コピーなし）であり、新しい保持root・worker・表・公開設定は加えない。
index破損はSQLiteのintegrity検査/REINDEXの対象で、通常起動に全本文再集計を追加しない。


G7の検索可視性のため、削除Threadの最終thread_path/thread_replacementは本文回収まで
所有証拠として残す。root判定は従来どおりlive Threadだけなので回収を妨げない。
sweepの既存node batch内でtombstone確定と同transactionに当該nodeの削除Thread pathと
当該出力replacementの削除Thread登録を消す。処理量は当該nodeの所有参照数に依存し、
新しい全履歴cleanup worker/queueやschemaを作らない。生存Threadのpath/登録は変更しない。

## 履歴収集の段分離（R2-8）

history_stepの既存単一write transaction内でroots/provenance/sweepをprivate関数へ分け、破壊的tombstone手順を一箇所にまとめる。TEMP marks/session、BATCH、有限horizon、root revision変更時とlate pinの到達性再検証、cursor進捗、全space vector削除→node tombstone→所有証拠解放→output_item→incomplete attemptの順序は不変。段分離はGC再開状態やworkerを増やさず、Stageの保存済みsnake_case bytesを維持する。


## Index-space collection (#55, #253)

Paid vectors are not deleted automatically. Maintenance and `storage collect --category index` (also `all`) run the same routine, which takes only what holds no paid work: a `discarded` partial candidate (replaced by a newer rebuild, or a cancelled or failed int8 conversion) and an `inactive` space with no vectors. An `inactive` space with vectors, the old space after a rebuild or conversion, stays. `storage collect` no longer counts as an explicit request to delete such a space, because it names none and offers no preview of one. Only `index remove --space ID` deletes it, for one named space, after its dry run. Candidates superseded before this change were saved as `inactive` and now count as retained, so they stay until removed the same way.

Transactions cover at most 32 spaces and yield between batches. Current active pointers, current rebuild candidate pointers, the source of a running conversion (its `index.shrink.<space>` record) and any running embedding job protect a space. Validate generated vector-table identifiers before DDL. Drop vectors and associated chunks/jobs/input/preview/query/request-mode/conversion metadata in the same transaction; rollback retains the whole batch. Report `inactive_index_spaces_collected` separately from reclaimed source vectors. This does not delete conversation originals or promise physical SQLite/WAL shrink. See [index operations](index-operations.md).


## Finished submission copies (#60)

History/all collection uses an existing DB-worker transaction per at-most128
eligible submitted copies, before history marking. Save the small receipt binding
and remove the matching submitted row atomically. Only admitted or known-admission
cancelled input qualifies; never submitting, unknown, editing or archived rows.
No receipt, original node, cancellation archive or attachment is removed by this
step. Report submitted_drafts_collected, not physical SQLite shrink.

## Dead delivery and shell-event retention (#170)

History/All maintenance scans at most 64 delivery and 32 event rowids per
transaction, changing at most 128 rows in total, then yields to other DB jobs.
An in-memory cursor advances on a
successful commit and wraps at the end; restart can repeat scans. A pending
delivery is removed only when its recipient Run is completed, stopped or failed.
The event remains, including undelivered `context_changed` for a later Run.
Late arrivals to a terminal Run are handled by a later pass; live, stopping and
recovery-waiting Runs retain their deliveries. `shell_ended` events are removed
only for terminal Runs, with no pending/unknown delivery and
no retained event-node reference. At most one of their processed deliveries is
removed per candidate per pass; events disappear only after their last delivery.
The derived node-reference and event/delivery-state indexes keep the existence
checks independent of unrelated history. This is structural best-effort cleanup,
not a hard row-count bound: retained references, maintenance backlog and
throughput greater than the scan rate can retain rows indefinitely.
No conversation text, raw shell output or other event kind is removed by this
policy. Existing SQLite pages are reusable but not automatically returned to OS.

## Deleted-Thread message receipts (#197, #194 step 3)

The history-GC requester owns receipt reclamation. Thread deletion first commits
recipient pending-reply terminal transitions and outcome notices in its deletion
transaction, with indexed lookups bounded by the per-recipient 128 cap. Sender
cancellation is deferred to this receipt phase: each of at most 128 scanned rows
is checked by receipt primary key for a deleted model sender, then conditionally
terminalized with cancel_reason=sender_deleted and a recipient cancellation
notice in the same GC transaction before eligibility is checked. No database-wide
pending scan or migration is needed; GC never deletes a still-pending reply. History/all collection then
removes at most 128 eligible receipt rows per background-lane transaction before
node marking, yielding between batches. Deletion, explicit collection, and
startup resumption of pending history jobs are the existing triggers; no timer
or separate unbounded collector is introduced.

A receipt targeting a deleted Thread is eligible only when its reply is not
pending and no live model sender Thread owns the cross-Thread result. A live
sender preserves the original receipt (including terminal reply facts) and its
message/reply bodies as roots until that sender is deleted. External/human
senders have no inbox: after recipient deletion and collection, message get and
new waits may report target_unavailable; deletion is not indefinite archival.
A deleted sender does not remove receipts targeting live recipients: those are
still recipient-owned history. Once both model endpoints are deleted, terminal
receipts are eligible. The GC transaction rechecks live endpoints and terminal
state before deletion. A collection chunk does not remove operation_receipt
idempotency records, whose existing independent retention policy still applies.

## Explicit prune of replacement originals (#235)

`thread prune --before NODE` is the only way to release originals that
compaction keeps. It sets `replacement.state='pruned'` (the column is free text;
no schema change) for the completed replacements this Thread holds whose output
precedes NODE on its path, summaries and spills alike, plus the nested closure
(a source that is itself an adopted output the Thread holds). A replacement held
by another live Thread (a `thread_replacement` row or the output on its path) is
skipped and counted. The same transaction bumps `root_revision` and enqueues an
ordinary history `gc_job`, and the caller marks the same maintenance work thread
delete marks; the command deletes nothing itself and promises no timing.

Reachability changes, all keyed on `state='pruned'`: provenance expansion
(`provenance_step`) and the sweep revalidation and `searchable` consumers walk
skip pruned replacements; root rule 2 (Turn ownership) does not protect a node
that is a source of a pruned replacement the same Thread holds; `searchable`
treats such a node as not retained unless another root holds it. Path, other
Threads' Turns, pins, receipts, attachment deliveries and unpruned consumers keep
their effect, so shared originals survive. A job that already marked the nodes is
conservative; the prune's own job reclaims them. Tombstones record
`pruned_unreachable`. Replacement outputs, edges and `replacement_source` rows
stay. Closure bound: 65,536 replacements or sources, checked while sources are read so an oversized prune fails early with `input_limit`. Messages with a receipt to a live Thread stay roots (rule 3); the result reports them as `kept_receipt_messages`, apart from the candidates. A hidden-marker re-summary also revalidates `state<>'pruned'` in its adoption transaction. Spills
are included in the one rule. See [thread prune study](thread-prune-rewind-branches.md).

## Budget-resumable history GC (#225)

Each history stage persists its cursor with its data changes. Deferred batches
are halved to one; after committed units in the same stage the batch doubles
back up to the original 128, and each new stage or job starts at 128, so
transient pressure does not leave the rest of the pass slow. If a growth step
is itself deferred, the number of commits required before the next growth
doubles (at most 128) and a committed growth resets it, so sustained pressure
settles near the largest size that fits instead of alternating. The batch
size and wait are in-memory only. Provenance uses replacement-rowid and source-ordinal
subcursors. TEMP marks are cleared in bounded reset and cleanup stages; the
finite original job horizon is closed in bounded batches. Each destructive
node decision revalidates current reverse reachability and pins in the same
transaction as the tombstone. An interrupted single-node check or indivisible
tombstone retains the node for this pass and records a bounded count and up to
eight short node IDs, never content. Indivisible receipt cleanup is retained and
counted separately. If a single provenance expansion or reset mark cannot be
processed within budget, the pass conservatively retains all payload nodes;
unremoved TEMP marks are counted and disappear when the connection closes.
Job discovery scans a bounded rowid window and keeps its position in memory;
a worker restart safely restarts discovery. This trades reclamation for safety
and forward progress.

Tombstone and index deletion remain atomic: search filters deleted sources
(`src/index/search.rs`), but retry fee preview reads failed jobs and fixed inputs
without a tombstone gate (`src/index/observations.rs`). Splitting physical index
purge after tombstone would therefore expose stale job observations. A future
change may split it only after all readers enforce the required visibility.

## 明示的なstorage vacuum (#238)

GCはDB内のpageを空けるだけでファイルは縮まない。縮める手段は `shellboy storage vacuum [--dry-run]` のみで、明示したときだけ実行する（自動・incremental・auto_vacuumは採用しない）。`storage check` と同じstandaloneコマンドで、owner.lockを `OwnerLock::acquire` で取得できないとき（owner動作中）は `invalid_state` で拒否する。dry-runも同様。対象は会話DB `shellboy.db` のみ。standalone RAG storeは別ファイル・別lockで、同時に対象にしない。

報告値は推測しない。縮む量の下限 = `freelist_count × page_size`（VACUUMは半端に空いたpageも詰めるので実際にはこれ以上縮み得る。保証される下限であり正確な値ではない）、現在のDB/WALファイルサイズ、`live_bytes = (page_count − freelist_count) × page_size`。必要な空き = 一時コピー `live_bytes`（`temp_store_directory` でdata dir配下の `tmp` に置く。別deviceなら個別に判定）+ WAL `32 + live_pages × (page_size + 24)`（全live pageがフレームヘッダ24 byte付きでWALを通る）+ 現在の `-shm` サイズ。SQLiteの動作からの上限で、実行の実測ではない。`fs2::available_space` と比較し、足りなければ実行を `storage_limit` で拒否してDBに触れない（dry-runは `sufficient_space:false` で報告）。

実行順: WAL checkpoint(TRUNCATE)（owner.lockを持たない読み手（request inspect、RAG履歴read）が読取りtransactionを持つとbusyになるので、結果行を検査し、busyなら変更前に `invalid_state` で拒否）→ VACUUM → checkpoint(TRUNCATE)（読み手のため未完了なら成功とせず `invalid_state`。DBは整合、ファイルは未縮小）→ quick_check。前後のファイルサイズを返す。VACUUMはSQLiteの保証によりatomicで、失敗・中断でも元のDBが残る。新しい表・設定・workerは追加しない。

VACUUMはファイルをその場で書き換えるのでdev/inodeは変わらず、[docs-and-rag](docs-and-rag.md) のhistory参照が束縛するfile identityは保たれる。identityが変わる環境では従来どおり保守的に失効し、再検索が必要になる。node ID・版・本文は変わらない。

VACUUMはINTEGER PRIMARY KEYも索引も持たない表のrowidを振り直す。現行スキーマにはその表はなく、将来の表はそのようなrowidに依存してはならない。
