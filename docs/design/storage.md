# SQLite migration と専用worker

Schema20 adds derived event-retention indexes; it does not rewrite event or
delivery data. See [GC](gc.md#dead-delivery-and-shell-event-retention-170).

## DB job timing (#164)

The DB worker observes each accepted job from queue admission through execution.
If either queue wait (excluding asynchronous admission wait) or execution takes
at least one second, it may enqueue a `db_job_slow` diagnostic containing only
the compiler-provided closure call-site name (at most 160 characters), queue
milliseconds and execution milliseconds. No job arguments, results, SQL or
conversation text are recorded. Closure names are diagnostic identifiers and
may change with the compiler. The worker allows at most ten observations per
fixed 60-second window, and the existing diagnostic queue and rotating log
remain bounded and best-effort. Diagnostics attach after DB initialization;
startup migration and checkpoint work are not jobs. DB shutdown drains jobs
before diagnostics shutdown; diagnostics may still be dropped under pressure.

## DB worker lanes and background step budget (#171, proposed)

Status: design memo awaiting review. Nothing below is implemented yet. The
occupancy-visibility part of #171 (current-job start time and kind readable
from another thread, status-bar display) is a later step and is not covered here.

### Problem

One OS thread runs every SQLite job in FIFO order. One expensive job delays every
later job, and fixing individual queries (#164: `read_next_delay`, the GC
index-source scan) does not stop the next expensive query. Live `db_job_slow`
observations before this change (macOS, one owner, 2026-11-02): run time up to
209,841 ms for `gc.GcService.collect`, 19,177 ms for
`control.db.Store.collect_receipts`, 3,678 ms for `gc.GcService.raw_stats` and
2,076 ms for `index.queue.recover_inflight`. Foreground kinds queued behind them
for up to 209,851 ms (for example `control.driver.adoption.adopt_response`,
`control.driver.shell_output.collect_shell` and `frontend.select_thread`), even
though their own run times were 0-72 ms.

### Guarantee

A foreground job waits for the foreground jobs admitted before it plus at most
one background unit. A background unit is a background job interrupted after at
most `BACKGROUND_STEP_BUDGET` SQLite VM steps, plus one progress-handler
granularity. Steps are the bound. No wall-clock duration is promised. The time a
unit took is recorded as a measured fact.

Foreground jobs are not budgeted. Bounding their cost remains the job of each
query, the existing `db_job_slow` observation, and the presentation read-only
readers.

### Classification rule

A job is *background* only when both of these hold:

1. No user, model, shell, TUI or CLI request is waiting for its result. It is
   started by the owner maintenance loop (receipt collection, GC, managed-file
   retention) or by the index worker loop.
2. Interrupting it and rolling back its open transaction only delays the work.
   Running it again later from committed state gives the same result. Nothing
   outside SQLite (HTTP billing, file unlink, process spawn) depends on that
   transaction committing.

Every other job is *foreground*, including jobs that settle an external effect
which already happened, even when a background loop starts them. An example is
adopting or failing embedding claims after the HTTP request was billed: if that
job were interrupted, the billed result would be discarded, because claims are
never replayed. Such jobs must be bounded by construction (for example
`embedding.batch_max_inputs`) and remain observable through `db_job_slow`.

The lane is chosen by the caller, not by the shared helper. Helpers reached from
both kinds of caller (`IndexService::enqueue`, `load_source`, `enqueue_batch`)
take the lane from their caller. `storage collect` run explicitly by the user
uses the same background path as the maintenance loop. Its existing
`CollectionReport.deferred` flag reports a budget deferral.

### Admission API

- `DbWorker::call` and `try_call` are unchanged and admit to the foreground lane.
- A new async `DbWorker::call_background(op)` admits to the background lane and
  returns `Result<Background<T>, StorageError>`, where
  `enum Background<T> { Done(T), Deferred }`. Deferral is a typed outcome, not a
  `StorageError`. It therefore cannot reach the `save_failed` mapping
  (`into_api_error` -> `is_storage_failure` -> `fail_storage`), and the index
  worker's error branch cannot record it as a pause. Each background call site
  must match `Deferred` explicitly. There is no background `try_call`: every
  background caller is a task that can wait asynchronously for admission.

### Budget mechanism

- Enable the existing `hooks` feature of the pinned `rusqlite` 0.40.2. It adds
  no crate and no version change, and it exposes `progress_handler` and
  `commit_hook`. The dependency feature change will be recorded with its
  validation.
- After `open_connection` succeeds, the worker installs one progress handler
  (granularity `PROGRESS_GRANULARITY` VM steps) and one commit hook, both reading
  a worker-owned budget state. Startup migration and validation run before this
  point, and the idle and shutdown checkpoints run with the budget disabled.
- Before each job, the worker disables the budget for a foreground job. For a
  background job it arms the budget with `BACKGROUND_STEP_BUDGET /
  PROGRESS_GRANULARITY` ticks and clears the `hit` flag. The budget is cumulative
  over the whole job (every statement and every transaction in it), not per
  statement.
- When the ticks run out, the handler sets `hit` and returns true, so SQLite
  returns `SQLITE_INTERRUPT`. `hit` is sticky until the job ends: every later
  handler call also interrupts, and the commit hook turns any later `COMMIT`
  (explicit or autocommit) into a rollback. A closure that catches or maps the
  interrupt error (`map_err(sql_error)`, `unwrap_or`, `.ok()`) therefore still
  cannot commit after the hit. The existing `make_job` wrapper then rolls back
  any transaction that is still open. Because `hit` is set, the reply is
  `Deferred`, whatever the closure returned. Transactions that a multi-transaction
  job committed before the hit stay committed. They are completed batches.
- Not counted, and therefore not bounded by the budget: Rust work between
  statements; fsync during a commit; work inside one virtual-table or
  SQL-function call (for example sqlite-vec KNN in `xFilter`). Background
  closures must keep Rust-side loops proportional to the rows they fetched with a
  `LIMIT`. They must not perform vec KNN. Nested SQL issued by sqlite-vec during
  `xUpdate` or `xDestroy` runs on the same connection, so it is counted. A test
  will confirm this for `DROP TABLE` of a vec0 table, as used by
  `collect_inactive_spaces`.

### Choosing the budget by measurement (no guessed value)

Phase 2 measures, with the bundled SQLite and a file-backed WAL database on a
synthetic large fixture (row counts recorded in the test):

1. The step count of every background kind's *healthy* batch: each closure under
   a test-only tick counter, with representative data volume and current
   indexes.
2. VM steps per millisecond for those kinds on the same machine.

`BACKGROUND_STEP_BUDGET` is the smallest power of two that is at least the
largest healthy-batch step count from (1). If that value is unreasonably large,
the batch that produced it is split instead of raising the budget. The memo will
record the fixture, machine, command, step counts and resulting per-unit times
as facts. A preliminary data point, not the basis for the constant: system
SQLite 3.51.0 through Python's `set_progress_handler`, in memory, on the same
Mac, executed 72,500-106,700 VM steps per ms on scans and correlated subqueries.
`PROGRESS_GRANULARITY` is chosen in the same measurement as the largest power of
two whose overshoot is negligible next to the budget. SQLite's
`sqlite3_progress_handler` documents the granularity only as approximate.

### Two-lane queue

- There are two bounded Tokio mpsc lanes, each with the capacity passed to
  `open_with_capacity` (default 128). No new setting is added. The total bound
  is twice that capacity. The owner is the `DbWorker` admission handle, as
  today. A background flood can no longer fill foreground admission and cause
  `try_call` to return `QueueFull`. Background occupancy is bounded by the
  number of background tasks (one maintenance loop and `indexing.concurrency`
  index workers), each with one outstanding call.
- Selection runs on the DB thread. If only one lane has work, it runs that lane.
  If both have work, it runs the foreground lane unless `FAIRNESS_RUN`
  consecutive foreground jobs have run while a background job was waiting; in
  that case it runs one background job and resets the counter. Under sustained
  foreground load, background still completes at least one unit per
  `FAIRNESS_RUN + 1` jobs, and the guarantee above still holds. This is a policy
  ratio, not an estimate. Proposed value: 8 (see open questions).
- Waiting on both lanes: the DB OS thread blocks on a biased receive over both
  receivers. It uses a current-thread Tokio runtime owned by the DB thread,
  without I/O or time drivers, or the `futures-util` select already in
  dependencies. No new dependency is added, and the UI thread is not involved.
- Shutdown: admission closes for both lanes under the existing lock. The
  `Shutdown` marker is sent on the foreground lane. When the worker sees it, it
  keeps running every already-admitted job in both lanes (with the same lane
  rule) until both are empty, then runs the TRUNCATE checkpoint and close as
  today. Accepted background jobs may return `Deferred` during shutdown. Callers
  already stop on cancellation.
- Idle checkpoint: the "queue empty" condition becomes "both lanes empty".

### Budget diagnostic

A new `db_job_budget` event carries only `kind` (the same compiler-provided
call-site name as `db_job_slow`, at most 160 characters), `budget_steps`,
`queue_ms` and `run_ms`. It records no SQL, arguments, results or conversation
text. A separate fixed window limits it to 10 events per 60 s. It uses the same
best-effort bounded diagnostics queue and rotating log, and it is attached and
shut down like `db_job_slow`. A kind that appears repeatedly identifies a job
that must be split. There is no automatic budget escalation, because escalation
would break the bound.

### Caller behaviour on `Deferred`

| Caller | On `Deferred` |
|---|---|
| maintenance loop `Store::collect_receipts` | treated as 0 collected; retried on the next 60 s pass |
| `GcService::collect` category loops (raw, drafts, summary cache, index sources, inactive spaces, history steps, managed files) | set `report.deferred`, stop that category, continue with the next category, return `Ok` |
| `AttachmentService::collect_managed_observed` | stop marking or unlinking more files, report the remaining work as deferred; already-marked `deleting` rows are resumed by the next pass |
| index worker (`process_one_inner`, `process_key` claim, `drain_authorization`, `capacity_allowed`, worker-side `enqueue` path) | return `Ok(false)` (no progress this round, no pause written); the existing notify/delay loop retries |
| index `next_delay` | existing 30 s fallback |

### Call-site classification (origin/main e124aed)

There are 180 non-test `call`/`try_call` sites. Background sites are listed
individually. Everything not listed as background is foreground. Kind names are
the `db_job_slow` names.

Background lane, budgeted:

- `control.db.Store.collect_receipts` (maintenance loop).
- `gc.GcService.collect`: the submitted-draft batch, summary-cache loop,
  index-source loop and `index::collect_inactive_spaces` loop, plus
  `gc.GcService.raw_stats`, `raw_batch`, `collect_raw` (`begin_raw`,
  `raw_step`), `start_history` and `history_step` when reached from `collect`.
- `attachments.AttachmentService.collect_managed_observed`: the
  select-and-mark, mark-deleted and remaining-sum jobs. They are reached from
  GC, and from the `restart_cleanup` tail after its startup transaction.
- `index.IndexService.process_one_inner` (pause check), `process_key` (claim),
  `drain_authorization` (5 jobs), `capacity_allowed`, `identity...next_delay`,
  and `enqueue` / `load_source` / `queue::enqueue_batch` /
  `observations` only when called from `drain_authorization` in the worker.

Foreground lane, unbudgeted:

- Settling external effects from background loops: `index.IndexService.process_one`
  (pause write), `index.batch` adoption and `handle_request_failure`,
  `index.failure.fail_work` (2), `index.dimensions.fail_initial_dimensions`.
- Startup one-shots: `app.DataOwner.start_with_localized_progress`, the first
  `restart_cleanup` transaction (stale pins and interrupted creations, which
  must finish before admission), `index.queue.recover_inflight` and
  `recover_authorization_progress`.
- Admission gates for foreground work: `gc.GcService.require_capacity`.
- User and CLI requests: `control.db.Store.*` (`execute`, `execute_with_plan`,
  drafts, replay, reserve/finalize, stop, edit, send, `prepare_agents`),
  `control.db.run_action`, `control.runtime.*`, `control.compact.*`,
  `control.wait.*`, `control.turn_interrupt.*`, `control.Control.*`
  (`edit_ordered_request`, `stop_run`, `nudge_owner`, `kick_inner`,
  `retire_driver`, `prepare_shutdown`, `shutdown`), `control.owner_shutdown`,
  `control.memory`, `history.*`, `gc.GcService.details` (StorageStatus) and
  index user commands (`preview`, `rebuild`, `backfill`, `retry`, `probe`,
  `status`, `select_space`, `write_space`, `coverage`, `management_status`,
  `search_filtered_inner`, and `enqueue_tool_raw` and its `enqueue` path).
- Turn execution: `control.driver.*` (`admit_attempt`, `drive`,
  `turn_instructions`, `next`, `recover*`, `stop_without_driver`,
  `drain_and_stop`, `save_tool_result`, `run_shell`, `cancel_unstarted_shell`,
  `shell_result`, `adoption.adopt_response`, `proactive.maybe_compact`,
  `retry.retry_or_recover`, `shell_output.collect_shell` and `save_completion`,
  `tools.run_tool_group`), `control.output.*`, `compaction.*` (`save`,
  `snapshot`, `spill_snapshot`, `summarize_recorded`, `CachedPart.save`,
  `bounded.reduce_summary`, `edit.*`, `edit.trim.*`, `usage.UsageLedger.save`).
- TUI: `frontend.*` (`save_draft`, `select_thread`, `load_edit_request`,
  `receipt_states`, `restore_snapshot`, `activity.read`, `history_reader.*`,
  `input_history.recall`, `message_metadata.enrich`, `message_queue.*`,
  `received.*`, `recovery.enrich`, `streaming.deliver`, `thread_deletion.*`,
  `tools.*`, `usage.enrich`).
- Attachments for requests: `save_clipboard_pixels`, `plan_add`, `add`,
  `prepare_selected`, `mark_success`, `drop`.

Foreground cost is out of scope here; see open questions.

### Jobs that must be split before or with the budget

These jobs have a single unit whose cost grows with table size. If not split,
they would be deferred permanently once data grows. Each split uses a keyset
cursor and commits progress per batch, or keeps an in-memory cursor where
restarting from the beginning is safe.

- `control::receipt::collect` (via `Store::collect_receipts`): `LIMIT 128`
  bounds output, not the scan of `operation_receipt` with a `json_each` over
  every `compact.*` app_meta value. Split by an in-memory
  `(created_at, operation_id)` cursor over a bounded window, restarted per
  owner. The foreground call inside `reserve_with_limit` under pressure stays
  unbudgeted (open question).
- `GcService::raw_stats` when called from `collect_raw`: aggregates every
  `shell_execution` with correlated `output_chunk` sums. Split into rowid
  batches accumulated across jobs; the eviction decision is still rechecked per
  execution by `begin_raw`. Coordinate with issue-164/gc-index-source-scan,
  which touches `src/gc.rs`.
- The GC summary-cache loop in `collect`: it scans every `summary.part.%` key
  with `json_each` until 128 eligible are found. Use a keyset cursor on `key`.
- The GC index-source loop in `collect`, the 64-210 s query: issue-164 is fixing
  it. After the fix, measure its batch steps.
- `collect_managed_observed`'s first job: the whole `managed_file` table, with
  `json_each` over drafts per row, in one transaction. Use keyset batches on
  `(created_at, id)`; compute the ready-byte total with a separate `SUM`.
- To verify by measurement (they look bounded): `submission_retention::collect_batch`
  (scans submitted drafts), `raw_batch` (expression order on
  `shell_terminal_time`), `start_history`, `history_step` stages,
  `collect_inactive_spaces`, `queue::claim_authorization`, the index `claim`.

### Tests (phase 2)

- Structure: a synthetic heavy background job (a large table and a correlated
  cross-scan) is admitted, then a foreground job. Assert that the background job
  returns `Deferred` after at most budget + granularity counted steps, that its
  writes are absent, that the foreground job then runs, and that no later
  `COMMIT` in the interrupted closure persists, even when it swallows the error.
- Fairness: under a sustained foreground stream, a waiting background job runs
  within `FAIRNESS_RUN + 1` jobs.
- Callers: GC, receipt collection, managed-file collection and the index worker
  map `Deferred` to deferral, never to `save_failed` or an index pause.
- Shutdown drains both lanes; the idle checkpoint waits for both lanes to empty.
- Diagnostic limit and privacy, as for `db_job_slow`.

### Open questions for review

1. `FAIRNESS_RUN = 8`: a policy ratio, not a measurement. Strict alternation
   (`1`) gives the same per-job bound with lower foreground throughput.
2. Foreground heavy jobs are not addressed by this design: observed
   `control.db.Store.execute` up to 19,244 ms, `frontend.activity.read`
   1,285 ms, `index.queue.recover_inflight` 2,076 ms (startup),
   `require_capacity` (`logical_usage`, O(rows) covering-index sums, on every
   model or shell start), and the receipt `collect` inside foreground
   `reserve_with_limit` under pressure. Should these become separate issues
   (for example routing read-only queries to the presentation readers)?
3. `gc.GcService.details` (`storage status`) reuses `raw_stats`. Should it stay
   foreground, or report the background-maintained raw totals?
4. Splitting `raw_stats` and the GC loops touches `src/gc.rs` while
   issue-164/gc-index-source-scan is open. Should phase 2 wait for that merge?

## Current-only contract (Issue #33, 0.2.0)

Current conversation schema is v21. Migration 15 adds derived readback indexes; 16 adds Thread observation indexes; 17 adds the parent-Thread covering index; 18 accounts for summary usage; 19 adds the completed-attempt scope index; 20 adds index-source GC lookup indexes; 21 adds event-retention lookup indexes. Migration12 accounts for bounded Revisit
metadata;13 records selected-path admission;14 records Turn interruption and
indexes drafts. The existing `tool_result_node(node_id)` index from migration2
also covers retained result-source lookup after payload GC; the earlier #41
B+C work needs no additional index, while F adds the justified parent index. No new state table or runtime compatibility path is introduced. Fresh databases are created directly from `src/storage/current.sql`, with the complete current ledger. Existing older-schema upgrades run only through the isolated startup migration runner; ordinary runtime readers require the current schema. The historic v6 receipt-accounting repair and attachment-origin reconstruction are not runtime compatibility mechanisms.

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


## Saved readback indexes (#75, schema 15)

No new table, worker, queue, source-body copy or counter is introduced. Two shell
indexes cover normalized start-instant/ID and owner-Run/start-instant/ID. They
include terminal shells lacking tool links. The time keys use deterministic SQLite
built-ins only, preserve RFC3339 offsets/nanoseconds/leap seconds, and put unknown
starts last. Their SQL fragments and migration are checked by real query plans.
A partial operation-receipt index seeks queued compaction targets by Thread;
a partial attempt index seeks automatic-reduction observations by Turn. Both
partial predicates guard malformed optional JSON, including during startup upgrade.

On a synthetic 5,000 rows per indexed source, bundled SQLite with 4 KiB pages
reports B-tree sizes via dbstat: shell time 118,784 B, shell owner/time 139,264 B,
compaction receipt 221,184 B, automatic attempt 217,088 B. These are observations,
not quotas or size guarantees. Shell rows add two index entries; queued operation
receipts and automatic attempts add one eligible partial entry each. Startup index
construction and relevant writes have this explicit cost even when lists are not
used. Existing model/Run/Turn indexes serve usage periods; no redundant model index.

Read-only WAL connections share the existing two-slot presentation admission.
Normal mutations continue to use the writer. All source records/retention remain
unchanged. Upgrade from 14 appends only the migration ledger entry and indexes;
back up before opening with a new build. Old v0.4.0/schema14 binaries cannot open
schema 15, and no installed owner or user database is upgraded by validation.


### F parent lookup index (local schema17 candidate)

At origin/main a0f7022, conversation schema is16; its Thread observation
indexes remain unchanged. The F-only local candidate
adds `thread_parent(parent_thread_id,id)` in isolated startup migration0017.
Successor seeks for bounded descendant traversal require this covering index;
existing updated-at and Run indexes do not cover parent membership. It costs
one entry per Thread and index maintenance on parent/id changes. No table,
event, worker or queue is added. Fresh bootstrap and upgrade use the same index
and ledger. The upgrade transaction atomically commits DDL, ledger and version;
failed admission rolls them all back. Saved conversations are unchanged.
Before a release containing this upgrade, stop the owner and take a complete
data-directory backup (including any SQLite WAL/SHM files) before opening it
with the new binary. Older binaries reject schema17. Recheck main's actual
version and migration names immediately before F admission;17 is not reserved
against independent work. This primitive alone does not expose descendant UI.

## Last adopted attempt lookup (#142 part 1)

Cache diagnostics reuses the context display's bounded completed-attempt
selection over at most 33 recent Turns and the existing schema19 indexes.
There is no new migration, derived pointer, index or adoption write. Bounded
reported facts remain in the existing attempt manifest and GC accounting.
