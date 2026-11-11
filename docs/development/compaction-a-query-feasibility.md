# Sole #A Check: One Candidate And Source Counterexamples

Source4e6241a9; specificationb822f2b5. Design only, no SQL executed. No table,
column, index, application behavior or provider request added. Display33/201
caps and LIMIT alone are not a bound proof.

## Required Fields And Existing Sources

| Meaning | Existing field | Missing qualification |
| --- | --- | --- |
| Automatic trial identity/noop/failure/discard | model_attempt.error_json automatic_trim_attempt; triggering attempt PK; profile_id/model_id | Can locate one known attempt directly, but no persisted trial completion timestamp in begin_tx/FINISH_SQL; model_attempt.ended_at is provider completion BEFORE trial, not trial end. app_meta active started_at witness is transient, not completion chronology. |
| Manual adopted compression | replacement.created_at/format_version/state plus thread_replacement association and output node | No explicit connection/model columns or manual-vs-auto marker in replacement. Revert does not necessarily erase historical adoption chronology; current selection alone cannot identify last historical boundary. |
| Scope Thread/connection/model | thread.id/profile_id/model_id; attempt.profile_id/model_id | Current matching names do not distinguish A->B->A changes. Scope revision can change for unrelated settings, so cannot equate every revision with a scope switch. |
| First completed request after boundary | attempt.state/ended_at/started_at/usage_json; turn.thread_id | Provider input tokens come from context_usage::prepare's existing normalizer, not character estimate. Need a causal order deciding request start vs completion across trial/manual adoption, plus stable tie ordering. Existing attempts_by_turn orders start, not end. |

## Single Minimal Candidate (Conditional, Not Adopted)

Given an EXACT already-known first eligible Turn ID and a captured boundary
attempt-start lower bound, the existing index can seek a single Turn:

```sql
SELECT a.id, a.usage_json, a.started_at, a.ended_at
FROM model_attempt AS a INDEXED BY attempts_by_turn
WHERE a.turn_id=:turn_id AND a.started_at>:boundary
  AND a.state='completed' AND a.profile_id=:connection AND a.model_id=:model
ORDER BY a.started_at ASC
LIMIT 1;
```

Expected access shape is SEARCH attempts_by_turn(turn_id=? AND started_at>?).
That SEARCH bounds the interval by key, NOT the number of rows inspected. An
arbitrarily long failed/wrong-scope suffix may precede the first qualifying row.
The predicate also orders by request start rather than actual completion and
cannot locate the first eligible Turn without unbounded empty-Turn traversal.
Thus this is a candidate to REFUTE, not an exact bounded #A implementation.
Changing to attempts_completed_scope can seek completed rows for one scope but
has no timestamp order in its key; sorting or searching many scoped rows still
has no fixed bound. No existing cross-Thread completion-time covering index was
found; missing a record is not license to return fabricated0.

## Concrete Source Counterexamples / Stop Result

1. Provider attempt E finishes at t1 and triggers automatic trial. Trial X ends
   noop at t2. Another already-admitted request C completes between t1 and t2.
   Querying after E.ended_at chooses C, but querying after trial completion must
   not. begin_tx and FINISH_SQL do not persist t2. Stage elapsed_ms is neither a
   wall-clock ordering nor present for all noop/success trials. Existing facts
   do not supply the required ordering for that history. Even if current driver
   serializes some paths, manual/Run-independent ordering must be specified and
   proven rather than assumed from the token count.
2. Thread scope A->B->A with no compaction under B: names match an old A attempt,
   yet rule7 resets #A on scope change. Same final names and matching old attempts
   do not prove no intermediate switch. No dedicated scope-change chronology
   with exact causal witness identified in the inspected records.
3. For any cap N, construct one Turn with N failed attempts newer than boundary
   before one eligible completed attempt. LIMIT1 on attempts_by_turn scans N
   failures; alternatively N Turns with no matching attempt defeat a fixed
   recent-Turn cap. An index SEARCH and output LIMIT are not work bounds.

Result: existing query/record mapping is insufficient to establish exact #A
for all required histories. No production implementation should proceed on
this candidate. This is a SOURCE counterexample, not a runtime trace of a user
DB or proof that every alternative using existing storage is impossible.

## Question Before Further Design

May an existing app_meta per-Thread/scope key store a bounded #A state/witness
updated transactionally at trial completion (including noop), manual adoption,
scope change and first completed provider report? This would add NO table or
column, but it IS a new persistent writer/contract and is not approved merely
because app_meta exists. It could make reads fixed PK seeks rather than history
scans. Do not implement until USER/leader clarifies this interpretation of7/12.
If only retrospective existing history queries are permitted, report inability
rather than add storage/schema or infer missing boundaries.

## Historical Verification Proposal: Withdrawn / Not Needed

The earlier optional memory-only Python EXPLAIN/progress-count proposal was
withdrawn by the author and leader. It is NOT a follow-up task or prerequisite.
Missing chronology/manual scope/scope-change evidence cannot be recovered by
query-plan experiments. No experiment ran; do not reopen this investigation.
The source counterexamples remain bounded findings, not proof that every
possible existing-storage alternative is impossible or an actual user trace.
