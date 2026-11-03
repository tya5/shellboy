# DB slow-job diagnostics (#201)

## Goal and status

Design for implementation after the hotfix; it is not an implemented feature.
When a DB job exceeds its limit, identify the statement, evidence of why it was
slow, and the admitted request's origin without exposing private content.
Extend the existing `db_job_slow` and `db_job_active` events (#190); do not add a
new table, queue, worker, or dependency.

## Connection-level instrumentation

Instrument all SQLite connections used by the DB worker at the lowest practical
layer, including the inspection read connections. Use rusqlite's `trace_v2`
profile events to capture per-statement elapsed time and unexpanded SQL; never
log bound values. Track the currently executing statement per thread so the
existing #171 progress handler can identify it. After each statement collect
`sqlite3_stmt_status` counters: `FULLSCAN_STEP`, `SORT`, `AUTOINDEX`, and
`VM_STEP`, along with the row count. A nonzero full-scan count on a table that
grows is a signal of a potential missing index independent of wall time.

On the first progress-handler call after a job exceeds its limit, capture one
`std::backtrace::Backtrace` on the executing thread and the current unexpanded
SQL. Capture at most once per job. Record the origin at admission: requester
kind, execute kind, command name, and argument shape such as limits and scopes,
never argument values.

## Events, privacy, and bounds

Extend the existing slow/active events with normalized SQL fingerprint without
parameters, full-scan, sort, autoindex and VM step counts, rows, origin, and
backtrace symbol names. At overrun only, obtain `EXPLAIN QUERY PLAN` text for
the fingerprint using a separate read-only connection. Do not run the plan on
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
adopted. No overhead or binary-size claim is made until measured.
