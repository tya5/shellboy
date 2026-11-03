# DB slow-job diagnostics (#201)

## Goal and status

Design for implementation after the hotfix; it is not an implemented feature.
When a DB job exceeds its limit, identify the statement, evidence of why it was
slow, and the admitted request's origin without exposing private content.
Extend the existing `db_job_slow` and `db_job_active` events (#190); do not add a
new table, queue, worker, or dependency.

## Connection-level instrumentation

Instrument all SQLite connections used by the DB worker at the lowest practical
layer, including inspection reads. The always-on fast path subscribes only to
SQLite `trace_v2` profile events, using SQLite's elapsed time without a separate
per-statement timer. Only a statement reported at or above 1 ms is sampled for
`FULLSCAN_STEP`, `SORT`, `AUTOINDEX`, and `VM_STEP`. SQLite counters can include
previous executions of a reused prepared statement; report them as cumulative
statement-lifetime snapshots, not per-execution or job totals. No sample means
unknown, not zero. A row callback counts returned rows across the current job,
including its active statement; `row_scope` is `job_cumulative`. This count
is distinct from sampled statement-lifetime counters. Tests may opt into
exhaustive observation to check indexed
hot reads independently of the production sampling threshold.

The existing progress handler captures at most one backtrace on the executing
thread after a job exceeds its limit. Fingerprints are emitted only for exact
reviewed static statements after a slow profile event; unknown statements are
omitted, including their SQL and parameter values. Requester category and
execute kind are retained where available; argument values are never logged.

## Events, privacy, and bounds

Extend the existing slow/active events with available safe fingerprints,
sampled counters, origin, and backtrace symbol names. For recognized slow
statements, the existing diagnostic writer obtains `EXPLAIN QUERY PLAN` on a
separate read-only connection and emits only fixed plan categories. Do not run the plan on
the writer connection or log expanded SQL, message content, paths, or parameter
values. Ensure the plan text and fingerprint are privacy-safe before logging;
omit detail when that cannot be guaranteed. Emit at most one detailed record
per job and bound its rate. A failed plan or unavailable backtrace must not
block or fail the job.

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
