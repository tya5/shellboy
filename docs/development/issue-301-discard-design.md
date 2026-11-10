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

## A1. Actual DTO / Writer Trace And Proposed Signature

Source trace at d64357e0 (no runtime implementation yet):

| Surface | Actual existing behavior | Required additive change |
| --- | --- | --- |
| `status::WorkItem` | Public serde/JsonSchema DTO; attempt `reason` is top-level error kind/code only | Add optional `compaction_discard: Option<CompactionDiscard>`; no raw JSON exposure |
| `status::snapshot` | Bounded page enrichment already reads response refs for attempt rows | Read one allowlisted fact only for bounded attempt result page; same transaction |
| `status::observation` | Point observation emits the SAME WorkItem with fixed scalar channel | Enrich only qualified observed attempt IDs, same owner read; no unbounded scan |
| `compaction::attempt` begin/stage/finish SQL | Stores automatic observation under error_json; UUID token CAS; committed-stage merge | Keep discard INSIDE the existing automatic observation, preserve committed stages and existing fields |
| proactive normal / exception finalization | Finish replaces automatic subtree, merges committed stages | Carry the same bounded discard fact in both finish bodies; no overwrite of top-level error/persistence fields |
| proactive target verification | json_set target_verification only | Preserve discard untouched |
| overflow and retry failure writers | Build provider failure body, include automatic_trim_attempt if supplied | Preserve supplied automatic observation including discard; do not replace provider error or counters |
| state fail/incomplete | Replace error_json with normalized failure | Merge previous qualified automatic observation if new body lacks it; new explicit observation wins, no untrusted raw forwarding |
| completion | Does not rewrite error_json | Preserve fact |
| human interruption / startup recovery | Replace error_json for active attempts | Clear prior unfinished observation explicitly; late token must not recreate or overwrite it |
| run cancellation | Changes state without error_json rewrite | Existing retained completed diagnostic fact may remain, never permits adoption |
| history GC | Removes partial_text/reasoning/completed_items/raw_response, retains other error metadata | Retain bounded diagnostic fact; no payload extraction |

Persistence is NOT time-expiring: model_attempt has no TTL, the diagnostic fact
stays with the retained row until an authorized replacing writer or row deletion.
History payload GC is not attempt deletion and does not clear this metadata.
Do not promise automatic time-based deletion. Diagnostic files independently
use existing configured rotation / disabled logger behavior.

Proposed public signature (closed enums with serde snake_case + JsonSchema):

```text
WorkItem.compaction_discard?: CompactionDiscard
CompactionDiscard { reason: CompactionDiscardReason,
                    phase: CompactionDiscardPhase, elapsed_ms: u64 }
```

Attempt identity is the WorkItem's existing id/attempt_id; do NOT duplicate a
worker token, payload hash, or guessed ID in the public fact. Diagnostic event
uses the same three bounded fields plus existing DiagnosticIds. Public decoder
accepts ONLY these exact fields, recognized enums, and unsigned integer elapsed;
malformed/unknown/oversized legacy observations yield None, not `other`.
The stored path is `automatic_trim_attempt.discard`, never an arbitrary raw
error.details projection. No new error_json namespace containing provider data.

Elapsed starts with one `Instant` immediately before each actual compaction
stage calls its model, carried to adoption/rejection in memory; saturate millis
to u64. A model retry inside that stage retains the same Instant; a new stage
gets a new start. The public number is diagnostic duration, not billing or SLA.
Existing code has no such monotonic stage timer; do not derive it from wall
clock timestamps or the preceding original model request. Later finalization
must copy the measured number without restarting it.

Protocol surface changes: WorkItem schema gains one optional closed fact type;
update its generated/checked schema and protocol documentation through existing
schema tooling. This is an additive wire schema change, NOT migration 40 or a
new database column/version. No schema 41 is needed for this contract. Coordinate
with #299 before shared DTO/schema edits; foreign worktree remains untouched.

Query-safety refinement: for the combined thread qualification SELECT, keep one
statement in the original transaction and existing thread existence precedence.
Project booleans for revision/path qualification, profile, epoch in that same
statement and inspect them in current guard order; do not add SELECT/lock-order
changes. SQL NULL must cause the same invalid qualification as before, not a
new reason inferred from absent state. Run and Turn queries/check order remain
unchanged. Any inability to preserve equivalence blocks that branch's split;
leave it unclassified rather than relax a guard.

Scope of first implementation remains visibility only. All original adoption
conditions and #300 append witness/CAS behavior stay unchanged. A1 requires S6
signature review before runtime edits. B remains separately held.

## A2/B Inventory Checkpoint (Not A Guard-Relaxation Decision)

Current anchors are function/branch names, not the historical line hints.
All measured counts are UNKNOWN until Claude separately activates A and the
leader observes its actual workload. Reported totals cannot fill any row.

| Family / actual branch | Stage / protected dependency | Typed reason in A | Policy / deterministic control |
| --- | --- | --- | --- |
| `validate` thread existence, revision, profile, epoch | Adoption transaction; captured selection and request configuration/reasoning input | thread_unavailable / selection_changed / profile_changed / epoch_changed, in existing order | Retain. Same one SQL statement, nullable comparisons reject as before. Real late-adoption transaction negative. |
| `snapshot` stopping Run; `validate` Run present/absent fence | Pre-model and adoption; user stop and current driver consumer | run_stopped / run_started | Retain stopping refusal and active consumer fence. B needs baseline and input-acquisition trace first. |
| `validate` Turn present/absent generation fence | Adoption; generation/state protects active request consumer | turn_changed / turn_started | Retain. Equal visible state does not establish equal generation. |
| `validate` source revision + canonical body hash, prune protection | Adoption; actual summarized payload and released ancestry | source_changed; prune remains actual unclassified invalid_state -> other until branch instrumentation | Retain; no missing source promoted to a success or a fabricated reason. Source SQL errors remain storage errors. |
| `SummaryAdoption` nonappend generation, ordered full prefix, cursor=tail | Adoption; #300 append-only proof and ABA exclusion | nonappend_generation_changed / selected_prefix_changed / current_tail_changed | Retain all three in original short-circuit order, fresh revision path CAS unchanged. |
| Spill pre-adoption cancel; summary request token select; summary pre-adoption token | Result-ready / in-flight / transaction; stop work versus revoke publication | cancelled | Retain ALL checks in A, including in-flight model cancellation. Pre-adoption removal is investigation only, not code permission. |
| `segments`, source traversal and summary/reduce budget / model validation | Pre-dispatch, processing; indivisible size, finite graph/budget, valid model result | other only when an actual invalid_state stage rejection occurs; non-invalid-state provider/storage failures are not discard facts | Retain every preparation/budget guard. A does not expose private model error strings. More specific reasons require branch-by-branch instrumentation, not guessing. |

Cancellation producer trace (in progress, not an exhaustive claim):
`control::live` cancels owned DriverSlot on shutdown and on cancelled owned Run;
`control::turn_interrupt` cancels active binding for an interruption / dropped
request. `control::compact` cancels the owned compaction task on caller stop.
These are distinct from test-only cancellation injection. Thread deletion and
owner disposal need their call chains fully traced before any B decision.
No producer is classified safe to bypass, and no missing dependency is treated
as proof of independence. Model in-flight cancellation remains intact.

B must converge on one adoption-validator specification with row/case/branch/
test correspondence; no scattered permissive guards. Compare three structural
alternatives only after A baseline: append-tail rebasing (existing #300), proven
active-input-independent adoption, or deferring adoption until the active input
consumer releases it. Profile and epoch dependencies must be derived from the
actual generation request, not assumed irrelevant. Existing A implementation
only annotates the original validator and does not implement any alternative.

Timing clarification for current A implementation: Instant starts at the
automatic stage invocation and includes its preparation/cache/model/retry/adopt
work. It is NOT model-only/billed latency. This is more honest than claiming the
existing architecture has a model-start timer. Same-stage retries retain that
Instant; subsequent phases have a new Instant. Later serializers copy the fact.
