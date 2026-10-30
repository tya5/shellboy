# Model Usage Aggregates (#47)

Status: leader adopts this B design with the completion/cleanup, byte-accounting
and paging rules below. Independent design review must resolve remaining P1/P2
findings before schema/production implementation. This revision is documentation
only; it does not establish implemented or validated behavior. Source: issue #47 and its latest comments plus
subsequent explicit scope/design decisions. Base is main 5350fc8 after #49's
successful three-OS gate. #50 is queued separately and is not included.

## Scope And Withdrawn Alternatives

Cover normal/summary model requests and separate Run/model/shell time, at
Turn/Run/Thread/child-Thread scopes. Exclude every embedding producer and
standalone RAG: coverage is out_of_scope/not_recorded, with no zero usage,
request counts, unused embedding rows/columns or owner totals. Index/index-CLI
files are not edited. A later separately authorized follow-up owns those hooks.

B is the only period design: all-time totals, coarse UTC-day totals and exact
terminal-instant additive totals. Reads select aggregate rows only. Withdraw A's
measurement-fact edge exception, minute-aligned restrictions, prefix/Fenwick
amplification, the generic future-byte reservation system, fixed-age metrics GC,
and any automatic reclamation triggered by the existing warning setting.

## Typed Schema Candidate

Two new tables, not a general job framework:

1. `usage_aggregate`: typed sums/counts, fixed purpose/outcome categories, three
   grains. It holds every public numeric total; no request JSON or body.
2. `usage_summary_attempt`: minimal begin/finish identity and active/restart
   observation only for actual summary requests, which have no model_attempt.
   Normal HTTP never gets a duplicate ledger row.

`usage_aggregate` proposed key and ownership columns:

```sql
thread_id TEXT NOT NULL REFERENCES thread(id) ON DELETE CASCADE,
run_id TEXT REFERENCES run(id) ON DELETE CASCADE,
turn_id TEXT REFERENCES turn(id) ON DELETE CASCADE,
scope_kind TEXT NOT NULL, scope_id TEXT NOT NULL,
grain TEXT NOT NULL, terminal_sec INTEGER NOT NULL, terminal_fraction TEXT NOT NULL,
purpose TEXT NOT NULL, outcome TEXT NOT NULL,
PRIMARY KEY(scope_kind,scope_id,grain,terminal_sec,terminal_fraction,purpose,outcome)
```

Checks restrict scope_kind to thread/run/turn and derive scope_id from its actual
owning FK: Thread rows have run_id/turn_id null, Run rows have run_id and no turn,
Turn rows have both. A composite `(run_id,thread_id)` FK uses existing Run ownership;
Create `UNIQUE turn(id,run_id,thread_id)` as the explicit target of a composite
Turn ownership FK on both new tables; the existing `run(id,thread_id)` UNIQUE
already supplies the Run target. Also validate ownership by primary-key lookup
in the writer transaction. Do not invent unchecked polymorphic owners. Thread cascade alone reclaims all its
aggregate scopes even when run/turn metadata survives. Current Thread deletion
is soft: its existing deletion transaction explicitly deletes these rows and
summary attempts; SQL cascade also protects physical deletion. Forks inherit no
consumption counters from shared history nodes.

Grains: all_time uses zero/empty instant sentinel, day uses normalized UTC day
start and empty fraction, instant uses exact normalized terminal instant.
Purposes: normal, summary_manual, summary_automatic, shell, run_lifetime. Outcomes:
success, non_success, not_started (normal/summary only); duration-only categories
use their explicit terminal outcome, without creating token-unknown counts.
No Cartesian product of empty categories is materialized.

Wide typed payload columns, rather than one row per metric:

- attempt_count, client_request_count, client_request_unknown_count,
  pre_request_failure_count, and an explicit count-overflow flag.
- Each of the five #49 token fields: known_sum (nullable), unknown_count,
  overflow flag. Explicit reported zero is known; all unknown is not known zero.
- elapsed_ms_sum, elapsed_ms_unknown_count and elapsed_ms_overflow for the
  category's own clock. Run/model/shell remain separate categories, not added.
- First/last attempt-start instants and aggregate revision/coverage metadata.

Concrete payload column families for the DDL review (not JSON blobs):

```text
attempt_count, client_request_count, client_request_unknown_count,
pre_request_failure_count: nullable INTEGER; count_overflow: INTEGER boolean
input_sum, cached_sum, output_sum, reasoning_sum, total_sum: nullable INTEGER
input_unknown, cached_unknown, output_unknown, reasoning_unknown, total_unknown: INTEGER
input_overflow, cached_overflow, output_overflow, reasoning_overflow, total_overflow: INTEGER boolean
elapsed_ms_sum: nullable INTEGER; elapsed_ms_unknown: INTEGER; elapsed_ms_overflow: INTEGER boolean
first_start_sec, last_start_sec: nullable INTEGER
first_start_fraction, last_start_fraction: nullable TEXT
revision: INTEGER; logical_payload_bytes: generated INTEGER
```

Numeric totals start null until a known observation, not fabricated zero. Each
unknown counter starts zero; a known zero observation establishes sum=0. If the
shared count-overflow flag is set, publish those counts as unknown/overflow rather
than expose any saturated value. Text and enum checks restrict writer-generated
keys/fractions; counts/sums have nonnegative checks. The logical-byte expression
is exactly 8 times the number of non-null INTEGER payloads (excluding the generated
byte field), plus 1 per non-null boolean flag, plus the sum of octet_length over
stored TEXT payloads including keys/identifiers/timestamps. Treat booleans only
once, not also as integers. These named fields plus the ownership/key fields
fully define that expression; physical indexes/pages are not in this logical sum.

Each scalar is a nonnegative signed-64-bit SQL value with CHECK constraints;
Rust uses checked arithmetic. Overflow publishes null plus overflow, never a
clamped exact number. Unknowns are field-specific; do not infer cached, reasoning
or total tokens. Empty-but-complete scopes may report known empty request counts;
initializing or excluded scope/categories cannot masquerade as complete zero.

Indexes: the primary aggregate key supports scope/grain/time seeks; add a
Thread-owned deletion lookup and live `thread(parent_thread_id,id)` traversal
index. Source pending/active indexes below are partial. A declared/generated
logical-byte expression has its own covering index. Exact migration number is
assigned with the integrator against latest main; do not collide with parallel
migrations or alter their fixtures speculatively.

## Source Markers And Summary Ledger

Normal requests reuse model_attempt id and lifecycle; add a small accounting
marker (legacy_pending / observer_pending / accounted), not another normal HTTP
ledger. Run and shell sources add accounted markers for terminal initialization
and exact once. Preserve source terminal state/error/output/CAS semantics.
The metrics writer receives typed StandardUsage directly from the completion
observation; only old-row initialization uses #49's SQL scalar extraction.

Summary-only ledger columns: generated id, Thread and optional actual Run/Turn
FKs, purpose, start instant, nullable first terminal instant, state (active or
settled). Begin inserts one admitted active row with a fresh internal UUID; the
caller cannot recreate an old id. Finish only updates an existing begun active
id, adds the same typed values to aggregates, then settles the marker in the same
transaction. No INSERT/UPSERT on missing, deleted or settled finish. Cached summary
reuse has no HTTP attempt and no charge. Empty/refused/failed summary attempts
still record actual reported consumption. At startup, abandoned active records
settle interrupted/unknown, never repeat the request. Scope deletion cascades;
no independent age/pressure deletion is introduced.

### Summary Payload Bytes And Existing Storage Lookup

The summary-only table is counted too; it is not free operational metadata.
Concrete reviewed-direction DDL for its fields and generated logical payload:

```sql
CREATE UNIQUE INDEX usage_turn_owner ON turn(id,run_id,thread_id);
CREATE TABLE usage_summary_attempt (
 id TEXT PRIMARY KEY NOT NULL,
 thread_id TEXT NOT NULL REFERENCES thread(id) ON DELETE CASCADE,
 run_id TEXT REFERENCES run(id) ON DELETE CASCADE,
 turn_id TEXT REFERENCES turn(id) ON DELETE CASCADE,
 purpose TEXT NOT NULL CHECK(purpose IN ('summary_manual','summary_automatic')),
 started_sec INTEGER NOT NULL, started_fraction TEXT NOT NULL,
 first_terminal_sec INTEGER, first_terminal_fraction TEXT,
 state TEXT NOT NULL CHECK(state IN ('active','settled')),
 logical_payload_bytes INTEGER GENERATED ALWAYS AS (
   octet_length(id)+octet_length(thread_id)
   +coalesce(octet_length(run_id),0)+coalesce(octet_length(turn_id),0)
   +octet_length(purpose)+octet_length(started_fraction)+octet_length(state)
   +coalesce(octet_length(first_terminal_fraction),0)
   +8*(1+(first_terminal_sec IS NOT NULL))
 ) VIRTUAL,
 CHECK((state='active' AND first_terminal_sec IS NULL AND first_terminal_fraction IS NULL)
    OR (state='settled' AND first_terminal_sec IS NOT NULL AND first_terminal_fraction IS NOT NULL)),
 CHECK(turn_id IS NULL OR run_id IS NOT NULL),
 FOREIGN KEY(run_id,thread_id) REFERENCES run(id,thread_id) ON DELETE CASCADE,
 FOREIGN KEY(turn_id,run_id,thread_id) REFERENCES turn(id,run_id,thread_id) ON DELETE CASCADE
);
CREATE INDEX gc_bytes_usage_summary_attempt
 ON usage_summary_attempt(logical_payload_bytes);
CREATE INDEX usage_summary_active ON usage_summary_attempt(thread_id,id)
 WHERE state='active';
```

Both tables use the shown composite Turn FK and the same explicit UNIQUE target;
no FK references a non-unique tuple. Thread/Run/Turn shape CHECKs prevent a
Turn-only owner without its Run. Admission also verifies the referenced owners
are not soft-deleted. The new tuple index scans only ownership metadata once
when migration builds it; schema-only migration is not a claim of O(1) index
construction on old data. No usage JSON or aggregate backfill runs in migration.
There are no boolean payload fields in this ledger, and the generated field does
not count itself. Fraction/state/purpose/ids are TEXT bytes, start is one INTEGER,
nullable terminal second contributes 8 only when present. No nested/usage JSON.

Create `gc_bytes_usage_aggregate(logical_payload_bytes)` for the first table using
the payload formula already enumerated. In `src/gc/usage.rs::HISTORY_QUERIES`, add
both explicit covering-index sums:

```sql
SELECT coalesce(sum(logical_payload_bytes),0)
 FROM usage_aggregate INDEXED BY gc_bytes_usage_aggregate;
SELECT coalesce(sum(logical_payload_bytes),0)
 FROM usage_summary_attempt INDEXED BY gc_bytes_usage_summary_attempt;
```

They contribute to Logical.history, StorageDetails.logical_history_bytes and the
same logical_usage.total() used by require_capacity_tx. Never count the generated
byte field itself, twice-count boolean flags or include B-tree/index pages.
The small coverage/revision metadata under app_meta `usage.*` also contributes
UTF-8 key/value bytes through a matching covering expression index and history
sum. Existing source accounting markers are operational metadata, like source
state, not a second copy of the measurements. Tests independently recompute both
new-table formulas and metadata bytes after begin, finish, rollback and deletion;
EXPLAIN must show covering scalar reads without payload Function opcodes.

## Terminal And Client-Observation Rules

Counts distinguish admitted attempts from client invocations. Pre-request
failures are not client calls. `client_request_count` means a client invocation
whose execution was observed, never proof of POST bytes or provider receipt.
A crash in an unobserved admission-to-call gap yields unknown invocation, not an
invented zero or definite call. Preparation failures without an admitted
model_attempt/summary id are not invented model-request rows.

After stream_response returns, capture its typed usage and terminal observation
before the current driver cancellation early return discards it. The completion
observer owns a single observer_pending -> accounted CAS. Normal adoption/failure
and metrics finalization share their existing DB transaction whenever they settle
together. A request that actually ran must not disappear merely because output
adoption becomes obsolete; a metrics-only finalization may record consumption
without adopting late output or rewriting settled Core audit fields. A deleted
scope/missing source is obsolete and is never recreated.

Finalization table (implementation is reviewed alongside #53/#56):

| Observation | Accounting |
| --- | --- |
| admitted but definitely no client invocation | attempt + not_started; client count 0; no invented token usage |
| client completed and Core completion adopted | one success with reported typed usage |
| client failure/cancellation/received-incomplete | one non_success, retaining any reported fields |
| client finished but Core adoption is obsolete/cancelled | one non_success/discarded observation; consumption retained, no late adoption |
| failed -> incomplete after the same observation | same non_success, no second request or period movement |
| duplicate finish or obsolete/missing source | no additions; no row recreation |
| restart before client invocation/usage is established | invocation/usage unknown, never inferred zero |

first_terminal_at is fixed by that observation, not a later trim/adoption delay.
Retries are new ids; cross-success/non_success correction is outside current
source semantics and is not added. Do not subtract from overflowed sums and guess
an exact replacement. Observer-pending live work is excluded from initialization
and protected from source GC until its actual completion settles it. On restart
its owning observer no longer exists and it may be settled unknown. Active
observations remain separate from terminal totals.

Run/shell terminal Applied CAS updates their sums in the same transaction,
including stop-before-spawn, bulk cancellation, shutdown fallback and restart.
Unknown restarted shell elapsed is unknown; never synthesize monotonic duration
from wall clock. Turn.run_ms is null/not_applicable with reason; its own lifetime
is separately labeled. Run lifetime is counted once at Run/Thread, not per Turn.

### Function-Level Hooks And Cancellation Races

These are accounting hooks, not relaxations of existing Core predicates:

| Existing Function/Boundary | Required Accounting Behavior |
| --- | --- |
| request::Snapshot::admit_attempt_tx | Insert the existing source with observer_pending marker; no duplicate normal ledger. |
| driver::drive, stop check immediately after prepare_attempt | If it returns before stream_response, settle DefinitelyNotInvoked in the stop/cleanup DB transaction. |
| request::stream_response | Record client invocation only when the existing client future is actually polled; capture its return's typed values and finish instant before any cancellation branch or other await. |
| adoption::adopt_response / state::complete_attempt | Pass the retained observation into the same transaction; Core Applied/AlreadyApplied rules remain unchanged; independently CAS accounting once. |
| retry/overflow/trim -> state::fail_attempt or incomplete_attempt | Pass the same observation and captured instant; failed-to-incomplete does not create another request or change first terminal time. |
| state::cancel_run_attempts / stop_drained_runs | May cancel Core state, but do not account live observer_pending requests as unknown merely because cancellation wins first. |
| driver::drain_and_stop / owning driver exit cleanup | Settle the captured observation before releasing ownership; no returned usage is discarded by the cancellation early return. |
| app startup -> state::interrupt_previous_work | Under the exclusive owner lock, before new producers start, settle prior-owner pending observations unknown once. |
| live backfill/GC | Never steal an observer_pending live request; legacy_pending alone is initializable. Missing/deleted scopes are not recreated. |

Accounting CAS is a conditional UPDATE of the existing marker plus aggregates
in one transaction. All paths that finish an actual call use that same helper.
Core cancellation may therefore win first while accounting remains pending; its
real client-return callback subsequently wins accounting and preserves reported
fields. It cannot restore Core state/error/audit/output. A duplicate callback or
AlreadyApplied source path loses the accounting CAS and adds nothing. A genuine
returned call with obsolete adoption counts non_success with reason discarded,
not as an uninvoked request. Deleted Thread/Run/Turn or missing source is obsolete
and contributes no recreated row. New generation does not erase old consumption.

Immediately before the client call, cancellation caught by the outer drive check
means DefinitelyNotInvoked. Cancellation after the client future has been polled
means Invoked, even if its internal cancellation check prevents a POST; the count
is expressly a client invocation, not transmission/provider receipt. The
invocation witness is set inside the polled wrapper with no intervening await,
not when merely constructing an async future. Restart without that witness
retains invocation_unknown. Do not turn a pre-request failure into known usage=0.

### Producer Exit, Abort And Restart Ownership

An admitted observation belongs to the existing driver/compaction task, not a new
observer task or queue.
Use the current DriverSlot/compaction owner cleanup context to retain one optional
pending completion, with a synchronous tiny in-memory handoff before an await;
never put it only in a stack value that cancellation drops. This is one owned
value for the existing in-flight request, not another registry, delivery queue or
observer worker. The existing owner exit/error/stop/shutdown cleanup consumes it.
An admitted DbWorker job already owns its captured values and executes even if
its awaiting caller disappears; losing that reply does not undo its accounting
CAS. If DB admission has not happened, the owner's retained value remains the
cleanup source. A CAS probe after an uncertain reply distinguishes applied from
still pending without resending HTTP or inventing a second completion.
 Keep its id and at-most-one returned typed completion as
an owned value in that producer's existing cleanup context. Capture it before
any cancellation check or await after the client returns; normal/stop/error
finalizers consume it through the idempotent writer. Do not rely on an async Drop
or spawn a detached cleanup worker. Summary HTTP is sequential; cached/reused
parts create no observation. Persist an ended failed normal call before beginning
another summary/client request so a completed observation is never overwritten.

On normal return, cancellation, error and existing cleanup/join, first settle any
retained completion; only a request with no returned completion may settle as
unknown/producer_interrupted. Keep invocation-known vs invocation-unknown separate.
A cleanup CAS must not race ahead of a live future that can still report usage.
If an exceptional hard abort/panic prevents its existing finalizer from running,
expose orphan/pending_unknown coverage (not falsely active/known zero) until the
owning cleanup or next startup can prove the producer is gone. No metrics-specific
runtime pin or new history root is installed; dropped input/replay/file leases
follow existing RAII, so the tiny pending marker cannot permanently pin payloads.
A returned completion already retained by the owner must be preferred over the
unknown fallback. Actual storage failure leaves uncertain/pending facts rather
than pretending a successful settlement.

Startup acquires the existing exclusive data-directory lock before DB recovery.
Its interruption transaction runs before Control/maintenance/new producer admission.
Thus old-owner unknown settlement and a live old-owner callback cannot coexist.
A normal maintenance pass never applies that startup assumption to a live owner.
After cleanup/restart, a repeated callback sees accounted/missing and adds nothing.
Tests must abort/drop at pre-call, in-call, returned-before-cancel and DB-admission
boundaries, proving coverage, no permanent metrics pin and no lost returned values.

## Aggregate-Only Arbitrary Periods

Accept arbitrary RFC3339 half-open intervals without minute/day rounding:
`since <= first_terminal_at < until`. Normalize UTC whole seconds and canonical
decimal fraction (remove trailing zeros; zero is empty). Keep full accepted
fractional precision in boundary comparisons, not an unconditional millisecond
cast. Scope/time index comparisons must be equivalent for offset spellings and
same-instant fractions; explicit parse/ordering tests precede implementation.

Read fully contained UTC-day aggregate rows and instant aggregates for the at-most
two partially included days. An interval within one day reads that interval once.
Same-instant/scope/purpose/outcome contributions add to the saved instant row.
No model_attempt/shell/Run scan, JSON extraction, measurement-ledger read, or
fallback from a dense edge occurs in a public totals query.

One shared budget covers interior/edges/all children: 4,096 aggregate rows plus
one sentinel. Fetch/step bounded rows before adding; an unbounded SQL SUM behind
LIMIT is prohibited. Dense boundaries return partial, requested and covered time
ranges, per-scope coverage reasons and continuation. Page totals are not presented
as full-period totals. Cursor binds scope/filter/revision; new historical
initialization or concurrent changes expire it rather than mixing snapshots.

Worst case: every request has a distinct terminal instant, so instant-row storage
and the work needed for a complete paged answer grow linearly. Do not claim a
complete exact answer in one bounded query. This cost is explicit and verified
with logical bytes, physical/index bytes and VM steps, not just wall time.
All-time reads use fixed aggregate category rows, independent of event history.

### Ordered Segments, Seek Tuple And Covered Watermarks

Enumerate selected scopes deterministically by bounded lineage traversal with
children ordered by id; recompute that at-most-100 list on continuation and bind
its hash. Within each scope process phases in order: left partial-day instant
rows, fully-contained day rows, right partial-day instant rows. Empty segments
are omitted. If both bounds fall in one day, use exactly one instant segment
[S,E), not overlapping left/right scans. Aligned midnight bounds make the adjacent
instant segment empty. A complete day never contributes both day and instant rows.

Within a fixed scope/phase, rows are ordered by the full key
`(terminal_sec,terminal_fraction,purpose,outcome)` using BINARY text comparison.
The cursor seek position is `(scope_index,phase,last_full_row_key_or_start)`;
it also binds instance/filter/selected-scope/revision fingerprints. A timestamp
alone is not a seek cursor. One phase's pseudo-query is:

```sql
SELECT /* typed aggregate columns and full key */
 FROM usage_aggregate
 WHERE scope_kind=:kind AND scope_id=:id AND grain=:phase_grain
   AND (terminal_sec,terminal_fraction)>=(:lo_sec,:lo_fraction)
   AND (terminal_sec,terminal_fraction)<(:hi_sec,:hi_fraction)
   AND (:at_start OR (terminal_sec,terminal_fraction,purpose,outcome)
        >(:last_sec,:last_fraction,:last_purpose,:last_outcome))
 ORDER BY terminal_sec,terminal_fraction,purpose,outcome
 LIMIT :remaining_budget_plus_one;
```

Production SQL uses separate at-start/keyset statements so an OR cannot defeat
the seek index. The outer scope and phase ranges are fixed from requested bounds,
never inferred from the last observed timestamp. Read at most remaining+1, consume
only remaining, and stop on the first unconsumed sentinel. Carry the last consumed
full key, not an advanced timestamp. If a phase is exhausted, move to the next
phase/scope with at_start. With zero remaining, seek LIMIT1 through empty phases
until one sentinel or the end; no further nonempty rows are read. Across the whole
page this consumes <=4,096 rows and at most one unconsumed aggregate sentinel.

Responses distinguish `page_totals` and exact `page_key_ranges` from
`cursor_chain_coverage`. The latter reports each selected scope's fully completed
segments and its open phase's complete-time prefix. An unvisited scope has an
empty prefix [S,S), not the watermark of the already visited child. Never claim
one global covered interval from the furthest visited child. A derived all-selected
prefix is their intersection; descendant-limit/initialization incompleteness stays
separate. `complete=true` requires all selected scopes/phases and the requested
bounded lineage coverage to be complete, not merely a full page.

If the sentinel shares terminal instant t with the last consumed row but differs
in purpose/outcome, the complete-time prefix stops strictly before t. Return
`partial_instant=t` and full key coverage for the fragment; its known page
contributions are not described as the full instant or full period. Later pages
resume the rest of t's categories. A valid cursor chain can complete that instant,
but each page still returns page totals, not an invented cumulative total.

Example: at instant t the ordered rows are normal/non_success, normal/success,
summary_manual/success. With one remaining slot the first is consumed; the cursor
retains its entire key and the sentinel is normal/success. Coverage is [lo,t) plus
a key fragment at t; next page includes the latter two rows without skipping t.
If child A completes and child B is only partially read, A's chain coverage is
complete and B's is its own prefix; unvisited child C has no covered time. A full
UTC-day row and its neighboring left/right instant rows remain disjoint even when
a page boundary falls exactly at midnight.

Revisions are read from bounded per-scope/Thread metadata under app_meta usage.*,
not computed by scanning period rows. Any selected-Thread usage update, historical
initialization or scope-list change invalidates continuation explicitly. This
adopted conservative policy can prevent pagination from completing on a constantly
active Thread even for a closed past period. Record that limitation; do not silently
combine snapshots, claim eventual completion, add historical snapshot tables or
weaken invalidation without separate policy approval. UI retains neither mixed
page totals nor stale full-coverage claims after cursor_expired.

## Minimal Capacity And Retention Policy

Use existing limits.data_max_bytes (default 10 GiB) and the existing storage_limit
admission/terminal exceptions. data_warning_bytes (default 8 GiB) stays a warning,
not a new metrics-GC trigger. No new quota, global reservation manager, fixed age,
watermark or timer. Existing tool_raw retention applies to raw shell output only.

Define metrics logical payload bytes from actual stored text bytes and typed
scalar widths in a generated SQL expression; index it for existing logical-history
accounting. The adopted logical convention counts non-null INTEGER payloads as 8 bytes,
boolean flags as 1 byte and TEXT as octet_length, excluding SQLite record/page/
index overhead. This is an explicit logical definition, not physical file size.
Final DDL enumerates the exact column expression; tests compare it after inserts,
updates, cascades and rollback. Do not trust an application-maintained stale byte
counter or load large payloads to compute it.

At begin, apply the existing guard in the source/summary admission transaction,
including the newly inserted immediate payload in its logical check, before a
client invocation. If it reaches the configured limit, roll back begin and return
storage_limit. Do not reserve all possible future aggregate rows or promise a
strict future-result byte ceiling. Terminal recording may grow logical usage over
max, just as existing terminal writers may; source/metrics updates are atomic.
Actual DB persistence failure rolls both back and invokes existing storage-failure
handling. Over-limit state prevents later new work, not stop/terminal/GC/read.

Retain all-time, coarse and instant aggregate rows while their scope exists;
summary markers live with their scope. Deletion transaction removes them together.
No pressure reclamation is introduced now: this avoids undefined historical
boundary coverage loss and policy changes. Consequence: a long-lived scope's
metrics can eventually help fill the existing limit, and arbitrary-old exact
periods stay available only while data remains and queries finish pagination.
Any future pressure compaction needs separate coverage/atomicity policy approval.

## Bounded Initialization Without A New Worker

Migration is schema/index/empty metadata only. Old terminal model/Run/shell rows
use indexed pending markers. A small app_meta coverage/revision record identifies
initialization and historical summary coverage start; it is not another job queue.
Fresh scopes have no invented historical requests. Old scopes with unrecoverable
summary history expose its coverage boundary even after normal-source backfill. Every batch (at most 256 sources) marks and aggregates
in one short DB transaction; new terminal writes use the same exclusion guard.
GC accounts a pending source before clearing its only usage or defers that source;
never race backfill with deletion. Historical summary attempts absent from durable
records remain coverage_unknown, not reconstructed from overwritten cache.

The existing Control maintenance task and handle own initialization. Instead of
one batch per its 60-second collection cycle, its initialization phase repeatedly
submits one bounded DB job and awaits it, then yields to the existing runtime/DB
FIFO before submitting the next; check owner stop each batch. No new OS worker,
poller, interval, queue, detached task or synchronous startup/read loop. Startup
only starts the existing task, and reads use aggregates plus initializing/partial.
Keep the existing receipt/GC collection cadence, checking its existing deadline
between batches, rather than running full GC after every batch or delaying it
until initialization completes. This reuses the existing maintenance task's
60-second scheduling/cancellation responsibility, not a second timer/poller.
Byte-bound legacy rows without hiding SQL parsing cost: probe source byte
lengths first and process at most 256 records / 1 MiB source documents per DB
transaction. As with existing raw batching, a single older oversized row may
occupy its own transaction so initialization cannot starve forever. This is a
bounded-row progress exception, not a hard per-row parsing-time guarantee; #49's
recorded legacy SQLite parsing cost remains. Never load that document into Rust.

Test deterministic steps, queue fairness and notification generations, not elapsed
latency. A continuously nonempty DB queue and slow legacy parsing must not imply
an invented throughput guarantee. Shutdown cancels further submissions and joins
the existing maintenance handle; admitted DB transactions finish normally.

## Read Surface, Active Values And Validation

Existing Turn/Run/Thread get includes own all-time usage. Read-only usage get
handles children/period; no receipt. Children follow live parent_thread_id index,
visited set, depth64, at most 100 Threads including root. Deleted/cyclic/limited
branches produce honest partial reasons. Shared period budget is not per child.
Active-only source indexes have a separate cap256 plus sentinel; read-time counts/
elapsed and observed_at are separate from terminal sums. TUI F6/F8 open/Changed
queries the DB worker and caches by generation/revision; render never queries DB.

Model-only non-2xx extension: body.rs returns existing redacted ModelError plus
optional StandardUsage from its already acquired complete <=64KiB JSON body.
Preserve the existing diagnostic-only http_error call for read_json/catalog/
embedding without adding an unused compatibility framework or changing their APIs.
Responses/summary failure settlement receives typed values. Malformed, truncated,
interrupted and absent fields remain unknown; no raw error body/appendix storage,
new HTTP, retry or enlarged limit. Test exact cap/+1, partial fields and unchanged
shared diagnostics. Index/embedding wiring remains excluded.

Design/implementation review must check rollback, duplicate/replay, live cancellation,
obsolete adoption, observer initialization races, source GC, soft/physical deletion,
summary cache reuse/retry, unknown/overflow, exact fractional boundaries, page gaps,
children cycles/limits and UI query generations. Synthetic scale: 100k attempts,
10k shells, 1k Runs, 100 children. Assert EXPLAIN seeks, VM-step/row budgets and
logical/physical growth; no timing correctness assertions or paid/user DB tests.

## Integration And Remaining Gate

The leader adopts this specification and its observer-independent accounting,
capacity/retention and B paging policy. The schema, accounting module and regression
code have not been implemented. Independent design review must first resolve
P1/P2 findings, including the two byte expressions, seek/coverage details and
producer cleanup hooks; only then begin the authorized schema/implementation.
No additional age/pressure/embedding policy is introduced here.
Coordinate edits touching control/state.rs, db.rs, driver.rs and driver/tools.rs
with parallel #53/#56; keep helpers local to accounting and do not rework their
cancel/ownership semantics. Before push the integrator fetches/rebases current
main, checks diff/range-diff, obtains needed new-SHA review and runs the standard
check again, then normal main push and exact three-OS CI. #50 stays queued.
