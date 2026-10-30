# Usage Aggregates Proposal (#47)

Status: revised proposal, not adopted and not implementation authorization.
Source: issue #47, especially comments 6513644118 and 6513653608, and the
leader's subsequent design directions. #49 is reviewed separately. Do not mix
#47 into main before #49's exact native gate succeeds. No index/CLI edit is
part of this proposal. Endorsed directions below are not blanket approval of
storage, period or shared-API details that remain open.

## Adopted Scope Only

The latest explicitly approved scope excludes all embedding accounting and
standalone RAG. Complete this change for normal/summary model requests, success/
non-success/unknown usage, Run/model/shell time and Turn/Run/Thread/child-Thread
scopes. Publish embedding coverage as out_of_scope with reason not_recorded;
never create zero aggregates, empty numeric totals or fake request counts for it.
Do not add unused embedding columns/rows or owner accounting solely for embedding.
A small follow-up may add hooks after the parallel #42/#43/#55 index work lands.
`src/index/mod.rs`, `src/index/store.rs`, `src/index/search.rs` and index CLI remain
outside the edit scope. The previously proposed embedding HTTP wrappers/hooks and
origin attribution plan are withdrawn, not deferred requirements of this change.

## Endorsed Directions, Not Complete Design Adoption

- Typed dedicated aggregates, with bounded constant categories per scope;
  independent known sums, per-field unknown counts, overflow and request counts.
- Separate Run, model-request and shell time; never add them as effective work.
- Only a successful terminal CAS applies accounting in the same transaction.
  Repeated/obsolete results and receipt replay add nothing; rollback applies neither.
- One HTTP request remains one measurement across failed/incomplete reclassification.
- Small schema migration; indexed, bounded initialization on the existing
  maintenance DB worker, partial coverage until complete and GC protection.
- Separate bounded active observations and Thread-lineage child traversal;
  do not duplicate an entire parent Run lifetime into each Turn.
- Unrecoverable historical summary attempts are coverage unknown,
  not known zero. Do not infer them from overwritten/reused caches.
- Read-only CLI totals and generation-bound detail-screen caches updated through
  the existing DB worker on open/change, never on each render.

Still open: exact table/column budget, period implementation, point-fact lifetime,
source-marker duplication, outcome-correction mechanics and the complete
implementation plan. Commit an adopted
specification and decision record only after the leader confirms these details.

## Proposed Typed Aggregates And Ownership

`usage_total` has a key of scope kind/id, measurement category/purpose and
outcome. Scope is Thread, Run or Turn. Categories separate normal Responses,
summary requests, Run lifetime and shell duration.
There is a fixed category set, not one total row per attempt. Avoid a Cartesian
product of unused categories. A nullable sum plus an explicit overflow flag is
not the same as a reported zero; keep field-specific known/unknown counts.

Each sum/count uses checked arithmetic. Stored exact values are nonnegative
signed-64-bit integers; overflow publishes null plus overflow, never a clamped
number advertised as exact. The five #49 counts remain independent. Do not infer
cached/reasoning/total tokens or fill absent fields from other counts. A failed
request with some known fields contributes those fields and unknown counts for
others, in a separate non-success category. All-unknown is distinguishable from
all-reported-zero. Track first/last request start and fixed first terminal time.

The reason for a dedicated typed table instead of many Run/Thread columns is
localizing purpose/outcome/period dimensions and query projection. It is not a
new job framework. The conversation writer owns it; the existing DB worker owns
transactions. Thread/Run/Turn keys need explicit FK ownership or separate typed
key tables/checks, not an unchecked polymorphic string that silently orphans
rows. Exact DDL is pending. Thread deletion removes its totals, periods, point
facts and open measurement rows in the deletion transaction; deleted scopes are
excluded from backfill and descendant reads. No owner embedding totals are part
of this change.

All new table/index bytes must enter logical-storage accounting. Fixed aggregate
rows and worst-case period/point-row reservations count toward begin admission,
before HTTP. Reserve bounded terminal-write space as existing admission permits;
never issue an unadmitted paid request and assume its accounting can be dropped.
If terminal persistence still fails, source/result and aggregates roll back
together, preserving existing uncertain-storage handling rather than reporting
successful adoption. Detailed reservation sizing and failure tests remain open.

## Exactly Once And Outcome Corrections

Proposed outcome mapping for model attempts:

| Source Transition | Accounting Action |
| --- | --- |
| preparing/inflight -> completed | First measurement: success. |
| preparing/inflight -> failed/incomplete/cancelled | First measurement: non-success; unknown fields remain unknown. |
| failed -> incomplete for the same received request | No new request; both map to non-success, so no category movement. |
| terminal -> same terminal / obsolete generation | No addition or replacement. |
| persisted result reclassified across success/non-success, if later explicitly supported | Correct the same measurement atomically; do not keep the old outcome total. |

The current model_attempt preparing state can fail before an HTTP call. The
final schema must distinguish attempt/pre-request failures from actual attempted
HTTP requests rather than reporting an invented provider request count. Existing
model_attempt.started_at-to-first-terminal duration follows that saved attempt's
clock, not proof of server processing or receipt. The request-count naming and
pre-request time treatment require an explicit final decision; do not hide this
under the word request. Summary begin is immediately before its actual client
call, after preparation and storage admission.

Current completion CAS does not permit failed -> completed. Do not invent that
transition for accounting. If cross-outcome correction becomes possible, subtract
the saved typed contribution from the previous outcome and add it to the new one
in the same transaction, retaining id and first_terminal_at. Because an overflow
marker cannot be reversed into an exact sum by subtraction, either retain an
exact wider accumulator or disallow that correction until a bounded rebuild of
the affected aggregates is agreed. Never clear overflow by guessing. This is a
remaining design choice, not an implemented promise.

`first_terminal_at` is immutable for filtering and duration of that HTTP attempt;
later diagnostic reclassification does not extend the HTTP time or create a
second measurement. New actual retries have new ids. Run and shell terminal
CAS, stop-before-spawn, shutdown fallback and startup bulk interruption must be
covered, not just the ordinary completion callback. Unknown restart-recovered
shell duration is not synthesized from wall clock. Active values stay separate.

## Initialization, Maintenance And GC

Migration creates schema/indexes and initialization metadata only, with no
large source scan. The existing maintenance tick performs at most one batch of
256 eligible records, with a bounded work/byte budget and one short transaction;
it yields to normal DB work instead of looping to completion on startup/read.
No new worker, timer, poller or generic persistent work queue is introduced.
The existing maintenance owner and cancellation/join lifecycle remain responsible.

An indexed unaccounted predicate selects old terminal sources. New terminal
writes set their accounting marker together with aggregates; batch initialization
uses the same guard/transaction. Do not rely only on rowid/high-water cursors that
can miss concurrent writes. Reads return aggregate rows plus initializing/partial
coverage; they do not initialize an unbounded history or present a partial sum as
complete. Notifications must not turn every tick into an unbounded UI refresh.

Before GC removes a source or strips its only remaining old raw usage, account
it in the same GC transaction or defer its reclamation. Deleted Thread sources
are excluded, not resurrected into metrics. The legacy initialization path alone
may use #49's guarded SQL scalar extraction, with no large usage document loaded
into Rust. New terminal writes pass typed #49 values directly, not parsed JSON.
Historical summary requests without durable attempt facts remain
coverage unknown; cache counts cannot reconstruct lost retries or failures.

## Arbitrary Periods: Revised Alternatives (Decision Required)

The minute-aligned-only proposal is withdrawn. Accept arbitrary RFC3339 instants
with a half-open interval `since <= first_terminal_at < until`; do not round the
user's boundaries. Define representation/comparison at full accepted timestamp
precision before implementation. Coarse UTC buckets must handle offset input by
normalizing instants, not comparing different RFC3339 spellings lexicographically.
Run/model/shell duration is wholly assigned by first terminal time, not clipped
at range boundaries; a crossing interval is either included or excluded.

### A: Coarse Buckets With Bounded Typed Edge Facts (Recommended For Review)

Persist UTC day (or hour, pending measurement) aggregate rows in the same terminal
transaction. Entirely contained buckets read these aggregates only. At most two
partially intersected buckets read typed per-measurement facts through indexes
on `(scope_id, first_terminal_at, measurement_id)`. Their fields are already
validated numbers, outcome/purpose and durations, not raw usage JSON or response
bodies. If both boundaries fall in one bucket, read that interval once.

Apply one common query budget across interior buckets, both edges and all
children: proposed 4,096 aggregate/point rows plus one sentinel. Indexed seeks
and bounded stepping replace unrestricted SUM/source scan. A 100k-request edge
can therefore return partial; it cannot secretly scan 100k rows. Expose requested
range, covered bucket/edge intervals, per-scope completeness, initialization/GC/
row-budget reasons and continuation. A page sum is explicitly a page sum, not
the entire requested-period total. Cursors bind filters and aggregate revision;
concurrent backfill/correction invalidates the cursor rather than silently
mixing snapshots. All-time reads remain fixed aggregate-row lookups.

This preserves exact timestamps for the rows covered and requires far less
write amplification than a prefix tree. It does NOT guarantee a complete answer
in one bounded query for a dense edge. It is an explicit narrow exception to the
latest comment's aggregate-only read rule: edge readers add typed measurement
facts, never source usage JSON. The leader must approve this exception; it must
not be smuggled in as equivalent to reading only total rows.

### B: Fine-Grained Additive Edge Aggregates

Replace individual edge facts with pre-summed rows keyed by exact terminal
instant/scope/category/outcome. Their read rules and global budget are the same.
This adheres more literally to aggregate-only reads. Identical timestamps can
coalesce, but arbitrary provider activity may give every request a distinct
instant, making storage and edge stepping effectively one row per measurement.
It has no worst-case constant-time full-precision guarantee and still needs
partial/continuation. Using a fixed minute granularity instead would lose strict
boundary precision and is not an approved alternative.

### C: Prefix/Fenwick Time Aggregates

Bounded prefix/range decomposition can give complete exact-range reads with
roughly a fixed number of indexed nodes, but updates touch approximately 64
nodes per scope/category and may retain very many nodes. Neither that write/
storage multiplier nor a precision/range restriction is adopted by default.
This is the alternative if complete dense-edge answers under a fixed read budget
are required. Compare VM steps, new bytes and terminal-write work, not just a
single read wall time.

The unavoidable choice is strict arbitrary-boundary precision + bounded work +
compact storage, with partial answers (A/B), versus greater persistent/write
cost for complete bounded reads (C). Full source scans are not an alternative.

## Measurement Facts, Dedupe And Lifetime (Decision Required)

Normal model attempts already own lifecycle ids. Compare adding typed scalar/
first-terminal/accounting columns directly to model_attempt, Run and shell rows
against one minimal metrics sidecar. The former avoids a redundant normal-request
ledger, but A's edge read then accesses typed source columns and requires that
explicit exception. The latter supplies uniform edge indexes while duplicating
a small amount of fact storage. A source backfill marker may still be necessary
for indexed pending initialization; it is not a second copy of five numbers.

Summary requests lack a complete durable HTTP-attempt ledger today. They
need a minimal begin/finish record for active observation, exact once, interruption
and period facts. It contains generated id, owning Thread/Run/Turn ids and purpose, start/first end,
outcome and typed values only, never raw bodies/vectors/appendices. Cached summary
reuse creates no HTTP record. Rejected/empty/failed/aborted attempts still settle
their actual request record, without claiming provider usage if absent.

`begin_request_tx` generates a fresh internal id after quota admission; callers
cannot request recreation of a historical id. `finish_request_tx` only updates an
existing begun active id and applies aggregates in that same transaction. Missing,
deleted or terminal ids return obsolete/already-applied without INSERT/UPSERT.
Consequently a repeated finish after GC cannot recreate a row or add usage.
Startup settles active records as interrupted/unknown; it never repeats HTTP.

Proposed reclamation: retain all-time and coarse bucket totals while a scope
exists; reclaim terminal edge facts in bounded GC batches under an explicitly
chosen age/byte policy. Before pruning a point, mark the affected coarse bucket's
edge coverage incomplete in the same transaction. A full-bucket query still has
its exact total; a historical partial-bucket query reports boundary_facts_pruned
and unknown missing count, not zero. All-time totals survive point GC. Never
forget coverage metadata while claiming an exact historical edge. Exact age/byte
limits and reservation size require leader agreement.

## Active Values, Lineage And UI

Read active-only indexes with a separate budget (proposed 256 plus sentinel).
Expose active counts and read-time elapsed in a separate object with observed_at;
do not persist them into terminal totals. Time is an observation, not proof of
process liveness. Turn.run_ms is null/not_applicable with a reason; Turn's own
lifetime_ms remains separately labeled. Run/Thread totals contain actual Run
lifetimes once, never multiplied by Turn count.

Children follow a live parent_thread_id index, not owner_run_id. Track visited
ids, maximum depth 64 and at most 100 Threads including root, excluding deleted
Threads. Cycles and limits are partial coverage reasons. The global period row
budget spans all these scopes, not 4,096 per child. No-period totals read only a
bounded number of fixed aggregate rows per visited Thread.

Proposed CLI: existing turn/run/thread get exposes all-time own totals; a separate
read-only usage get handles children/period options without adding unrelated
flags to every command sharing ThreadTarget. Reads create no operation receipt.
F6/F8 detail-open and relevant Changed notifications query the existing DB worker,
with generation/revision validation and cached rendering only. No footer polling,
DB work on the UI OS thread or per-frame recomputation is added.

## Model-Only Shared HTTP APIs (Not Yet Authorized To Implement)

Minimal conceptual signatures, with actual names/visibility to be reviewed:

```rust
struct HttpFailureMeasurement {
    error: ModelError,
    usage: Option<StandardUsage>,
}
async fn http_error_measured(response: Response, context: &mut RequestContext<'_>)
    -> HttpFailureMeasurement;
// Existing diagnostic-only wrapper remains source-compatible.
async fn http_error(response: Response, context: &mut RequestContext<'_>) -> ModelError;

// Add an internal typed field, without exposing new protocol error-body fields.
struct AttemptFailure { /* existing fields */ usage: Option<StandardUsage> }

fn begin_request_tx(tx: &Transaction<'_>, scope: UsageScope, purpose: Purpose,
    started_at: &str) -> Result<RequestId>;
fn finish_request_tx(tx: &Transaction<'_>, id: &RequestId,
    usage: StandardUsage, outcome: Outcome, at: &str) -> Result<FinishOutcome>;
```

Parse the already acquired <=64KiB non-2xx body once, keeping the existing
status/classification, redaction, diagnostic text and limit behavior. Extract
known provider-envelope usage numbers only; never return/persist the whole error
body. Exact-cap complete JSON is eligible; truncated, interrupted, malformed or
unreported usage is unknown. Preserve partial known fields. No extra HTTP read,
retry or large-body load. The internal typed failure must not accidentally expose
new error-body fields in serialized protocol diagnostics.

Direct model-only call changes: `src/model/http/body.rs` adds the measured internal
result and retains the diagnostic wrapper; `src/model/http/response.rs` consumes
it for non-2xx Responses and attaches typed usage to its internal failure.
`src/control/driver/retry.rs`, `src/control/driver/overflow.rs`,
`src/control/driver/trim.rs` and `src/control/state.rs` pass the typed observation
to the terminal writer instead of reading saved JSON. Summary request settlement
in `src/compaction.rs` consumes the same failure measurement. Success and 200/SSE
failure envelopes use #49's same validated five fields.

Indirect shared callers remain unchanged: catalog/embedding use `read_json` and
its existing diagnostic-only `http_error` wrapper in `src/model/http.rs`.
No embeddings_measured API, typed embedding field, index hookup or RAG accounting
is introduced. This intentionally does not solve embedding failure accounting;
coverage is out_of_scope/not_recorded, not zero. Regression tests must establish
that shared catalog/embedding error classification, body limits and redaction
are unchanged without altering their caller APIs. Index and index-CLI files
remain untouched; their parallel coder does not need a new hook in this change.

## Planned Independent Validation

Use synthetic temporary stores and loopback only. Validate same-TX original/
aggregate changes, every terminal producer, duplicate/replay/rollback/storage
failure, startup interruption, init concurrent with new writes and GC, missing
finish after GC, scope deletion, reclassification, overflow and per-field unknown.
Non-2xx tests include fully known/partially known/unknown, exact 64KiB/+1,
truncation/transport failure and unchanged diagnostic redaction.

Use 100k model attempts, 10k shells, 1k Runs and 100 children. Assert EXPLAIN
indexed seeks, bounded VM steps/returned rows and explicit coverage/continuations;
do not use timers as correctness or hide dense-edge scans behind period flags.
Prove all-time reads independent of attempt/shell/Run history size and verify
UI cache/query generation through DB state and notifications with long watchdogs.
Native CI and real-model dogfood are separate evidence, not inferred from mocks.
