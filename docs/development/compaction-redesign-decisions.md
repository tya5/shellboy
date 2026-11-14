# Engineering Decisions Under USER Delegation

These are LEADER/CLAUDE RECOMMENDATIONS under USER's general engineering
judgment delegation, not individually USER-authored choices. Alternatives,
reasons, risks and controls are recorded here. No paid/installed/DB authority
is inferred. Runtime proof remains absent until cloud checks actually execute.

## D1 Bounded #A State In Existing app_meta (Review Before Implementation)

Alternative retrospective history queries cannot prove the required boundary/
scope chronology with existing indexes. Choose ONE fixed key per Thread:
compact.a_state.<Thread>, versioned JSON, bounded bytes. No table/column/index.
Fields: version, scope_generation, connection, model, boundary_generation,
first_report_attempt (optional), first_input_tokens (optional). Read PK only.
ScopeGeneration is a causal counter, not a time or character-token estimate.
Absent legacy state reads #A=0 per specification, with an absent evidence marker;
malformed/oversized state is unknown/error, never a fabricated measured0.

Writer seams, all within original caller transaction:
- Thread profile/model mutation in control/db.rs: if EFFECTIVE connection/model
  changes, increase scope generation and clear first completion. A->B->A causes
  two increments, never restores old A state. Unrelated title/effort changes do
  not invent a scope change. Initial scope is established explicitly.
- Trial finish including noop/failed/discard: advance boundary generation and
  clear first completion only at terminal transition, idempotent by trial ID.
  begin_tx is not completion. Duplicate terminal callback must not reset twice.
- Successful manual adoption: reset boundary in same SummaryAdoption.adopt_tx
  transaction; failed/reverted operation is not an adoption. Tag origin explicitly
  so automatic adoption and trial-finalization do not double-reset. A manual
  success is chosen to participate as the original Claude recommendation.
- Applied provider complete_attempt: use normalized provider input tokens only;
  in its existing completion transaction, fill first_report iff current scope
  matches and no first completion is present. Later completions cannot replace
  it. Missing provider input is not token0; retain pending first eligible report
  semantics for independent review. No wall-clock ordering or history scan.

Concurrency: SQLite writer commit order is the causal order. A completion before
boundary commit belongs to old boundary, after it to new boundary. Carry scope/
boundary tokens into async operations and CAS on return; retired operations must
not reset a newer scope. Rollback rolls back state with the original operation.
This is not a claim that thread request start time and completion time coincide.
Concurrent manual adoption + trial finish need expected-generation CAS; losing
old completion must not overwrite a newer reset. Whether first completion with
missing input consumes the slot must be resolved in review before code: proposed
record report identity as unknown, never substitute a later report silently.

Controls planned: absent0 vs corruptunknown; first of two completions wins;
manual adoption resets; noop resets; duplicate terminal no reset; rollback;
A->B->A scope; retired operation CAS; missing usage; fixed key count/size bound.
Risk: incomplete callsite coverage could give stale #A. Do not connect selector
until every writer seam has actual transaction tests. This decision authorizes
proposal only pending independent S6 source/design review, then implementation.

## D2 Settings Persistence Proposal (Review / Number Assignment Separate)

Existing Thread table needs nullable target percentage and summary character
cap; connection optional values stay in config. Reuse proposed old0043 number
ONLY after leader fresh-max/reassignment confirmation, never import old SQL.
Draft target column auto_compact_target_percent INTEGER CHECK(NULL or integer
1..100). Draft summary_max_chars INTEGER CHECK(NULL or integer>0). Existing
settings convention suggests explicit integer validation, empty CLI clear,
effective get while threshold0, Thread->connection->default20/default20000.
These ranges/names are engineering proposals, not carried from historical303
without review. 40-42 immutable; no DDL/registry change in this child. Old rows
NULL, no attempt history backfill. Removing obsolete public controls is breaking
pre-release API: canonical schema/docs and strict consumers must coordinate.
Risk: zero/too-small output cap; retain refusal/nonshrink and record shrinking
oversize without resend. No quality gate, hidden floor or budget expansion.

## D3 Failure Witness (Already Adopted Leader Recommendation)

Two latest slots per Thread, closed/active, original endpoints+connection/model/
outputcap. Alternative unbounded range set and capacity-stop were rejected.
Same-kind replacement intentionally allows older range later. No epoch or source
revision extends identity authority. Atomic adoption remains independent.
Original identity is captured once in the new pure Trial object, not recomputed
from a halved prefix; success is terminal and never schedules its remainder.

## D1 Review Conditions Resolved (Leader Recommendation Under Delegation)

Use max4096 UTF-8 JSON bytes, identifiers1..256 UTF-8 bytes, version1,
u64 counters with checked_add; overflow/corrupt/oversize returns InvalidState,
not0. Add bounded last_reset_origin (one ID, no historical set). Terminal writer
must own the existing durable attempt-row terminal CAS (or one manual adoption
row transition); reset also requires expected scope+boundary generation. Matching
last origin makes immediate duplicate idempotent. A delayed old origin with a
stale generation cannot reset any newer unrelated boundary. Manual automatic
origin must be explicitly shared/skipped, not deduplicated by a timestamp.
Trial-noop uses its existing qualified automatic attempt-row finalization, not a
new unbounded operation ledger. Production integration must make reset and that
terminal CAS one transaction and rollback both if either storage step fails.

A request started before a boundary but Applied-completed AFTER the boundary is
eligible as the new boundary's first report. Completion carries scope token only;
NEVER reject it solely for old boundary generation. Scope ABA invalidates the
old scope token even if names return. A first completion with absent input usage
records identity with tokensUnknown; later completions cannot replace it. The
selector must propagate Unknown to the existing bounded read-refusal/defer path,
never0 or an additional automatic request. No-completion and legacy absence are
0 by the adopted rule, with explicit evidence kinds rather than measured0.

Thread deletion must delete the one #A key and closed/active failure keys in the
same Thread-delete transaction. No periodic scan/TTL/new cleanup daemon. An
inflight retired Thread cannot recreate keys: transaction writers must check
nondeleted Thread and their existing owner transition before save. Initial
helper source is unconnected; lifecycle/writer-ownership checks are mandatory
at integration, not assumed from pure fixture success.

D2 summary character cap accepted range is1..i64::MAX; public u64 parsing rejects
larger values instead of casting/wrapping. Thread percentage remains1..100.
No migration number is selected by this clarification.

## D2 Adopted Leader Storage Scope / DDL Delta For Review

Leader adopts reusing the OLD UNAPPLIED43 slot only after fresh max confirmation;
new SQL is authored independently, not copied from historical427c branch.

| Item | Historical unlanded43 | New reviewed design proposal |
| --- | --- | --- |
| Thread target percent | nullableINTEGER typeof integer1..100 | same nullableCHECK, but percentage only compares #A, no removal estimate |
| Thread summary_max_chars | absent | add nullableINTEGER CHECK NULL or typeof integer AND1..9223372036854775807 |
| Existing rows | NULL/inherit | NULL/inherit20/20000, no attempt backfill |
| Bound rationale | percent1..100 | outputcap upper=i64MAX storage exactness/publicu64 subset; not a quality/performance-derived ceiling |
| Existing40-42 | immutable | immutable;43 registry/FRESH only after source review/fresh guard |

DraftDDL: ALTER TABLE thread ADD COLUMN auto_compact_target_percent INTEGER
CHECK(auto_compact_target_percent IS NULL OR (typeof(auto_compact_target_percent)
='integer' AND auto_compact_target_percent BETWEEN1AND100)); second ALTER adds
summary_max_chars INTEGER CHECK(summary_max_chars IS NULL OR
(typeof(summary_max_chars)='integer' AND summary_max_chars BETWEEN1AND9223372036854775807)).
This paragraph is notation, not executable SQL (actual statements must use
separate BETWEEN numeric tokens). No SQL/registry change in this checkpoint.

## Public Pre-Release Compatibility Removal

Old Thread/manual segment_chars/reduce_rounds and manual max_percent flags,
DTO fields and UI form controls are removed, not ignored compatibility shims.
Old connection segment_chars/reduce_rounds and old [compaction] summary/map/
reduce controls are rejected as unknown input. [compaction] auto_at_tokens is
retained; new output/target settings resolve Thread->connection->defaults.
Persisted old Thread columns/cache data remain untouched. Internal snapshot /
preview scaffolding still being removed is not a supported public setting.
Strict consumers must regenerate against final schema before release; old
configuration must be edited by its owner before activation. This task does
not read/edit real user configuration or migrate installed data.

## Structural Repair Of Repeated Migration Failures

| Failure group | Root cause | Authority / repair |
| --- | --- | --- |
| Defaults overrode Thread settings; preview and effective output diverged | Multiple independent priority implementations | SummarySettings::resolve is sole default/priority authority for selector, request, preview and readback; deleted old separate resolvers and OutputLimit default logic. |
| DTO fields removed while fixtures still construct obsolete contracts | Positional field repetition rather than semantic construction | ContextEditArgs::for_range and ThreadSetArgs::summary_settings own construction; protocol and settings fixtures use these typed entry points. Removed fields remain removed. |
| Raw/function boundaries damaged by syntax-guessing substitutions | Regex treated Rust text as syntax | Stopped syntax-guessing edits. Restored explicit original boundaries; subsequent edits use named exact semantic blocks only. |

Request formats model input only; Trial owns original identity/cursor/termination;
adoption owns final saved-projection shrink check and transactional revalidation.
This refactor does not merge those responsibilities, weaken guards, add legacy
fallbacks, or claim tests passed. Remaining obsolete runtime fixtures are part
of this same migration group, not a reason to restore removed public fields.

## 2026-11-12 — #A request-start boundary ordering (leader decision)

A #A report belongs to the first request initiated after the completed compaction
boundary, not the first provider response that completes afterward. `input_tokens`
reports the request's start input; therefore persist both request-start scope and
boundary generations and require both to match when completion is recorded. A
stale pre-boundary completion does not become #A and does not reset/retry; the
next request captures the current boundary. Preserve scope ABA guards. Required
race fixture ordering: old request start → boundary commit → old completion
ignored → new request start/completion records #A.

## 2026-11-12 — #A request-start boundary ordering (leader decision)

A #A report belongs to the first request initiated after the completed compaction
boundary, not the first provider response that completes afterward. `input_tokens`
reports the request's start input; therefore persist both request-start scope and
boundary generations and require both to match when completion is recorded. A
stale pre-boundary completion does not become #A and does not reset/retry; the
next request captures the current boundary. Preserve scope ABA guards. Required
race fixture ordering: old request start → boundary commit → old completion
ignored → new request start/completion records #A.


## 2026-11-14 - Migration 43 factual correction (source preparation)

The earlier pre-release paragraph claiming persisted old Thread columns remain
untouched is historical and is superseded by the final applied migration 43.
`0043_thread_single_summary_settings.sql` adds the two current override columns
and drops `segment_chars` and `reduce_rounds` without archival or translation.
It does not rewrite history, receipts or usage JSON. This note corrects the
contract description; it does not approve that loss as a new preservation policy.

Do not edit the applied migration or infer that removed values can be recovered
from the current schema. Preservation of values still present in an older
synthetic schema and recovery from a separately authorized source are distinct
forward-migration tasks. No user database, installed owner or migration is
executed by this documentation correction.


## 2026-11-14 - Raw overrides on branches

The new target/output overrides were copied by child creation but omitted by
fork/rewind, unlike the threshold and later output-token/spill overrides. No
explicit reset policy was found. The selected correction is one typed raw
override snapshot shared by creation and branches, preserving NULL rather than
flattening connection defaults. The alternative of resetting only the two newer
settings would retain an undocumented exception. Parent changes remain isolated
and a copy failure rolls back branch creation. This does not inherit GC owner
state or alter the public settings/schema contract.

## 2026-11-14 - Canonical scope identity / forward migration 46

A private public-name-length bound in GC factual state rejected otherwise valid
ordinary requests even with automatic compaction disabled. The chosen correction
uses one domain-tagged JSON tuple digest for all names, not truncation, a larger
arbitrary bound or a long-name-only special path. Scope/boundary generations and
original first-known evidence are preserved rather than resetting derived state:
reset-to-zero could revive an old request token after ABA.

The leader assigned current-main migration 46 after inspecting tracked max 45;
the unlanded scheduler reservation moves separately to 47 with its existing owner.
No scheduler code or placeholder is included. Historical format parsing exists
only in the startup conversion module; runtime accepts current v2 only. Invalid
raw values retain their bytes and SQLite type, while real SQL errors roll back
the same Immediate transaction and ledger checkpoint. Applied migrations 1-45
are unchanged. Implementation review and native qualification remain separate
from the accepted contract; no installed/user database operation is authorized.
