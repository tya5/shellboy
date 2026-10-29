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
The group scan visits all group metadata in primary-key order on each startup,
with predicate probes into call/result primary keys. Keyset pages bound working
RAM, not total scan time: startup work remains proportional to stored groups,
including retired GC groups. No partial index is added without measured benefit.
No table,
worker, queue, Core event, dependency or paid indexing request is introduced.
Actual SQLite/I/O failures, missing global root_revision, savepoint failure,
snapshot failure, or later startup failure still abort the outer transaction.
Fatal repair errors identify the Thread/group and retain SaveFailed classification.
Only explicitly classified group-local structural inconsistencies are isolated;
SQL constraint/storage failures are never silently skipped. No user data is repaired
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

Startup prevalidates the exact current Revisit state in schema-allowed app_meta
records before writing that Thread's interruption details. Malformed Revisit
records skip only that Thread's capture and emit a fixed diagnostic reason;
healthy Threads still capture and start. Revisit selection SQL guards malformed
JSON before evaluating JSON paths. Ordinary SQLite/I/O/global metadata failures
remain fatal. A capture failure identifies only its Thread, without assigning
an unrelated group ID. No malformed body is copied into diagnostics.

The recovery_reason_json column has a JSON-validity CHECK. Invalid syntax there
is a database integrity violation detected by the unchanged startup quick_check
before capture. It remains fatal; per-Thread isolation does not bypass integrity,
SQL or I/O failures. This partially adopts review L2: isolate schema-allowed
metadata defects, but retain the existing global integrity boundary rather than
pretending a CHECK-violating database is healthy.
