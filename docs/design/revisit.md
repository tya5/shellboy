# Revisit: a message delivered when the Run completes

Issue #187 replaces the per-Run reservation of #36. A Revisit is an ordinary
message sent with a delivery timing: `message send --deliver complete`. It is
not bound to a Run, has no separate store, command, event kind, setter record,
counter or limit, and adds no worker, queue or table.

## Delivery timing

`message send` takes one optional `--deliver` value. All three timings are the
same message, receipt, cancellation and inspection path; only the moment of
admission differs.

| Timing | Meaning |
| --- | --- |
| omitted | Queue for the next safe boundary (tool-group or request boundary). |
| `now` | As omitted, and also release a foreground shell wait. Never interrupts a model response. |
| `complete` | When the target Thread's live Run would complete normally. With no live Run, exactly the omitted timing. |

`--deliver complete` against a live Run saves the message as a held receipt
with `hold_reason='run_completion'` and no Run/Turn assignment. The result says
`deliver: complete`, `delivery_status: held`, `assignment: pending`. When no Run
is live, the result is that of an ordinary send (`new_run`, or `held` behind a
stopping Run under the ordinary rules). The caller therefore sees whether the
message was deferred or delivered, and receives the message ID either way.

Ordering is per timing class, not one FIFO. Default and `now` messages sent
later are admitted at their own boundaries and overtake waiting `complete`
messages; a waiting `complete` message never blocks or delays them. Waiting
`complete` messages keep their send order among themselves.

## Completion boundary

`driver::next` is the normal completion decision under the existing Thread
boundary and DB transaction. Only after active/recovery Turns, pending events,
attachments and owned shells/children are absent, it assigns every waiting
`complete` message of the Thread, in receipt order, to one new Turn of the
completing Run (`accepted`, with `delivery_context.reason='run_completion'`).
The existing receipt admission then appends them like any message and the Run
continues. A child therefore reports `child_run_ended` only after that work.
A human Turn interruption (Esc) is no exception: when the Run would then
complete, the waiting messages are delivered the same way.

A waiting message never outlives its Run (#187, user rule of 2026-11-03).
When the Run ends or stalls in any other way, every waiting message becomes,
at that moment, the same held input an ordinary send at that moment would have
produced; the existing message rules alone decide what happens next:

- stop (explicit, Ctrl+C, owner stop): `held` with `stopping`, unassigned.
  The next explicit resume (a human message or a live owning parent's
  restart) admits it before the new input, like any input held by a stop;
- application shutdown: `held` with `previous_instance_interrupted`. Startup
  applies the same hand-off for a forced exit. The next resume admits it;
- entering recovery wait: assigned to the recovery Turn, `held` with
  `recovery_wait`. An explicit retry sends it with the retried request. A
  `complete` send while a Run is already in recovery wait is likewise an
  ordinary send.

Once request-start admission closes for shutdown, the completion decision does
not start a Turn for waiting messages; the shutdown stop hands them off. The
interruption record captures them as unreflected messages.

Messages are bounded by the existing message limits (`limits.draft_bytes`,
logical capacity at send). Cancellation is `message cancel`; cancelling one
waiting message changes only that receipt. Inspection is `message get` and the
pending-message queue, where the hold reason shows the waiting timing. Thread
deletion cancels them with every other queued message.

## Authority

Unchanged from `message send`: a model addresses its own Thread by omitting
`--thread`; a live parent may send to its child under the ordinary ownership
check; humans may send to any Thread. There is no Run target and no replace
operation: several `complete` messages may wait at once.

## Observation

Run, Thread-list and status projections keep the optional typed summary
`{pending, consumed_count}`: `pending` means at least one `complete` message of
the Thread is waiting; `consumed_count` is the number of `complete` messages
that Run received at completion. The TUI keeps `Revisit N`, `next reserved`
and the narrow `Revisit N+`, and the `↻` row kind for messages delivered at
completion. Schema migration 24 adds the partial index
`receipt_run_completion(run_id) WHERE deliver='complete'` so the count does not
scan a Thread's receipts.

## Limit decision

`limits.revisit_max_per_run` is removed. It capped repeated self-registration
on one Run. A model can already continue indefinitely with ordinary messages to
its own Thread, so the cap guarded nothing that messages do not; stop and Turn
interruption remain the human controls. No replacement setting is added.

## Migration

Migration 24 deletes the old `revisit.RUN_ID` app_meta rows (they are not moved;
pre-1.0 policy) and adds the index above. The `revisit.%` pattern stays in the
history-byte index and the #171 usage-counter triggers; no row matches it after
the delete, and rewriting those triggers is not worth a schema change. Old
`revisit` events in history render as ordinary saved events.
`scripts/check-release-upgrade.py` and `scripts/error_catalog.py` target schema 24.

## Verification

Integration tests use temporary stores and a local HTTP mock: completion
delivery in send order, immediate delivery without a live Run, a Run completing
between the original send and the `complete` send, `now`/default overtaking
waiting messages (including interleaved sequences), cancelling one of several,
release at stop, at Esc, at shutdown (admitted after restart) and at recovery wait,
owned background shell ordering, and the model shell CLI path. A unit test
covers the closed start gate.
