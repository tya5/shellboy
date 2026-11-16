# Compaction: Current Contract

This document describes the current Core v2.1 implementation contract. Historical proposals, superseded guard rules, review narratives and past failure receipts live in [Compaction Design History](compaction-history.md); they are not alternate runtime authorities. Historical workload reports remain attributed observations, not causal proof of any source change.

## Summary adoption

Automatic proactive, manual summary, active-Turn summary and context-limit summary results share one final transaction: `compaction::summary::adopt_current_r_tx` (`FinalAdoption`). A provider result is eligible for replacement only when the transaction verifies that:

1. the target Thread still exists and is not deleted;
2. the exact ordered R node IDs remain contiguous on the current selected path, located by identity rather than captured ordinal;
3. each R fact is unchanged (payload revision/hash, Turn/group identity, attachment state, unprocessed-message protection and tool-group completion), the interval is not pruned, and its boundaries do not split a tool call/result group;
4. the job's cancellation token is not cancelled at each adoption checkpoint.

The transaction writes the replacement and ordered source provenance, splices only R in the current path, computes the current reasoning projection/keep set, selects that path, and records the terminal attempt/token CAS. Prefix and suffix outside R are read from current state and preserved, including unrelated appends and outside-R edits. The summary's captured path, profile/model/effort, reasoning epoch, Run/Turn start or generation are not independent final-adoption vetoes. Do not add another caller-side validation authority or write a captured keep/path after this transaction.

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

## Planned U2: nonblocking proactive turn (not current runtime)

A separate follow-up recorded in [the adopted #301 direction](https://github.com/tya5/shellboy/issues/301#issuecomment-6902558988) aims to launch an eligible proactive job and let the ordinary request proceed, retain that same Thread-owned job through natural Run/Turn completion, reuse it from later Runs without duplicate summary requests, and join it only on context-limit recovery or actual shutdown/Stop. Eligibility should be established before spawn; ownership must remain with existing Thread/job lifecycle, not a detached task or new registry. These are planned acceptance conditions, not behavior delivered or qualified by this U1 source commit. Keep U2 in its separate work unit and report its actual status independently.

## Validation and evidence

The U1 deterministic suite must establish outside-R prefix/suffix preservation for summary and multi-range spill, current reasoning epoch/keep consistency across the whole spill transaction, and restore marker/source/retained-anchor ordering; changed/pruned/protected/tool-split R, cancellation, failure-slot CAS and FINISH rollback remain negative controls. U2 separately requires controls for natural Run exit, ordinary request progress, same-job context-limit joining, duplicate prevention and shutdown cleanup; those are not U1 evidence.

Source review, focused tests, full native `scripts/check.sh`, independent review, and exact-SHA required CI are separate gates. A green native check does not imply CI or main landing. No provider/runtime comparison, private measurement, installation, owner restart, tag, publication or release is part of implementation qualification. Core release remains STOPPED.

## Source map

- Summary final adoption: `src/compaction/summary.rs` (`FinalAdoption`, `adopt_current_r_tx`)
- Automatic spill and stages: `src/compaction.rs`
- Restore and explicit edits: `src/compaction/edit.rs`, `src/compaction/edit/trim.rs`
- Failure identity/slot CAS: `src/compaction/single_request.rs`, `src/compaction/failure_memory.rs`
- Writer-owned proactive lifecycle: `src/compaction/gc_coordinator.rs`, `src/control/driver/proactive.rs`, `src/control/driver.rs`
- FINISH and source-reason facts: `src/compaction/attempt.rs`, `src/compaction/discard.rs`
