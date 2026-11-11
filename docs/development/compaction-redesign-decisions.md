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
