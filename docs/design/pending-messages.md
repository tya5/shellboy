# Pending messages, explicit delivery and human Turn interruption (#52)

This extends interrupted-message recovery (#53) and factual tool cancellation
(#56). It uses existing Control admission, DbWorker, presentation workers,
Changed notifications, drafts and popup framework. It adds no table, worker,
poller, independent job engine, timer or model tool.

## Saved queue and paired reads

A pending message belongs to the target Thread, has an accepted/held receipt,
is not in that target's selected path, and has no positive path-admission fact.
Known admitted input removed by rewind is never a queue entry. Unknown historical
admission is a visibly uncertain held entry and requires cancellation/restoration,
not automatic replay. Include human, CLI and other-Thread senders using saved
identity without credentials. Preserve receipt created_at/rowid ordering.

Read the selected history page/window and queue in one SQLite read transaction
on the existing DB worker. Deliver them atomically in one history update, after
ordinary generation/view-revision validation. Carry an ephemeral read-order
revision produced in the serialized DB read; reject older paired replies,
including when only queue state changed. This is not a durable mutation version.
Bound the first 128 entries, excerpts to 512 Unicode characters each, and include
the actual total/overflow count. Full content remains obtainable by source ID.
No expiry removes pending input. Reuse Changed-driven refreshes; clocks do not
query the queue.

Show compact user-style rows above the composer, with sender, pending/held reason
and immediate-delivery mark. A shared-framework queue popup permits inspection,
single cancellation and cancellation of explicitly displayed IDs. Show counts
by sender before cancelling other senders' input. Keep ordinary conversation
history immutable; pending rows are a separate current projection.

The UI's local PendingSend tracks accepting/failure only. Once committed, the
DB queue or selected history owns the saved message, regardless of notification
ordering. A submitted draft revision must not reappear as unsent after a
commit-before-ack crash. Persist its frozen draft/operation correlation before
acceptance as a separate existing draft row and atomically mark that frozen row
submitted in the send transaction. Keep the editable row independently keyed by
the active draft ID: newer edits must not overwrite frozen content. Frozen JSON
keeps ordinary snapshot fields at the root and adds bounded submission metadata
(source draft ID/revision/operation ID). Selection suppresses an editable revision
already submitted, while a strictly newer revision remains editable. Reject stale
autosaves that could restore a submitted revision. Failed precommit content stays
restorable, including a first send before any Thread exists. Never identify sends
by text. Submission IDs derive from the operation identity, not wall-clock text.

## Cancellation and restoration

`message cancel --message ID [--message ID ...]` accepts 1..128 fixed unique IDs
under one operation receipt and transaction. Return each ID's outcome:
cancelled, already_cancelled, already_admitted, not_found, forbidden or unavailable.
Per-item domain outcomes permit partial success; SQL/storage failure rolls back
the whole transaction. Ordinary operation replay returns the saved outcomes.
Humans may cancel any sender's input in the target Thread; an authenticated
model may cancel only messages whose saved sender Thread equals its own Thread.
Do not infer authority from a claimed sender, current Run, parent or target.

Check admission and path membership in the cancellation transaction. The single
DB lane serializes this race with driver/attachment adoption; the first commit
wins. Cancellation changes the receipt only, saves actor/reason through existing
facts, never appends the unread node to model history, and emits ordinary Changed.
No cancellation wakes the model. Admitted-but-rewound messages are too late.

Archive each newly cancelled message independently using an existing archived
draft source reference. Preserve original content/attachment order and immutable
provenance; do not overwrite the active draft. Expose durable archives through
the existing restore flow. On TUI cancellation only, concatenate successful
cancellations in reception order with blank lines if the captured active draft
is still empty and unchanged and the result fits its existing byte budget.
Otherwise retain the individual archives. Replayed/already-cancelled outcomes
must not trigger another automatic restore or notification.

Cancel results use `items` of `message_id`, typed `outcome`, optional `thread_id`
and `archive_draft_id`; no body is copied into the result. An archived reference
has kind `cancelled_message`, source_message_id/source_receipt_id/source_operation_id
and bounded cancellation actor/time. Existing GC roots must protect that original
source and its attachment references. Frozen submission snapshots retain the
ordinary attachment layout for existing file GC. All remain subject to existing
draft/payload limits and Thread deletion; no unbounded in-memory archive cache is
added. `/restore list` and `/restore draft ID` expose durable archives; preserve
argumentless `/restore` as the existing context-restore operation.

## Explicit foreground release

Normal configured send key and CLI send queue at the next tool-group/request
boundary without releasing a foreground shell. `message send --now` explicitly
releases that wait; its process continues and the next model input receives the
saved messages. Persist the bounded delivery-mode fact with the receipt/result
and operation fingerprint. Human and model CLI follow the same rule.

TUI Ctrl+Enter sends now unless Ctrl+Enter is configured as the ordinary send
key. Ctrl+X then Ctrl+S always offers immediate delivery while the composer owns
input. Reset incomplete chords on focus/overlay changes; unknown second keys
retain normal handling. No send mode interrupts response generation. Only an
unadmitted, unselected immediate receipt can release a newly registered wait;
an already admitted/unacknowledged receipt cannot. Models use ordinary delivery
for routine reports and now only for urgent information. Automatic time-based
backgrounding remains deferred.

## Human-only Turn interruption

Esc first dismisses the existing overlay/completion/selection. In the bare
conversation it requests interruption of the exact observed current Turn.
Ctrl+C and /stop retain full Run-stop semantics. No CLI message-send interrupt
option is added. The frontend uses a bounded human-control path; no DB/I/O or
queue-capacity wait moves onto the UI thread.

Add one nullable scalar `turn.interrupt_requested_at`, owned for that Turn's
lifetime (schema v14 startup migration, fresh-schema equivalent). A first human
request CASes the exact active Turn, records its timestamp, increments generation
to reject late response adoption, cancels its in-flight model attempts and drops
pending Revisit. Keep the Turn active until its already-adopted tool group has
complete real/synthetic result pairs, preserving attachment ordering. Repeated
requests are idempotent and never affect a replacement Turn. Run stop/shutdown
and deleted-target facts dominate.

Use a single Turn-bound cancellation token and native-start gate inside the
existing DriverSlot registration. Never use the Run-wide stop token. Binding a
snapshot reads the durable intent, covering commit-before-registration. Reuse
the binding throughout the same Turn, and replace it only after acknowledgement.
Interrupt admission acquires the same asynchronous gate used immediately around
native process creation/publication; no DB callback acquires it. Thus a call
already started may continue, while a call losing to the interrupt cannot spawn.
Acquire the gate before existing native-group mutexes; do not block a runtime
thread on an asynchronous mutex.

Interrupting HTTP saves received partial content audit-only and bypasses normal
retry/recovery. An already adopted response must finish every tool-call result:
unstarted calls save executed:false/human_turn_interrupted; a running foreground
shell returns running, its execution ID and the interruption reason; it continues
in the background. Later calls save the structured prerequisite cancellation
reason from #56. The interrupt never stops background shells or owned child Runs.

Acknowledgement terminalizes only that Turn, settles its already-admitted event
deliveries (not genuinely later completions), repairs admitted receipts, and
carries its known unadmitted input to a new same-Run Turn without reassigning
original receipt/node provenance. The factual context_changed interruption marker
is admitted once before carried/new input. A marker alone does not wake a Turn.
Then always pass through normal driver next(): no work completes the Run, owned
background work keeps it alive, pending input starts a fresh Turn, and later real
completion events retain their usual wake behavior. Do not leave an empty live
Run or restart from an old interrupted event/Revisit.

## Verification

Use temporary stores, synthetic files and loopback HTTP, never user data/paid
APIs. Verify paired reads and reordered replies, post-commit crash/draft restore,
CLI/other-sender input, limits/overflow, rewind/unknown/held input, cancel-vs-admit
serialization, mixed per-ID outcomes/authority/idempotence and durable archives.
Verify normal/now release, no extra model request for routine reports, both send
key configurations and unchanged streaming. At response receive, post-adoption
and foreground-shell interruption boundaries verify saved partials/results,
no losing OS side effect, background/child survival, new same-Run Turn ordering,
Revisit removal, no-work completion, later real completion and full Run-stop
precedence. Record native environments separately from mocks and earlier results.
