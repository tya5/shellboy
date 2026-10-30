# SQLite migration と専用worker

## Current-only contract (Issue #33, 0.2.0)

Current conversation schema is v12. Migration 12 extends the existing logical-history byte index to include bounded Revisit metadata; it adds no state table. Fresh databases are created directly from `src/storage/current.sql`, with the complete current ledger. Existing older-schema upgrades run only through the isolated startup migration runner; ordinary runtime readers require the current schema. The historic v6 receipt-accounting repair and attachment-origin reconstruction are not runtime compatibility mechanisms.

Before 1.0.0, release-specific backward-compatibility paths are not part of the contract. The general schema migration runner, `user_version`/ledger validation, and protocol-version checks remain. Earlier sections describing an old-format fallback, deprecated alias, metadata repair, or compatibility shim are historical and superseded by this section; they must not be used to justify a runtime fallback.

採用設計。履歴保存・所有lock・DB worker・migration・GCの確定境界を示す。初期の論理DDLは配布migrationではなく、現行schemaは `migrations/` を正本とする。初期設計資料は非同梱。

初回migrationを `migrations/0001_initial.sql` とし、33 core表を維持し、順序付き経路の `thread_path` とmigration ledgerを加える。配布版は論理候補に次の補強を加える。候補には確定契約を表せない箇所があるため、この差分を先に採用する。

- `schema_migration` ledger と `PRAGMA user_version=1` を同transactionで確定する。未知の将来版・版なしの既存表・ledger不一致は変更せず拒否する。空DBだけ初期化する。失敗時にDBを削除しない。
- Threadの採用経路は `thread_path(thread_id, ordinal, node_id)` に順序付きnode IDとして保存し、cursorだけで分岐を推定しない。`selection_revision` と `profile_revision` は準備・圧縮のCASに使い、元の `revision` は一般状態revisionとして残す。FKと順序uniqueに加え、アプリtransactionで隣接edgeとordinal連続性を検査する。
- `app_meta.root_revision` はGC root CAS用の全体revision。`turn.recovery_reason_json` は不確実状態の復旧理由を保持する。
- `receipt` は `generation`、`updated_at`、`hold_reason` を持ち、公開状態accepted/held/applied/cancelledで受理・保留・適用・送信前取消を表せる。内部のcancelled_before_requestはstate=cancelled、hold_reason=cancelled_before_requestとして保存する。保存済みRun/Turn割当は一度決まった後に変更できない。`operation_receipt` は現在instanceの冪等キーを保存する。state/世代/byte数/ordinal/JSONはSQLでも可能な範囲で検査する。
- run所有辺は挿入・更新双方で循環を拒否する。会話edgeも挿入・更新双方で到達性を検査する。生存owner・Turn割当・選択経路・同期wait辺との複合循環などはControlの同transaction検査を要する。
- 候補の不足index（Thread一覧更新順、原文保持時刻、managed/receipt回収）を補う。動的vec0表はIndexSpace登録時に固定sqlite-vec版で作り、33表に数えない。vecの外部キーcascadeは仮定しない。

`DbWorker::open` は専用OS threadで接続を作る。SQLite呼出し、migration、checkpoint、接続closeはそのthreadだけで行う。Tokio bounded mpscでclosureを順次配送し、oneshotで型付き結果を返す。`call` のキュー空き待ちはasyncなのでUI threadに置かずControl/taskから使う。`try_call` は即時受付または満杯を返す。個々のclosureは短いDB作業だけに限定し、ネットワーク・ファイル読取り・子待機を入れない。

接続前に固定sqlite-vec 0.1.9をauto extensionへ一度登録し、毎接続で実版を照合する。版確認は書込みPRAGMAより先に行い、未知DBをWALへ変換しない。許可DBにはWAL/FULL/FKを設定して実効値を読み戻す。新DB作成以外はmigration・版確認失敗でファイルを消さない。

`shutdown` は受付を閉じ、既受付のDB要求を順に処理してcheckpoint/closeまで確認する。OS threadのjoinはTokio blocking taskで行う。呼出し元はshutdown完了を待ってからowner file lockを解放する。 OwnerLockはDataOwnerと実DB writerのArc共有で保持し、最後の所有者のDropで明示unlockしてからFileを閉じる。Unixのfork→exec前やdupによる同一open-file-descriptionの別FDが残っても、正当owner終了後の排他を延長させない。writerがArcを保持する間はunlockしない。これは終了後の複製FD境界の補強であり、並列runtime試験の再取得失敗原因をforkと断定するものではない。handleのDropだけを正常終了の保証には使わない。channel消失でもworkerは接続を閉じる。panicしたDB closureはその要求を失敗にし、transactionが開いたままならrollbackして次要求へ進む。

検証は `tests/storage.rs`。配布migrationの再open/ledger/FK/integrity、durable receiptと同ID拒否・rollback、所有/DAG循環、世代CAS、未知DBバイト不変、固定vec insert/delete rollback、workerキューとshutdown順序を対象とする。SQLite FULLはOS/媒体のflush保証を拡張しない。電断・3 OS実機ゲートは別検証であり、この試験だけで完了扱いしない。

2026-10-24 macOS / Rust 1.99.1 で `cargo test --test storage` の8件が成功。`cargo clippy --test storage --lib -- -D warnings` も成功（その後の変更は数値CHECK補強とshutdown観測取消テスト追加）。未知schemaのmain DB byte不変、接続再open、実vec rollbackを確認した。実媒体電断・Windows/Linux・長時間負荷は未実施。

schema v2 (`0002_gc_indexes.sql`) はreceipt→message、replacement→output、tool_result→node、output_item→nodeの逆参照indexを追加する。v1の既存データを保ったまま短いmigration transactionで適用し、user_versionとledgerをともに更新する。未知の版・不一致ledgerは適用前に拒否する。GCの参照探索で全table scanを繰り返さないための追加であり、本文形式は変更しない。v1→v2保存試験を追加。

### Schema v3

`0003_context_indexes`は未処理のcontext_changed eventだけの部分indexとThread内Turn時系列indexを追加する。正常completedの採用時にcontext eventの`target_run_id`を消費したRunへ設定し、未処理indexから外す。通知原文・deliveryは残す。通信失敗ではNULLのままなので次の安全境界で再配送できる。v1/v2から保存データを保った段階migrationを適用する。


`0004_attachment_provenance` はThread別のattachment_delivery台帳と出典indexを追加する。確定attachment_refはINSERT/UPDATE triggerでNULL sourceを禁止し、既知sourceに順序付きattachment_ref要素を補完する。不明な旧出典には不確実性を明記した独立sourceを作り、各Threadで安全境界の採用を待つ。移行は単一transactionで行い、正常終了後のreopenで再生成しない。論理DDLのNOT NULLをSQLite既存表の強制再構築で実現する代わりに同等の書込み制約を使う。詳細は [添付の所属](attachment-provenance.md)。

## DB workerを越える操作エラー

採用是正（R1-1、2026-10-25）: `StorageError::Operation(Box<ApiError>)` はDB closure内で生じた
操作エラーをcode/message/hint/detailsのまま運ぶ。文字列へ変換してworker故障にしない。
共通の `StorageError::into_api_error` はこの値を返し、queue満杯・受付閉鎖・入力状態不一致を
`invalid_state` として扱う。SQLite/worker故障・panic・未commit transactionは `save_failed` を維持する。
対象不在は各queryの `optional()` 境界で判定し、NoRowsを全体で特定の対象エラーへ変換しない。
既存のConfigurationによるdomain用途は別途型付きへ置き換え、起動設定と状態競合を混同しない。
操作エラーでclosureが戻る場合も開いたtransactionはrollbackし、workerは後続操作を受けられる。
これは新たな永続型・schema・公開wireの追加ではなく、既存の操作結果を失わない内部transportである。

### Schema v5（P3）

`0005_origin_credentials` は既存receipt/runtime intent/deliveryのoriginまたはsender JSONから既知の `token` fieldを除き、未完了wait metadataの `caller.token` も除く。識別子、operation fingerprint、保存結果、履歴payloadとshell原文は変更しない。JSON形式が壊れた旧metadataは保持して起動時の無関係なJSONエラーを避ける。migrationはDB worker上の単一transactionで適用し、schema version/ledgerを更新する。read-only RAG adapterも同ledgerを照合する。新規の型・表・event・worker・依存は増やさない。

この移行は論理的な資格情報除去であり、旧DB page・WAL・バックアップからの物理的な安全消去は保証しない。旧instanceのtokenは再起動で失効する。任意shell出力を改変しない境界は [Originの認証と保存](protocol.md#originの認証と保存境界p3w6) に従う。

### Schema v6（P2）

`0006_receipt_budget` はoperation_receiptへ非負のreserved_bytes列を追加する。runtime副作用前に同transactionで容量予約を保存し、結果確定で解放する。旧rowは0で、再起動後に旧workerを再実行せず未確定intentを維持する。既存tableを再構築せず、履歴形式・資格情報・原文を変更しない。予約と保持の契約は [operation](operation-observability.md)。 既存quota_counterに実metadata/result量とpending metadata量の導出row2件を置き、INSERT/UPDATE/DELETE trigger3件で同transaction内に更新する。current pendingの予約SUMだけは専用部分indexを使い、各要求で保存result全文を再走査しない。migration時に既存量から初期化し、通常起動で再初期化しない。新規table/workerはなく、counterは一次履歴ではない。

### 読取りsnapshotと終了時checkpoint（C9）

終了時のTRUNCATE checkpointは有限のbusy timeout内で試す。外部read-only接続が古いsnapshotを保持している場合、busy statusは未回収のWALを意味し、保存済みcommitの喪失とは扱わない。writerを正常closeし、WALを削除せずSQLiteの回収へ任せる。checkpointの実エラーとclose失敗は引き続き保存障害として返す。根拠は[SQLite checkpoint API](https://www.sqlite.org/c3ref/wal_checkpoint_v2.html)。

### Idle checkpointの頻度（C10.1）

PASSIVE checkpointは専用DB workerのqueueが空の境界でのみ試し、前回の試行から
最低1秒を空ける。読み取りだけで繰り返さず、接続の`total_changes`を安価な書込み活動の
指標とする。これはdirty page数やcommit数ではないため、rollback後などに余分な試行を
一度行うことは許容する。checkpoint結果の全frame回収を確認できた場合だけ活動の基準を
進める。古いreader等による部分回収・実エラーでは未回収扱いを残すが、再試行も同じ
最小間隔を守る。通常のcommit/FULL同期、shutdown時のTRUNCATE/closeは変更しない。

1秒は小さなDB要求ごとのSQLite呼出しをまとめる内部の最小間隔であり、永続化の猶予や
周期timerではない。jobが来ない間はworkerを起こさず、未回収WALは次のidle境界または
shutdownまで残り得る。連続してqueueが空にならない場合・一度の大きな書込みでは、
既定のSQLite auto-checkpoint（1000 page）を維持してWAL回収の機会を残す。
新たなworker、公開設定、schema、永続状態は追加しない。これはC10.1の頻度是正であり、
V7の性能受入そのものを示すものではない。

### Schema v7（G4/G11）

`0007_usage_and_source_indexes`はlogical bytes集計用scalar index13本とbootstrap/autoの
node照合用partial expression index2本だけを追加する。新table/column/trigger/workerはない。
既存本文・payload_bytes・quota_counterを変更せず、旧DBの実本文からSQLiteが一度構築する。
ledger/user_versionと同transactionで確定し、失敗時は既存データとschemaをrollbackする。
writerとread-only RAGはmigration名/版の同じ内部constantを照合し、RAGへ別ledgerを複製しない。
通常起動は再構築しない。容量の算入境界と残るO(N)は[GC設計](gc.md#容量集計の本文読取り除去g4)、
不正JSON/他prefixの保全は[Index設計](index.md#source削除のmetadata照合g11)に従う。

## 開発途中v6のforward互換とdowngrade（PR1-13）

Historical: the development-v6 accounting workaround is superseded by Issue #33. Fresh databases use the complete current schema; do not recreate or infer partially developed schemas during normal open. Keep only the general migration runner and strict schema/ledger checks. Downgrade and release-specific compatibility are not promised.

## 共通のbyte digest（R2-8）

SHA-256 lowercase hexの共通化は既存の32byte digestを64文字へ変換する処理だけを共有する。各domainのserde順序/UTF-8 bytes/区切り/範囲/版fingerprintは呼出し側に残す。incremental file hashやcursor fingerprintの入力順序を一律に変えず、保存済みbody hash・receipt fingerprint・引用hash・embedding identity・docs cursorのbytesを維持する。標準UTF境界関数への置換もpage開始検証と無進捗拒否を維持し、上限や未知cursorを緩めない。


## Turn出典探索のcovering index（PR2-15、schema v9）

`0009_compaction_turn_indexes`は`node_turn ON node(turn_id,id)`と
`receipt_turn_source ON receipt(turn_id,target_thread_id,message_node_id)`の2本だけを追加する。
compaction seedのTurn所属node/legacy割当receiptを狭い範囲でseekし、無関係なreplacementの全走査を
避けるための導出indexである。node/receipt全row分の追加diskと書込み時のindex保守費用を負うが、
新しい一次情報・table・column・trigger・workerは作らない。利益/走査の限界は[compaction](compaction.md)参照。

schema v8以下からの初回upgradeで一度構築し、失敗時は両indexと新ledger/user_versionを同transactionで
rollbackする。既存migrationを改変せず、node本文/source/receipt割当/予約を保存したまま前進する。
旧binaryは従来どおり対応上限を超えるschemaを拒否する。read-only RAGも既存MIGRATION_NAMESを共用し、
別の版台帳や自動downgradeは追加しない。

operation receiptの終端結果には通常のSavedOutcome objectのほか、回収済み本文を示すJSON scalar `0`を許す。NULLのpendingとは区別し、ID/hash/origin/元terminal stateを同じrowに保持する。両replay入口でoperation_expiredへ投影し、同IDの変更を再実行しない。既存quota triggerで1byte markerとmetadataを計上する。新DDLは不要で、v0.1.2のschema9前進後だけ新形式を書き込む。旧binaryへ戻す手順と回収規則は [operation設計](operation-observability.md) を参照する。

## Hot query index（Issue #26、schema v10）

`0010_hot_query_indexes`でactive shellのowner/state、Threadごとの最新Run、terminal shellの時刻順の3本を追加する。計画・書込み費用と採用/見送りの根拠は[経路とhot query設計](history-adoption-performance.md)。既存rowは保持し、索引・ledger・user_versionを同一transactionで前進する。旧binaryの対応上限を超えるため自動downgradeはせず、戻す場合は更新前backupを別data-dirへ復元する。独立RAGのschema 3とは異なる版番号である。

## Schema v11 (Issue #33 review)

Startup-only `0011_current_index_metadata` drops the unused `receipt_turn_source` index and removes non-current `index.common_failures.*` counters. A valid counter must be a JSON object with a string cause and a nonnegative integer count; JSON scalar `0` is not a current counter. This derived reset preserves jobs, source data, saved pauses, and all current counters. It neither repairs missing query-profile identity nor authorizes embedding work. Fresh databases omit the dropped index and include ledger v11. Historic SQL stays in `migrations/`, referenced only by the startup migration module and explicit migration tests; ordinary Core fixtures use the current schema.


## Message path admission (schema13, #53)

Receipt gains constrained path_admission (unadmitted/admitted/unknown), separate
from response acknowledgement and original immutable assignment. Startup-only
migration13 uses saved positive admission evidence and marks other prior facts
unknown; Core reads the current column only. The existing state machine, tables,
worker ownership and payload accounting remain. See [message recovery](message-recovery.md).


## Pending Input And Turn Intent (Schema14, #52)

`0014_turn_interrupt` adds nullable turn.interrupt_requested_at and ordinary
draft indexes for state/Thread/newest-ID reads and submitted source-revision
watermarks. Fresh schema and the complete migration chain describe the same
columns/indexes. No table, durable queue version or new lifecycle state is added.

Existing draft rows hold separate frozen submitting/submitted snapshots and
archived cancellation source references. Submitted metadata binds operation ID,
source draft/revision and accepted message; editable rows remain independent.
Raw draft text and serialized metadata have separate bounded budgets as specified
in [pending messages](pending-messages.md). Existing full-payload byte accounting
and GC ownership remain effective. receipt.result_json stores explicit now and
bounded cancellation/delivery-context facts; original assignments stay immutable.
Historical admission classification remains isolated in migration13.


#60 retains only a bounded draft_submission binding in receipt.result_json after
terminal copy disposal; schema remains14. See pending-messages for the exact
unknown/submitting guards and the existing GC transaction lifecycle. Cancelled
archive references and original message/attachment storage remain unchanged.
