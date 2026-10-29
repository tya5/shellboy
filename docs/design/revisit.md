# Revisit: one pending self-instruction per Run

Issue #36 is adopted after the interrupted-tool invariant repair (#37).
Revisit supplies one execution opportunity at normal Run completion, without
interpreting goals, classifying questions or adding a general hooks system.

## Ordering and authority

`driver::next` is the normal completion decision under the existing Thread
boundary and DB transaction. Only after active/recovery Turns, pending events,
attachments and owned shells/children are absent, consume a pending reservation
and create an ordinary `revisit` event/delivery and Turn instead of completing
the Run. The original set operation, setter, target Run, consumed count and
instruction travel in a developer-role event source. A child therefore reports
`child_run_ended` only after its Revisit work finishes. Other input and stop
mutations serialize on the same boundary; stop/shutdown admission wins over
starting a further request. No scheduler, worker or extra queue is added.

Models may address their own authenticated live Run (omitting --run), or a Run
they directly own while their own Run remains live. Humans must specify --run.
Use direct ownership, like retry, because this mutates one Run's instruction;
descendant cancellation is intentionally broader. Own-Run operations differ
from retry's recovery-only guard. Reject unrelated or non-direct descendants.
All three operations reject terminal/stopping Runs. Set replaces a single
pending reservation; get exposes setter identity, full bounded text, count and
last firing time. Set reports whether another setter was replaced and a bounded
preview. Cancel is idempotent. Get is receipt-free; mutations retain existing
operation fingerprint/replay/outcome budgets. Unlimited re-registration is the
default; optional `limits.revisit_max_per_run` rejects set at the consumed limit
with `revisit_limit_reached` (zero disables registration). Text is 1..8192 UTF-8
bytes. Reject blank text.

## Persistence and bounds

Use one `app_meta` key `revisit.RUN_ID` holding pending reservation (or null),
consumed count and last firing time. Setter/operation/time are saved with the
instruction. Repeated cycles overwrite this bounded state rather than creating
metadata per firing; events/history grow under normal retention/compaction.
Count bytes in existing history usage by extending its derived covering index
through isolated startup migration 12. Enforce existing logical capacity for set
and firing. Receipt admission remains the normal mutation budget. No table,
Core lifecycle enum or dependency is introduced. The DB worker owns state;
request-local decoded state is released at transaction completion.

Consume and enqueue in one transaction; this is once-only durable delivery,
not exactly-once model execution or external effects. Existing retry/recovery
semantics apply. The event source is associated with its new Turn and protected
as its current request by trim and summary compaction. Include its kind and
original operation provenance in raw history; the presentation uses English
`Revisit` labels and an instruction preview. Add pending/count/time facts to
existing Run, Thread-list and status projections; animation is not a Core fact.

## Stop, shutdown, restart and deletion

Explicit stop drops pending reservations atomically with freezing ownership.
Shutdown captures original pending text/setter/count in the existing interrupted
context first, then drops pending reservations; forced exit is repaired on the
next startup. Startup captures previous reservations, then drops them before
old work is terminalized. A normal startup context_changed event carries those
facts at the Thread's next actual Turn; no automatic Revisit is resumed. Preserve
consumed counts for inspection, and remove metadata when its Thread is deleted.
No pending reservation can revive a stopped Run, and a second owner never fires
an earlier owner's reservation.

A question to the user is not detectable by Core. The Agent Guide instructs the
model to cancel/rewrite before ending with a question, or finish briefly if a
Revisit fires while awaiting the user. An unnecessary request is an accepted
cost; no question classifier or special silent completion state is added.

## Verification

Use temporary stores and local HTTP mocks: one-shot delivery, many re-registered
cycles, optional limit, operation replay/replacement/cancel, own/direct/unrelated
authority, completion/input/stop races, owned work ordering, child reporting,
recovery and shutdown exclusion, interrupted capture, deletion and compaction
protection. Native signal behavior is tested separately under #38. A synthetic
mechanical model does not demonstrate improved real-model completion judgment;
that evaluation, including missed requirements and question false triggers,
must be reported separately from delivery correctness.

## Review refinements: completion, damaged metadata and capacity (#36)

Closing the request-start gate prevents firing an existing reservation, while an
idle Run without a reservation retains ordinary normal completion (including
its normal child completion report). The shutdown check therefore applies only
after a pending reservation is known to exist.

Current-format State decoding stays strict; missing required counters are not
interpreted as zero and no compatibility defaults are added. A malformed optional
Revisit summary is omitted from Run/list/status observations; explicit `revisit
get` reports an invalid-state error with a stop/restart hint. Stop and lifecycle
clear paths discard unreadable metadata without reconstructing a counter. Valid
metadata retains consumed counts when pending text is removed. Interrupted
capture prevalidates each Thread before writing its snapshot; a malformed
Thread is diagnosed and isolated rather than blocking startup or other Threads.

If capacity blocks firing, the completion transaction rolls back: the pending
instruction and consumed count remain unchanged. Existing preparation recovery
makes the Run recovery_wait. After capacity is fixed, explicit retry may issue
one ordinary request without a new input before normal completion retries the
reservation. This reuses recovery ownership and avoids a second retry engine.

`child_turn_completed` reports a Turn, including each Revisit Turn; only
`child_run_ended` reports the child Run's terminal completion. A directly owning
parent's Revisit instruction arrives as the same developer-role event as a
self reservation, rather than the user-role instruction created by message send.
