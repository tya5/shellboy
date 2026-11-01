# Bounded Saved Inspection Reads (#115)

The notification-driven Thread inspector reads finite indexed native prefixes,
not lifetime-sized canonical queries stopped by a SQLite progress callback.
There is no new index, migration, persisted counter/cache, worker or queue. G
continues to own admission, the read transaction, cancellation, scope/selection
fences and off-DB preparation. Explicit canonical CLI reads remain exact.

## Context Usage

Display reads inspect at most 33 recent Turns (32 plus a coverage witness), at
most 33 Attempts per Turn, and 201 retained replacement links. Existing
turns_by_thread, attempts_by_turn and thread_replacement primary-key ranges bound
these sources. Candidate Attempt reloads use rowid point lookups after the finite
prefix; no automatic whole-table index or state filter precedes its limit.
The selected candidate is materialized before usage JSON projection. There are
two SELECT statements including the Thread/selection scope check, not one per row.
The bound includes up to 1089 Attempt range rows plus 1089 point reloads, not merely
one returned result. Native VM/row evidence is tested under tenfold owned growth.

Native max rowid and timestamp order are not equivalent (including clock rollback
within a live Turn). A saturated prefix therefore cannot claim canonical latest.
For complete small scopes the old exact last_successful_attempt shape is retained.
Incomplete display evidence supplies an explicitly bounded observation/candidate
and coverage notice; it never replaces canonical latest with an invented value.
A negative replacement witness is unknown when its prefix is incomplete. A
positive witness is canonical only relative to a certified latest Attempt; an
uncertified candidate cannot establish global next_request_pending. Independently
unknown usage numbers remain unknown, with no fallback to older measured usage.

## Latest Compaction

The display reader uses bounded source candidates and point enrichment instead
of the canonical GROUP BY/full-history ordering query. Receipt creation order,
Attempt start order, path position and replacement completion time are distinct;
partial coverage must survive inspector preparation instead of being discarded
when extracting the first item. Explicit CLI compaction history is unchanged.
No source aggregate or provenance count may silently rescan lifetime history.
Oversized/unconfirmed metadata is distinct from no saved compaction. The previous
connection-scoped progress/length hooks are removed, not raised or made optional.

## Small Query Maintenance

GC output-size sampling uses one streaming indexed batch and its original shared
output-witness budget; an outer LIMIT after a whole-range sort is not a bound.
Empty/unconfirmed executions and the one combined sentinel retain their meaning.
Shell inspection reads one indexed last-sequence chunk for both its timestamp
and length witness, not MAX(received_at), which can disagree after clock changes.
CLI syntax metadata is derived from clap while typed operation meanings, field
identity, effects, permissions and intentional TUI limitations remain explicit.

## Validation And Limits

Use synthetic temporary stores and loopback fixtures only. Count actual query
VM/rows and statements, compare saturated owned sizes at 1x/10x, inspect EXPLAIN
and preserve scalar/copy limits. Clock rollback, partial selection, missing/huge
usage, unknown latest values, source gaps and stale G adoption are independent
cases. Time is recorded only, never a new latency acceptance gate. Python SQL
preparation is not native Rust evidence. Local checks are focused through
rust-slot; the full standard check and all three OSes run on the pushed branch CI.

## Display/Canonical Boundary Decision

Adopted for this implementation: bounded display evidence is a separate private
observation, not a silent redefinition of canonical `context metrics` or
`compact list`. With the existing timestamp indexes there is no proof that an
unobserved old timestamp cannot have a higher Attempt rowid, or that a receipt
created earlier cannot finish later. No migration or counter is introduced to
pretend otherwise. Revisit this choice if an indexed canonical ordering witness
is separately approved. The #125 proactive trigger continues to call the exact
reader with no display bound; bounded observations cannot trigger compaction.

For context usage, saturation produces `bounded_observation` with a candidate and
explicit latest/replacement coverage. `last_successful_attempt` is null when
canonical latest is unconfirmed; pending is null without canonical evidence.
Small complete scopes retain their original shape. A positive replacement
witness can still establish pending for a certified latest Attempt even when
negative replacement coverage is incomplete.

For compactions, native receipt creation and retained replacement membership
prefixes are 201 each; automatic records come from 33 Turns and 33 indexed
records per Turn. Ordering, grouping and point enrichment happen only inside
these finite sets. At most five projected records per source reach off-DB
preparation, under the existing aggregate 256 KiB copy budget. Source documents
are capped at 128 KiB before JSON projection. Earlier failure counts and
replacement provenance counts inspect at most 201 candidates; saturation is
unknown, not zero. Spill association is not inferred from incomplete provenance.
Display coverage retains `latest_complete` through raw capture, preparation and
UI adoption; incomplete records are labeled bounded candidates, not no matches
or canonical latest. CLI history, source bytes and transaction state are unchanged.

The GC batch is a bounded UNION ALL of indexed per-execution streams. A single
outer output limit applies the shared budget plus one sentinel. There is no
whole-range sort, per-owner materialized JSON aggregate, or budget refill.

`CLI_BINDINGS` is initialized once from the pure clap command tree. Owned syntax
strings live in that immutable catalogue, without leaked allocations or
self-referential storage. Only operation/path/field/alias semantic links remain
explicit. Effects, permissions, confirmation, TUI defaults and its intentional
form limits are not inferred from syntax. In particular attachment forms still
prefer file before image, independently of clap's enum declaration order.

Native synthetic evidence on macOS (before final rebasing): context query
VM/fullscan steps are `(41300, 696)` at both 64 and 640 owned Turns, each with 64
Attempts; audit membership simultaneously grows from 256 to 2560. The actual
receipt/automatic/replacement queries record respectively `(104976, 200)`,
`(10086, 96)`, `(5461, 200)` at both 256 and 2560 owned records of every source
family. These are VM/row-work evidence, not latency thresholds, Python estimates,
or a promise that incomplete candidates are canonical latest. Tests also retain
the exact unbounded canonical readers as a contrast. Final SHA/OS qualification
belongs to the task handoff and CI, not these pre-rebase measurements.
