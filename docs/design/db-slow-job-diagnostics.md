# DB slow-job diagnostics (#201)

## Goal and status

The implementation is awaiting independent review.
When a DB job exceeds its limit, identify the statement, evidence of why it was
slow, and the admitted request's origin without exposing private content.
Extend the existing `db_job_slow` and `db_job_active` events (#190); do not add a
new table, queue, worker, or dependency.

## Connection-level instrumentation

Instrument all SQLite connections used by the DB worker at the lowest practical
layer, including inspection reads. The always-on fast path subscribes to
SQLite `trace_v2` statement, profile, and row events, using SQLite's elapsed
time without a separate per-statement timer. Only a statement reported at or above 1 ms is sampled for
`FULLSCAN_STEP`, `SORT`, `AUTOINDEX`, and `VM_STEP`. SQLite counters can include
previous executions of a reused prepared statement; report them as cumulative
statement-lifetime snapshots, not per-execution or job totals. No sample means
unknown, not zero. A row callback counts returned rows across the current job,
including its active statement; `row_scope` is `job_cumulative`. This count
is distinct from sampled statement-lifetime counters. Tests may opt into
exhaustive observation to check indexed
hot reads independently of the production sampling threshold.

The existing progress handler captures at most one backtrace on the executing
thread after a job exceeds its limit. The statement active at that instant is
frozen for the active and slow records, and a later statement must not replace
its attribution. Until an overrun capture is available, `sql_scope` marks a
last sampled statement rather than claiming it is currently executing. Requester
category and execute kind are retained where available; argument values are never logged.

## Events, privacy, and bounds

Emit a normalized SQL shape for any statement, not an exact-statement allowlist.
String, blob, and numeric literals and positional parameters are replaced by `?`;
unsupported named parameters use the hash fallback. Identifiers are also
suppressed because identifiers may contain user data.
Whitespace is collapsed. Unsupported or ambiguous syntax falls back to a
SHA-256 hash of the unexpanded statement, never the statement itself. Normalized
output is limited to 256 bytes to bound event size while retaining a useful
query shape; longer shapes use the hash rather than a potentially misleading
truncation. Hashes can reveal equality of statements and are not encryption.
Bindings, content, paths and source lines are never emitted. Safe fixed plan
categories may be generated on a separate read-only connection for reviewed
static queries; this separate plan allowlist does not restrict SQL shapes.

An Apple M1 macOS release microbenchmark measured the SQLite PROFILE no-op path
at 11.513125 ms versus 10.615541 ms baseline for 50,000 cached reads, about
18 ns/read for SQLite timing plus callback and benchmark effects, not an
isolated clock measurement. Other builds ran concurrently; this is noisy and
not an end-to-end overhead claim. SQLite provides the elapsed time, so there is
no additional per-statement Rust timer. The existing progress handler reads
`Instant` every 1,000 VM steps; seven 10-million-call optimized loops on the
same machine measured 18.642-20.855 ns/call (median 19.273 ns), without load
control. This is a Rust clock-call microbenchmark, not SQLite timer cost. The
production threshold is 1 ms; statements
below it have unknown counters, not zero. This is an explicit production
tradeoff: a slow job can still consist of many individually fast statements.
Test-only exhaustive instrumentation observes every execution and checks the
growing-table scans before they become slow.

## Verification and cost

Use a large synthetic database in tests for hot operation kinds and assert
`FULLSCAN_STEP == 0` on tables that grow. Pair these tests with the writer-lane
guard being added for the hotfix. Cover per-job and rate bounds, admission
origin, overrun attribution, read-only plan collection, and absence of private
values from emitted events.

Measure and report the overhead of the hooks, including per-statement tracing,
on the benchmark, with workload and environment and a baseline without hooks.
Inspect release-profile backtrace symbols and measure binary-size impact before
deciding whether to use `debug = "line-tables-only"`; that setting is not yet
adopted. No release-profile overhead or binary-size claim is made until measured.

A preliminary debug-profile measurement on Apple M1, macOS 26.3.1, used an
in-memory 10,000-row primary-key table and five passes of 50,000 cached point
reads. The baseline had no hooks; the instrumented run used statement profile
and row callbacks plus a 1,000-step progress handler. Observed overheads were
16.60%, 57.44%, 30.61%, 6.99%, and 16.39% (median 16.60%). This noisy debug
microbenchmark is not a release-performance claim; the release build was
interrupted before completion and the workload does not exercise slow captures.
Command: `cargo test --lib storage::statement_observation::tests::measure_hook_overhead -- --ignored --nocapture`.
