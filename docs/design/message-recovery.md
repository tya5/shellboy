# Interrupted message delivery (#53)

A message node and its original receipt/Run/Turn assignment are immutable
provenance. Stop, shutdown and startup must not silently strand accepted input
or replay previously admitted history. Use existing Control transactions,
history adoption, context observations and Changed notifications; no new table,
worker, poller or delivery engine is added.

## Admission evidence

Receipt state alone cannot tell whether input entered the selected path before
an interruption. Add one current-format receipt column, `path_admission`, with
values `unadmitted`, `admitted`, `unknown`. It is an admission fact, not another
Run/Turn lifecycle. A fresh receipt starts unadmitted. The transaction that adds
it to its target Thread path marks admitted, even though normal receipt applied
acknowledgement still waits for response adoption. This mark never goes back.
Mark admission in ordinary driver preparation, attachment ordering and explicit
recovery; do not couple all generic history selection to receipt delivery.

Startup migration v13 adds the column and classifies existing rows: applied
receipts, current target-path membership and saved model-attempt receipt IDs
are positive admission evidence. Other old rows become unknown. Absence from
current selection cannot prove never admitted: a user may have rewound or
compacted. Unknown is a valid current fact, not a read-time legacy fallback.
Do not invent timestamps, duplicate nodes/receipts or rewrite prior assignments.

## Terminal repair and explicit recovery

One helper classifies accepted/held receipts whose assigned Turn or Run is
terminal. Invoke it after normal stop drainage, startup interruption repair and
shutdown fallback, and revisit eligible held rows on every startup.

- Admitted input becomes applied with a factual terminal-after-admission reason.
  This means path adoption, not an answered request. No answer is invented.
- Unadmitted input remains held with a reason identifying interrupted/stopped
  before admission. It is eligible for explicit recovery.
- Unknown historical admission remains held with an explicit uncertain-admission
  reason. Never automatically reintroduce possibly removed instructions. It can
  be explicitly cancelled/restored through #52. Deleted targets or missing source
  bodies are cancelled/diagnosed, never resurrected.

On a human's new send, or an authenticated live direct parent's explicit restart
of a terminal child, admit eligible unadmitted nodes into the newly established
Run's selected history before the new message. Preserve original node/receipt
Run/Turn/sender and attachments. Previously unassigned held input joins this same
ordered recovery set and receives only its permitted first assignment; it must
not be appended later behind younger assigned input. Mark those original receipts applied, retaining
an explicit recovered-at-resume reason, and record one existing context_changed
observation with source message/receipt and old/new Run/Turn IDs. The model must
see that these are earlier interrupted instructions. Do not claim the old model
never received an unknown input. Ordinary machine messages do not authorize
recovery; live recovery_wait retains its separate explicit-resume rules.

The send transaction atomically creates the new owner, adopts recovered input,
records provenance and accepts the new message. Original reception order is
created_at then receipt rowid; the new message follows all recovered inputs.
Operation replay returns its original acceptance without repeating recovery.
At most 128 recoverable IDs may be admitted by one explicit resume transaction;
if more exist, reject before commit with input_limit and guide explicit queue
review/cancellation. Never silently skip the older remainder or overtake it.

## Visibility and following work

Message context/status retain held reasons; #52 adds the bounded current queue
projection from the same DB snapshot as selected history. Unknown historical
admission is visibly distinct from ordinary pending input. Its recovery uses
explicit cancel/restore, rather than automatic resending. This conservative
exception to automatic recovery is necessary because pre-marker data cannot
establish that removed instructions were never admitted.

Frontend receipt progress follows the saved delivery fact, not the assigned
Turn's terminal state. Applied input is shown as admitted to the conversation;
that alone does not claim a completed answer. Held input stays held and tracked
until a later saved outcome, without a cancellation/failure restore hint.
A stopped or failed Turn cannot reclassify an applied receipt as cancelled.

Tests use temporary stores and loopback models: stop and actual process death,
pre/post admission, restart of a pre-marker fixture, parent/child and unrelated
caller authority, immutable provenance, ordering/exactly-once, repeated startup,
capacity rollback, source deletion and selection changes. Existing immutable
receipt-assignment triggers and #37 attachment/tool-group rules remain intact.

The bounded admission fact is scalar metadata like existing receipt.state and
hold_reason. Existing logical receipt accounting measures sender/result payloads,
not scalar columns; its formula is unchanged. Recovery observation JSON remains
inside existing event payload accounting.
