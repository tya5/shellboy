# Issue 301: Discard Observability And Separate Adoption Design

Status: PROPOSED, source-first review. No runtime guard relaxation authorized.
Base: 3e788ea97672777aac64f054665a0342ba5b7e86, includes #300 6163d243.
User counts (35/199, roughly two hours, then 3/5 discards) are reports, not an
independent reproduction. Their actual rejection reasons remain UNKNOWN.

## A. Minimal Observability Contract (First Implementation)

Keep existing error code/message and automatic diagnostic event/counters.
Carry a typed reason from the exact rejecting branch, not string matching.
Proposed snake_case reason enum and first-failing-guard precedence:

| Reason | Authoritative branch / fact |
| --- | --- |
| cancelled | Cancellation checked before or during adoption |
| thread_unavailable | Captured thread absent/deleted |
| selection_changed | Strict selection revision mismatch, without eligible append witness |
| profile_changed | Captured profile_revision differs |
| epoch_changed | Captured reasoning_epoch differs |
| run_started | No captured Run but a current live/stopping Run exists |
| run_stopped | Captured Run no longer live |
| turn_started | No captured Turn but an active Turn exists |
| turn_changed | Captured Turn generation or eligible state changed |
| nonappend_generation_changed | Storage-owned #300 generation differs (including ABA) |
| selected_prefix_changed | Full captured path is not an ordered prefix |
| current_tail_changed | Current cursor does not equal actual path tail |
| source_changed | Source payload revision/hash changed, missing source |
| source_pruned | Expanded marker released by prune |
| owner_changed | Driver slot/owner qualification rejects before adoption |
| other | Known rejection not yet split; never infer a more specific reason |

The existing combined thread query must be decomposed inside the SAME database
transaction to select a real reason. Report only the first actual failure; do
not pretend all guards were evaluated. Missing SQL rows/SQLite failures remain
storage errors, not guessed source mismatches. Owner qualification needs an
explicit emitting branch; enum presence alone is not coverage evidence.

Proposed additive diagnostic fact type:

```text
CompactionDiscard {
  attempt_id: existing opaque model-attempt ID,
  phase: closed_spill | closed_summary | active_spill | active_summary,
  elapsed_ms: bounded u64 measured by local monotonic clock,
  reason: CompactionDiscardReason
}
```

No text, node arrays, payload/hash values, prompts, model responses, filesystem
paths, URLs, error messages, profiles, credentials, or task content. Existing
safe thread/run/turn IDs may use existing DiagnosticIds only. No new worker,
queue, table, or cache. Logger stays bounded try_send and existing rotation;
this does not promise delivery when logging is disabled or its queue drops.

Status proposal: add an optional bounded `discard` fact to the existing automatic
attempt observation, stored under existing model_attempt.error_json and exposed
through the supported status projection after source tracing confirms its exact
public type. Preserve all current fields/codes. The latest fact belongs to the
same attempt/token and stage; late tokens cannot overwrite newer observations.
No independent global last-discard singleton. Persistence shares the attempt's
existing retention/GC lifetime, not a new TTL; diagnostic files share configured
rotation, not a time-retention promise. Phase/elapsed must not reset on retries.
Unknown legacy observations remain absent/unknown, never a fabricated reason.
No migration anticipated; migration 40 remains byte-immutable. Any discovered
schema requirement needs a separate migration 41 decision first.

Acceptance controls before claiming visibility: each newly exposed rejection
branch produces the exact enum through the real adoption/error/driver path;
actual rejected synthetic attempt appears in machine-readable diagnostics AND
supported status; existing error fields/counters survive; secret sentinels never
appear; disabled/full logger remains bounded; late token cannot overwrite status.

## B. Run/Turn Adoption Safety (Design Only)

Current `validate` fences absent/present Run and Turn identity/generation/state.
Those checks prevent adoption from changing the prompt/selection/reasoning state
under a new driver consumer. #300 only qualifies append-only path extension; it
does not prove the new Run/Turn has not captured an input or acquired protected
state. Therefore simply removing run_started/turn_started is NOT safe evidence.

Possible future eligibility (not permission to implement): a new Run/Turn may
adopt only if a storage-owned generation proves no model-input snapshot, live
attempt, or protected source consumer was acquired across the transition, and
the driver can refresh its input BEFORE dispatch. Proving this requires tracing
Run/Turn creation, preparing->running and model-request snapshot linearization.
If current persisted facts cannot prove that boundary, remain unsupported.
A timestamp, equal Run/Turn names, or current active state alone is insufficient.

Transaction/linearization contract: in one original adoption transaction, check
cancellation, owner/slot authority, profile, epoch, immutable source revisions /
hashes, storage nonappend generation, ordered full-path witness, cursor=tail,
and all permitted Run/Turn input-generation facts. Use the freshly read revision
as select_path CAS; preserve every eligible appended tail in order. Replacement,
selection/epoch, automatic stage fact and any driver-input invalidation must
commit together. No await or callback between checks and commit. On CAS or
qualification failure, rollback fully and report the actual first reason.
The commit is the adoption linearization point, NOT model completion.

| Deterministic race | Required result for any future relaxation |
| --- | --- |
| Append only, same Run/Turn authority | Preserve #300 positive; one commit, all tail nodes retained |
| New Run/Turn, no acquired input, proven generation | Candidate positive only after approved storage/driver contract |
| New Run/Turn already captured/dispatched input | Reject; no late rewrite of that consumer's context |
| Run stopped/replaced; Turn generation ABA | Reject despite restored visible state |
| Rewind/edit/path restore ABA | Reject by storage nonappend generation |
| Prefix reorder, cursor disagreement, non-tail mutation | Reject atomically |
| Profile/epoch/source payload/prune change | Reject; no replacement or stage fact |
| Cancel/owner replacement immediately before commit | Reject; no stale publication |
| Append between witness and CAS | Serialize transaction or fail CAS, never drop tail |

Manual/expanded-marker snapshots without append witness, multiple concurrent
consumers, and unprovable input-acquisition transitions remain unsupported.
Run/Turn relaxation requires S6 design review plus explicit leader approval,
then a separate implementation/validation change. Part A must not enable it.

## Operational Boundaries

Do not modify the user's reported auto_compact_at=600000. Only an actual request
rejection specifically for context limit authorizes restoring 400000 and
reporting that rejection; no such rejection has been observed by this task.
No provider capacity inference, paid call, user DB/RPC, installed Core replacement,
owner restart, browser, or release action. Shared knowledge hybrid search was
degraded/empty; no reindex or embedding was requested. Current Git is authority.

Source inspection only at this checkpoint; no tests/runtime behavior claimed.
