# Saved Usage Totals (#47)

Adopted first version: [issue comment 6515082156](https://github.com/tya5/shellboy/issues/47#issuecomment-6515082156).
Measure indexed read-time aggregation before considering persistent totals.

## Read Contract

Expose all-time totals for Turn, Run, Thread and Thread plus bounded descendants.
Use saved model_attempt states completed, failed and incomplete and the validated
standard usage numbers from #49. Separate those states and per-field unknown
counts; missing/invalid values are not reported zero. Do not infer client calls,
provider receipt, cancellation observations or unrecorded requests. Ignore active
attempts in terminal totals and show read-time active observations separately.

Sum saved Run lifetimes, model-attempt start/end durations and terminal shell
elapsed_ms as three separate values, not effective work. Turn Run-time is N/A;
its own saved lifetime is a separate label. Invalid/missing durations are unknown.
Summary and embedding/RAG requests are not recorded here; disclose that coverage
rather than synthesizing requests from summary caches or returning known zero.
No since/until filtering is part of this version.

Execute indexed SQL in the existing DB worker. Explicit get projections and `usage get` use the same aggregation implementation;
internal metadata helpers used by writes/ordinary history refresh do not aggregate. Descendants follow
parent_thread_id, exclude deleted Threads, and respect existing bounded traversal
safety. Any limit/degraded coverage is explicit, never a complete total. Counts
and numeric overflow stay distinguishable from reported values. No receipts are
created by reading. TUI detail-open/Changed requests use generation-bound cached
results; no query runs on the UI thread or during rendering.

## Measurement Gate

First use synthetic temporary data: 100k model attempts, 10k shells, 1k Runs,
100 children. Exercise the actual public aggregation queries for all four scopes.
Record placement/distribution, states, JSON size/shape (standard small usage
separately from legacy large usage), indexes/EXPLAIN, SQLite version, Rust/build/
OS environment, cold/warm definition, repetitions, median/p95 wall time and VM
steps. Verify exact totals/unknown/active/child behavior independently of timing.
The initial100ms target is advisory, not a CI threshold. After removing obvious
query/index/JSON/date waste, roughly500ms for an extreme100k-request Thread is
acceptable for on-demand DB-worker reads; it is not UI-thread/render latency.

If reads meet that practical goal, add no schema, persistent totals,
producer/state/driver hooks or new workers. If too slow, report evidence and the
cause before proposing only one simple total row per Run/Thread for separate
approval. Do not preemptively redesign persistence. No paid API or user DB is used.

## Boundaries

Preserve #49 storage/normalization and current storage/HTTP diagnostics. No schema,
markers, client observer, summary ledger, new GC/retention policy or period scheme.
Index/index-CLI/RAG and parallel #53/#56 lifecycle changes are outside the edit
scope. #50 and other work are outside this task; stop after #47 main exact CI succeeds. Independent review, latest-main
fetch/rebase and required rechecks precede normal main push and exact three-OS CI.

Current UI read-cache contract: explicit F8 usage/get detail forms register one
scoped request, refreshed on Changed using
existing worker admission; Esc/new operation/Thread switch closes it. Close carries
an epoch through the existing bounded control mailbox so a saturated ordinary
queue cannot rearm an older request. No Core cancellation/producer path changes.
Reference navigation (`FollowManagement`) preserves the existing one-shot query:
it closes the prior generation and opens an immutable result without registering
a Changed refresh. F6 pages inspect only the cached response. Explicitly re-query
the F8 form or follow a reference again for newer values; neither surface promises
a freshness deadline. Each actual reference aggregation uses `open` once under
the same slow-only threshold below. Closing/switching drafts leaves no hidden
usage refresh; ordinary Thread selection stays metadata-only, without aggregation.

Changed refreshes use a fixed three-second post-result cooldown and the existing
500ms UI tick; one pending/one in-flight scoped read, no added timer or sleep job.
Normal eligibility is within3.5s plus existing worker/DB backlog, not a hard deadline.
Only monotonic DB aggregation durations >=100ms enter the existing diagnostic
writer as usage_totals_slow with typed trigger/scope/elapsed fields and no content
or IDs. Faster reads emit no aggregation diagnostic. Trigger context follows the
existing request task into the DB job; it is not a protocol option or setting.

## Bounded grouped CLI readback (#101)

`usage get --group-by hour|day|thread|model` uses the existing finite presentation pool
and read-only WAL snapshot, not the write DB worker. Ungrouped totals are unchanged.
Without an explicit scope, grouped reads cover non-deleted Threads. Thread groups
provide the multi-conversation alternative; one explicit Thread/Run/Turn filter
and bounded existing descendant selection remain available.

The saved-start interval is half-open, defaults to the last 24 hours, and cannot
exceed 31 days. Unknown starts are excluded. Each scan page reads at most 10,000
candidates plus one lookahead in existing primary-rowid order, before time/scope/
deletion filters. UTC `started_at` string ranges gate scalar JSON extraction;
the bounded pass validates current UTC format and exact fractional boundaries.
Missing/malformed/non-current time formats increment `excluded_time_count`;
valid UTC rows outside the interval increment `outside_period_count`.
No migration or index is added. A plain started_at index may be proposed only
with measured row/VM-work evidence and separate approval. JSON projection and
retained samples are bounded by the candidate page. No table, worker, queue,
producer hook or persistent aggregate is added.

Responses contain at most 168 groups (default168, `--limit` can lower it).
A cursor first pages groups in the same candidate window, then continues the
next candidate window. `scan_truncated` explicitly marks incomplete coverage;
statistics are exact for the displayed scan window, not whole-period estimates.
The same group can occur in multiple scan windows. Do not average percentiles or
ratios across pages. Cursors bind filters, limits, owner instance and an insertion
watermark; they do not promise an immutable snapshot across separate requests.

Time buckets use owner local time by default (`local`), or `UTC`/a fixed
`+HH:MM`/`-HH:MM` offset via `--timezone`. Local hours retain their offset so DST
repeated hours remain distinct; local days use calendar date. Arbitrary IANA zone
names are not accepted. No timezone dependency is added.

Each group preserves per-state numeric sums/unknown/overflow fields. Input
statistics use known input tokens on completed/failed/incomplete attempts only:
mean, max, nearest-rank p50/p95, known/unknown counts and the max attempt identity
with its Thread/Run/Turn references. Cache ratio is sum(cached)/sum(input) over
terminal attempts with both fields known; zero denominator is null, inconsistent
provider measurements are not clamped. Paired/unknown counts expose coverage.
No money, unrecorded summary/embedding usage or inferred tokens are added.


### Thread shell exit grouping (#272)

`usage get --group-by shell-exit` groups terminal shell executions by their owning
Thread and persisted `exit_code`. Ownership follows `shell_execution.owner_run_id`
to `run.thread_id`; no attribution is inferred from the current Turn. Missing exit
codes remain a separate unknown group. The saved `started_at` period is half-open,
and a page scans at most 200 shell rowids plus a lookahead before applying scope,
state and period filters. Cursor pages retain the original candidate watermark;
`scan_truncated` and page-local statistics make partial coverage explicit. No
schema/index migration or write-side aggregation is added.
