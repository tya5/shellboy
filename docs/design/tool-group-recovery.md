# Interrupted tool-group completion

Issue #37 repairs the current-format lifecycle after an owner disappears. Startup
captures interruption context first, then completes missing tool results in the
same transaction before terminalizing old Runs, Turns, shells and attempts. This
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
current attempt/call/node binding. Missing or contradictory retained bindings fail
startup rather than inventing an assistant source. A deleted owner's group is
skipped only when every bound call source is an explicit current GC tombstone
(node and output_item agree), all bindings identify the one adopted source, and
the shared current-root/replacement reachability rule finds no live consumer.
Do not recreate collected payloads or finalize those retired groups. Outstanding
shell attachments are still cancelled by the existing policy. Live forks,
selected sources, summary/spill provenance and current ownership prevent skipping;
inconsistent retained histories remain a hard error. Completed call/group metadata and
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
The group/call/result lookups use existing primary/foreign-key indexes. No table,
worker, queue, Core event, dependency or paid indexing request is introduced.
All writes roll back together if snapshot, attachment policy, repair, DAG/cursor
adoption or subsequent startup terminalization fails. No user data is repaired
outside the explicitly opened owner store.
