# Delegation and coordination

[Guide index](README.md) · [Agent Guide](agent-guide.md) · [CLI reference](cli-reference.md)

A Shellboy child agent (also called a subagent) is a Run executing on a child Thread. A Thread holds stored conversation and configuration; a Run owns shells and child Runs; a Turn is one response interval. Creating a child alone does not start execution. Sending a request assigns a Run/Turn. Parent and child share the runtime and filesystem, not separate isolated environments.

## Ownership and human operations

A Thread's `parent_id` describes conversation hierarchy. Check a Run's `owner_run_id` for execution ownership. When a model shell sends to a child Thread whose parent is its own Thread and starts a new Run, that execution belongs to the current parent Run. Merely sending a request to an independent Thread without that hierarchy does not give ownership of its independent Run.

A human's ordinary external CLI has no model Origin. Creating a Thread relationship with `--parent` does not let a human send impersonate ownership by an arbitrary running parent Run. External CLI calls also have no implicit current Thread: specify `--thread` where required, and `--data-dir` for another data directory. A running owner is required; the CLI does not automatically start an owner.

Model shells inherit an issued Origin. There is no need to display, copy, or manually set its token. Do not make an external CLI impersonate model authority. A model can stop only work it owns, and recovery retry decisions belong to the live immediate parent. Grandparents or siblings cannot retry a child in place of its immediate parent. Humans can intervene explicitly.

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

Use `shellboy status --thread CHILD_THREAD_ID` to list current stored state, then get individual objects using the returned Run/Turn/shell/message IDs. Thread hierarchy and Run ownership differ: do not infer stop or recovery authority from the Thread's appearance.

Creating with a parent inherits cwd, AGENTS reference, connection, model, and effort. Corresponding create arguments can explicitly override cwd, connection, model, effort and the instruction reference (`--agents PATH`). Without `--agents`, changing cwd when creating a child still preserves its parent's AGENTS reference. The parent's entire history is not copied. Include needed text or node IDs to read through `history get` in the request. See [History and search](history-and-search.md) for independent Threads and history forks.

`assignment` is `new_run`, `new_turn`, `existing_turn`, or `pending`. With `pending`, no Run/Turn has been determined yet; do not wait on a nonexistent Turn. Track the same message with `message get`, inspect the `held` reason and the target's `thread get` / `run get`. Once assigned to a definite Run/Turn, a message is not later reassigned to different IDs. Do not bypass pending by sending a duplicate request.

## Waiting for replies

Wait on the accepted message, even while assignment is pending. A reply is a
specific response to that request, not the recipient's last Turn text.

```bash
shellboy wait --conditions-json '[{"type":"reply_received","message_id":"CHILD_MESSAGE_ID"}]' --timeout-ms 30000
shellboy message get --message CHILD_MESSAGE_ID
# For reply.state=replied, use reply.node_id:
shellboy history get --node REPLY_NODE_ID --limit-bytes 65536
```

The child receives the message ID and replies once after it is applied:

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
they become `ignored`. A stopped or recovery-waiting Run leaves the reply pending.
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
inspection, not an assumption of success. Model waits are cycle-checked; do not
work around `wait_cycle` with sleep loops. Ctrl+C or connection loss releases
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

Even after the parent's Turn answers, its Run remains live while it owns child/grandchild Runs or shells. Normal Run completion is finalized only after confirming that no owned work, unprocessed instructions, or similar obligations remain. Explicitly stopping the parent Run or exiting the application stops and reclaims its owned descendants. Do not use a child as a persistent daemon that survives application exit. Independent Runs on independent Threads are outside the parent Run's ownership cleanup.

After reading the results and waiting for the child's Runs to end, the parent or user decides whether to retain the child.
Preview with `shellboy thread delete-preview --thread CHILD_THREAD_ID` (optionally `--descendants`), then use `shellboy thread delete --thread CHILD_THREAD_ID` with the same descendant selection only if deletable; deletion rechecks live/stopping Runs.
Deletion removes access to that child's conversation and excludes deleted Threads from usage totals (required shared sources may remain); retain children needed for audits or later cost checks.

## Deciding recovery

```bash
shellboy run get --run CHILD_RUN_ID
shellboy turn get --turn CHILD_TURN_ID
# After inspecting and fixing the cause, the immediate parent or a human explicitly retries
shellboy run retry --run CHILD_RUN_ID
# Alternatively, to discontinue
shellboy run stop --run CHILD_RUN_ID
```

Retry and stop are alternatives; the example does not instruct running both consecutively. An ordinary model message to a child in `recovery_wait` is held, not substituted for automatic recovery. After restart, earlier Runs are not automatically executed again. Decide on new execution using interruption information, stored end messages, reply states, and unprocessed messages. Retain IDs of unexecuted instruction messages when stopping.

In PowerShell, use `$env:SHELLBOY_THREAD_ID`. Adapt single-quoted JSON examples to the shell's argument passing; see [Shell](shell.md). These delegation command examples have not been verified on physical Windows hardware.

## Waiting for several Threads

For completed work or a recovery decision in any child, use one bounded wait:

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
not stop or resume work. Changing conditions or losing a source anchor rejects
the cursor rather than silently skipping records. No background poller is needed.

ThreadList/Get also separate `last_end_message` (latest own Turn end message, with
Turn/node/time/excerpt) from `last_input` (latest durably accepted input, including
held/cancelled, with receipt state and credential-free sender/label). The latter
does not prove application or model completion. Existing `excerpt` is unchanged.
Labels remain self-asserted; inspect originals by node ID when needed.

## Leader's operating flow

Send an instruction with `message send --thread T --expect-reply` and retain its
message ID. Wait for `reply_received`; after a timeout, reissue the wait rather
than the instruction. Inspect `message get` and retrieve `reply.node_id` for a
`replied` result. For `ignored`, inspect `last_end_message` and history for
diagnosis; it is not a substitute for a reply. Use `thread_settled`/`status`
for stalled or recovery-waiting work. Cancel an obsolete request with `message
cancel` (including after admission), and use ordinary messages for progress.
A separate `--deliver complete` "did you report?" reminder is unnecessary:
the Run-end reply reminder provides that check.
