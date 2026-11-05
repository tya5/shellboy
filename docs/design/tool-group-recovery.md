# Interrupted tool-group completion

Issue #37 repairs the current-format lifecycle after an owner disappears. Startup
captures interruption context first, terminalizes old Turns and shell executions,
then completes missing tool results in the same transaction before terminalizing
old Runs and attempts. Repaired saved_execution facts describe that post-startup
shell state; the interruption snapshot retains original observed states. This
is a current-state invariant repair on every startup, including groups whose
Turn was already terminalized by a previous startup; it is not a historical
format reader or schema migration.

Repair groups which are not completed, contain pending/running calls, or have a
call without its committed tool_result. Cancel unapplied authenticated shell
attachment deliveries from that group before marking it completed. Keep applied
attachments, all original payloads/IDs and existing result rows unchanged.

A pending call with no saved execution admission receives a synthetic
function_call_output with status not_started and result_uncertain false. Other
missing results receive status interrupted and result_uncertain true. Both use
reason previous_instance_interrupted and output_complete false: the original
canonical tool result was never committed, and this message does not fabricate
its stdout or side effects. A saved execution ID and its actual state/exit code/
raw-output completeness are separate saved_execution facts; startup does not
inspect, resume or repeat OS processes. Existing real results always win.

The repair node uses the ordinary tool_result envelope and group_id, with a
startup_repair marker. The adopted function_call source comes from output_item's
current attempt/call/node binding. Missing or contradictory retained bindings do
not invent an assistant source. Each group has a savepoint: a structural current-
format inconsistency rolls back all changes for that group and leaves it unresolved,
while other groups and Threads continue startup. Capture at most 32 diagnostic
previews containing bounded Thread/group IDs and a fixed reason classification;
the existing diagnostic writer receives them after the outer transaction commits.
Replay of an unresolved group remains blocked before HTTP by the existing guard. A deleted owner's group is
skipped only when every bound call source is an explicit current GC tombstone
(node and output_item agree), all bindings identify the one adopted source, and
the shared current-root/replacement reachability rule finds no live consumer.
Do not recreate collected payloads or finalize those retired groups. Outstanding
shell attachments are still cancelled by the existing policy. Live forks,
selected sources, summary/spill provenance and current ownership prevent skipping;
inconsistent retained histories remain unresolved. Completed call/group metadata and
all inserted result/path records commit together; a second startup adds nothing.

New result nodes are spliced after the group's last existing selected result,
or immediately after its call-source node if none exists. This puts new results
before later interruption/audit/input nodes in the ordinary crash case. Preserve
existing nodes and every old DAG edge: do not move an existing result backwards
across its ancestors. Model replay separately groups committed results adjacent
to their call source and rejects unresolved/inconsistent groups before HTTP.

The existing DB worker owns this one startup transaction. Scan groups, calls and
affected Threads in keyset pages of 32. Insert selected-path rows with SQL ordinal
shifts and window ranking; do not collect all history payloads or paths in RAM.
Recovery candidates are recorded in an indexable `tool_group_recovery_candidate`
marker table keyed by group ID. A marker exists exactly when the legacy recovery
predicate is true: group state is not completed, or any call is pending/running,
or any call lacks its committed result. Startup reads markers in group-ID keyset
pages of 32 and then applies the existing per-group repair/savepoint behavior;
it never walks completed groups to discover candidates. Marker candidate reads,
per-group inspection, and repair retain bounded keyset work per page, but no
wall-time budget or timing promise is made.

All marker maintenance is in SQLite triggers within the transaction that changes
the group, call, or result. Insert/update/delete of any relevant row recomputes
only its affected group's predicate and inserts/deletes its marker. Thus opening
a group/call and completing a call/result cannot commit without its matching
marker; a crash before commit rolls back both, and a crash after commit leaves
both. The schema migration creates the marker table and triggers and performs a
one-time backfill using the exact legacy predicate in its own atomic migration
transaction. A pre-migration database therefore cannot be read by the new
candidate-only recovery path until backfill commits. No permanent legacy-scan
fallback is retained. Fresh databases install the same table and triggers. On the one 3.4 GiB VACUUM
INTO snapshot (57,465 groups), candidate enumeration measured 1.031 s cold and
0.238/0.354 s warm for the legacy query; after migration, the marker keyset loop
measured 40 microseconds cold and 8-11 microseconds warm with zero candidates.
This is candidate-discovery work only, not a full-stage or startup timing claim;
the snapshot was deleted after measurement.
A second read-only `VACUUM INTO` copy of the local schema-27 database contained
59,471 groups. On the same copy, the legacy full-predicate keyset enumeration
found zero candidates and took 0.598 s on its first pass and 0.206 s warm. After
migration 28 (0.141 s), marker enumeration found zero candidates and took 55
microseconds on its first pass and 12 microseconds warm. That copy was deleted.
These measurements still cover candidate enumeration only. On a later one-copy
stage run (59,522 groups; same rollback-safe copy before/after), the body of
`interrupt_previous_work` took 1.097229 s with the legacy full-predicate query
and 37.912 ms after migration with marker lookup; both reported zero repaired,
unresolved, and retired groups. The timer excludes transaction begin/rollback
and migration 28. On arm64 macOS with SQLite 3.51.0, this is a local observed
result, not a timing promise or full application startup measurement.

No separate worker, queue, Core event, dependency or paid indexing request is
introduced. Actual SQLite/I/O failures, missing global root_revision, savepoint
failure, snapshot failure, or later startup failure still abort the outer
transaction. Fatal repair errors identify the Thread/group and retain SaveFailed
classification. Only explicitly classified group-local structural
inconsistencies are isolated; SQL constraint/storage failures are never silently skipped. No user data is repaired
outside the explicitly opened owner store.

## Live-process limitation

A non-storage execution conflict that leaves an unresolved group in the current
owner is still rejected by the existing replay admission guard until restart.
Do not repair a possibly executing shell in place or claim its side effects are
finished. This path has not been reproduced as a normal execution regression;
startup repair does not authorize automatic live replay or external retries.

## Startup capture with an invalid cursor

Before writing interruption nodes for a startup Thread, compare its cursor with
the selected-path tail in the same transaction. An exact mismatch skips that
Thread's capture without changing its nodes/path and reports a fixed capture-
cursor diagnostic. SQL errors and global metadata failures still abort startup;
no blanket Schema-error catch is added. Other Threads retain their snapshots.
Group reconciliation validates destination cursors even when all real results
already exist, so a bad path is left unresolved instead of finalized silently.
Capture and repair previews share a maximum of 32 diagnostic records after the
outer commit; group ID is absent for Thread-level capture failures; group repair reports its
actual group identity separately. Global root metadata is checked before any
per-Thread isolation, even when every Thread would otherwise be skipped.
The owner and read/delete commands remain available to remove the affected
conversation; this is not an automatic path reconstruction mechanism.

## Unresolved histories and malformed per-Thread capture data

A group that startup cannot reconcile remains unresolved. Replay rejects it
before HTTP with guidance to inspect the saved records, fork at an earlier
complete boundary, or delete that Thread. Repeated restart cannot reconstruct
missing or contradictory source bindings. A fork must end before the incomplete
group, never at its unresolved call/result. The original Thread is unchanged
until explicit deletion.

Startup prevalidates each Thread's cursor before writing that Thread's
interruption details. A mismatch skips only that Thread's capture and emits a
fixed diagnostic reason; healthy Threads still capture and start. (The #36
Revisit metadata check was removed with that metadata in #187.) Ordinary
SQLite/I/O/global metadata failures
remain fatal. A capture failure identifies only its Thread, without assigning
an unrelated group ID. No malformed body is copied into diagnostics.

The recovery_reason_json column has a JSON-validity CHECK. Invalid syntax there
is a database integrity violation detected by quick_check after an unclean or
unknown shutdown, or by the manual storage check. Clean startup skips this scan;
undetected pre-existing corruption is not a successfully isolated Thread defect.
Detected integrity, SQL and I/O failures remain fatal. This partially adopts review L2: isolate schema-allowed
metadata defects, but retain the existing global integrity boundary rather than
pretending a CHECK-violating database is healthy.

## Candidate-backed startup capture follow-up

Startup's interrupted-thread selector and tool-call snapshot also use the
migration-28 recovery-candidate table. The marker was backfilled from the
legacy unresolved predicate before startup can read it, and its triggers update
atomically with group/call/result changes. Thus a startup thread is still
selected for any group the legacy predicate would select, and the call snapshot
still captures every call in every unresolved group. The candidate table is the
outer loop of the tool-call query (`CROSS JOIN` is intentional to keep SQLite
from reverting to a full completed-history scan). The existing snapshot writer
continues to flush bounded chunks; this changes candidate discovery only and
makes no wall-time promise.

The follow-up snapshot measured 60,840 groups, 62,903 calls, 65,678 attempts,
and 49,250 app_meta rows; it contained nine interrupted-work Threads, one
pending wait, zero recovery candidates, and one active attempt. In a warm same-
copy query comparison, the nine-Thread tool-call snapshot fell from 249 ms to
0.064 ms, with identical empty output. Replacing the thread selector's full
group predicate with a marker existence check reduced its warm median from 70 ms
to 8.8 ms while returning the same nine Threads. The complete
`capture_startup_interrupted_context` plus `interrupt_previous_work` sequence
on that same copy was 1,796 ms before and 881 ms after; both captures had zero
failures and both repair runs had zero repaired, unresolved, and retired groups.
A partial wait-prefix index cost 40.9 ms to build and saved under 18 ms for
one wait; the active-attempt partial index cost 380 ms to build for a 30 ms to
0.16 ms one-row update. Neither index's one-time cost is justified here. The
step-specific and full-stage readings have different boundaries; these are
local SQLite 3.51 / arm64 macOS observations, not end-to-end startup guarantees.

## Active-attempt interruption index (#219 follow-up)

`interrupt_previous_attempts` updates only `preparing` and `inflight` attempts.
Migration 29 adds the partial `model_attempt_active_recovery` index on `state`
for exactly those rows. This preserves the UPDATE predicate and transaction;
there is no eligibility marker to maintain, and completed/failed/incomplete/
cancelled attempts remain outside the index. Existing schema-28 databases
receive the index via migration 29 before startup recovery; fresh databases
create it from the current schema. The migration is index-only, with no data
backfill. Recovery still runs its original update, so any eligible row is
matched; the index cannot hide an eligible attempt.

On the local schema-28 read-only-source `VACUUM INTO` copy (70,056 attempts,
three eligible), SQLite 3.51 / arm64 macOS measured the original UPDATE at
292.5 ms on its first trial and 97.1/29.0 ms on subsequent trials; it scanned
the table. Creating the partial index took 30.2 ms. The same UPDATE then used
`SEARCH ... USING COVERING INDEX model_attempt_active_recovery` and measured
0.36/0.30/0.27 ms, updating the same three rows. Trials rolled back. This is
local statement-level evidence, not a full `InterruptedWork` timing guarantee.
