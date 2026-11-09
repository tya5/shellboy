# Recovery, stopping, and storage

[Guide index](README.md) / [User Guide](user-guide.md) / [Agent Guide](agent-guide.md)

When responses stop, inspect saved acceptance, Turn, and Run state before repeatedly sending the same request. Use the CLI in another terminal while the owner remains running. Examples below use Bash; replace IDs with values from actual responses.

## Terminal closure and operating-system exit

Delivered SIGTERM/SIGHUP request the same draft-preserving shutdown as Ctrl+Q.
Losing the terminal stops further drawing/input and leaves shutdown running.
Repeated HUP/session-loss notifications do not force exit. A second explicit
SIGTERM can force exit. Unix cleanup uses the configured native stop/drain and
shutdown grace (9.25 seconds with defaults; five seconds before configuration
is ready). Windows console close retains a four-second OS callback cap. Known
owned Unix process groups receive a best-effort KILL before forced process exit;
that does not prove saving or reaping completed. If blocked terminal output
prevents restoration, Unix raw mode can be restored with `stty sane`.
OS deadlines, SIGKILL and power loss can still interrupt saving: inspect the
previous shutdown record after restart. A signal is a request to stop, not proof
that every effect was saved.

On Unix, newly launched model shells have an owner-lifetime guardian: owner death
closes a private lifeline and requests KILL for that execution's process group.
This is not a stop-time guarantee. Processes deliberately escaping the group
(e.g. via `setsid`, or detached/nohup launch arrangements) may survive. Windows
continues to use its kill-on-close Job object. On Unix restart, interrupted active
executions are recorded as failed with
`previous_instance_interrupted_os_state_unverified`, not as confirmed OS stops;
exit status and group cleanup remain unknown. This also covers older executions
without a guardian. Startup does not signal stored PIDs/PGIDs or replay commands.

Windows console close uses the same bounded path when the OS delivers its event.
Logoff/shutdown delivery is best effort, particularly when GUI libraries suppress
console notifications. Native Windows desktop behavior is not established by
Unix tests. See the [OS termination design](../design/os-signal-shutdown.md) for
these platform limits. Ctrl+C inside the interactive TUI keeps its Run-stop role.

The Thread list marks the latest Run with `[exit]` / `Interrupted by app exit`
when its saved reason is `app_shutdown` or `previous_instance_interrupted`.
Explicit user stop and normal completion are unmarked. `thread list` exposes
`latest_run_stop_reason`; starting a new Run clears that indication naturally.
Opening an interrupted conversation and inspecting its record does not resume it.

## After an interrupted shell

On startup, Shellboy pairs adopted tool calls that lack a committed result,
including calls left in an already stopped Turn. `not_started` means the call
was still pending with no saved execution admission. `interrupted` means its
result is uncertain; it does not mean the command had no effects. Saved real
results are preserved. No command is automatically replayed.

Read the previous shutdown record and, when an execution ID is available, use
`shellboy shell get --execution EXECUTION_ID` and
`shellboy shell read --execution EXECUTION_ID` to inspect retained state/output.
Confirm external side effects before requesting another execution. Unapplied
shell-origin attachment deliveries from an interrupted group are cancelled;
already applied attachments and source files are preserved. You can send a new
instruction in the same Thread after restart.

## Distinguish acceptance from completion

```sh
shellboy thread list
shellboy thread get --thread THREAD_ID
shellboy message get --message MESSAGE_ID
shellboy turn get --turn TURN_ID
shellboy run get --run RUN_ID
```

| Observation | What it means | Avoid this confusion |
|---|---|---|
| CLI `ok: true` | The specified acceptance or query succeeded | Does not imply a successful model answer, shell, or indexing job |
| Message `accepted` / `held` | Body saved / saved with execution held | Do not duplicate it with another new send |
| Message `applied` | Included in model input | Answer may not be complete |
| Message `cancelled` | Instruction treated as cancelled | Investigate the cause before deciding the next instruction |
| `assignment: pending` | Run/Turn not assigned yet | Check `message get` rather than guessing IDs |
| Turn `completed` | Normal answer finalized | Background shells or child Runs may still be active |
| Run `live` | Execution lifetime continues | Includes waiting for owned background work after answering |
| Turn `recovery_wait` | Failure reason saved, awaiting an explicit decision | Time or configuration changes alone do not resume it |

For a reply-expected request, inspect `message get` and retrieve `reply.node_id` when `reply.state` is `replied`. For a Turn or Run end message use its `end_message_node_id`. Follow `next_cursor` for long bodies. A stopped or Esc-ended Turn may have no end message; do not invent a result. A stopped Run leaves an outstanding reply pending until a reply, cancellation, or another terminal reply state.

In the TUI, start via F8 `待機 | 条件を観測` and cancel only observation with lowercase w in management or `/cancel-wait`. Esc closes the display without stopping the target. Ctrl+C interrupts the current Thread instead; see [wait controls](tui.md#cancelling-observation-versus-interrupting-its-target).

Use `shellboy wait` to wait for execution. `timeout` does not mean the target stopped or failed. `satisfied` need not mean target success: inspect the returned snapshot and state. [Delegation](delegation.md) and [CLI reference](cli-reference.md) show JSON and state-dependent branches.

## Follow an explicit recovery retry

`run retry` and `message send --resume-recovery` acknowledge **acceptance, not
execution success**. Keep the returned `run_id` and `turn_id`; do not substitute
the Thread's latest Run/Turn, which can belong to other independent work. Retry
usually reprepares the same Turn, not a new Turn. A model attempt is allocated
later, so acceptance does not promise a new attempt ID or even a provider call.
The CLI response remains machine JSON; no human explanation is mixed into it.

```sh
shellboy run get --run RUN_ID
shellboy run retry --run RUN_ID
# Use only the acknowledgement's turn_id here:
shellboy wait --conditions-json '[{"type":"turn_completed","turn_id":"ACK_TURN_ID"}]' --timeout-ms 30000
shellboy turn get --turn ACK_TURN_ID
# Only if turn get returns end_message_node_id:
shellboy history get --node END_MESSAGE_NODE_ID
```

Read the wait snapshot's `state` and then saved Turn facts. `satisfied` can mean
`completed`, `failed` or `stopped`; only `completed` establishes normal answer
completion. `recovery_wait` means inspect `context.recovery_reason` (including
its saved code/message), not that retry succeeded. An observer `timeout` or
`interrupted` says nothing about target success and does not stop it. A Turn may
finish while its Run still owns background work; `run_ended` can wait longer
than the answer. Never infer success from a fixed sleep or an empty live preview.

### Recognize another same-reason failure

A repeated failure can have the same Turn ID, state and reason. For a fresh saved
transition, **before retrying**, capture a baseline with the existing wait API:

```sh
shellboy wait --conditions-json '[{"type":"thread_settled","thread_ids":["THREAD_ID"],"states":["completed","recovery_wait","failed","stopped"]}]' --timeout-ms 0
# Save result.cursor, then issue the explicit retry and keep its Run/Turn IDs.
shellboy run retry --run RUN_ID
shellboy wait --conditions-json '[{"type":"thread_settled","thread_ids":["THREAD_ID"],"states":["completed","recovery_wait","failed","stopped"]}]' --after BASELINE_CURSOR --timeout-ms 30000
```

Keep the conditions unchanged when reusing the cursor. Inspect returned changes,
match their `run_id` and (when present) `turn_id` to the acknowledgement, and read
that exact Turn. A recovery change is saved for each recovery generation, even
if the error repeats. Other independent work is only a wake hint; never call its
answer/failure this retry's result. Continue from the returned cursor when a
change belongs to another target. A `more` response can require further pages.
A terminal Run change may omit `turn_id`; inspect the acknowledged Turn instead
of assuming an answer. This is an observation route, not a new atomic retry
correlation token: concurrent callers can retry the same Turn again.

Without a pre-retry cursor (or another observed boundary), a post-hoc identical
state/reason cannot identify a retry epoch. On `cursor_expired`, take a fresh
snapshot and report that the intervening retry outcome is **unknown**; do not
silently claim that a new baseline proves an earlier attempt. Read retained
facts and explicitly decide the next action. Failure timestamps are facts, not
a reason to sleep or a guaranteed per-retry identity.

### Held input progress is not another model failure

A retry releases at most 128 recoverable held inputs. While more remain, the
Turn can stay in `recovery_wait`; `run get`/the acceptance `recovery_hint` can
include `remaining_held` inside its existing JSON-encoded string (parse that
string; it is not a new nested response object). This can be queue progress **without a new model
attempt**, not a repeated provider failure. An explicit next operation ID may
advance the next unit. Replaying the **same operation ID** returns its saved
acceptance, not a new unit and not the current Turn outcome. Use `operation get`
to resolve ambiguous receipt delivery, and `turn get`/wait for execution facts.
Do not automatically retry or resend input. In particular, a new input with
`--resume-recovery` is still an instruction, not a status query.

## Inspect a held message without resending it

A saved `delivery_status: held` does not say the original body was discarded.
Use the existing message ID to read saved delivery context and then its body:

```sh
shellboy message get --message MESSAGE_ID
shellboy history get --node MESSAGE_ID
```

`message get` already exposes `context.hold_reason`; it is not a new DTO field.
Inspect that existing field with the saved Run/Turn assignment and timestamps.
`terminal_before_path_admission` is a **held reason**: the assigned Run or Turn
ended before the input was admitted to the selected path. It is not a terminal
receipt state, an answer, proof of consumption or proof that the body was lost.
Saved context reports recorded facts, not the present OS/process state.

`history get` reads the retained node body and its provenance without submitting
another instruction. Follow `next_cursor` with `--cursor` for a paged body. An
unselected input can remain saved and readable without appearing on the current
recipient path or a rewind backup branch. A missing/unavailable body must be
reported as such; never reconstruct it from a guess. Message identity, path
membership and body availability are separate facts.

Stop/startup/drained shutdown can record this hold reason before a rewind.
Observing it after rewind does not establish rewind caused deletion. The private
incident's root cause remains unknown; the source and synthetic fixture do not
prove that its actual four inputs were retained or lost. The recipient TUI queue
can show held reasons/body excerpts, but this is not a new sender push notice.

An explicit human resume can admit eligible saved unadmitted input before its
new instruction, in bounded units. Retyping the old input under a fresh message
ID can therefore duplicate instructions when the original is admitted too.
Inspect saved facts first. These read commands neither repair nor resend, and
this guidance creates no automatic recovery, retry, sender event or new Run.
Do not infer automatic recovery for unrelated independent model sends or for
already-admitted history removed by rewind.

## Automatic retries for temporary model communication failures

Temporary communication failures trigger up to eight consecutive additional attempts by default. TUI `一時的なエラーのため再試行中` (retrying a temporary error) is a normal waiting display. Ctrl+C or stopping the Run cancels the wait. Exhausting the budget enters recovery wait with the latest error and count. Successful final-response adoption resets consecutive failures; this is not an eight-retry cap over an entire long conversation/tool loop.

Eligible failures are HTTP 429/500/502/503/504 and temporary connection failures, disconnects, EOFs, and receive-idle timeouts. By default, partial output also retries, with received text preserved in the failed attempt. Tool calls are not executed before final-response adoption. Permanent authentication, configuration, format, or explicit provider failures are not automatically resent. HTTP error bodies are distinct from model output.

If headers for HTTP 429/500/502/503/504 have arrived and reading their error body stalls or disconnects, the saved HTTP status still determines retry eligibility. HTTP 400/401/403 and similar responses do not retry solely because their body retrieval timed out.

Per-connection `response_max_retries = 0` disables automatic retries. `response_retry_partial = false` sends only partial-output failures to recovery wait; communication failures containing metadata/heartbeats without output remain retryable. `response_retry_max_delay_ms` caps exponential delay (default 60000); a server `Retry-After` has a separate ten-minute automatic-wait ceiling. See [configuration](configuration.md). Retries may duplicate charges when upstream processing already occurred. Failed attempts save the count, delay, and this possibility. Recovery wait after automatic retries does not resume merely with time. Retry-After over 600 seconds is preserved/displayed and enters recovery wait rather than being shortened to 600 seconds and resent.

A 429 whose body reports `usage_limit_reached` with a reset time (including when LiteLLM wraps it in a `litellm.RateLimitError` message) is not retried before that reset when it is later than the automatic retry schedule; the reset time is shown in the recovery reason.

An HTTP 429 also establishes an in-memory wait shared by work using the same
configured connection profile, including conversations, summaries, embeddings,
and model lists. Existing requests are not cancelled; waiting does not create a
new HTTP attempt or silently resend a request. Waiters are admitted FIFO, with
one probe after a cooldown; cancellation releases its place. At most 1,024
waiters per profile are retained, and excess or over-ten-minute waits are
reported as deferred. Separate configured profiles wait independently even if
they share an upstream quota and can each receive 429s. The owner forgets this
state on restart; it does not automatically resume recovery or reconstruct a
provider deadline from old records.

Partial output includes not only visible body text but reasoning items received via `response.output_item.added`, including encrypted reasoning (`encrypted_content`). With `response_retry_partial = false`, a communication failure after such an item enters recovery wait even when visible text is empty.

Automatic retries can take a long time. If each attempt receives headers and then reaches the default 600-second receive-idle timeout without Retry-After, nine requests including the initial one plus 183 seconds of exponential delays take `600 × 9 + 183 = 5583 seconds` (about 93 minutes). This is not a total communication/processing deadline: reception resets the idle clock and Retry-After changes waits. Adjust `stream_idle_timeout_ms`, `response_max_retries`, or `response_retry_max_delay_ms`, or stop with Ctrl+C. `stream_idle_timeout_ms = 0` disables the idle deadline.

## When saving a submission fails

If saving the initial submission fails, its text remains in the draft. Resolve input-limit or queue-full causes before resending. A canonical DB `save_failed`, however, stops new acceptance: do not repeatedly retry in place. Save any unsaved text visible on screen before exiting; fix storage after stopping/shutdown. After restart, inspect drafts and acceptance that were actually persisted. Restoration of text held only in RAM is not guaranteed. If preparation or API startup fails after acceptance was saved, recover by inspecting the original message/Run/Turn. Creating another message differs from retrying the original Turn.

Draft revisions prevent slow responses from clearing a newer draft. See [TUI controls](tui.md) for `/restore REV` of a failed snapshot.

If only the CLI reply was lost and the operation ID is known, query it first:

```sh
shellboy operation get --operation OPERATION_ID
```

After disconnection following send, returned JSON retains the query target in `operation_id` and `error.details.operation_id`. Use this generated ID even if you did not supply one. `request_may_have_been_applied: true` means outcome unknown; it proves neither execution, nonexecution, nor failure. `next_operation` provides typed guidance to the receipt query. Pass the ID as an argument to `--operation`; never evaluate it as a new command.

If the process exited without returning JSON and the ID is unknown, this guidance cannot necessarily recover it. Inspect known Thread/Run state, `status --all`, and saved history; do not automatically resend.

For `operation_not_found` / `operation_expired`, compare target get results and saved history. For `operation_conflict`, inspect differences between the existing receipt and the request. None justifies repeating an uncertain side effect under a new ID. For `save_failed`, preserve unsaved input and investigate persistence instead of repeatedly sending new mutations.

Within the same instance, resending the same operation ID and same request returns the saved result without duplicate acceptance; changed content with the same ID yields `operation_conflict`. After restart, an old ID with a retained result for the same request also returns it without repeating side effects. Without its old record, it yields `operation_expired`. Never unconditionally resend uncertain shell input under a new ID.

Follow `operation get` `targets` to target queries. `downstream` reports a message's current delivery state or compact queued/running/completed/failed state and terminal outcome. Receipt `state=completed` is separate from downstream success. `result_uncertain` or `replay_policy=do_not_reexecute` does not indicate safe reexecution of uncertain effects. Queries themselves do not save receipts; even with the same query ID they observe current state.

Get-result `context.recovery_reason` is structured. Restart interruption with `result_uncertain=true` is not a confirmed failure, and recovery wait is not synonymous with failure. Read saved times/ownership without inferring unrecorded started_at or every state transition. Run/Turn `lifetime_ms` spans creation to termination including waits and is omitted before termination.

## Inspecting the previous shutdown record

After restart, opening a conversation that had background work shows `前回の終了時の記録` (previous shutdown record), with counts of work, response processing, commands, children, and interrupted wait observations. In F6 select that record and Enter for IDs/reasons or O for saved source. Entries labeled `前回の保存記録 | コマンド` (previous saved commands), `子の作業` (child work), and `前回の待機観測は中断済み` (previous wait observation interrupted) lead to individual targets. Use get commands or F8 for their current saved Run/Turn/shell state.

The shutdown record is a snapshot before stopping begins. Distinguish `予定した状態` (intended state) from actual cleanup outcome; uncertain external side effects remain uncertain. Normal Ctrl+Q shutdown alone does not prove every external outcome. Restart does not automatically rerun previous work: inspect saved targets, answers, and unapplied requests before choosing an action.

## Resuming recovery wait

The TUI preserves the cause and unapplied instructions below the conversation; other queries or configuration changes do not erase them. `/retry` retries current recovery wait with the same content; ordinary input sends a new instruction to resume. `/stop` or Ctrl+C stops current-conversation work. F8 Run/Turn details and retry/stop forms default to current work, never a Run ID inherited from an old management result.

Recovery wait is time awaiting human action, not continued model processing. An unknown failure stage is shown as unrecorded. Confirmed tool results are reused, but inspect details rather than interpreting that as a guarantee against reexecution of every uncertain side effect.

1. Read recovery reasons/hints in `turn get` / `run get` / `thread get`.
2. Resolve the cause: fix a referenced file, check connection/model, or reclaim capacity.
3. Explicitly retry the recovery Turn in the same live Run.

```sh
shellboy run retry --run RUN_ID
```

Large held-input queues recover in ordered units of at most 128 receipts, not
by rejecting the queue. While older input remains, the Turn stays in
`recovery_wait`; `run get` / `run retry`'s `recovery_hint` and `thread get`'s
`recovery_reason` include `remaining_held`. Repeat an explicit retry with a new
operation ID to advance the next unit. Replaying the same operation ID returns
its saved result and does not release another unit. New human input follows the
older held input; no remaining instructions are silently dropped. The final
unit lets the Turn resume normal preparation. Unknown-admission receipts still
require inspection rather than guessed delivery.

If a provider rejects an attachment, inspect `shellboy attachment list --thread
THREAD_ID`, then exclude the specific attachment before retrying:

```sh
shellboy attachment remove --thread THREAD_ID --attachment ATTACHMENT_ID
shellboy run retry --run RUN_ID
```

The TUI equivalents are `/attachments`, `/detach ATTACHMENT_ID`, and `/retry`.
In 0.6.1, retry also recovers a conversation stuck with `source is outside the
selected path` after a removed attachment's source was compacted; no migration
or manual data repair is required.
Exclusion does not delete the file; the model receives an exclusion notice
instead of its contents. Confirm the Thread, attachment, and recovery-wait Run
before acting. If removal is unavailable, an external file can instead be
moved or renamed by its owner so that the saved absolute path is unavailable;
the retry sends an unavailable notice. Do not move a managed or shared file
without checking its impact. Restoring the original path can cause later sends
to include it again and repeat the rejection. Neither workaround proves the
provider accepts other attachments.

An external human CLI or the direct owning parent decides recovery. Merely being an ancestor model or an independent chat does not confer the same authority. An ordinary parent message is not a retry. A human's new instruction can explicitly resume the same recovery Turn, but the command above clearly retries the original Turn after its cause is fixed.

`run retry` does not revive a terminal Run. After stop/restart, send a new instruction to create a new Run/Turn. Restart never stops an old PID or automatically repeats shell side effects. Previously uncertain state becomes history/restart context.

For conversations that received HTTP 400 because histories mixed endpoint-specific item IDs, update, restart the owner, and send a new request in the same conversation. `/retry` cannot target the old Run stopped by restart. Saved history passes through new conversion on send, removing only endpoint-specific message/function_call IDs. Source remains intact; this does not guarantee resolution of other provider constraints such as reasoning items.

Explicit context-capacity overflow triggers automatic compaction and up to two retry stages. Communication failures are not guessed to be context overflow. Failure-driven reduction follows the consecutive retry budget as described below. For manual `/compact`, see [history and search](history-and-search.md).

## Interrupting and quitting

| Action | Meaning |
|---|---|
| TUI Esc, with no popup/completion/selection | Interrupt only the observed Turn; foreground shell moves to background; owned background work continues |
| TUI Ctrl+C | Request stopping the current Run, including owned background work |
| `shellboy run stop --run RUN_ID` | Accept stopping the specified Run and its owned descendants |
| `shellboy shell stop --execution EXECUTION_ID` | Request stopping that shell |
| Ctrl+C during external CLI wait | Disconnect observation; target Run/shell continues |
| TUI Ctrl+Q | Stop work, save, close DB, and shut down normally |

Successful stop acceptance does not prove all PIDs were reaped. Wait for Run termination and inspect state when needed. Any caller may stop any Run; stopping a Run also stops its currently owned descendants and their shells. Check the Run ID first, because stopping another conversation's Run interrupts it. The operation receipt records the calling Origin.

After normal-shutdown grace expires, the TUI offers continued waiting or force quit. W/Enter waits for the same shutdown. F then Y explicitly forces exit; N/Esc cancels force confirmation. Force quit can leave unsaved data/unreaped descendants and differs from normal shutdown.

Canonical DB persistence failure stops new acceptance and proceeds to stop owned work, rather than continuing as a display-only error. Do not delete DB/lock files to force startup. After shutdown, investigate storage, permissions, and free space.

## Retention and collection

```sh
shellboy storage status
shellboy storage collect --category raw
shellboy storage collect --category managed-images
```

`collect` follows configured retention and reference protection; it does not unconditionally erase an entire category. Categories are `all` / `raw` / `managed-images` / `history` / `index`, default `all`. Read its result and `storage status` for reclaimed/protected amounts.

| Kind | Default handling |
|---|---|
| Conversation, answers, reasoning, summaries | No automatic expiry deletion |
| Shell pipe source | Terminal shells: seven days / total 1GiB; live work and pins protected |
| Managed clipboard images | Seven days / total 256MiB; active use, send pins, saved drafts, and similar protection take precedence |
| External files/images | Path references; Shellboy GC never deletes external originals |
| Deleted-Thread history bodies | Reachability GC reclaims bodies no longer needed by live forks, summary sources, and similar references |

Preview impact before deleting a Thread. Preview starts neither deletion nor GC.

```sh
shellboy thread delete-preview --thread THREAD_ID
# Include descendants in the deletion target
shellboy thread delete-preview --thread THREAD_ID --descendants
```

`deletable_now` means selected targets have no live/stopping Run. Inspect blocking Runs, descendant counts, attachment references, direct sharing with other Threads, and direct sources of completed replacements. These counts are neither whole-GC reachability nor physical file counts. `reclaimable_bytes=null` with a reason means capacity is unestimated. Sharing or child Threads alone do not prohibit deletion. State may change after preview, so actual delete checks again within its transaction.

If blocked, stop the blocking Run and confirm its termination before deleting:

```sh
shellboy thread delete --thread THREAD_ID
```

`--descendants` also targets lineage children; verify the scope first. Shared fork paths and necessary provenance remain; tombstones retain reasons after collection. Dedicated bodies of deleted Threads are not guaranteed to remain readable forever.

## Releasing originals of summaries and spills

Compaction keeps originals by design. When you decide you no longer need them, release them explicitly for one Thread:

```sh
shellboy thread prune --thread THREAD_ID --before NODE_ID --dry-run
shellboy thread prune --thread THREAD_ID --before NODE_ID
shellboy storage collect --category history   # optional: reclaim now, read nodes_tombstoned / history_bytes_collected
```

Summaries and spills adopted before `NODE_ID` (nested summaries included) lose this Thread's hold on their originals; the reachability GC reclaims whatever no other root holds. Originals that a fork or another live Thread still holds, pins, user messages with a receipt to a live Thread (reported as `kept_receipt_messages`) and attachment deliveries are kept, and replacements another live Thread holds are skipped and counted. The dry-run reports summary and spill counts separately, the raw size of the candidates, and `reclaimable_bytes: null`; the real reclaimed amount is whatever the GC reports. You lose, for that range: restoring the summary, searching the originals, cached partial summaries and rewinding into it. The summary and spill placeholders remain. Reading a pruned original reports "pruned / not retained". This cannot be undone and nothing does it automatically. The GC frees pages inside the database file but does not shrink the file (no automatic VACUUM).

## Reading capacity figures

Key `storage status` fields:

| Value | Meaning |
|---|---|
| `logical_live_bytes` | Logical body/raw/index/managed-image usage; basis for capacity control |
| `db_bytes` / `wal_bytes` | Actual file sizes |
| `managed_bytes` / `log_bytes` | Physical managed-file/log sizes |
| `filesystem_free_bytes` | Filesystem free capacity, when available |
| `disk_free_min_bytes` / `disk_free_state` | `limits.disk_free_min_bytes` and where `filesystem_free_bytes` stands against it: `ok`, `warning` (below twice the minimum), `held` (below the minimum; new work is held), or `unknown` (the filesystem reported no free space; nothing is held and `filesystem_free_bytes` is omitted) |
| `reclaimable_bytes` | Lower bound on reclaimable capacity, not an exact estimate over all GC categories |

Default logical warning is 40GiB and maximum 50GiB. At the maximum, conversations are not automatically deleted: new work is rejected with `storage_limit` or enters recovery wait. Distinguish a body already accepted/saved from an operation rejected before acceptance. Stop, shutdown records, and GC remain allowed. Going below the limit after collection still requires explicit retry of a recovery Turn.

**Low disk space.** A full disk is fatal, so the same gate also holds new work when the volume that holds the data directory (a symlinked data directory is resolved to its target volume) has less than `limits.disk_free_min_bytes` free (default 5GiB). It is read from the OS on every check, so there is no stale value: held work resumes after you free space and explicitly retry (`run retry`, or the TUI retry), with no other step. The error is `storage_limit` with `details.reason` of `disk_free_low` (and `filesystem_free_bytes`, `disk_free_min_bytes`); the logical limit has `details.reason` of `logical_limit`. When both apply the logical limit is reported. Below twice the minimum nothing is held, but `storage status` shows `disk_free_state: warning`, and the TUI storage view shows the same state next to the free bytes. If the filesystem cannot report free space, nothing is held and `storage status` shows `unknown`. Stop, shutdown records, observation, GC and `storage collect` stay available while held. Automatic indexing is held too, but unlike the logical limit a low-disk hold saves no index pause and discards no queued work: claimed inputs are released and the queue is claimed again by itself once space returns (the worker re-checks every 30 seconds and on any wake), with no explicit index retry. An int8 conversion (`index shrink`) respects the same minimum: it is refused as `low_disk` when it would leave less than the minimum free, and a conversion that meets a short disk fails alone (no pause, partial candidate given back) and resumes with an explicit `index shrink`. Removing a retained index (`index remove`) frees space for reuse inside the database file and only `storage vacuum` shrinks the file; that is a person's decision, never a model's. While it waits, `storage status` shows `disk_free_state: held`; `index status` shows no pause because there is none. Collecting returns space to the database file, not to the OS; to get free bytes back, remove files outside Shellboy (or prune Threads, then `storage vacuum` once the volume has the space VACUUM needs) or lower `limits.disk_free_min_bytes` on a small disk. Limit: only new work is held. Commands already running in a model's shell keep writing, and a command the model starts after a free-space check can still write; they are not stopped.

Reclaiming SQLite bodies need not immediately shrink DB files. Reusable pages differ from OS free capacity. No automatic VACUUM runs; to shrink the file, use the explicit command below. Avoid copying/deleting DB/WAL/SHM separately while running; for backups shut down normally and handle the saved data as a whole.

### Shrinking the database file (explicit VACUUM)

After `thread prune` and the history GC (`shellboy storage collect --category history`) free pages inside `shellboy.db`, the file keeps its size. To return that space to the OS, stop Shellboy (the owner) and run, from a shell outside it:

```
shellboy --data-dir DIR storage vacuum --dry-run
shellboy --data-dir DIR storage vacuum
```

Nothing runs it automatically and there is no incremental or auto vacuum. It is refused (`invalid_state`) while any owner holds the data directory, so a model running inside Shellboy cannot run it; ask the user.

The preview (and the start of a real run) reports SQLite facts, not estimates: `reclaimable_bytes` is `freelist_count x page_size`, a guaranteed lower bound (VACUUM also repacks partly empty pages, so the file can shrink by more); `db_file_bytes` and `wal_bytes` are the current file sizes. The space VACUUM needs, `required_free_bytes`, is the sum of three parts, with `live_bytes = (page_count - freelist_count) x page_size`: `required_temp_bytes` = `live_bytes` (SQLite's temporary copy, placed in `DIR/tmp`; if that directory is on another device it is checked there separately), `required_wal_bytes` = `32 + live_pages x (page_size + 24)` (VACUUM writes every live page back through the WAL as one frame with a 24-byte header, plus the 32-byte WAL header), and `shm_bytes` (the current size of the `-shm` index file). This is an upper bound derived from how SQLite works, not a measurement of a particular run. If `filesystem_free_bytes` is smaller, the real run fails with `storage_limit` before touching the database.

A real run checkpoints the WAL (TRUNCATE) first and refuses (`invalid_state`, nothing changed) if another process, such as a running `shellboy request inspect` or a RAG history read, holds a read transaction on the database; then it runs VACUUM, checkpoints again, verifies `quick_check`, and reports `db_file_bytes_before`, `db_file_bytes_after` and `bytes_freed`. VACUUM is atomic: if it fails or the process is killed, SQLite restores the previous database; leftover files in `DIR/tmp` are temporary. If a reader appears during VACUUM so the WAL cannot be truncated afterwards, the command fails with `invalid_state` (database consistent, file not yet shrunk) instead of reporting success. Large databases take time and the owner cannot start meanwhile. Only the conversation DB `shellboy.db` is vacuumed; the standalone RAG store is a separate file that this command does not touch.

VACUUM rewrites the file in place, so its device/inode stays the same and history search references that bind the database file identity remain valid; if a platform changes the identity, those references are conservatively invalidated and a new search is needed. Nothing else is invalidated: VACUUM does not change node IDs, versions or content.

RAM body/layout/history/Base64-image cache budgets differ from storage retention. Zero caches do not delete conversations or impose a whole-application RSS cap including in-flight working memory. See [configuration](configuration.md) for each budget.

Even when logical capacity stops new indexing, semantic searches of existing indexes remain available. Query embedding may cost money; search alone neither resumes indexing nor backfills old data. Physical disk exhaustion and operation receipt-budget rejection have separate recovery procedures.

## Common problems

| Symptom / code | Check and response |
|---|---|
| `shellboy` not found | Build/install location and PATH ([installation](configuration.md)) |
| `app_not_running` | Start the owner with the same data directory in an interactive terminal; CLI never autostarts it |
| Duplicate-start lock error | Use the owner already managing that data directory; explicitly choose another directory for a separate environment |
| `config_invalid` / missing credentials | Reported key, connection profile, API root, environment variable name; restart to read edited TOML |
| API balance/authentication/model error | Read saved provider reason and choose an available connection; no automatic other-provider fallback |
| `thread_not_found` / `target_unavailable` | Target ID, data directory, deletion/collection state |
| `invalid_state` / `wait_cycle` | State/ownership; do not wait on self/ancestors, take over another owner's Run, or retry a terminal Run |
| `operation_conflict` / `operation_expired` | Do not change content under one ID; query old work and never blindly resend uncertain effects |
| `cursor_expired` / `cursor_encoding_mismatch` | Restart list/body retrieval without cursor; do not reuse for another target/encoding |
| `output_expired` | Raw reclaimed; separately inspect saved quotes/answers, never invent source |
| `input_limit` | Reported body/attachment/frame limit and target; no silent truncation |
| `reply_limit` | Target Thread already has 16 pending reply duties (`details.limit`, `details.current_count`); reply to completed requests or cancel obsolete ones, then retry |
| `storage_limit` | Inspect usage, reclaim according to retention or revise configuration, explicitly retry |
| `save_failed` / `io_failed` | Free space, permissions, destination/reference state; distinguish unsaved input from saved acceptance |
| Search unavailable / few results | Embedding settings, selected space, index status; explicitly choose historical backfill |
| Model will not create a subagent | Give a real Agent Guide path to read; natural-language phrasing alone does not guarantee tool use |
| Enter conflicts with IME confirmation | Change send_key/newline_keys and restart; terminal preedit notification is not guaranteed |

Reports should include operation, target state, code, and conditions with secrets removed. Never share API keys, origin tokens, environment dumps, or full real conversations in logs. [CLI reference](cli-reference.md) lists fixed codes and JSON format.

## Diagnostic logs

Normal diagnostic logs are `logs/shellboy.log` in the data directory, with rotation. macOS default: `~/Library/Application Support/shellboy/data/logs/shellboy.log`. They record event names, timestamps, and target IDs for startup/shutdown and DB/model/shell failures, not continuous conversation bodies, API credentials, or complete error bodies.

Windows default: `%LOCALAPPDATA%\shellboy\data\logs\shellboy.log`. With a custom location, inspect that data directory. Model `model_failure` records fixed classification and `attempt_id`. `model_stream_difference` diagnoses streamed/final differences; alone it means neither failure nor recovery wait. Correlate Thread/Run/Turn/attempt IDs, then use `/received` in the same Thread for saved source/final response. Source contains conversation/tool arguments; do not share it externally unchanged.

TUI `message_send` traces individual submissions using `send.draft_id` / `revision` / `operation_id`. `queued` means the frontend took the request, `processing` means send processing began, `saved` means persistence succeeded, and `failed` / `queue_full` indicate failure/admission shortage. Link saved `message_id` and Thread ID to history. `operation_id` also works in existing operation queries.

If `送信受付中` (accepting submission) persists, inspect the last stage: only `queued` precedes processing, `processing` precedes confirmed save, and `saved` without screen progress points toward notification/history-fetch/rendering investigation. `queued` does not record Enter or a rendered frame; `saved` does not prove visible reflection. Logging is best effort through the existing bounded asynchronous writer: disabled settings, capacity, or crashes may omit records. Absence alone proves neither nonacceptance nor cause. Body/attachment paths are not additionally logged.

Fatal aborts such as stack overflow bypass normal shutdown and may leave no cause text in the file. Terminal stderr, crash time, screen, and preceding actions also help investigation. Missing normal shutdown alone does not identify the cause.

### When operation receipt capacity rejects a request

A mutation rejected with `storage_limit` and receipt-budget guidance was not accepted. `shellboy status`, `storage status`, and `operation get --operation ID` observe capacity/saved work without creating a new receipt. Get/list/search/preview reread even under one ID rather than accumulating results. Repeated semantic query embedding may cost money. Wait retains a receipt because it saves Turn wait state.

The original mutation ID, identical request, and valid same caller can retrieve a retained result. Terminal results become collectible by internal maintenance 24 hours after created_at or under budget pressure. Pending and references from unapplied messages/attachments/compaction remain protected. After outcome collection, ID/hash metadata remains: resending that ID yields `operation_expired` without reexecuting changes. Inspect `result_expired` / `replay_policy`, target state, and history; never blindly repeat effects under a new ID. This does not bypass authentication for an expired Origin.

`run stop` / `shell stop` can use fixed headroom beyond the normal 256MiB. Inspect `control_headroom_bytes` / `control_limit_bytes` / `control_available_bytes`. If stop itself exhausts headroom, new IDs are rejected; TUI Ctrl+C and `/stop` remain receipt-free. Available bytes never guarantee acceptance of an arbitrary next request.

Expired-ID metadata counts against the same budget. If extreme mutation volume or protected entries fill it, end work normally and explicitly restart the owner to reclaim old terminal entries. Internal GC continues while preserving uncertainty for old pending work and protected references. No automatic restart or unauthorized pending deletion occurs. The 8MiB-per-outcome / normal-256MiB budgets are logical, not SQLite physical-size caps.

GC `managed_deleted` / `managed_failures` contain at most 128 details each. `managed_deleted_count` / `managed_failure_count` are real totals; `managed_details_truncated` indicates omitted details, not skipped collection work.

### Text received before failure

Text streamed on screen remains readable on recovery wait when saved source exists, labeled `途中で切れた応答（保存済み・会話には含めない）` (interrupted response, saved but excluded from conversation). `/received` reaches older failures and successful-response stream differences; Enter opens saved source. Attention color applies only during the original Turn's recovery wait, then becomes a normal notification after recovery/stop. Request excerpts and timestamps identify records. See [receive records](tui.md#reading-saved-receive-records) for paging. Preserving provisional text differs from adopting it into model retry input.

### Shutdown grace and Unix execution identity

After Quit, Runs may briefly continue until persistence and owned-work stop finish. Wait for normal shutdown; forcing exit while saving can leave uncertain external effects. Unix stop checks owned-process records, but PGID inspection and signal delivery cannot atomically exclude OS ID reuse between them.

Processes detached outside the owned group with `setsid` or similar can survive stop. Complete cleanup is also not guaranteed for `nohup` use that detaches beyond ownership tracking. Shellboy does not track or maintain self-daemonized work across restarts.

## When HTTP reception stalls

After at least thirty seconds without observed reception for the same model request, conversation activity adds receiving-wait or elapsed-since-last-reception information. Receive notifications occur at most once per second, introducing less than a second of display difference. This does not assert that generation/reasoning stopped. Ctrl+C or `run stop` can stop it.

Responses use separate per-connection `response_header_timeout_ms` and `stream_idle_timeout_ms` deadlines. The first bounds waiting for response headers; after headers arrive, only the stream idle deadline applies to silent reception. Summary requests share these separate deadlines but remain outside conversation retry policy. At a conversation timeout, received text is preserved and retry/partial-output settings choose early large-input reduction, ordinary retry or recovery wait. Inspect the saved transport phase, elapsed milliseconds and configuration key. `stream_idle_timeout_ms` defaults to 600000; raise it for models with long post-header silence or use 0 to disable that idle deadline. It does not disable the header deadline. The fixed thirty-second connection deadline and manual cancellation remain. Unknown failures are not automatically resent. A before-body connection failure follows normal retry policy. The header deadline includes upload time. Eligible repeated failures use the same finite retry budget; reduction does not add a separate request allowance. Source consumption is not proof of peer delivery.

Embeddings, semantic search, and RAG use per-connection `request_timeout_ms` (default 60000, minimum 1000) as the total request deadline. Model lists use its minimum with 20000ms. Index-worker timeout follows existing bounded retries; repeated failures pause the space. Read the status reason, repair connection/settings, then explicitly retry. Owner changes require restart. Search CLI disconnection or owner shutdown also cancels remaining search HTTP requests.

## Investigating incomplete EOF

`EOF before response.completed` means Shellboy had not recognized a completed event when the HTTP body ended. This wording alone does not prove the API never sent one.

EOF failures save diagnostic fields in their reasons. These fields include `received_bytes`, `parsed_events` (parsed JSON count), `completed_items`, `done_markers`, `pending_line_bytes` / `pending_data_bytes` (unfinished tail), and fixed-category `last_event`. Inspect the same Turn/attempt reason through `/received` source and share these diagnostic values rather than the full body. Normal `model_failure` logs still contain classification and attempt ID, not automatic complete error bodies.

`last_event=untyped` means JSON lacked a string type; `done_markers` counts `[DONE]` markers. Neither proves a completed response. Nonzero bytes with zero parsed events can mean comments only, so avoid inferring a cause. Events lacking their final blank line remain incomplete and are not automatically committed as answers or tool execution.

## Failure-driven reduction (#50)

There is no pre-POST size policy or token estimate. With
`auto_trim_on_disconnect=true`, a first eligible transport/header/5xx failure
retries unchanged; from the second consecutive failure it attempts API-free
maximum safe omission. 429 and stream-idle reception timeouts retry without
that trigger. Explicit context-limit errors immediately reduce, with the
existing latest-spill/summary stage as a last resort on another overflow.
Every resend counts against response_max_retries (default eight), even after
reduction; zero disables automatic resends. Transient failures with no safe
candidates retry unchanged. Explicit overflow never resends unchanged input:
if maximum omission has no target, the final spill/summary stage is tried
immediately; absent/rejected/exhausted reduction enters recovery without a resend.
Successful responses reset the consecutive budget, so a long Turn can reduce
again after growing. Partial-output opt-out, stop and Retry-After safety remain.

When maximum omission is planned during a live Turn, every user message in that
Turn is retained verbatim as an anchor, including instructions delivered after
the initial request. Omissible work between those messages can still be
replaced by one notice; the request and later user messages are not silently
removed. This is separate from whether an interrupted request is retried.

While the model's latest response asked for a tool (the tool loop is still
open), Shellboy does not omit or spill earlier tool results: signed thinking is
bound to the exact history before it. In that state an explicit overflow
summarizes the active Turn instead (all its tool rounds become one summary, the
latest round as text, user instructions kept in the summary), else the earlier
Turns. The selected path after an active-Turn summary is
`[earlier Turns][request][summary]`. The omission after transient failures waits
and the failure retries
unchanged. If the provider still rejects replayed thinking as bound to a
different conversation, Shellboy resends once without earlier reasoning and
records it on the failed attempt.

Recovery shows the saved failure, retry count, last-failure time and transport
stage, plus protected request/latest-tool character/byte counts, known attachment
sizes and available actions in error details. Unknown sizes remain null. These
are facts, not a cause diagnosis or ranked recommendation. /compact-max needs
safe reducible context; /detach requires attachments; /restore requires an
omission marker. /retry, /edit, /model (selection) and /stop retain their existing
semantics. Inspect the facts in F8 if the minimal recovery line is clipped.
Settings reload still requires owner restart and is outside this change.

## When an interrupted tool group cannot be reconciled

Startup normally supplies missing interrupted tool results without rerunning
commands. If its saved source bindings or selected history are inconsistent,
Shellboy leaves that group unresolved and blocks that conversation locally
before contacting the model. Other conversations and the owner remain available.
Repeated restart does not repair missing or contradictory records.

Inspect the saved interruption/history records first. Continue from a known
complete point **before** the incomplete tool group with
`shellboy thread fork --thread THREAD --node EARLIER_NODE`, or remove the affected
conversation explicitly with `shellboy thread delete --thread THREAD`. A fork
inside an incomplete group is rejected; it cannot invent the missing results.
Files and external command effects are not rolled back by either operation.
Use `shellboy docs get --id shellboy/history-and-search` for history inspection
and fork boundaries.

A Thread whose saved cursor disagrees with its path cannot receive an
interruption snapshot. Startup records its Thread ID and a fixed reason in
diagnostics without logging its content, and continues with healthy conversations.
Actual database/I/O failures still stop startup instead of being treated as a
recoverable conversation defect.

Per-conversation isolation does not bypass database integrity checks. For example,
invalid JSON syntax in `turn.recovery_reason_json` violates its database CHECK
is detected by `quick_check` when it runs after an unclean or unknown shutdown,
or by `shellboy storage check`. A clean restart does not run that scan and can
therefore leave pre-existing CHECK violations undetected. Preserve the database
and inspect any reported storage/integrity failure; do not treat corruption as a
successfully isolated conversation defect.

## Manual database integrity check

Stop the owner, then run `shellboy storage check` with the same `--data-dir`
and configuration. The command acquires the owner lock and runs SQLite
`quick_check` and `foreign_key_check` without creating or migrating a database.
It requires write access to open SQLite in read-write mode, but enables
`query_only` before checking; SQLite may recover existing WAL state on open.
Use it after suspected storage corruption or when a full manual check is needed.
Normal startup checks foreign keys after migrations and runs `quick_check` after
an unclean or unknown previous shutdown; a clean restart skips those scans.
A successful check does not prove that every application-level invariant holds.
