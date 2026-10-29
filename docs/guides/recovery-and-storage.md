# Recovery, stopping, and storage

[Guide index](README.md) / [User Guide](user-guide.md) / [Agent Guide](agent-guide.md)

When responses stop, inspect saved acceptance, Turn, and Run state before repeatedly sending the same request. Use the CLI in another terminal while the owner remains running. Examples below use Bash; replace IDs with values from actual responses.

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

When `answer_node_id` is returned, retrieve it with `shellboy history get --node NODE_ID`. Follow `next_cursor` for long bodies. Terminal `failed` / `stopped` states are not successful answers.

In the TUI, start via F8 `待機 · 条件を観測` and cancel only observation with lowercase w in management or `/cancel-wait`. Esc closes the display without stopping the target. Ctrl+C interrupts the current Thread instead; see [wait controls](tui.md#cancelling-observation-versus-interrupting-its-target).

Use `shellboy wait` to wait for execution. `timeout` does not mean the target stopped or failed. `satisfied` need not mean target success: inspect the returned snapshot and state. [Delegation](delegation.md) and [CLI reference](cli-reference.md) show JSON and state-dependent branches.

## Automatic retries for temporary model communication failures

Temporary communication failures trigger up to eight consecutive additional attempts by default. TUI `一時的なエラーのため再試行中` (retrying a temporary error) is a normal waiting display. Ctrl+C or stopping the Run cancels the wait. Exhausting the budget enters recovery wait with the latest error and count. Successful final-response adoption resets consecutive failures; this is not an eight-retry cap over an entire long conversation/tool loop.

Eligible failures are HTTP 429/500/502/503/504 and temporary connection failures, disconnects, EOFs, and receive-idle timeouts. By default, partial output also retries, with received text preserved in the failed attempt. Tool calls are not executed before final-response adoption. Permanent authentication, configuration, format, or explicit provider failures are not automatically resent. HTTP error bodies are distinct from model output.

If headers for HTTP 429/500/502/503/504 have arrived and reading their error body stalls or disconnects, the saved HTTP status still determines retry eligibility. HTTP 400/401/403 and similar responses do not retry solely because their body retrieval timed out.

Per-connection `response_max_retries = 0` disables automatic retries. `response_retry_partial = false` sends only partial-output failures to recovery wait; communication failures containing metadata/heartbeats without output remain retryable. `response_retry_max_delay_ms` caps exponential delay (default 60000); server `Retry-After` has a separate fixed ten-minute cap. See [configuration](configuration.md). Retries may duplicate charges when upstream processing already occurred. Failed attempts save the count, delay, and this possibility. Recovery wait after automatic retries does not resume merely with time. Retry-After over 600 seconds is preserved/displayed and enters recovery wait rather than being shortened to 600 seconds and resent.

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

After restart, opening a conversation that had background work shows `前回の終了時の記録` (previous shutdown record), with counts of work, response processing, commands, children, and interrupted wait observations. In F6 select that record and Enter for IDs/reasons or O for saved source. Entries labeled `前回の保存記録 · コマンド` (previous saved commands), `子の作業` (child work), and `前回の待機観測は中断済み` (previous wait observation interrupted) lead to individual targets. Use get commands or F8 for their current saved Run/Turn/shell state.

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

An external human CLI or the direct owning parent decides recovery. Merely being an ancestor model or an independent chat does not confer the same authority. An ordinary parent message is not a retry. A human's new instruction can explicitly resume the same recovery Turn, but the command above clearly retries the original Turn after its cause is fixed.

`run retry` does not revive a terminal Run. After stop/restart, send a new instruction to create a new Run/Turn. Restart never stops an old PID or automatically repeats shell side effects. Previously uncertain state becomes history/restart context.

For conversations that received HTTP 400 because histories mixed endpoint-specific item IDs, update, restart the owner, and send a new request in the same conversation. `/retry` cannot target the old Run stopped by restart. Saved history passes through new conversion on send, removing only endpoint-specific message/function_call IDs. Source remains intact; this does not guarantee resolution of other provider constraints such as reasoning items.

Explicit context-capacity overflow triggers automatic compaction and up to two retry stages. Communication failures are not guessed to be context overflow. At the first eligible large-input failure, deterministic reduction may instead run once as described below. For manual `/compact`, see [history and search](history-and-search.md).

## Interrupting and quitting

| Action | Meaning |
|---|---|
| TUI Ctrl+C | Request stopping the current Run |
| `shellboy run stop --run RUN_ID` | Accept stopping the specified Run and its owned descendants |
| `shellboy shell stop --execution EXECUTION_ID` | Request stopping that shell |
| Ctrl+C during external CLI wait | Disconnect observation; target Run/shell continues |
| TUI Ctrl+Q | Stop work, save, close DB, and shut down normally |

Successful stop acceptance does not prove all PIDs were reaped. Wait for Run termination and inspect state when needed. A model can stop its own Run, currently owned descendants, and their shells. Ability to send requests to independent Threads does not imply authority to stop their execution.

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

## Reading capacity figures

Key `storage status` fields:

| Value | Meaning |
|---|---|
| `logical_live_bytes` | Logical body/raw/index/managed-image usage; basis for capacity control |
| `db_bytes` / `wal_bytes` | Actual file sizes |
| `managed_bytes` / `log_bytes` | Physical managed-file/log sizes |
| `filesystem_free_bytes` | Filesystem free capacity, when available |
| `reclaimable_bytes` | Lower bound on reclaimable capacity, not an exact estimate over all GC categories |

Default logical warning is 8GiB and maximum 10GiB. At the maximum, conversations are not automatically deleted: new work is rejected with `storage_limit` or enters recovery wait. Distinguish a body already accepted/saved from an operation rejected before acceptance. Stop, shutdown records, and GC remain allowed. Going below the limit after collection still requires explicit retry of a recovery Turn.

Reclaiming SQLite bodies need not immediately shrink DB files. Reusable pages differ from OS free capacity. No automatic VACUUM runs. Avoid copying/deleting DB/WAL/SHM separately while running; for backups shut down normally and handle the saved data as a whole.

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

Responses use separate per-connection `response_header_timeout_ms` and `stream_idle_timeout_ms` deadlines. The first bounds waiting for response headers; after headers arrive, only the stream idle deadline applies to silent reception. Summary requests share these separate deadlines but remain outside conversation retry policy. At a conversation timeout, received text is preserved and retry/partial-output settings choose early large-input reduction, ordinary retry or recovery wait. Inspect the saved transport phase, elapsed milliseconds and configuration key. `stream_idle_timeout_ms` defaults to 600000; raise it for models with long post-header silence or use 0 to disable that idle deadline. It does not disable the header deadline. The fixed thirty-second connection deadline and manual cancellation remain. Unknown failures are not automatically resent. A second consecutive request-body-source-incomplete failure enters recovery instead of more automatic shrinking or retries; source consumption is not proof of peer delivery.

Embeddings, semantic search, and RAG use per-connection `request_timeout_ms` (default 60000, minimum 1000) as the total request deadline. Model lists use its minimum with 20000ms. Index-worker timeout follows existing bounded retries; repeated failures pause the space. Read the status reason, repair connection/settings, then explicitly retry. Owner changes require restart. Search CLI disconnection or owner shutdown also cancels remaining search HTTP requests.

## Investigating incomplete EOF

`EOF before response.completed` means Shellboy had not recognized a completed event when the HTTP body ended. This wording alone does not prove the API never sent one.

EOF failures save diagnostic fields in their reasons. These fields include `received_bytes`, `parsed_events` (parsed JSON count), `completed_items`, `done_markers`, `pending_line_bytes` / `pending_data_bytes` (unfinished tail), and fixed-category `last_event`. Inspect the same Turn/attempt reason through `/received` source and share these diagnostic values rather than the full body. Normal `model_failure` logs still contain classification and attempt ID, not automatic complete error bodies.

`last_event=untyped` means JSON lacked a string type; `done_markers` counts `[DONE]` markers. Neither proves a completed response. Nonzero bytes with zero parsed events can mean comments only, so avoid inferring a cause. Events lacking their final blank line remain incomplete and are not automatically committed as answers or tool execution.

## Early reduction of large inputs

Connection `auto_trim_on_disconnect` defaults to true. Above an estimated 75,000 input tokens, the first header timeout, 5xx or connection/EOF failure considers API-free maximum safe omission before ordinary retries are spent. A smaller input follows normal retry policy. `auto_trim_on_disconnect=false` disables this reduction; `response_max_retries=0` disables ordinary retries but does not disable the separate reduction allowance. Partial-output opt-out, terminal failures and an excessive Retry-After still prevent automatic resending.

Optional positive-integer `compact_hint_tokens` changes activation guidance only and also permits reduction before the first POST when the estimate exceeds that hint. Without the hint, no pre-POST reduction occurs. It does not specify how much context remains or a provider capacity limit.

Maximum omission removes all safe groups outside the current request, latest group, latest complete tool group, unfinished work and protected/unapplied instructions. Any available safe omission is adopted; there is no half-size target or requirement to reach a numeric goal. Automatic result `target_tokens` is null. Original nodes and effects are preserved. If no safe group exists, a failing request enters recovery with unchanged history.

One adopted reduction allows one resumed POST per Turn/generation. If that request fails, recovery follows; there is no second reduction or renewed retry loop, including after a successful tool response in the same Turn. A configured proactive reduction consumes the same allowance. A fixed context notice retains original ranges/counts, estimates and restoration guidance. No summary API is called automatically.

Use `shellboy rag search --scope history:THREAD --mode keyword --query TEXT` to retrieve omitted sources without an embedding model. Bounded `history get` and a selected range summary also remain available. Restoring a large range may reproduce the same failure; search or summarize the needed portion first. Executed commands, file changes and side effects are not undone; inspect current state before repeating earlier actions.

Argument-free TUI `/restore` or CLI `shellboy compact --restore --thread THREAD` restores the omitted range while retaining subsequent conversation and leaving attachments excluded. `/restore NUMBER` is the separate failed-draft restoration feature. See [input size and range editing](history-and-search.md#input-size-and-range-editing).

If reduction was considered but not adopted, attempt `automatic_trim_attempt` distinguishes below-threshold input, absence of safe groups, and adoption rejection. Estimates use recent successful usage/input measurements when available; otherwise the character/4 fallback is an approximation and can underestimate some languages or attachments. Compare the latest successful request's measured usage.
