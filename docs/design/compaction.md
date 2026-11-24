# Compaction: Current Contract

This document describes current compaction source contracts. Superseded proposals, review narratives and past failure receipts remain in repository history; they are not alternate runtime authorities. Operational qualification and release status are recorded in run reports, not in this design document.

## Summary adoption

Automatic proactive, manual summary, active-Turn summary and context-limit summary results share one final transaction: `compaction::summary::adopt_current_r_tx` (`FinalAdoption`). A provider result is eligible for replacement only when the transaction verifies that:

1. the target Thread still exists and is not deleted;
2. the exact ordered R node IDs remain contiguous on the current selected path, located by identity rather than captured ordinal;
3. each R fact is unchanged (payload revision/hash, Turn/group identity, attachment state, unprocessed-message protection and tool-group completion), the interval is not pruned, and its boundaries do not split a tool call/result group;
4. the job's cancellation token is not cancelled at each adoption checkpoint.

The transaction writes the replacement and ordered source provenance, splices only R in the current path, computes the current reasoning projection/keep set, selects that path, and records the terminal attempt/token CAS. Prefix and suffix outside R are read from current state and preserved, including unrelated appends and outside-R edits. The summary's captured path, profile/model/effort, reasoning epoch, Run/Turn start or generation are not independent final-adoption vetoes. Do not add another caller-side validation authority or write a captured keep/path after this transaction.

The reasoning projection is a single path-state invariant shared with `history::select_path`, not a caller-owned side write. Summary adoption records the projected epoch/keep returned by that same positional edit rule and verifies the committed values. A multi-range spill validates every R interval against one transaction-current path, checks disjoint ordered identity/facts, splices all intervals as one batch, then computes one projection. Active spill's DROP_ALL transition advances once from that projection and clears keep atomically; payloads retain the common pre-drop epoch so old signed reasoning is not replayed. No source-derived captured epoch may be assigned back to the Thread after this operation. Virtual preview uses the same positional reasoning-edit predicate as actual path selection.

Context restore has its own undo operation, not summary replacement. Before any retained-anchor removal, every anchor must be present exactly once after the selected marker and in the recorded original order; reject reordered anchors as `SourceChanged` before mutation. Only then remove verified positions descending and splice the originals while preserving the current unrelated prefix/suffix.

Cancellation remains an authority boundary: explicit Run Stop, owner shutdown and
Thread deletion cancel the job; cancellation before or during final adoption
rolls back the transaction. ThreadDelete emits deleted Thread IDs only from its
committed MutationResult; Control applies that effect to the matching retained
Thread slot and joins its job. Receipt replay returns the saved value without
replaying cancellation effects. The operation does not publish a result after
cancellation. Source, protection, pruning and tool-closure checks remain
mandatory even when outside-R state changes.

## Other path edits

Spill, restore and failure-memory bookkeeping are distinct operations; none is converted to a summary merely to reuse its payload writer.

- Spill derives payloads locally and may replace multiple selected complete tool groups. The whole batch is resolved by current R identity/source facts, spliced into the current path, and passed through one reasoning projection. Active spill's drop-all rule advances the Thread once from that live projected epoch and clears the keep set in this same authority. Every spill replacement is first stamped with the same projected pre-drop epoch; the Thread's one-step advance intentionally makes any signed reasoning in those payloads stale and non-replayable. No caller writes a captured epoch or keep after adoption. The operation preserves current prefix/suffix and outside-R changes. Replacement nodes, selected path and automatic stage/attempt terminal record commit atomically. If spill commits as its own stage before a later summary starts, its committed result remains adopted if that later summary fails or is cancelled.
- Context restore is an undo of one selected marker. It verifies the marker and retained originals are still present, unchanged, unpruned, correctly ordered and tool-group complete; retained anchors may only be removed from their verified current suffix location. It replaces that marker in the current path and preserves unrelated current prefix/suffix. Marker validation, cancellation and path selection are one transaction. Restore does not use the summary-node writer and never reactivates excluded attachments.
- Failure-memory writes do not change history. They are qualified by the owner/cancellation lifetimes, exact failure identity, region and observed-slot CAS. A changed unrelated path is not a reason to discard a still-current failure observation; a stale slot cannot overwrite a newer value.
- Other explicit context edits (trim, selection, preview) retain their operation-specific range, marker, provenance and transaction rules. Prefix trim still uses `validate_snapshot_for_strict_edit` because it computes a user-directed path edit from the captured prefix; this strict edit fence is not an alternate summary-adoption authority. These operations must not be weakened by mechanically deleting snapshot checks.
- Completed replacement provenance remains attached to the Thread through `thread_replacement`; restore does not delete that audit/retention root. Pruning remains the operation that releases retained originals.

## Driver and writer lifecycle

U1's baseline driver awaited `proactive::maybe_compact_with_run_stop` at a safe
request boundary. The U2 source candidate instead runs typed eligibility
preflight while the boundary is current and only publishes a `BackgroundJob`
for a report that passes threshold and pending-request checks. Ineligible
reports allocate no job. The job handle in the existing per-Thread `DriverSlot`
is the sole retained-job authority; no separate activity flag, registry,
detached observer, coordinator or retry layer is introduced.

After publication, the ordinary request proceeds. The job uses the same
Thread-boundary/adoption transaction and exact original Thread/attempt/token
receipt. Request profile/model/effort/epoch and replay projection are captured
coherently from current Thread facts in the GC capture transaction; origin
configuration equality is not a veto. Receipt Thread/attempt/token validation,
source/protection/pruning/tool closure, cancellation and atomic FINISH remain
mandatory. Natural Run completion does not cancel or join the job. The Thread
slot retains its handle, including after completion, until the next Run reuses
and joins it. Only a missing handle permits idle-slot pruning. Context-limit
recovery joins that same handle before existing overflow behavior; it does not
start a duplicate summary or resend the same POST. Stop/error and owner
shutdown cancel and join through the same slot. The existing writer coordinator
continues to own producer completion, terminal facts, FINISH and retirement. Matching
job-ID lifecycle events consume exit and join receipts; ordinary database
replies neither advance nor retire the producer.
An occupied GC slot is a transient pre-capture refusal, not a full DB lane.
Retry only that capture after cancellation-aware bounded backoff, retaining the
same stage and uncalled closures. A full foreground lane already waits for
admission. Neither case repeats a committed capture or a provider request.


Manual summary and preview retain their strict operation-specific snapshot,
range, marker, provenance and transaction rules. This current automatic path
contract does not weaken those explicit-edit controls.



## Deterministic acceptance properties

Summary and multi-range spill preserve outside-R prefix/suffix and current
reasoning epoch/keep. Restore validates marker, source and retained-anchor order
before mutation. Changed, pruned, protected or tool-split R; cancellation;
failure-slot CAS; and FINISH rollback remain negative controls.

The nonblocking lifecycle uses barriers, not elapsed-time thresholds: hold the
summary response, observe an ordinary provider request, keep the path unchanged
while that request is in flight, let Run A retire naturally, and prove Run B on
the same Thread reuses the same job. A context-limit request joins that job with
no duplicate summary or identical POST. Stop/error and owner-shutdown controls
prove cancellation and join. Test runs, full-normal checks, independent review,
CI conclusions and release gates belong to their exact-SHA run reports; source
or test presence alone is not qualification.


## Limits and readback

Automatic thresholds and explicit summary bounds are configured independently:
`auto_at_tokens`, target percentage, and `summary_max_chars` affect the automatic
path; manual `compact --summary` supports its own validated bounded-summary
options. Spill policy is the per-Thread override, then connection setting, then
the built-in default. There is no global spill setting.

`compact list` and `usage get` report persisted summary usage and post-compaction
input observations from their saved source records. Unknown historical metrics
remain unknown; no provider comparison or inferred measurement is added.

## Automatic attempt receipt

A provisional attempt is owned by its original Thread, model-attempt ID and
exact token. A stage may be installed only while that receipt is provisional
and matches its original owner. A captured request uses current Thread
profile/model/effort/epoch as one transaction-current configuration snapshot;
these fields are not compared back to the stage's origin snapshot. The final
adoption transaction still authenticates the original receipt and enforces its
source, cancellation, protection, tool-closure and atomic terminal-write rules.

## Source map

- Summary final adoption: `src/compaction/summary.rs` (`FinalAdoption`, `adopt_current_r_tx`)
- Automatic spill and stages: `src/compaction.rs`
- Restore and explicit edits: `src/compaction/edit.rs`, `src/compaction/edit/trim.rs`
- Failure identity/slot CAS: `src/compaction/single_request.rs`, `src/compaction/failure_memory.rs`
- Writer-owned proactive lifecycle: `src/compaction/gc_coordinator.rs`, `src/control/driver/proactive.rs`, `src/control/driver.rs`
- FINISH and source-reason facts: `src/compaction/attempt.rs`, `src/compaction/discard.rs`

The proactive eligibility preflight uses only the latest provider-reported
`input_tokens`, configured threshold and `next_request_pending`; it runs before
BackgroundJob allocation. The worker carries that typed plan, then rechecks its
sampled profile/revision and exact original attempt receipt before beginning the
provisional attempt. Current summary request profile/model/effort/epoch are
captured coherently from the Thread inside the capture transaction, not vetoed
against the origin receipt configuration. Receipt Thread/attempt/token identity
and provisional FINISH ownership remain mandatory.


## Persisted summary message role

The summary-generation request continues to use a developer instruction. The generated summary artifact is persisted as a user conversation message; these roles are intentionally distinct. Schema 49 changes only old nodes positively identified by the canonical `summary` kind, `shellboy-summary-v1` payload, and one matching completed replacement with the same output node, format, and exact source ordering. It preserves summary text, source/replacement provenance, and ordinals while refreshing payload byte count, body hash, and payload revision in the startup migration transaction. Ambiguous, malformed, pending, mismatched, tombstoned, or non-summary developer records remain unchanged.
