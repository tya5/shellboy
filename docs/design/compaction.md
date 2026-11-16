# Compaction: Current Contract

This document describes the current Core v2.1 implementation contract. Historical proposals, superseded guard rules, review narratives and past failure receipts live in [Compaction Design History](compaction-history.md); they are not alternate runtime authorities. Historical workload reports remain attributed observations, not causal proof of any source change.

## Summary adoption

Automatic proactive, manual summary, active-Turn summary and context-limit summary results share one final transaction: `compaction::summary::adopt_current_r_tx` (`FinalAdoption`). A provider result is eligible for replacement only when the transaction verifies that:

1. the target Thread still exists and is not deleted;
2. the exact ordered R node IDs remain contiguous on the current selected path, located by identity rather than captured ordinal;
3. each R fact is unchanged (payload revision/hash, Turn/group identity, attachment state, unprocessed-message protection and tool-group completion), the interval is not pruned, and its boundaries do not split a tool call/result group;
4. the job's cancellation token is not cancelled at each adoption checkpoint.

The transaction writes the replacement and ordered source provenance, splices only R in the current path, computes the current reasoning projection/keep set, selects that path, and records the terminal attempt/token CAS. Prefix and suffix outside R are read from current state and preserved, including unrelated appends and outside-R edits. The summary's captured path, profile/model/effort, reasoning epoch, Run/Turn start or generation are not independent final-adoption vetoes. Do not add another caller-side validation authority or write a captured keep/path after this transaction.

The reasoning projection is a single path-state invariant shared with `history::select_path`, not a caller-owned side write. Summary adoption records the projected epoch/keep returned by that same positional edit rule and verifies the committed values. A multi-range spill validates every R interval against one transaction-current path, checks disjoint ordered identity/facts, splices all intervals as one batch, then computes one projection. Active spill's DROP_ALL transition advances once from that projection and clears keep atomically; payloads retain the common pre-drop epoch so old signed reasoning is not replayed. No source-derived captured epoch may be assigned back to the Thread after this operation. Virtual preview uses the same positional reasoning-edit predicate as actual path selection.

Context restore has its own undo operation, not summary replacement. Before any retained-anchor removal, every anchor must be present exactly once after the selected marker and in the recorded original order; reject reordered anchors as `SourceChanged` before mutation. Only then remove verified positions descending and splice the originals while preserving the current unrelated prefix/suffix.

Cancellation remains an authority boundary: explicit Run Stop, owner shutdown and Thread deletion cancel the job; cancellation before or during final adoption rolls back the transaction. The operation does not publish a result after cancellation. Source, protection, pruning and tool-closure checks remain mandatory even when outside-R state changes.

## Other path edits

Spill, restore and failure-memory bookkeeping are distinct operations; none is converted to a summary merely to reuse its payload writer.

- Spill derives payloads locally and may replace multiple selected complete tool groups. The whole batch is resolved by current R identity/source facts, spliced into the current path, and passed through one reasoning projection. Active spill's drop-all rule advances the Thread once from that live projected epoch and clears the keep set in this same authority. Every spill replacement is first stamped with the same projected pre-drop epoch; the Thread's one-step advance intentionally makes any signed reasoning in those payloads stale and non-replayable. No caller writes a captured epoch or keep after adoption. The operation preserves current prefix/suffix and outside-R changes. Replacement nodes, selected path and automatic stage/attempt terminal record commit atomically. If spill commits as its own stage before a later summary starts, its committed result remains adopted if that later summary fails or is cancelled.
- Context restore is an undo of one selected marker. It verifies the marker and retained originals are still present, unchanged, unpruned, correctly ordered and tool-group complete; retained anchors may only be removed from their verified current suffix location. It replaces that marker in the current path and preserves unrelated current prefix/suffix. Marker validation, cancellation and path selection are one transaction. Restore does not use the summary-node writer and never reactivates excluded attachments.
- Failure-memory writes do not change history. They are qualified by the owner/cancellation lifetimes, exact failure identity, region and observed-slot CAS. A changed unrelated path is not a reason to discard a still-current failure observation; a stale slot cannot overwrite a newer value.
- Other explicit context edits (trim, selection, preview) retain their operation-specific range, marker, provenance and transaction rules. Prefix trim still uses `validate_snapshot_for_strict_edit` because it computes a user-directed path edit from the captured prefix; this strict edit fence is not an alternate summary-adoption authority. These operations must not be weakened by mechanically deleting snapshot checks.
- Completed replacement provenance remains attached to the Thread through `thread_replacement`; restore does not delete that audit/retention root. Pruning remains the operation that releases retained originals.

## Current proactive driver and writer lifecycle (U1)

At present, the driver calls `proactive::maybe_compact_with_run_stop` at a safe request boundary and awaits it before refreshing the snapshot and dispatching the next model request. The thread boundary and Run cancellation remain active for this operation. The GC coordinator owns producer completion, terminal facts, joining/retirement, transactional adoption and attempt FINISH; natural Turn completion is not a substitute for the explicit cancellation token. This U1 source does **not** establish that ordinary requests progress while a summary is held, that a later Run reuses a still-live proactive job after natural Run exit, or that only a context-limit response waits. Do not describe those outcomes as implemented here.

## U2: nonblocking proactive turn (source implementation; qualification pending)

The U2 source candidate launches the eligible proactive operation as a joined
`BackgroundJob` stored in the existing per-Thread `DriverSlot` in `LiveRegistry`.
The normal request proceeds while the job waits for the current Thread boundary;
adoption still occurs only through the shared final transaction above. No second
coordinator, registry, detached observer or retry layer is introduced.

Natural Run/Turn completion does not cancel or join the task. The Thread slot
retains the same handle, including after task completion, until a later Run
reattaches and joins it; only that join permits idle-slot pruning. Context-limit
recovery joins the same job before following existing overflow rules, without a
duplicate summary or identical POST. Stop/error and owner shutdown cancel and
join through the shared slot. The existing writer coordinator continues to own
producer completion, terminal facts, FINISH and retirement.

The U2 source draft contains deterministic controls for next-request progress
while a summary is held, natural Run A exit and same-Thread Run B reuse, same-job
context-limit joining, no duplicate summary, Stop/error join and owner-shutdown
join. Source presence is not test evidence: focused native results, full normal,
independent review and exact-SHA CI remain separate qualification gates.


## Validation and evidence

The U1 deterministic suite must establish outside-R prefix/suffix preservation for summary and multi-range spill, current reasoning epoch/keep consistency across the whole spill transaction, and restore marker/source/retained-anchor ordering; changed/pruned/protected/tool-split R, cancellation, failure-slot CAS and FINISH rollback remain negative controls. U2 additionally requires controls for natural Run exit, ordinary request progress, same-job context-limit joining, duplicate prevention and Stop/shutdown cleanup; these are separate U2 evidence.

Source review, focused tests, full native `scripts/check.sh`, independent review, and exact-SHA required CI are separate gates. A green native check does not imply CI or main landing. No provider/runtime comparison, private measurement, installation, owner restart, tag, publication or release is part of implementation qualification. Core release remains STOPPED.

## Source map

- Summary final adoption: `src/compaction/summary.rs` (`FinalAdoption`, `adopt_current_r_tx`)
- Automatic spill and stages: `src/compaction.rs`
- Restore and explicit edits: `src/compaction/edit.rs`, `src/compaction/edit/trim.rs`
- Failure identity/slot CAS: `src/compaction/single_request.rs`, `src/compaction/failure_memory.rs`
- Writer-owned proactive lifecycle: `src/compaction/gc_coordinator.rs`, `src/control/driver/proactive.rs`, `src/control/driver.rs`
- FINISH and source-reason facts: `src/compaction/attempt.rs`, `src/compaction/discard.rs`

The active GC planner may cross an epoch change only when the original `model_attempt`
reasoning epoch, exact automatic-attempt token, and committed spill stage fact all match
that attempt and the current Thread epoch. An unrelated later epoch change cannot borrow
the earlier spill witness. The proactive entry guard also refuses to re-trigger from a
provider report marked `next_request_pending`; tests of post-spill stage continuation must
enter the already-admitted stage path rather than weakening that gate.
