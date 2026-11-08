# Delegation and coordination

[Guide index](README.md) · [Agent Guide](agent-guide.md) · [CLI reference](cli-reference.md)

A child agent is an independent Run on a child Thread. The Thread hierarchy organizes history and settings, not
execution ownership. A Run owns shells only. Child work does not keep a parent Run alive, and stopping/retrying a
parent never stops/replays another Thread's Run. Application shutdown still stops every live Run independently.

## Identity and operations

Model shells inherit an authenticated Origin identifying the actual sender; do not copy or display its token.
External CLI calls have no implicit model Thread: select `--thread` and `--data-dir` explicitly where needed.
Thread parentage confers no additional authority. Shellboy assumes personal use; stop/retry/write operations are not
a host security boundary. Act only on work you were asked to handle, checking the exact ID first.

## Creating a child and sending a request

This example runs in the parent model's Bash shell. Replace `CHILD_THREAD_ID` and similar placeholders with actual IDs from each returned JSON. Check `ok` and result/error after each command; do not mechanically continue after a failure.

```bash
shellboy thread create --parent "$SHELLBOY_THREAD_ID"
# Retain result.thread_id
shellboy message send --thread CHILD_THREAD_ID --expect-reply --text 'Task: read-only audit of the parser. Make no changes. Return definite defects, supporting evidence, and the scope verified.'
# Retain result.message_id / assignment / run_id / turn_id
shellboy message get --message MESSAGE_ID
```

Use `shellboy message get --message MESSAGE_ID` to inspect the saved sender;
`history get --node MESSAGE_ID` / `history list` expose the same `provenance.sender`.
A verified model sender has originating instance/Thread/Run/Turn/execution IDs,
not merely the receiving Run's starter. A `label` is self-asserted and grants no
authority; `external_cli` does not prove human input, and legacy missing evidence
stays `unknown`. When sending across data directories, use `--sender-label` to
name yourself: Origin is stripped and the receiver sees an external CLI plus
that unverified name.

Creation, sending, and retrieving the reply are separate steps. Distinguish create's `result.thread_id` as `CHILD_THREAD_ID`, send's `result.message_id` as `CHILD_MESSAGE_ID`, and the assigned `result.turn_id` as `CHILD_TURN_ID`. Do not try to retrieve the recipient's reply or shell output using the execution ID of the observing shell that ran send.

Use `shellboy status --thread CHILD_THREAD_ID` to list current stored state, then get individual objects using the returned Run/Turn/shell/message IDs. Thread hierarchy is organizational; independent Run stop/retry acts on that Run and its shells only.

Creating with a parent inherits cwd, AGENTS reference, connection, model, and effort. Corresponding create arguments can explicitly override cwd, connection, model, effort and the instruction reference (`--agents PATH`). Without `--agents`, changing cwd when creating a child still preserves its parent's AGENTS reference. The parent's entire history is not copied. Include needed text or node IDs to read through `history get` in the request. See [History and search](history-and-search.md) for independent Threads and history forks.

`assignment` is `new_run`, `new_turn`, `existing_turn`, or `pending`. With `pending`, no Run/Turn has been determined yet; do not wait on a nonexistent Turn. Track the same message with `message get`, inspect the `held` reason and the target's `thread get` / `run get`. Once assigned to a definite Run/Turn, a message is not later reassigned to different IDs. Do not bypass pending by sending a duplicate request.

## Waiting for replies

After accepting the child request, end your Turn when only the result remains. Do not block the parent Run on a
child Run/Turn. The explicit reply is delivered as ordinary conversation input and can start the next Turn/Run.
A reply is tied to the request message, not to the child's most recent final message. `message get` exposes the saved
reply state and `reply.node_id`, including while assignment remains pending; do not resend a duplicate request.
For an external observer, explicit `wait` with `reply_received` remains an inspection option, not the delegation
lifecycle. Ordinary progress updates use `message send`; there are no automatic child lifecycle notifications.

Any model or human caller may reply once the message is applied:

```bash
shellboy message reply --message CHILD_MESSAGE_ID --text 'Findings, evidence, and remaining work.'
```

A model reply is also delivered to the sender's conversation; an external CLI
sender reads the original receipt and reply node. If the recipient ends without
replying (`ignored`), or its Thread is deleted (`recipient_deleted`), a model
sender receives a `reply_outcome` notice in its live Run. Without a live Run,
the notice is available at the next Turn and does not start a Run. If an admitted
duty is cancelled, the recipient receives `reply_cancelled` at its next request
boundary; this notice does not wake it. Progress reports remain ordinary
messages. `satisfied` is not necessarily success: inspect the reply snapshot's
`state` (`replied`, `ignored`, `cancelled`, or `recipient_deleted`). A pending
reply has no deadline; a timeout limits only that wait call. At normal Run end,
one reminder is delivered for unanswered applied requests; if still unanswered,
they become `ignored`. Human decision questions are distinct: they do not trigger
these outcomes or pause another reply duty you hold. Tell your requester you are
blocked on the human before ending the Run. A stopped or recovery-waiting Run
leaves an ordinary reply pending.
Use `thread_settled` for stalls and an explicit recovery decision; do not resend
the same request merely because it is held or the wait timed out. Cancellation
can withdraw a reply duty even after admission; inspect the receipt.

Multiple conditions use **any** semantics:

```bash
shellboy wait --conditions-json '[{"type":"reply_received","message_id":"MESSAGE_A"},{"type":"reply_received","message_id":"MESSAGE_B"}]' --timeout-ms 30000
```

Inspect `matched` and `snapshot.conditions` and wait again for outstanding
requests. A wait can also combine `reply_received` with `thread_settled` for
`recovery_wait`, `failed`, or `stopped`. `interrupted` and `unavailable` require
inspection, not an assumption of success. Waits do not detect dependency cycles. Direct own Run/Turn/shell waits return `self_wait`; other dependency cycles can time out. Do not replace
normal reply delivery with a sleep loop. Ctrl+C or connection loss releases
the observation, not the target. Follow `next_cursor` for long reply nodes.

## Progress reports and lifetime

A child can inspect `parent_id` in `thread get` and send a needed one-off progress report to its parent.

```bash
shellboy thread get
shellboy message send --thread PARENT_THREAD_ID --text 'Progress: read-only audit completed for 3 files; 1 remains. No changes. No reply needed.'
```

This is an ordinary message notification. A child need not wait for a reply to every report; the parent adopts it at a safe boundary. A progress report does not replace the requested reply. Completion of the child's answer and termination of its Run also differ. Background shells can keep a Run live after its Turn answers. If all owned work must finish, wait for `run_ended`.

```bash
shellboy wait --conditions-json '[{"type":"run_ended","run_id":"CHILD_RUN_ID"}]' --timeout-ms 30000
```

A parent Run can complete while the child independently continues. Stopping the parent stops only its own shells,
not child or grandchild Runs. Application exit stops every live Run independently. Never rely on a Thread's tree
position as a cleanup policy; select the exact Run or shell when a stop is needed.

After reading the results and waiting for the child's Runs to end, the parent or user decides whether to retain the child.
Preview with `shellboy thread delete-preview --thread CHILD_THREAD_ID` (optionally `--descendants`), then use `shellboy thread delete --thread CHILD_THREAD_ID` with the same descendant selection only if deletable; deletion rechecks live/stopping Runs.
Deletion removes access to that child's conversation and excludes deleted Threads from usage totals (required shared sources may remain); retain children needed for audits or later cost checks.

## Deciding recovery
Ordinary external or model `message send` input stays held when the target Run is in
`recovery_wait`. Inspect `run get` / `thread get` and use `run retry` for the explicit
recovery boundary. Only pass `--resume-recovery` when the message itself is the reviewed
resume instruction; the `recovery_resumed` event records the caller actor. Human TUI/chat input and an
answer through `message reply` remain explicit resume actions.


```bash
shellboy run get --run CHILD_RUN_ID
shellboy turn get --turn CHILD_TURN_ID
# After inspecting and fixing the cause, the parent, any other caller, or a human explicitly retries
shellboy run retry --run CHILD_RUN_ID
# Alternatively, to discontinue
shellboy run stop --run CHILD_RUN_ID
```

Retry and stop are alternatives; the example does not instruct running both consecutively. An ordinary model message to a child in `recovery_wait` is held, not substituted for automatic recovery. After restart, earlier Runs are not automatically executed again. Decide on new execution using interruption information, stored end messages, reply states, and unprocessed messages. Retain IDs of unexecuted instruction messages when stopping.

In PowerShell, use `$env:SHELLBOY_THREAD_ID`. Adapt single-quoted JSON examples to the shell's argument passing; see [Shell](shell.md). These delegation command examples have not been verified on physical Windows hardware.

## Explicit observation of several Threads

For human/client inspection of completed or recovery-waiting work, an explicit bounded wait remains available.
Models delegating tasks normally finish their Turn and await reply delivery instead:

```sh
shellboy wait --conditions-json '[{"type":"thread_settled","thread_ids":["CHILD_A","CHILD_B"],"states":["completed","recovery_wait","failed","stopped"]}]'
```

Read `result.snapshot.changes` for the Thread/Run/state/stored time and keep
`result.cursor`, including after a timeout. Continue with the **same conditions**:

```sh
shellboy wait --conditions-json '[{"type":"thread_settled","thread_ids":["CHILD_A","CHILD_B"],"states":["completed","recovery_wait","failed","stopped"]}]' --after 'CURSOR'
```

`--after` catches saved transitions between calls, including a completed Run
followed by new work or recovery followed by resume. It survives owner restart.
`snapshot.more=true` means continue immediately with that cursor for the rest;
each reply contains at most64 changes and each request names at most64 Threads.
No-after is a current-state observation plus a new baseline, not a replay of all
past completions. An empty Thread has no settled Run. `satisfied` is not success:
inspect failed/stopped/recovery records before deciding what to do. The wait does
not stop or resume work. When a wait times out on a Thread whose Turn ended but whose
Run still owns a shell, `snapshot.conditions[].owned_shells` gives the `execution_id` and
`elapsed_ms`; inspect it with `shell get`. Changing conditions or losing a source anchor rejects
the cursor rather than silently skipping records. No background poller is needed.

ThreadList/Get also separate `last_end_message` (latest own Turn end message, with
Turn/node/time/excerpt) from `last_input` (latest durably accepted input, including
held/cancelled, with receipt state and credential-free sender/label). The latter
does not prove application or model completion. Existing `excerpt` is unchanged.
Labels remain self-asserted; inspect originals by node ID when needed.

## Leader's operating flow

Send an instruction with `message send --thread T --expect-reply` and retain its
message ID. End the current Turn when only the result remains; the explicit reply is delivered to the sender
Thread normally. An external observer can wait for `reply_received`; after a timeout, reissue that observation rather
than the instruction. Inspect `message get` and retrieve `reply.node_id` for a
`replied` result. For `ignored`, inspect `last_end_message` and history for
diagnosis; it is not a substitute for a reply. Use `thread_settled`/`status`
for stalled or recovery-waiting work. Cancel an obsolete request with `message
cancel` (including after admission), and use ordinary messages for progress.
A separate `--deliver complete` "did you report?" reminder is unnecessary:
the Run-end reply reminder provides that check.
