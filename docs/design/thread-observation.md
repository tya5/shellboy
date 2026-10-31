# Thread observations and resumable wait (#74)

## Scope

Add thread_settled to the existing any-condition wait, without a subscription
service, table, worker or queue. Reuse the existing notification-before-read
loop, receipt, cancellation, ownership graph and final deadline reread. A wait
never stops its targets. No timer polling is introduced.

A condition supplies 1-64 thread_ids and a nonempty states list drawn from
completed, stopped, failed, recovery_wait. Across a request there are at most64
Thread IDs. IDs are bounded to128 bytes; duplicate IDs/states within a condition
are rejected. Existing non-Thread wait conditions can coexist. --after is only
valid when a Thread condition is present. At most64 changes are returned.

Without after, read current settled states and a baseline cursor atomically.
An active Run is not completed just because an older Run ended. A Run with a
recovery Turn is reported as recovery_wait. An empty Thread has no settled Run.
With after, report saved matching transitions even if a newer Run is already
active or the recovery Turn has resumed. Recovery is satisfied for this explicit
condition, not a claim of successful model work. Other existing wait semantics
are unchanged. snapshot.changes contains Thread/Run/state/stored at and optional
Turn identity; missing timestamps remain null. cursor is the Thread cursor when
these conditions are present, otherwise the existing shell-output cursor.

## Durable source and bounds

Terminal Run rows already retain state and ended_at. Recovery facts use the
existing child_recovery_wait event: root Runs also retain that same fact without
a parent delivery. This small write at the existing recovery transaction is
necessary because recovery_reason_json is cleared on resume. It creates no new
event kind and changes no admission, delivery or lifecycle decision. It is not
inferred from arbitrary failed attempts, retry diagnostics or error prose.

Seek by Thread/state and implicit indexed row order for terminal Runs; seek the
same Thread's saved recovery events through an expression index. These source
rows are retained while the Thread exists. A versioned, query-scoped URL-safe
base64 cursor stores a position and immutable ID anchor per Thread/state stream.
It is independent of owner instance and wall-clock ordering. Validate anchors
rather than silently skipping after removal/replacement. Deleted Threads become
unavailable; unknown targets are errors. Changed filters or malformed/excessive
cursors are rejected. External SQL changes/VACUUM are not a supported continuation
mechanism; invalidated anchors require a fresh observation.

A request uses at most256 streams. Read only one next row per stream, then use
round-robin selection (also retained in the cursor) until64 changes are emitted.
Advance only consumed stream prefixes. This bounds work/storage and avoids both
clock-regression loss and starvation when one Thread remains busy. more means
another saved matching row is already available; continue with the returned
cursor. Initial baselines and timeout results also return a cursor. Keep that
baseline within a running wait so rapid finish/restart between wakeups is seen.
Human observers do not read the global model ownership graph. Model observers
retain self/ancestor/composed-cycle protection, including Thread-to-live-Run
edges, through the existing registry.

Schema16 adds only derived lookup indexes. Actual hot SQL must have EXPLAIN
QUERY PLAN tests proving Thread/state/range or primary-key seeks without a
history-wide scan or temporary order. No saved PID authority or provider work
is part of these observations.

## Latest answer and input

ThreadList and ThreadGet retain existing excerpt and add nullable last_answer
and last_input. last_answer means the latest own saved Turn with answer_node_id,
not a tool call, provisional response or another Thread's fork-only ancestor.
Include node_id, turn_id, node creation time and at most512 characters of natural
assistant content. last_input is the latest durably accepted receipt for that
Thread (including held/cancelled), with message/node identity, stored time,
receipt state and the existing credential-free MessageSender projection.
It does not imply the input was applied. Labels remain self-asserted.

Use one indexed latest-row seek for each projection, then bounded natural-text
excerpts (at most64 text fragments and512 characters), never collecting whole histories. Missing/reclaimed bodies produce an
empty excerpt with body availability stated, not invented content. These are
saved per-Thread facts, not an expansion of the selected fork path. Initial
scope excludes a live change subscription, an idle-with-no-Run state, a new
TUI watch screen and cross-Thread fork-answer fallback. Existing list paging and
limits are unchanged. All tests use synthetic temporary data and the shared
watchdog, never latency assertions or real model requests.

Root recovery recording is the only additional transition-side work: the
already-existing child_recovery_wait fact is retained even when no parent is
present. This is necessary to observe recovery/resume in an unobserved interval;
there is no delivery for root Runs and no new event kind or state machine.
