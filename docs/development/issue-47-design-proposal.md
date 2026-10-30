# Model Usage Aggregates: Final Design Candidate (#47)

Status: B direction approved; this complete schema/implementation candidate still
requires leader and independent design review. No production/schema implementation
is authorized by this document. Source: issue #47 and its latest comments plus
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
Turn ownership is verified by primary-key lookup in the writer transaction.
Do not invent unchecked polymorphic owners. Thread cascade alone reclaims all its
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
no independent age/pressure deletion is introduced in this candidate.

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

Candidate finalization table (requires explicit review alongside #53/#56):

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

## Minimal Capacity And Retention Policy

Use existing limits.data_max_bytes (default 10 GiB) and the existing storage_limit
admission/terminal exceptions. data_warning_bytes (default 8 GiB) stays a warning,
not a new metrics-GC trigger. No new quota, global reservation manager, fixed age,
watermark or timer. Existing tool_raw retention applies to raw shell output only.

Define metrics logical payload bytes from actual stored text bytes and typed
scalar widths in a generated SQL expression; index it for existing logical-history
accounting. The candidate convention counts non-null INTEGER payloads as 8 bytes,
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

The schema, accounting module and regression code have not been implemented.
Leader confirmation is still required on this candidate, especially source observer
CAS vs obsolete Core adoption, exact byte expression and bounded initialization
interleaving/cancellation. No additional age/pressure/embedding policy is requested here.
Coordinate edits touching control/state.rs, db.rs, driver.rs and driver/tools.rs
with parallel #53/#56; keep helpers local to accounting and do not rework their
cancel/ownership semantics. Before push the integrator fetches/rebases current
main, checks diff/range-diff, obtains needed new-SHA review and runs the standard
check again, then normal main push and exact three-OS CI. #50 stays queued.
