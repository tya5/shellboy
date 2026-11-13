# SQLite migration と専用worker

## Thread automatic-spill override (schema45, S6a)

Migration45 adds only `thread.spill_override`, nullable INTEGER with
`CHECK(spill_override IS NULL OR (typeof(spill_override)='integer' AND
spill_override IN (0,1)))`. Existing rows remain NULL (inherit); 0 is explicit
off and 1 explicit on. The current schema executes this migration in order and
must converge with upgrade44->45. Readers strictly decode only NULL/0/1. Child
creation, fork and rewind copy the source raw value in their existing transaction;
no parent traversal or extra table is added. The migration is source/synthetic
fixture only here, never an installed database operation.

Schema21 adds derived event-retention indexes; it does not rewrite event or
delivery data. See [GC](gc.md#dead-delivery-and-shell-event-retention-170).

## Startup observations (#191)

The startup screen and diagnostics must use the same fixed stage definitions.
The user-approved display reports total elapsed time and, only during an
existing-database migration, the current step out of the known number of
steps. Steps are not equal-cost and are not startup-wide progress. New and
already-current databases have no migration step count. When stage markers do
not fit, show the numeric count without markers. The startup trace uses fixed
stage classifications and numeric durations, never IDs, data or SQL. It is
bounded per startup, nonblocking, and best-effort through the existing rotating
diagnostics writer; a dropped record is not a zero-duration stage. Do not
perform additional full-table scans merely to derive a progress denominator.

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

For Store.execute, the call-site name includes a fixed protocol-command label
chosen by an exhaustive match on the protocol command enum. New variants must
supply a label at compile time. Status has its own label. The label is independent of request
arguments, origin, IDs and SQL. Existing run and queue timings are whole-job
measurements, not acceptance, dispatch or commit phase timings. The labels do
not change DB queue admission, diagnostic sampling or privacy bounds.

## DB worker activity visibility (#190)

Adopted 2026-11-03. Each dequeued job publishes an in-memory snapshot containing
the fixed `kind`, foreground/background lane, wall-clock start and monotonic
start used for elapsed time. The snapshot also exposes the existing bounded
no-progress kinds and deferral counts. A Tokio watch channel gives the frontend
the latest snapshot without querying or waiting on the DB writer queue; no state
is persisted and no worker is added.

The existing frontend tick observes an active job while it runs. Once its run
time reaches the existing one-second slow-job threshold, it emits at most one
`db_job_active` event per job, sharing the existing slow-job limiter. The
completion `db_job_slow` event retains final queue/run times. Both events contain
only fixed call-site labels, lane and elapsed durations, never SQL, arguments,
results or conversation content. The existing `db_job_no_progress` event and
state remain authoritative for no-progress; the UI shows its first sorted kind
in preference to a timed active job.

The first existing status/footer row places `DB <operation> <elapsed>` at its
right edge only after run time exceeds one second; a no-progress entry instead
shows `DB! <operation> no progress` (localized in Japanese). With no qualifying
activity it shows nothing. The display updates through the existing UI clock,
adds no row or setting, and may truncate lower-priority footer facts while
visible. This is observability, not a wall-clock completion guarantee.

## History GC diagnostics (#225)

The existing GC service publishes one content-free in-memory watch snapshot for
the current or most recent history pass. It contains the stage, cumulative
scanned work, budget deferrals, restarts, retained-over-budget node count, the
separate retained receipt and TEMP mark cleanup counts, pass start and last
committed cursor advance. The frontend coalesces updates; the UI
never queries the writer to display them. When no higher-priority DB activity is shown, the footer identifies an active
GC and points to F1; the DB / GC section at the top of the F1 or `/help` guide displays
the full details in English or Japanese, updating its clock once a second.
The elapsed pass clock freezes on completion; age since the last cursor advance
continues. These are observational counters, not a completion guarantee. The
snapshot is one bounded current-pass value and is discarded at process exit.

## DB writer occupancy: requester contract and backend safety net (#171)

Status: adopted (revision 2, approved 2026-11-02) and implemented on
`issue-171/db-step-budget`. Revision 1 protected the writer only on the
backend: two lanes plus a step budget. Review rejected that approach. In the incidents behind #164, the root cause was always
the *requester*:

- The index kept enqueuing while it was paused.
- `next_delay` rescanned every queue with no progress.
- GC reissued a 1–3 minute index-source query about every 2 minutes, and the
  query found 0 rows.

This revision makes a requester contract the primary mechanism. The backend
does two things: it keeps the UX cost of that contract small, and it catches
contract violations. Current-job visibility (start time and kind readable from
another thread) is specified separately in [#190](#db-worker-activity-visibility-190);
it does not change the requester contract or step budget.

### Problem (measured)

One OS thread runs every SQLite write job in FIFO order. Live `db_job_slow`
observations (macOS, one owner, 2026-11-02):

- Run time: up to 209,841 ms for `gc.GcService.collect`, 19,177 ms for
  `control.db.Store.collect_receipts`, 3,678 ms for `gc.GcService.raw_stats`,
  and 2,076 ms for `index.queue.recover_inflight`.
- Queue time: foreground kinds that run in 0–72 ms (`adopt_response`,
  `collect_shell`, `frontend.select_thread`) waited up to 209,851 ms behind
  them.

### Guarantee

The guarantee covers only waiting that background work causes:

- **Foreground:** a foreground job waits for the foreground jobs admitted
  before it, plus at most one background unit.
- **Background:** a background unit completes whenever foreground demand has a
  gap.
- **Under unbroken foreground load:** CI enforces the per-unit bound for every
  background kind on a large synthetic database. Repeated deferral of a kind is
  made visible as no-progress state and as a diagnostic.

Steps are the bound. No wall-clock duration is promised, and measured times are
recorded only as facts. Foreground jobs are outside this guarantee and must be
bounded individually. At adoption, the writer-side foreground jobs needing work
were listed in the separate issue (`control.db.Store.execute`,
`frontend.activity.read`, startup `recover_inflight`, `require_capacity`, the
receipt collection inside `reserve_with_limit`, `gc.GcService.details`). #172
bounds the receipt collection as described below; the other work has separate
scope. Contract rule 4 below says where those jobs should run instead.

### Requester contract (primary)

1. **Declared bounded unit.** A background request is issued only through
   `BackgroundRequester::unit(limit, op)`. The `limit` (a nonzero row or batch
   count) is passed into the closure. The closure must apply it as `LIMIT` on an
   indexed path: a keyset seek on rowid or a covering index, never a sort or
   scan of a whole table. An unbounded background request cannot be written
   without visibly ignoring `limit`. The plain `DbWorker::call` stays
   foreground-only. CI enforces the bound: for each background kind, a test on a
   large synthetic database asserts that the kind's normal unit uses at most
   `BACKGROUND_STEP_BUDGET` VM steps. A sanity query in the same test proves that
   a full scan of that fixture exceeds the budget, so an unbounded unit fails
   CI.
2. **At most one outstanding request per requester.** Each requester owns one
   non-`Clone` `BackgroundRequester`, and `unit` takes `&mut self`. The next
   request cannot be issued until the previous reply is received. The background
   lane's occupancy is therefore bounded by the number of requesters. The
   requesters are the maintenance task and the `indexing.concurrency` index
   workers.
3. **No work without progress.** Every unit reports `Progress::Advanced` or
   `Progress::Idle` (it found or did nothing).
   - After `Advanced`, the requester may issue the next unit immediately. The
     foreground-first lane keeps that from hurting UX.
   - After `Idle`, the requester may not repeat that kind on a timer. It waits
     for that kind's *change trigger* (see the table), or for a *deadline*
     computed from saved data when the work is age-based.
   - Triggers are coalescing in-process permits (`tokio::sync::Notify`), set by
     producers after their commit, the same pattern as the existing
     `index.wake()` and `availability_changed`.
   - Deadlines come from one indexed `MIN(...)` query, never a scan.
   - Startup marks every kind dirty once, and an explicit `storage collect`
     marks all GC kinds dirty, so a missed trigger costs at most until the next
     restart or explicit collect.
4. **Foreground requesters.**
   - Read-only views and queries go through reader WAL connections (the existing
     `presentation::work` path, bounded by its admission slots), not the writer
     worker, wherever the read does not have to be ordered with a write in the
     same Control step.
   - Writes stay small by construction: keyed or indexed, with any read inside
     them bounded.
   - Moving the existing heavy foreground reads is tracked in the separate
     issue. #171 adds only the rule and moves `gc.GcService.details` and the
     capacity totals (see below).

### Per-requester table

| Requester (kind) | Bounded unit | Trigger after `Idle` | UX impact of the constraint | Mitigation |
|---|---|---|---|---|
| Receipt collection (`Store::collect_receipts`) | rowid window of 1024 receipts, at most 128 changed; the `app_meta` `json_each` check uses a key range instead of `LIKE` (seekable) | (a) deadline: oldest eligible result is 24 h old (current instance) or 7 days old (old instances); (b) admission needs space | Admission can wait while background windows reclaim capacity | Retry admission after each window; reject only after an exhausted stable pass |
| GC raw retention (`collect_raw`: `raw_stats`, `raw_batch`, `begin_raw`, `raw_step`) | 128 executions on `shell_terminal_time` / 1 MiB of chunks per step; **`raw_stats` removed from the unit**: the total comes from an incrementally maintained raw-bytes counter | (a) capacity: shell output committed past the cap (producer: output save); (b) deadline: oldest terminal execution's `ended_at + tool_raw_days` | Reclaim starts after a trigger instead of within 60 s | Output producers signal only when the counter crosses the cap; work runs during idle gaps |
| GC history (`start_history`, `history_step`) | 128 rowids per stage (existing) | Thread deletion commit | Deleted Threads' space is reclaimed after the deletion signal, not on the next 60 s tick | Deletion signals immediately; steps run back to back |
| GC submitted drafts (`submission_retention::collect_batch`) | 128 drafts, `ORDER BY updated_at,id` on the submitted-state index | Draft submission admitted or cancelled (commit) | None visible | — |
| GC summary cache | keyset on `app_meta.key` from the last key, 128 keys | Source tombstones (history step that tombstoned >0, or Thread deletion) | Cache bytes freed later | Coalesced with the history trigger |
| GC index sources (issue-164 fix, 1044d64) | 128 nodes via the issue-164 bounded path | Node tombstone or payload removal committed (history/raw GC that advanced) | None | Coalesced with history/raw progress |
| GC inactive index spaces | 32 spaces (existing) | Space deactivation (`rebuild` completion, `select_space`) | None | — |
| Managed images (`collect_managed_observed`) | keyset on `managed_retention(state,created_at)`, 128 rows; total from the existing `gc_bytes_managed_file` sum | (a) clipboard image saved past `clipboard_bytes`; (b) deadline: oldest ready `created_at + clipboard_days`; (c) draft/attachment release (unpin) | Images may remain until their trigger | Unlink stays outside the DB (existing file workers) |
| Restart cleanup | one bounded startup transaction (stale pins) | Startup only | — | — |
| Index scheduler (`next_delay`) | one indexed `MIN(next_attempt_at)` over `jobs_ready` per unpaused space (no queue walk) | `wake()` on enqueue/authorization/adoption (existing); resume (`retry`, `select_space`); deadline = that minimum | **Paused: no timer at all** (today it polls every 30 s); resume is a signal | Existing wake sites already cover new work |
| Index authorization drain (`drain_authorization`) | `claim_authorization`, one row by key-range seek | New authorization (turn adoption `wake`); while paused the consumer discards one item per unit (`discard_paused`, #173) | Authorizations wait during a pause | Durable authorization rows keep the request; resume drains |
| Index claim and process (`process_key`, `capacity_allowed`) | `embedding.batch_max_inputs` claims via `jobs_ready` | Same as the scheduler | Catch-up after a burst is paced by one outstanding unit per worker | Back-to-back units while `Advanced`; HTTP dominates anyway |
| Capacity checks (`require_capacity` foreground, index `capacity_allowed` background) | O(1) read of incrementally maintained logical-byte counters (today: O(rows) covering-index sums on every model/shell start) | Not periodic: per request | Counters must stay exact | Counters maintained in the same transaction as each write (triggers, as `quota_counter` already does for receipts); see open question 1 |
| Storage status (`gc.GcService.details`) | Reader connection; logical totals from the same counters; raw protected/eligible split as a bounded lower bound | On request | Status is a snapshot at its read, not the writer's latest uncommitted state | Already labelled `reclaimable_is_lower_bound` |

`raw_stats`, the 60 s maintenance timer and the 30 s paused-index poll are
removed. The only remaining timers are data-derived deadlines.

### Asynchronous receipt pressure (#172)

If admission exceeds its budget, it does not collect receipts inside the
foreground transaction and does not immediately reject. The requester waits
asynchronously outside the SQLite worker while the existing receipt background
requester runs one bounded rowid window (1,024 visited rows, at most 128 changed
receipts, and at most 8 MiB of current result bodies validated). Admission is
retried after each completed window and succeeds as soon as capacity permits.
If another writer changes the database while the requester waits for the lane,
admission is retried before collecting further receipts. Regular receipt
maintenance releases the requester between windows so these retries can proceed.
Large request data is shared across attempts, and the request fingerprint is
computed once rather than reserialized and rehashed by every admission probe.
Deferred windows are retried without holding the writer. Pending and protected
receipts and malformed results retain their existing protection; a validation
byte-budget boundary resumes before the first unexamined candidate.

Only an exhausted pass followed by a still-failing admission returns
`storage_limit`. Concurrent writer changes invalidate an exhausted pass, which
then restarts before refusal. Under continuous writes or repeatedly deferred
windows, admission can wait longer; there is no wall-clock bound. Exact usage
accounting, per-candidate SQLite/JSON work, migration and writes still cost time.
The UI retains its existing accepting/sending state until admission completes;
no new indicator or protocol status is introduced.

### Backend (secondary): keep the constraints cheap, catch violations

- **Two lanes, foreground first.** These are bounded Tokio mpsc lanes, each with
  the existing capacity. Selection is strict alternation: after a foreground
  job, a waiting background unit goes next. When only one lane has work, that
  lane runs. Waiting uses a biased `select!` on a current-thread runtime owned
  by the DB thread. Shutdown closes both lanes and drains every admitted job.
  Admission is the send made under the admission lock that shutdown takes; a
  reserved lane slot is not admission. A request whose slot was reserved but
  not yet sent when shutdown closed admission gets `Closed` and never runs, so
  no accepted job is lost (#195). Tests observe admission by awaiting it, not
  by lane capacity, which already counts reserved slots.
  The idle checkpoint waits for both lanes to be empty. A background flood
  cannot fill foreground admission.
- **Idle-gap execution.** Because requesters issue their next `Advanced` unit
  immediately, and the lane rule interleaves foreground work, backlog work (GC
  reclaim, index catch-up) uses the writer's idle gaps without a timer.
- **Coalescing triggers.** Each trigger is one `Notify` permit per kind, so a
  burst of producer events becomes one wake. Producers signal after commit only,
  and the signal carries no data.
- **Incremental totals.** Capacity and raw totals are maintained in the same
  transaction as the writes that change them. This removes the O(rows) sums from
  both the capacity gate and GC.
- **Conditional step budget (safety net only).** For a background unit, the
  wrapper installs a progress handler and a commit hook (pinned rusqlite 0.40.2
  with its `hooks` feature: no new crate).
  - The handler interrupts only when the unit has exceeded
    `BACKGROUND_STEP_BUDGET` VM steps *and* a foreground job is waiting. An
    atomic counter of admitted foreground jobs not yet taken provides the second
    condition.
  - After the hit, every later step is interrupted, and every later commit is
    turned into a rollback. The open transaction is then rolled back, and the
    reply is `Deferred`, a typed outcome, never `save_failed`.
  - Hitting the budget means the requester broke rule 1. The worker emits
    `db_job_budget` (`kind`, `steps`, `queue_ms`, `run_ms`; at most 10 per
    60 s).
  - After `NO_PROGRESS_DEFERRALS = 2` consecutive deferrals of one kind with no
    completed unit in between, it emits `db_job_no_progress` and exposes the
    kind in `DbWorker::background_no_progress()` for a later status surface.
    Two is structural: every deferral already consumed a full budget slot, and
    the retry starts from the same committed state, so a second deferral shows
    the unit does not fit.
  - Steps that are not counted: Rust work between statements, fsync, and work
    inside a single virtual-table or SQL-function call.
- **Budget value by measurement.** `BACKGROUND_STEP_BUDGET` is the smallest power
  of two that is at least the largest healthy unit measured by the per-kind CI
  tests (bundled SQLite, file-backed WAL, recorded fixture). A preliminary data
  point, not the basis for the constant: about 72,500–106,700 VM steps per ms
  (system SQLite 3.51.0 in memory on the development Mac).

### Classification rule

A unit is background when rolling it back only delays independent maintenance.
A capacity-constrained admission may await such receipt reclamation outside the
writer; it does not depend on any particular window result and retries admission
after each one. Settlement of an external effect is never background.

Settlement of an external effect that has already happened is foreground, even
when a background requester issues it. An example is adopting or failing
embedding claims after the HTTP request was billed. Such settlement must be
bounded by construction. The lane is chosen by the requester, not by a shared
helper.

### Call-site classification (origin/main e124aed)

There are 180 non-test `call`/`try_call` sites. Background sites are listed
individually. Everything not listed as background is foreground. Kind names are
the `db_job_slow` names.

- **Background, budgeted:** `control.db.Store.collect_receipts`; the
  `gc.GcService.collect` loops and the `raw_batch`, `begin_raw`, `raw_step`,
  `start_history` and `history_step` jobs; `raw_stats`, which is replaced by a
  counter; the three `attachments.collect_managed_observed` jobs; and, in the
  index worker, `process_one_inner`, the `process_key` claim, the five
  `drain_authorization` jobs, `capacity_allowed`, `next_delay`, and the
  worker-reached `enqueue` / `load_source` / `enqueue_batch` path.
- **Foreground:**
  - Settlement: the `process_one` pause write, batch adoption,
    `handle_request_failure`, `fail_work` and `fail_initial_dimensions`.
  - Startup one-shots: `app.start_with_localized_progress`, the first
    `restart_cleanup` transaction, `recover_inflight` and
    `recover_authorization_progress`.
  - `require_capacity`.
  - Request handlers: every other `control.*`, `history.*`, `frontend.*`, and
    `compaction.*` site; index user commands and search; and attachment request
    paths.

### Implementation notes

- **Requester API.** `storage::BackgroundRequester::unit(limit, op)` is the only
  background entry; `DbWorker::call`/`try_call` stay foreground. Requesters:
  receipts (`Store`), GC (`GcService`, also held by tests that step history GC
  by hand via `GcService::hold`), managed images (`AttachmentService`), and the
  index worker (`IndexService`, one requester shared by its workers, so at most
  one scheduling unit is outstanding; settling claims whose HTTP request ran
  stays foreground). The DB job kind is the requester closure's call site.
- **Signals.** `maintenance::Signals` (one coalescing bit per kind plus one
  `Notify`) lives with the `DbWorker`, so every committing producer can mark
  it. A mark inside a writer job before its commit is safe: the single writer
  runs no maintenance unit until that job commits or rolls back, and a rollback
  costs one idle pass. Producers and their kinds:

  | Producer (after commit) | Kinds marked |
  |---|---|
  | Thread deletion (`ThreadDelete`) | History, SummaryCache, IndexSources, Managed, Drafts |
| Run terminal (driver retirement, driverless stop) and every saved shell completion | EndedRuns, with the Run id (`run_ended`) |
  | Shell completion saved | Raw |
  | Run retired (`retire_driver`) | Drafts, Raw (and EndedRuns above) |
  | Clipboard image saved ready, or its creation failed | Managed |
  | Runtime pins released (`PinLease` drop) | Managed, Raw |
  | Index space transitions (`select_space`, rebuild start, candidate activation on authorization drain or adoption, end or cancel of an int8 conversion). The pass takes only discarded partial candidates and spaces with no vectors, never a retained space with vectors ([gc](gc.md)) | InactiveSpaces |
  | `require_capacity` observing the logical cap | every GC kind (backstop) |
  | Startup (default `Signals`) and explicit `storage collect` | every kind / the category's kinds |

- **No timer safety sweep (review decision).** Startup recomputation and an
  explicit `storage collect` cover a forgotten signal. Storage growth itself is
  the backstop: `require_capacity` gates every model, shell, child, summary and
  index start, and when it observes the logical cap it marks every storage kind;
  receipt reservations directly request bounded background windows on pressure.
  A forgotten signal can delay reclaim, but cannot bypass the storage cap.
  Each producer has a test that it emits its signal.
- **Deadlines.** Receipt windows return the earliest future age eligibility of
  their rows; managed retention reads one `MIN(created_at)` seek on
  `managed_retention`; raw retention keeps the oldest retained execution's
  `ended + tool_raw_days` and the resume cursor for an age-only pass. The
  maintenance loop sleeps only until the earliest deadline or the next mark.
  The index worker sleeps until its earliest scheduled retry or a wake; while
  paused or idle it has no timer. The one remaining bounded fallback is the
  existing `next_delay` scan bound (256 rows), which clamps to 1..30 s.
- **Run-end retention (#170), targeted.** A Run's terminal commit (driver
  retirement or a driverless stop) and every saved shell completion call
  `Signals::run_ended(run)`, which queues the Run id (bounded at 1,024; beyond
  that the backlog sweep is marked instead) and marks `EndedRuns`. One
  `collect_ended_runs` unit deletes up to 64 of that Run's pending deliveries
  by seek on `delivery_pending(recipient_id,state)`, then visits that Run's
  shell events 32 at a time by seek on the schema22 partial index
  `event_run_shell_ended(target_run_id) WHERE kind='shell_ended'`, deleting an
  eligible event and its remaining dropped/processed deliveries. Work per Run
  end is proportional to that Run's rows, not the tables. A non-terminal Run is
  skipped. A deferred Run is re-queued at the front and restarted (units are
  idempotent). The #170 round-robin sweep over all rows runs only as
  `EventBacklog`: at startup (rows that predate this owner), on explicit
  `storage collect`, and on queue overflow.
- **Raw retention without `raw_stats`.** The raw total is the schema22
  counter. A raw pass touches no execution row unless the total exceeds
  `tool_raw_bytes`, an age deadline is due, a tombstone is pending, or a rescan
  was requested (release signal or explicit collect). Windows are 32
  executions on `shell_terminal_time` with a row-value keyset
  `(COALESCE(ended_at,started_at), rowid)`. `protected_raw_bytes` is exact for
  a full pass from the start (explicit collect) and 0 otherwise.
  `gc.GcService.details` (`storage status`) runs the full split on a reader
  connection through `presentation::work`, not on the writer.
- **Schema22 (`0022_logical_usage_counters`).** Four `quota_counter` rows
  (`logical.history`, `logical.raw`, `logical.managed`, `logical.index`)
  maintained by AFTER INSERT/UPDATE OF/DELETE triggers on the 12 counted
  tables, plus `index_space` insert/delete/dimension changes. Migration 22 also
  adds the partial `event_run_shell_ended` index for targeted Run-end
  retention. They reproduce the
  former indexed sums exactly; the old sums remain only as a test oracle that
  is compared after mutation and rollback. `require_capacity_tx` is O(1). The
  partial index `attachment_delivery_operation(operation_id) WHERE status IN
  ('accepted','held')` makes the receipt-collection delivery probe a seek. The
  upgrade computes the counters once from saved rows in the migration
  transaction; IF NOT EXISTS / OR REPLACE keep downgrade fixtures simple.
  Writes to counted tables pay one counter UPDATE per trigger.
- **Index scheduling queries.** The authorization claim seeks the profile's
  byte-exact `index.auto.<profile>.` key range instead of `LIKE`. The job claim
  and batch fill read `jobs_ready` in `(next_attempt_at, rowid)` order instead of
  sorting every pending job by id.

### Measurement and the budget constant

Environment: macOS (Apple silicon development Mac), debug test build, bundled
SQLite, file-backed WAL database in a temporary directory, 2026-11-03. Command:
`cargo test --lib maintenance::step_tests -- --nocapture`. Fixture
(`src/maintenance/step_tests.rs`): 20,000 live and 60,000 tombstoned nodes,
40,000 completed operation receipts (half from an old instance), 20,000
applied attachment deliveries, 20,000 terminal executions with distinct end
instants and one chunk each, 10,000 summary parts, 10,000 deleted and 1,000
ready managed images, 40,000 pending jobs scheduled in the future, 40,000
shell events of a live Run and 10 of an ended Run. The index profile is then
paused for one more scheduling round (`discard_paused`, #173). Nothing is
past a cap or an age, so every unit is a normal pass, not a bulk deletion.

Counted steps are multiples of `PROGRESS_GRANULARITY` (1,024), so kinds under
one interval read as 0. Largest unit per kind (one run; facts, not bounds):

| Kind | Max steps per unit | Units |
|---|---|---|
| `attachments::AttachmentService::collect_managed_observed` | 87,040 | 19 |
| `control::db::Store::collect_receipts` | 53,248 | 40 |
| `gc::GcService::collect_held` (drafts, history, summary, index sources, spaces) | 4,096 | 550 |
| `gc::GcService::collect_raw` | 1,024 | 627 |
| `gc::GcService::collect_ended_runs` (one ended Run, 10 events, with 40,000 events of a live Run present) | < 1,024 | 2 |
| `gc::GcService::collect_event_backlog` (40,000 events and deliveries) | < 1,024 | 1,251 |
| index `process_key` (`discard_paused` while paused, capacity, claim), `next_delay`, `drain_authorization`, `capacity_allowed_background`; `start_history_held` | < 1,024 | 1–3 each |

All 2,497 units took 3,832 ms in total (a fact, not a bound). The fixture
also holds 40,000 shell events with pending deliveries of a live Run; a full
event scan takes 439,296 steps.

`BACKGROUND_STEP_BUDGET = 2^17 = 131,072`: the smallest power of two at or
above the largest healthy unit (87,040). The same test proves the fixture is
large enough to expose unbounded work: full scans of the tables the kinds visit
take 179,200 steps (tombstoned nodes), 399,360 (receipts) and 359,424
(executions with chunks), all above the budget. The test asserts both facts.

Before the requester fixes in this change, the same fixture measured
368,640 steps for the index claim (sort of every pending job), then 49,152 for
its bootstrap `LIKE` over all `app_meta`, 49,152 for the authorization drain
(`LIKE` over all `app_meta`) and 120,832 (equal end
instants) to 200,704 for raw windows; each would have failed the assertion.
Remaining known limits: a run of executions with identical end instants
degrades the raw window to a filter within that run, and the
`next_delay` bootstrap read and submitted-draft batch are bounded only by
their live row counts. These are visible to the per-kind test if they grow.

The preliminary rate from the design phase (72,500–106,700 steps per ms,
system SQLite 3.51.0 in memory) suggests one budget slot is on the order of
a few milliseconds on the development Mac; this is not a guarantee.

### Validation

- Storage-level structural tests cover the backend:
  - A foreground job admitted while a synthetic endless background unit runs
    completes after exactly one budget slot (budget + one granularity, counted).
  - Writes and autocommit writes after the hit do not persist.
  - Without foreground demand, an over-budget unit runs to completion.
  - Lane alternation and draining both lanes at shutdown.
  - The no-progress threshold, with diagnostics limited and free of content.
- One CI test per background kind measures its unit's steps on a large synthetic
  database and asserts them against the budget.
- Requester tests: after an `Idle` unit, no further unit of that kind is issued
  until its trigger fires or its deadline passes. Each producer commit fires its
  trigger. A paused index issues no DB unit until resume.

### Review decisions (2026-11-02)

1. Running counters with triggers ship in #171 as migration 22 (renumbered after #170 took 21). Exact counters,
   not a cache with invalidation, make capacity checks constant-time.
2. The partial index on `attachment_delivery.operation_id` is in the same
   migration 22.
3. There is no timer safety sweep; see "No timer safety sweep" above. Each
   producer has a test that it emits its signal.
4. Only `gc.GcService.details` moves to a reader connection in #171. Other
   foreground reads move under #172.
5. The per-kind CI step test plus the runtime safety net is the enforcement of
   rule 1.
6. Selection between the lanes is strict alternation (no fairness constant).

## Current-only contract (Issue #33, 0.2.0)

Current conversation schema is v27. Migration 27 adds the interrupted embedding-job recovery index (#172); Migration 26 adds the pending reply partial index; Migration 25 adds the tool-group attempt lookup index for history execution links. Migration 15 adds derived readback indexes; 16 adds Thread observation indexes; 17 adds the parent-Thread covering index; 18 accounts for summary usage; 19 adds the completed-attempt scope index; 20 adds index-source GC lookup indexes; 21 adds event-retention lookup indexes; 22 adds the logical usage counters (#171); 23 adds bounded activity lookup indexes (#172); 24 drops the #36 Revisit metadata rows and adds the `receipt_run_completion` partial index (#187). Migration12 accounts for bounded Revisit
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

`0010_hot_query_indexes`でactive shellのowner/state、Threadごとの最新Run、terminal shellの時刻順の3本を追加する。計画・書込み費用と採用/見送りの根拠は[経路とhot query設計](history-adoption-performance.md)。既存rowは保持し、索引・ledger・user_versionを同一transactionで前進する。旧binaryの対応上限を超えるため自動downgradeはせず、戻す場合は更新前backupを別data-dirへ復元する。独立RAGのschema（現行は4）とは異なる版番号である。

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

## Pending Reply Lookup (#194)

Provisional schema26 adds `receipt_reply_pending(target_thread_id)` for
`json_extract(result_json,'$.reply.state')='pending'`. Reply state remains in
acceptance receipts; the index supports the 128-duty cap and bounded Thread
lookup. Renumber this migration mechanically if another schema change lands
first. Run-end transitions are a subsequent implementation step.

## Reply lifecycle and deleted receipt ownership (#194 step 3, #197)

Pending replies are bounded to 128 per recipient and use receipt_reply_pending.
Only normal Run completion checks applied pending duties: already-reminded
messages become ignored, and previously unreminded messages receive one durable
reminded_at/reminded_run_id fact and one reply_reminder event containing each
message ID and credential-free sender. Stop, recovery wait and shutdown do not
consume or ignore duties. The reminder is once per message across restart.

Thread deletion terminalizes recipient duties in the delete transaction using
one target_thread_id-indexed lookup per deleted Thread, bounded by the 128-duty
cap. It does not scan sender JSON or all pending replies. Deleted model senders
are handled later in the existing history-GC receipt phase, at most 128 rows
per transaction: pending becomes cancelled with cancelled_by and
cancel_reason=sender_deleted before reclamation eligibility is checked. This
reuses the existing deletion-triggered collector and needs no migration or
sender index. Recipient deletion wins if both endpoints are deleted.
Terminal receipts with a live model sender remain owned
by that sender, while receipts targeting a live recipient remain recipient
owned. Other deleted-target receipts are reclaimed by history GC in at-most-128
row background transactions, before payload marking. See gc.md for visibility
and root retention; operation_receipt retention is unchanged.

### Reply foreground writer unit (#194, migration 26)

`MessageReply` declares one recipient-scoped foreground unit, not a background
VM-step guarantee. Receipt and sender tombstone reads are indexed lookups. Reply
writes at most one input node, one delivery receipt, one index authorization and
constant lifecycle metadata. Shared recovery performs at most 128 receipt
repairs and releases at most 128 oldest held receipts per explicit pass, using
`receipt_target` for recipient/state lookup and primary keys for writes. It no
longer runs global terminal repair from a send/reply transaction. New turn/run
creation and context events are constant-count; previous turn end uses the
existing thread/end index. Index authorization receives a singleton node list.
The legacy writer exemption list is not expanded.

### Migration-Scoped Foreign-Key Validation (#210)

The startup migration registry declares each migration's optional typed foreign-key
impact beside its version and SQL: `IndexOnly` or `Tables(&[...])`. No SQL text
classification is allowed. Missing metadata means a full foreign-key check.
Index-only means no row/FK changes apart from the migration ledger/version;
migrations 25-27 explicitly have this classification. Table metadata declares
all affected tables, including data/constraint changes. Validation checks those
surviving tables and their referencing child tables (transitively, discovered
from SQLite FK schema metadata, not SQL text). A changed parent must not hide
violations in an untouched child. Unknown classifications, empty table metadata,
and malformed/legacy pending markers conservatively require a full check.

Before the first upgrade transaction, persist the union of the entire outstanding
migration range and any prior pending scope in `integrity.migration_pending`.
Full dominates table scopes; index-only adds nothing. Persist even an index-only
scope: interrupted migrations must still resume schema/ledger validation before
clearing pending. On restart the saved scope applies even if user_version is
already current. Clear pending only after the selected check and remaining startup
integrity checks succeed; errors, cancellation or interruption retain it. Fresh
creation has no upgrade check. Ordinary current-schema reopen has no upgrade FK
scan. This policy is for applied schema migrations, not arbitrary package-version
changes without a migration. Manual integrity checks remain full; unclean-start
quick_check is independent and unchanged. No new schema version or worker is added.

The reviewed registry initially marks only migrations25-27 as `IndexOnly` and
migration14 as `Tables([turn, draft])` (turn-column addition and draft indexes).
Migration30 (#229) and migration31 (#231) are also `IndexOnly`: a nullable `ADD COLUMN` without a foreign key whose existing rows are all NULL changes no relation, so no scan is needed. Migration34 (#222 stage 3) creates the new, empty `thread_tag` table (primary key `(thread_id, tag)`, foreign key to `thread`, index `(tag, thread_id)`) and the partial `threads_cwd_updated` index; it is also `IndexOnly` because no existing row can violate a constraint of an empty table. Tags are metadata: `thread delete` removes a Thread's rows in its own transaction, and the table is not a GC root. Other historical entries remain unclassified/full. Ledger/version writes are
inherent migration bookkeeping, not application-table changes requiring a scan.


## Storage capacity thresholds in status (#265)

`storage.status` reports the configured logical data warning threshold as `warning_bytes` and the admission limit as `limit_bytes`, alongside the existing observed live-byte total and `warning` / `limit_reached` booleans. These are the active owner's configured byte values, not measurements or estimates. The read adds two scalar fields and performs no extra database work; the UI can draw a meter only when both observed value and configured limit are reported. The separate filesystem free-space guard remains `disk_free_min_bytes` / `disk_free_state`.


Schema 38 adds `receipt_human_reply_pending(target_thread_id, created_at)`
for cross-Thread human inbox pagination. It is partial on pending human-directed
reply receipts only. The inbox keyset-seeks candidate Thread IDs from this index,
then computes a count capped at 17 per candidate on the bounded candidate page;
count is exact through 16 and marked lower-bound at 17+, and oldest is the
minimum `created_at` only for exact counts. Archived/deleted filtering also
stays within that page. The cursor is a live Thread-ID keyset, not a snapshot. This is distinct from
the per-recipient `receipt_reply_pending` index used for bounded Thread inbox and
lifecycle operations.
