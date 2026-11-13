# #301 S1: Terminal Compaction Recording Contract

Adopted recording-only contract. Implementation and validation are recorded in
status/acceptance separately. S1 changes no adoption, retry, suppression, #A,
settings or scheduling decision. S2 suppression, S3 adoption, S4 running progress
and S5 Studio rendering are separate reviewed units.

## One Current Fact

An existing automatic stage may record one terminal nonsuccess fact:

```text
CompactionOutcomeFact {
  phase: closed_spill | closed_summary | active_spill | active_summary,
  elapsed_ms: u64,
  outcome: Rejected | Failed
}
Rejected { kind: "rejected", reason: RejectionReason }
Failed { kind: "failed", reason: FailureReason, origin: FailureOrigin,
         http_status?: u16,
         final_group_count?: u64, final_source_nodes?: u64 }
```

All objects deny unknown fields. Failed-only fields are illegal on Rejected.
Maximum encoded fact: 1024 UTF-8 bytes, exactly the typed two-object shape.
Existing root error_json SQL prefilter remains 262144 bytes. Unknown, malformed,
oversized, deep, negative or unsafe numeric facts are unavailable individually;
parent work and independent content remain usable. Existing usage stays separate.
No raw error map, provider message/code, token, settings or node-list field.
Public provider-code allowlist is empty: that field is absent and forbidden.

Canonical reachable rejection reasons remain:

```text
cancelled, thread_unavailable, selection_changed, profile_changed,
summary_settings_changed, epoch_changed, run_started, run_stopped,
turn_started, turn_changed, nonappend_generation_changed,
selected_prefix_changed, current_tail_changed, source_changed, source_pruned
```

Failed reasons: `provider_error`, `context_limit_terminal`, `refused`, `empty`,
`not_shrinking`, `cancelled`, `unclassified`.
Origins: `provider_response`, `transport`, `protocol`, `preparation`,
`output_validation`, `storage`, `cancellation`, `internal`.

Reason is assigned by the trusted producer before error wrapping, never by
provider prose or arbitrary deserialized details. A private typed cause travels
with the original error; provider-owned JSON cannot construct that capability.
Unclassified is not proof of determinism. In-flight cancellation is failed with
cancellation origin; actual guard rejection keeps rejected/cancelled.

HTTP status, if present, is actual 100..599 and legal only for provider_error or
final context refusal. Missing HTTP is not zero. Final counts are optional,
strictly positive, context-terminal only, and describe the actual final trial.
Output reasons require output_validation; failed/cancelled requires cancellation.
Cross-field violations quarantine the optional fact.

## Producer And Semantic Consumer

Current first-failing guard order and all source/path/Run/Turn/epoch/cancel checks
remain unchanged. Provider exhaustion, final typed context refusal, refusal, empty
output and nonshrink author closed failure causes at their exact branches.
Preparation/transport/protocol/storage failures retain trusted origin where known.
Original error code/message/return and fatal-storage precedence are preserved.

Replace string-based deterministic failure selection atomically with a typed
predicate whose truth set is EXACTLY refused, empty, not_shrinking and final
context_limit_terminal. Preserve the existing direct nonshrink witness boundary,
original pre-halving endpoint identity, connection/model/resolved cap and the
latest closed/active two slots. No new provider/unclassified suppression, Turn
scope, all-ID fingerprint, slot, retry, POST or backoff change.

## Durable And Public Projection

One saved fact path:
`model_attempt.error_json.automatic_trim_attempt.compaction_outcome`.
Save under original parent/token/active_stage qualification. Retain it explicitly
through token-qualified FINISH_SQL with committed stage facts. Zero-row behavior
and parent state/response_id/usage_json authority are unchanged. An already
committed adoption survives postcommit recording failure.

One current public `compaction_outcome` field, identical shape and decoder:
- WorkItem status list and point observation, existing bounded parent selection.
- compact.list automatic_reduction top-level item. Existing item.reason remains
  trigger/policy; item.state remains parent outcome. Do not overwrite either.
- Existing thread.get last_compaction through bounded display read/prepare;
  preserve InputLimit/unavailable behavior, connection cleanup and CLI coverage.
- One diagnostics compaction_outcome event with IDs and same fact; no old/new
  double event or provider failure labelled adoption discard. Logger pressure
  never changes the compaction result.

Remove current compaction_discard field/type/schema/log writer atomically, no
alias or old-owner RPC fallback. Historical saved/log bytes remain historical;
old-only discard does not become a new fact. Existing retained metrics remain.
Schema generation and strict current consumer tests must agree. Optional absence
means no supplied fact, not compatibility capability. Protocol instance/version
validation remains unchanged.

Readback separately projects allowlisted UsageLedger summary_usage/model_requests
on terminal errors. Admissions begin before send and do not prove delivery or
billing. Preserve known_sum/unknown_count/overflow; unknown is not zero. No new
heartbeat/start/interrupted/adopted/skipped state machine in S1. Manual/overflow
may share typed errors/predicate but receive no synthetic automatic parent.

## Nine Paired Acceptance Groups

Use deterministic explicit cause injection and public assertions, no sleeps or
incidental phase/order oracles. Retain transaction/authority invariant negatives.
Paired vectors are explicit pre-S1 semantics, not paid/live comparisons.

1. All fifteen reachable guards: same first error/adoption/POST/path/epoch/
   provenance/attachment vector, same fact in status list/point/list/log;
   multi-guard precedence unchanged.
2. Exhausted provider401/429/500, protocol/noHTTP/transport/unknown codes: exact
   configured retries/admissions; no new deterministic slots or Turn suppression.
3. Refusal/empty/nonshrink/final-context: same requests and original two-slot
   endpoint identity before/after migration. Existing subsequent blocking only.
4. Context refusal then smaller-prefix success: same calls/source ordering and
   adoption; no failed-stage fact for intermediate retry; actual final counts.
5. Wrong/stale/missing token/stage, interruption/recovery/completed parent:
   zero-row operations cannot recreate observation; same-token finish preserves
   fact and committed stages, parent state/usage/response unchanged.
6. Secret code/message, forged cause JSON, oversized/deep/extra fields, unsafe
   numbers/unknown enums/status: no disclosure, trusted guard or new suppression;
   missing fact does not remove parent/content.
7. Missing usage/crash-before-send/overflow/failed response usage: same partial
   sums and unknown counts, admission distinct from delivery, no double count.
8. Earlier committed spill then summary failure: retained committed evidence,
   unchanged parent result, exactly one semantic failure event, no discard count;
   disabled/full logger cannot undo adoption or manufacture success.
9. All current consumers/schema migrate atomically: dynamic compact.list, status
   page/point and existing thread.get projection; historical facts unchanged;
   no progress/Studio writer or compatibility layer hidden in S1.

## Implementation Ownership And Equivalence Map

The error carrier must keep an in-memory, nonserializable owned cause across
StorageError::Operation; constructing or deserializing ApiError.details cannot
supply that cause. Serialization/deserialization intentionally drops capability.
This is not a protocol field, compatibility parser, global registry or second
classifier. The owned rejected/failed union is also the public outcome value;
only the final observation adds phase and elapsed measurements.

- validate/adopt/marker helpers construct canonical rejected causes, unchanged
  branch order and original code/message. No range_changed umbrella in S1.
- dispatch constructs exhausted provider/context causes before wrapping; retry
  classifier is untouched. Actual protocol/transport errors are unclassified with
  that trusted origin, not guessed provider metadata.
- typed refusal/empty/final projection nonshrink author output-validation failure.
- the deterministic witness consumer reads only the owned union. Its exact four
  reasons and direct nonshrink transaction remain the old behavior vector.
- observed measures the existing stage interval, publishes one final fact only
  under current token/stage, and returns the original operation result. Errors
  without an owned cause are unclassified at their actual available origin.
- FINISH_SQL retains only same-token saved outcome and committed stages; old
  historical discard is not converted into the new union.
- status SQL/decoder is shared by list, point and canonical/bounded compaction
  queries. Dynamic readback attaches the validated fact at item top level and
  separately sanitizes existing usage, preserving policy reason/parent state.
- diagnostics uses one renamed semantic event/envelope field. No legacy parallel
  writer and no change to parent aggregate counter meaning.

Machine evidence must name the exact test/command/exit/source SHA and distinguish
setup failures from intended missing-fact REDs. Original REDs remain preserved
in a test-only commit. No parent metadata CI result qualifies a new S1 commit.
