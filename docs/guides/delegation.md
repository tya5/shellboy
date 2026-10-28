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
shellboy message send --thread CHILD_THREAD_ID --text 'Task: read-only audit of the parser. Make no changes. Return definite defects, supporting evidence, and the scope verified.'
# Retain result.message_id / assignment / run_id / turn_id
shellboy message get --message MESSAGE_ID
```

Creation, sending, and retrieving the answer are separate steps. Distinguish create's `result.thread_id` as `CHILD_THREAD_ID`, send's `result.message_id` as `CHILD_MESSAGE_ID`, and the assigned `result.turn_id` as `CHILD_TURN_ID`. Do not try to retrieve the receiving model's answer or shell output using the execution ID of the observing shell that ran send.

Use `shellboy status --thread CHILD_THREAD_ID` to list current stored state, then get individual objects using the returned Run/Turn/shell/message IDs. Thread hierarchy and Run ownership differ: do not infer stop or recovery authority from the Thread's appearance.

Creating with a parent inherits cwd, AGENTS reference, connection, model, and effort. Corresponding create arguments can explicitly override cwd, connection, model, and effort. There is no create argument to override the AGENTS reference; changing cwd when creating a child still preserves its parent's AGENTS reference. The parent's entire history is not copied. Include needed text or node IDs to read through `history get` in the request. See [History and search](history-and-search.md) for independent Threads and history forks.

`assignment` is `new_run`, `new_turn`, `existing_turn`, or `pending`. With `pending`, no Run/Turn has been determined yet; do not wait on a nonexistent Turn. Track the same message with `message get`, inspect the `held` reason and the target's `thread get` / `run get`. Once assigned to a definite Run/Turn, a message is not later reassigned to different IDs. Do not bypass pending by sending a duplicate request.

## Waiting with any semantics and retrieving answers

Once you have a Turn ID, wait for that response interval. This is a Bash example; replace IDs inside the JSON with actual returned values too.

```bash
shellboy wait --conditions-json '[{"type":"turn_completed","turn_id":"CHILD_TURN_ID"}]' --timeout-ms 30000
shellboy turn get --turn CHILD_TURN_ID
# Retrieve result.answer_node_id if present
shellboy history get --node ANSWER_NODE_ID --limit-bytes 65536
```

Multiple conditions use **any** semantics. The call returns when one is satisfied, not necessarily when every child completes.

```bash
shellboy wait --conditions-json '[{"type":"turn_completed","turn_id":"TURN_A"},{"type":"turn_completed","turn_id":"TURN_B"}]' --timeout-ms 30000
```

Inspect `matched` and `snapshot.conditions`, then wait again only for unfinished targets. `turn_completed` observes terminal states including stopped/failed, so even `satisfied` requires checking the target's `state` and `answer_node_id`. Do not invent an answer for a stopped Turn without an answer node. If its body returns `next_cursor`, page through the same node.

| Wait status | Next action |
|---|---|
| `satisfied` | Inspect matched targets' states and retrieve needed answer nodes/output |
| `timeout` | Targets continue. Check progress and do other work, or wait again as a separate operation |
| `interrupted` | Observation was released. Do not treat it as a successful target stop; check current state and new instructions |
| `recovery_wait` | An explicit recovery decision is needed. Retrieve the cause; as immediate parent, retry/stop, or consult a higher authority if needed |
| `unavailable` | The target is unavailable, for example after deletion. Check snapshot/reason; do not repeat the same wait unconditionally |

Waiting on your own Run/Turn/current shell, or forming a cycle with ancestors or other waits, is rejected. A child waiting for its parent Run to end could cycle with the parent waiting to reclaim that child. Do not work around `wait_cycle` with sleep loops; fix the direction of requests and notifications. Ctrl+C or connection loss releases the CLI observation and does not stop the target Run/shell.

## Progress reports and lifetime

A child can inspect `parent_id` in `thread get` and send a needed one-off progress report to its parent.

```bash
shellboy thread get
shellboy message send --thread PARENT_THREAD_ID --text 'Progress: read-only audit completed for 3 files; 1 remains. No changes. No reply needed.'
```

This is an ordinary message notification. A child need not wait for a reply to every report; the parent adopts it at a safe boundary. A progress report does not replace retrieving the final answer. Completion of the child's answer and termination of its Run also differ. Background shells can keep a Run live after its Turn answers. If all owned work must finish, wait for `run_ended`.

```bash
shellboy wait --conditions-json '[{"type":"run_ended","run_id":"CHILD_RUN_ID"}]' --timeout-ms 30000
```

Even after the parent's Turn answers, its Run remains live while it owns child/grandchild Runs or shells. Normal Run completion is finalized only after confirming that no owned work, unprocessed instructions, or similar obligations remain. Explicitly stopping the parent Run or exiting the application stops and reclaims its owned descendants. Do not use a child as a persistent daemon that survives application exit. Independent Runs on independent Threads are outside the parent Run's ownership cleanup.

## Deciding recovery

```bash
shellboy run get --run CHILD_RUN_ID
shellboy turn get --turn CHILD_TURN_ID
# After inspecting and fixing the cause, the immediate parent or a human explicitly retries
shellboy run retry --run CHILD_RUN_ID
# Alternatively, to discontinue
shellboy run stop --run CHILD_RUN_ID
```

Retry and stop are alternatives; the example does not instruct running both consecutively. An ordinary model message to a child in `recovery_wait` is held, not substituted for automatic recovery. After restart, earlier Runs are not automatically executed again. Decide on new execution using interruption information, stored answers, and unprocessed messages. Retain IDs of unexecuted instruction messages when stopping.

In PowerShell, use `$env:SHELLBOY_THREAD_ID`. Adapt single-quoted JSON examples to the shell's argument passing; see [Shell](shell.md). These delegation command examples have not been verified on physical Windows hardware.
