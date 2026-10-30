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

Execute indexed SQL in the existing DB worker. The existing get projections and
read-only usage query use the same aggregation implementation. Descendants follow
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
The approximate target is 100ms per read, not a flaky absolute CI assertion.

If the measured reads are sufficiently fast, add no schema, persistent totals,
producer/state/driver hooks or new workers. If too slow, report evidence and the
cause before proposing only one simple total row per Run/Thread for separate
approval. Do not preemptively redesign persistence. No paid API or user DB is used.

## Boundaries

Preserve #49 storage/normalization and current storage/HTTP diagnostics. No schema,
markers, client observer, summary ledger, new GC/retention policy or period scheme.
Index/index-CLI/RAG and parallel #53/#56 lifecycle changes are outside the edit
scope. #50 is queued after #47 completion. Independent review, latest-main
fetch/rebase and required rechecks precede normal main push and exact three-OS CI.
