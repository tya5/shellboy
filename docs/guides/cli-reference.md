# CLI reference

[Guide index](README.md) · [Agent Guide](agent-guide.md) · [Configuration](configuration.md)

Conversation/execution-control subcommands send local RPC to an already running owner; they never start it automatically. `shellboy` without arguments starts the TUI owner in an interactive terminal. The owner tables below describe the current 40 operations and `attach` alias in `src/cli.rs` / `src/protocol.rs`. `docs` and `rag` are standalone commands requiring no owner; see [retrieving guides](docs.md) and [document/history search](rag.md) for syntax/results. The model has one tool, shell; these commands run inside that shell.

## Standalone commands

```bash
shellboy docs list --parent shellboy
shellboy docs search --query 'subagent'
shellboy docs get --id shellboy/delegation
shellboy rag --help
shellboy rag search --scope docs --scope history --query 'recovery' --mode keyword
```

`docs` reads same-version English guides embedded in the executable without an API; the offline catalogue is English only. `rag` searches registered documents, guides, and explicitly selected history scopes without starting a generative model. Standalone JSON has no owner operation_id; combining with `--operation-id` is rejected. RAG partial failures return `ok:false` alongside partial results; inspect per-scope errors.

## Common syntax and connection

`shellboy status` lists saved work. From a model it defaults to the caller's Thread; from an ordinary external terminal it covers all Threads. Explicit `--run RUN` filters membership in that Run without implicitly including owned descendants. Combined `--thread THREAD` applies both conditions. `--all` includes terminal work without widening Thread scope. `--state failed` and similar exact native-state matches can retrieve terminal states.

```sh
shellboy status --limit 50
shellboy status --run RUN --all
shellboy status --thread THREAD --state failed --since 2026-10-25T00:00:00Z
```

`items[].targets` contains typed operations to get/read targets. `filters` contains normalized conditions; `next_cursor` continues the same conditions. State updates/deletion alone do not expire the cursor. Items created after the initial watermark require a fresh list without a cursor. Reordering after updates can duplicate or miss entries. Different owner instance, scope, conditions, or cursor version produces `cursor_expired`: restart from the beginning. Mid-pagination limit changes within 1–200 are allowed. `fingerprint` identifies the query projection, not an immutable multipage snapshot. `observed_at` is query time, not guaranteed simultaneous observation of live OS processes or standalone RAG. `activity_at` is the maximum saved creation/start/end/request-update time, not an unrecorded state-transition timestamp. Thread `current_model` is current configuration; attempt `attempt_model` is what that request actually used.

The default excludes terminal work, so zero items does not mean Threads/history disappeared. Use `shellboy index status` for indexes, `shellboy rag status` for standalone stores, and `shellboy docs get --id shellboy/agent-guide` for instructions.

```text
shellboy [--data-dir PATH] [--config PATH] [--operation-id ID] <group> <operation> [flags]
shellboy --help
shellboy <group> <operation> --help
shellboy --version
```

Global flags may also follow subcommands. `--data-dir` selects owner storage, not work cwd; `--config` selects TUI startup settings, standalone config init/check, or standalone RAG embedding connection settings. CLI connection resolution is `--data-dir` > `SHELLBOY_DATA_DIR` > OS default; it does not read TOML. See [OS paths](configuration.md#locations-and-precedence).

The CLI automatically uses `SHELLBOY_*` context injected into the model shell. Do not display, copy, or manually generate origin tokens. Choosing another data directory does not inherit this context. Even when `--thread` is optional, external terminals have no current Thread: specify it. Omission resolves to the authenticated caller's Thread, never silently to the TUI's selected Thread.

Replace example `THREAD` / `NODE` / `RUN` / `TURN` / `EXECUTION` / `MESSAGE` / `ATTACHMENT` with returned IDs. A Run is an execution lifetime; a Turn is an instruction/response unit; execution identifies a shell; node identifies immutable history; message tracks acceptance/assignment.

## JSON, exit codes, and replay

Normal CLI success/failure writes one JSON line to stdout; help/version use ordinary text. Success exits 0; syntax/RPC/connection failure exits 1. A `wait` successfully observing timeout or recovery_wait also exits 0 because RPC succeeded; determine target success from `result`.

If a reply is lost, `details.read_only=true` identifies a nonmutating operation. Get/list/search/preview/status create no receipt and recompute current values even with the same operation ID. After disconnect, reissue the same command/filters, but semantic query embedding may charge again. `wait` is not read-only because it may save the caller's waiting state.

```json
{"protocol_version":2,"operation_id":"INSTANCE:NONCE","ok":true,"result":{"accepted":true}}
```

```json
{"protocol_version":2,"operation_id":"INSTANCE:NONCE","ok":false,"error":{"code":"invalid_state","message":"...","hint":"...","details":{}}}
```

`hint` / `details` are optional; success has no error and failure has no result. Errors before communication can have an empty operation_id. The whole protocol frame is limited to 8MiB. Owner and CLI must use the current protocol version (2); there is no older-owner fallback.

Without an explicit ID, the CLI generates an instance-specific operation ID. For mutations/wait, resending `--operation-id` with the same ID, content, and caller retrieves retained results; changed content yields `operation_conflict`. Reads reobserve under the same ID and create no receipt for themselves. Message acceptance and receipt share one transaction. An accepted side-effect intent with unknown outcome may return pending: **this does not justify reissuing it under another ID**. Inspect `operation get --operation ID` `state` / `outcome` and target state. Old-instance IDs query saved receipts, never execute as new operations. Mutation replay of an unsaved old ID or expired outcome yields `operation_expired`; querying a missing receipt yields `operation_not_found`. Current-instance terminal outcomes may be collected after 24 hours during maintenance or under pressure, retaining expiry metadata to prevent reexecution. Pending and unapplied references remain protected. Check `result_expired` / `replay_policy`; do not blindly repeat a mutation under a new ID after its result disappears.

For important changes, explicitly retain an ID in the already known same-instance form `INSTANCE:YOUR_UNIQUE_NONCE` so acceptance can be investigated after disconnection. The instance component matches an operation_id returned by that owner. Do not reuse Run/Turn/message/node IDs as operation IDs.

`message send` `delivery_status=accepted|held|applied|cancelled` describes acceptance/application. `assignment=new_run|new_turn|existing_turn` includes both run_id and turn_id; saved assignment never changes. `assignment=pending` has neither; query `message get` later for initial assignment. Successful send does not mean the assigned Turn has completed. Use `end_message_node_id` for source retrieval when available. For explicit replies, inspect `reply.state` and read `reply.node_id` when replied.

### Saved execution context

`run get`, `turn get`, `message get`, and `history get` require `context`. `thread get` can omit it for a fork. `history get` requires `provenance`; every `history list` item requires `provenance`, `context`, and `selection`. It includes meaningful state, saved created_at/updated_at/ended_at, thread_id/run_id/turn_id/node_id, Thread parent_thread_id versus Run owner_run_id, and structured recovery_reason. Unrecorded started_at and Thread created_at are omitted. Run/Turn `lifetime_ms` is wall-clock creation-to-end time including preparation/waits and is absent before termination. A node's Thread is its origin, not a fork's selected path.

`operation get` `state/outcome` is the saved acceptance-operation result. `replay_policy=return_saved_outcome` returns the original result; `do_not_reexecute` means pending effects are not restarted. `result_uncertain=true` is not evidence of nonexecution. `targets` includes only IDs in the saved result; `downstream` shows compaction processing or current message delivery. Otherwise follow targets to get commands; inspect indexing per space with `index status`. The query itself creates no receipt and rereads current values under one query ID. `result_expired=true` / `replay_policy=operation_expired` means the original outcome body was collected and that mutation ID will not execute again.

## Operation dictionary

Brackets mean optional; `|` means alternatives. Add common global flags and `--help` to the operation-specific flags listed here.

### Work status (1 operation)

| Operation and all specific flags | Main result and conditions |
|---|---|
| `status [--thread THREAD] [--run RUN] [--since RFC3339] [--state STATE]... [--all] [--limit N] [--cursor C]` | `items`, `filters`, `observed_at`, `scope`, `consistency`, `fingerprint`, `next_cursor`; default 50, maximum 200. Saved state from one DB read transaction; no new execution, API request, or operation receipt |
| `status --memory` | Owner-wide cache retained/limit/entry accounting and native process memory; unknown values have reasons. Exclusive with activity filters; on-demand and no provider request |

### Standalone configuration (2 commands)

| Command | Behavior |
|---|---|
| `config init` | Create the bundled commented template at `--config PATH` or the OS default; existing files/symlinks are never overwritten. `result.created=false` means an existing path was left intact |
| `config check` | Strictly read that file, resolve defaults, check key-variable presence and GET the model catalog once. No owner or conversation/embedding request. A missing required setting, HTTP/network error or invalid TOML exits 1 with path/facts in `error.details` |

An unadvertised model produces `result.warnings` but still exits 0 (proxy aliases
and wildcards may work). Failed template writes remove the newly created file;
cleanup failures report `incomplete_file_retained`.

Both commands return ordinary JSON and reject `--operation-id`: they are not
owner RPCs and create no receipt or owner database. Init is an explicit file
write; check is a read plus a bounded catalog network request. Keep keys out of
TOML. See [first setup](configuration.md#first-setup-without-an-owner).

`model list` results include the connection `api` and `unavailable_features`;
`config check` also reports them. These describe dialect limits, not a promise
that a particular provider/model implements every feature.

### Saved usage (1 operation)

| Operation and all specific flags | Main result and conditions |
|---|---|
| `usage get --attempt ATTEMPT_ID` | One saved model attempt's IDs/state, bounded standard usage scalars, reported `prompt_cache_diagnostics` or JSON null, and `input_bytes`: measured UTF-8 bytes of the request body actually built for sending, `total` and `categories` (system, instructions, context, user, assistant, reasoning, tool_call, tool_result, spill_placeholder, summary, notice, attachment, other; they sum to `total`), or JSON null when not recorded. Exclusive with aggregate scope/time/group/pagination options; read-only and no provider call. |
| `usage get [--thread THREAD] [--run RUN] [--turn TURN] [--include-children] [--since TIME] [--until TIME] [--group-by hour\|day\|thread] [--timezone ZONE] [--limit N] [--cursor CURSOR]` | Saved usage totals or bounded grouped request-size/cache statistics. At most one scope ID. Ungrouped default is the caller's Thread; grouped default spans non-deleted Threads. Children require Thread scope. Ungrouped results keep conversation requests in `model_attempts` and compaction summary requests in `compaction` (`compactions`, `requests`, map/reduce counts, input/cached/cache-creation/output tokens; coverage `summary: recorded`); Turn/Run scope counts compactions run while that Turn/Run was current. Grouped results count conversation requests only. Ungrouped results also include `latest_request`: the scope's newest admitted model attempt (own Turns only, newest 33 Turns searched, period not applied) with `usage` and `input_bytes`, or null. Group-only flags and partial/cursor semantics are described under bounded usage breakdowns. No provider call or receipt. |

`thread get`, `run get` and `turn get` also include `usage_totals`. Counts use saved
completed/failed/incomplete model attempts, not proof of client transmission.
Each number has known_sum/unknown_count/overflow; unreported values are not zero.
The attempt period reports first/last known saved start times and unknown count.
Active saved rows are separate read-time values; observed_at is null when none
are active. Run/model/shell durations overlap and must not be added as work time.
A Turn has its own lifetime but does not repeat its parent Run's full lifetime.

`coverage.scope_complete` concerns selected saved scopes, not knowledge of every
numeric value. Child traversal uses the existing 200-item read safety bound,
excluding deleted Threads; a limit reports thread_limit and a partial scope total.
Summary/embedding/RAG usage is not recorded in these totals and has explicit
coverage rather than a guessed zero. Repeating a read ID evaluates current data.

Slow aggregations only (`>=100ms`, fixed constant) appear in `<data_dir>/logs/shellboy.log` as `event=usage_totals_slow` with `usage_slow.trigger/scope/elapsed_ms`.
Elapsed time is monotonic work inside the DB aggregation, excluding queueing and rendering; faster reads emit no aggregation diagnostic.

### Models (1 operation)

| Operation and all specific flags | Main result and conditions |
|---|---|
| `model list [--connection NAME] [--thread THREAD]` | `connection` and `models` (model-ID array); fetch live endpoint list without changing settings |

Connection precedence: explicit `--connection` > explicit Thread > authenticated caller Thread > owner default. An explicit Thread is checked for existence even alongside a connection. External calls without arguments use the owner default. The same operation ID fetches a fresh list without query receipts. Embedding models and others may appear; listing does not guarantee generation capability. Unsupported list API or authentication failures return explicit errors.

### Thread (7 operations)

| Operation and all specific flags | Main result and conditions |
|---|---|
| `thread list [--limit N] [--cursor C] [--query TEXT]` | `items` (thread_id/title/cwd/parent_id/state/latest_run_stop_reason/optional revisit/updated_at/excerpt), optional `next_cursor`; default 50, range 1–200. Query filters ID/title/cwd |
| `thread get [--thread THREAD]` | thread_id/title/cwd/connection/model/effort/parent_id, current run_id/turn_id, children_count/shells_count, recovery_reason |
| `thread create [--cwd PATH] [--connection NAME] [--model ID] [--effort VALUE] [--parent THREAD]` | New Thread details, without starting instructions or a Run. Parent establishes ancestry; execution ownership is assigned when the parent Run sends. Normally inherits parent settings |
| `thread fork --thread THREAD --node NODE [--cwd PATH]` | New Thread sharing the selected path through NODE inclusive, without changing the original; invalid boundaries such as incomplete tool groups rejected |
| `thread delete-preview --thread THREAD [--descendants]` | Read-only impact: deletable_now, blocking Run count/details (max 50), target/descendant counts, direct sharing/replacement-source/attachment-reference counts, update time; reclaimable_bytes null with reason. Rechecked on deletion |
| `thread delete --thread THREAD [--descendants]` | Deletion acceptance; targets must have no live/stopping Run. Descendants require explicit selection. Subsequent reachability GC preserves required shared sources |
| `thread set [--thread THREAD] [--title TEXT] [--cwd PATH] [--connection NAME] [--model ID] [--effort VALUE]` | Updated Thread; at least one change required. Does not change in-flight HTTP settings; applies at next boundary. `--effort ''` clears |
| `thread request --thread THREAD [--attempt ATTEMPT] [--compare-attempt ATTEMPT] [--body]` | Debugging, standalone (no owner, no model call, read-only database). Without `--attempt`: the next request of the Thread's latest Turn from the saved path, excluding undelivered events/messages. `--attempt`: the request a saved attempt sent, rebuilt from its manifest. `items` (index/type/role/chars/short sha256), `total` (sha256/bytes/chars of the exact body, or null), `unknown` (parts saved state cannot determine, never guessed), `saved` sizes for attempts; `compare` gives `common_prefix_items` and `first_difference`. `--body` adds the body; extra_body values appear as hashes; headers and credentials are never shown |

`thread list` / `thread get` fill empty saved `title` from the first user body, preferring the selected path then accepted/held messages in the same Thread if no body exists. `thread list --query` can match that display name. Stored title/history are unchanged; no model API naming occurs.

Set with `thread set --thread THREAD --title 'Investigation and fixes'`. `--title ''` or whitespace-only clears to conversation-derived display; omission preserves saved title. Trimmed titles have at most 512 characters and no newlines/control characters. Title-only updates do not change model, effort, or reasoning epoch. External CLI requires `--thread`; omission from an agent targets itself.

Root cwd defaults to owner startup cwd; child cwd to the parent's. Relative cwd resolves against the creation/change target's base. Changing cwd does not change an existing AGENTS reference. See [configuration](configuration.md#startup-settings-and-thread-settings).

### Message / Operation (5 operations)

| Operation and all specific flags | Main result and conditions |
|---|---|
| `message send [--thread THREAD] [--deliver now\|complete] [--expect-reply] [--sender-label TEXT] (--text TEXT | --text-stdin)` | message_id/thread_id/delivery_status/assignment/run_id/turn_id, plus `deliver` when not the default; acceptance differs from completion. Active work takes additions at safe boundaries; stopping can yield held/pending. Ordinary machine messages never silently resume recovery_wait |
| `message reply --message MESSAGE (--text TEXT | --text-stdin) [--sender-label TEXT]` | Reply once to an applied pending reply request; returns reply_to/message_id and optional delivery |
| `message get --message MESSAGE` | Original immutable assignment, current delivery_status, sender facts, and optional expect_reply/reply/reply_to facts, and end_message_node_id when available |
| `message cancel --message MESSAGE [--message MESSAGE ...]` | Cancel 1..128 fixed message IDs. Queued input can be archived; an admitted pending reply duty returns reply_cancelled without removing history. Other outcomes are cancelled/already_cancelled/already_admitted/not_found/forbidden/unavailable. Does not wake the model |
| `operation get --operation ID` | operation_id/state/outcome, instance_id/created_at/current_instance, result_uncertain/result_expired/replay_policy/targets, target-dependent downstream. Global `--operation-id` identifies this query; `--operation` identifies its target |

`message get` returns `sender.kind`: `model` with verified credential-free
instance/thread/run/turn/execution IDs, `external_cli` for a new external CLI
send without Origin, `human` for a recorded in-process TUI submission, or
`unknown` when saved evidence cannot establish it. A no-Origin CLI is not proof
of a human. Sender IDs remain readable after the originating Thread is deleted.
User-node `history get` and `history list` expose the same facts at
`provenance.sender`, joined by the accepted `message_id`, including later input
appended to an already-running Run. Legacy null sender records are `unknown`
in these sender facts: old records cannot distinguish a TUI sender from an
external CLI.

`--sender-label TEXT` adds an optional self-asserted name: at most 64 Unicode
characters, without newlines, control characters or line/paragraph separators.
It is returned as `sender.label`, separate from verified identity. The label
never grants ownership, stop, retry or cancellation authority. Explicitly
connecting to another data directory still strips model Origin; only the name
survives, as `external_cli`. TUI F6 message details display it as `名乗り：…`,
not as verified identity. F6 also explains `unknown` as missing sender evidence,
including old records. The conversation's source bands are a separate display
convention: legacy null records keep the existing human band so old conversations
do not all change appearance, whereas new `external_cli` records use the existing
generic input band rather than the human band. That legacy human band is not
verified sender identity. This follows decision D-006 in issue #68; band styling
belongs to #41 and is not changed by this sender-inspection follow-up.

`--deliver` chooses when the message is admitted; it is the same message in
every case:

- omitted: queue for the next safe boundary.
- `now`: also release a foreground shell wait for urgent delivery. That shell
  continues under its execution ID; an in-progress model response is not
  interrupted.
- `complete`: deliver when the Thread's live Run would complete
  normally, after owned shells and children finish. The result is
  `delivery_status: held`, `assignment: pending`, `deliver: complete`. If no Run
  is live, it is delivered immediately exactly like an omitted timing, and the
  result shows that assignment (for example `new_run`).

Later default or `now` messages overtake waiting `complete` messages; waiting
`complete` messages are delivered together, in send order, as one new Turn of
the completing Run. Several may wait. A waiting message never outlives its
Run: if the Run stops, is shut down or enters recovery wait instead, it becomes
ordinary held input at that moment (`stopping`, `previous_instance_interrupted`
or `recovery_wait`) and the usual message rules apply; a human Esc still ends
in completion, so it is delivered then.
Inspect with `message get` and cancel with `message cancel`. Acceptance still
does not mean delivery or completion.

Humans may cancel any sender's queued input; authenticated models may cancel
only input sent by their own Thread. Check each result: admission may win the
race. Same-operation replay returns saved outcomes, without cancelling or
restoring twice. A human can retrieve archived input with TUI `/restore list`
or `/restore draft ID`; restoration does not send it automatically. See
[queued messages](agent-guide.md#queued-messages-and-human-turn-interruption).

Supply exactly one of `--text` and `--text-stdin`. Stdin is read as UTF-8 with an 8MiB limit and no truncation; configured message/draft limits also apply.

Bash, with the owner already running:

```bash
printf '%s\n' 'Investigate this change and report your findings.' | shellboy message send --thread THREAD --text-stdin
shellboy message get --message MESSAGE
```

PowerShell can use `--text '...'` or pipeline `--text-stdin`. Native stdin encoding and empty arguments depend on shell settings. UTF-8 files can help preserve quoting in complex JSON examples.

### Run / Turn (4 operations)

| Operation and all specific flags | Main result and conditions |
|---|---|
| `run get --run RUN` | run_id/thread_id/state/owner_run_id/turn_id/end_message_node_id, owned_run_ids/owned_execution_ids, recovery_hint; optional waiting_reason identifies a saved in-flight context edit at the safe request boundary. Owned IDs reference current live resources, not a ledger of all historical children |
| `run retry --run RUN` | Recovery Run/Turn references; explicitly reprepare a recovery_wait Turn in a live Run, reusing the Turn/original message without blindly rerunning executed shells. Human or direct parent decides |
| `run stop --run RUN` | Confirmed stopping/Run state; stops own Run and owned descendants without affecting independent Runs. Models limited to ownership, humans may explicitly target. Check get/wait for completed resource cleanup |
| `turn get --turn TURN` | turn_id/run_id/thread_id/state/end_message_node_id/recovery_hint. Background owned work may keep the Run live after answering |

### Storage (2 operations)

Common flag: `[--category raw|managed-images|history|index|all]`. Omitted collect uses all; status provides overall information.

| Operation | Main result and conditions |
|---|---|
| `storage status` | logical_live_bytes/reclaimable_bytes/db_bytes/wal_bytes/managed_bytes/log_bytes, optional filesystem_free_bytes, database_reusable_bytes, logical category breakdown, warning/limit_reached, and related fields |
| `storage collect` | raw/managed/index/summary counts/bytes, nodes_tombstoned/history_bytes_collected/deferred; pins/protected resources deferred; category narrows GC |

Logical usage, reclaimable bytes, DB/WAL physical size, and SQLite reusable pages differ. GC need not shrink files by the same amount; no automatic VACUUM. History GC targets unreachable portions after explicit deletion, never surviving conversations by age alone. See [recovery and storage](recovery-and-storage.md).

## Cursors

List/get/read cursors are opaque continuation tokens. Pass the exact returned string to the same operation, target, filters, and encoding; never construct, edit, or reuse elsewhere. A list without next_cursor ends that enumeration. Shell read returns an observation cursor for subsequent reads until output_complete.

Changed history path/revision or target can cause `cursor_expired`; encoding mismatch yields `cursor_encoding_mismatch`. Restart from the beginning. Shell cursors bind execution, raw retention generation, sequence/byte position, and decoder state. Reclaimed raw yields `output_expired`, not empty output. Explicitly changing read encoding invalidates the old cursor. `decode_errors=true` indicates invalid bytes during conversion; consider rereading raw with the correct encoding.

## Error codes

| Code | Meaning / next action |
|---|---|
| `invalid_argument` | Correct flags/JSON/values/explicit scope against help |
| `config_invalid` | Check connection/model/credential environment/settings; restart owner as needed |
| `app_not_running` | Check owner for the same data directory; CLI never autostarts it |
| `protocol_mismatch` | Align owner and CLI versions |
| `thread_not_found` | Check ID/deletion state |
| `target_unavailable` | Missing/unavailable Run/Turn/shell; requery original state |
| `operation_conflict` | ID reused for different content; inspect original receipt |
| `operation_expired` | Mutation replay with an unsaved old-instance ID or an expired outcome, including a collected current-instance result; inspect retained operation metadata and target state before deciding on duplicate execution |
| `operation_not_found` | Receipt missing; after disconnect also inspect target state |
| `invalid_state` | Current state/ownership disallows it; inspect details/hint/get |
| `wait_cycle` | Cycle through self/ownership/other waits; split waits or report |
| `cursor_expired` | Restart paging the same target from the beginning |
| `cursor_encoding_mismatch` | Restore encoding or reread without cursor |
| `output_expired` | Raw reclaimed; use saved excerpts/history rather than interpreting as empty output |
| `input_limit` | Body/attachment/stdin/frame limit; split or revise selection |
| `storage_limit` | Logical capacity limit; inspect storage status/collect, consider unnecessary Thread deletion, explicitly retry |
| `save_failed` | Canonical persistence failed; do not assume success, inspect acceptance/operation/storage |
| `io_failed` | Communication/file/shell I/O failure; decide recovery from hint/details/target state |

Errors/timeouts never justify unconditional repetition under a new ID. Inspect saved acceptance, target termination/recovery wait, and the same operation's result.

## Implementation references

Syntax follows [cli.rs](../../src/cli.rs), public types/constants [protocol.rs](../../src/protocol.rs), JSON/exit behavior [main.rs](../../src/main.rs), and results [Control](../../src/control.rs) / [DB operations](../../src/control/db.rs). Public CLI has no standalone subcommands for TUI-only edited sending or attachment reload; index preview is available from both CLI and TUI. See [TUI guide](tui.md) for screen controls.

## Compact range selection

Argument-free `compact` follows spill then summary. `--spill` / `--summary` / `--omit` with `--from NODE --to NODE` selects an inclusive range. `--omit` works for prefix or middle ranges, replacing them with a notification without an API request. `--trim --to NODE` omits a prefix; `--restore [--from MARKER]` restores omission. Modes cannot be combined. All support `--thread THREAD` and normal operation IDs and share queued/downstream semantics. Invalid boundaries return `valid_boundaries` for inspection.

`thread get` `context_usage` returns current estimates, connection guidance, and latest successful measured usage. Edit results include `before_tokens` / `after_tokens` / `target_tokens` / `cache_invalidated_tokens`, `omitted_nodes`, `marker_node_id`, and `warnings`. Automatic maximum reduction returns `target_tokens: null`; explicit manual edits retain their target. These are not exact tokenizer values or cache-hit guarantees. See [input size and range editing](history-and-search.md#input-size-and-range-editing).

`compact --restore --from ID` accepts a selected marker or the first node in its omitted source. Use it to restore the specific range named in a notification. Argument-free restore selects the most recently created marker and may target a different range when several omissions exist.

Specify repeated source exclusions with `rag source add PATH --name NAME --exclude '*.db' --exclude build`. Reregistering without exclusions preserves them; supplied exclusions replace them; `--clear-excludes` clears them. Inspect `rag index --dry-run` `skipped` and per-source `selection`. `rag status` reports source scan/content-change times, current-profile `semantic_missing`, and bounded failures. Positively identified semantic-not-ready hybrid results return `ok:true`, `degraded:true`, and `scope_status`/`warnings`; actual errors and explicit semantic mode retain nonzero exit. See [RAG guide](rag.md) for recovery and background-index result retrieval.

### Thread list activity (Issue #44)

`thread list` preserves `state` (latest Turn/native fallback) and separately
returns snake_case `activity`, using the same saved-fact classifier as the
conversation footer. In particular, `state: "completed"` can coexist with
`activity: "background"` or `"continuing"` while the latest Run remains live.
`background` requires an active owned shell or child Run; `continuing` does
not assert such work. Exit markers and Revisit are independent of activity.

### Observing long-running shells (Issue #45)

`shell get --execution ID` and shell entries in `status` expose saved
`started_at`, nullable `last_output_at` (latest retained output sequence),
and `elapsed_ms`. While starting/running/stopping, elapsed_ms is calculated
at read time from the saved start and wall clock, not a process-liveness
check or monotonic measurement. For terminal shells it is the saved completion
duration, if known. No output or reclaimed output gives last_output_at: null.
Status timing does not affect activity_at, sorting, cursor or fingerprint.

### Instruction reference selection

`thread create`, `thread set` and `thread fork` accept `--agents PATH`. The
relative base is the creation/change target effective cwd after any `--cwd`.
Explicit selection validates a nonempty readable regular UTF-8 file <=8 MiB.
Missing is invalid_argument; access/UTF-8 errors are io_failed; oversize is
input_limit. No reference clearing is provided. Default/inherited missing files
remain allowed. `thread get` returns the absolute `agents_path`. Changes are
atomic with other settings, auditable, and apply to the next new Turn, not a
same-Turn retry. Other-Thread changes retain existing thread.set authority.

### Failure-driven context editing (#50)

`compact --maximal` selects ContextEdit action maximal with no from/to nodes;
`/compact-max` is its TUI counterpart. It calls no summary API and does not
resume a Run. Edit results replace token estimates with before_characters,
after_characters and changed_suffix_characters (replay JSON Unicode scalars).
No target_tokens field remains. thread get context_usage reports only the last
successful measured usage, start/end time and next_request_pending after an edit.

## Noninteractive work with a JSON answer

An owner must already be running. `exec` saves ordinary conversation history and
allows normal model tool use; it is not a tool-free transform or a sandbox.

```sh
shellboy exec --schema result.schema.json "Inspect the repository and report its TODO items" 2>exec.metadata.json
shellboy exec --schema result.schema.json --thread THREAD --max-repairs 0 "Report the result"
cat request.txt | shellboy exec --schema result.schema.json --text-stdin
```

Without --thread it creates a new saved conversation (caller cwd by default).
New conversations accept --cwd/--agents/--connection/--model/--effort/--parent;
these conflict with --thread. Existing Thread settings/AGENTS are not changed.
--sender-label defaults to `shellboy exec`, a self-asserted name only. Single-RPC
--operation-id is rejected; the stderr diagnostic reports known per-step IDs.

stdout is the validated raw JSON value (outer whitespace trimmed) plus newline; stderr is a JSON
metadata/error object with Thread, message, Run, Turn, answer-node, operation IDs
and repair count, plus saved Turn-scoped usage (unknown stays null). These are
owner facts, not a per-exec bill; an existing active Turn can include other input.
Exit codes are0 success,2 invalid input/unsupported Schema,
3 JSON/Schema mismatch after repairs,4 recovery/held/stopped/failed/unavailable
answer,5 timeout,6 owner/transport/protocol/output failure. Schema mismatch after the repair cap includes the raw answer and bounded
validation errors. Output-size/page limits exit6 with answer IDs for inspection,
not a silently truncated successful result. Inspect saved targets before
resending an uncertain mutation. `--timeout-ms` defaults300000 (1..86400000),
starts after input reads and covers all RPCs/repairs; timeout never stops work.
Use existing run stop explicitly if needed. The command does not start an owner.
Default --max-repairs2 (0..5) sends corrections in the same saved conversation;
ordinary model/tool/index costs and Core retry policy still apply. Work can keep
running after the client times out. Prefer a dedicated Thread for automation;
concurrent human stop/recovery/configuration edits are not an isolated job API.

This initial validator adds no crate and supports only boolean/object schemas
with `$schema` (2020-12 or draft-07 URI), title/description, single type
(object,array,string,integer,number,boolean,null), properties, required, boolean
additionalProperties, single-schema items and minItems/maxItems/minLength/maxLength.
Counts must be integer literals0..1000000; string length counts Unicode scalars.
Integer checks use exact decimal lexemes, not f64 rounding; exponent forms and
large integer literals keep their spelling.
All unlisted keywords/shapes reject before owner contact, including $ref/$defs,
oneOf/anyOf/allOf, enum/const, pattern/format, numeric bounds, union types, tuple
items and schema-valued additionalProperties. Do not assume full draft support.
Schema64KiB/depth32/nodes2048, composed request128KiB, answer1MiB and100000
validation steps are bounded; excess is explicit failure, not accepted truncation.

To inspect exactly what was validated, use `history get --node END_MESSAGE_NODE_ID
--end-message-only`. Normal history reads still include the original natural
text/reasoning. End-message-only cursors cannot be reused for the ordinary projection.
Use an owner matching this CLI version for that additive read option.

PowerShell 7 example (stdout remains separate from diagnostic stderr):

```powershell
$OutputEncoding = [System.Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = $OutputEncoding
$json = & shellboy exec --schema result.schema.json 'Inspect the repository and report its TODO items' 2>exec.metadata.json
if ($LASTEXITCODE -ne 0) { throw "shellboy exec failed: $LASTEXITCODE" }
$result = $json | ConvertFrom-Json
```


Do not synchronously exec against your own currently blocked foreground model
Turn: existing wait-cycle protection can reject that wait after message acceptance.
Inspect the reported message/operation IDs rather than resending. Prefer the new
saved Thread default or a separate conversation. A timeout is not cancellation.

Completion means a saved end message for the assigned Turn, not proof that all
background shells/child Runs have drained. Inspect run get/status for continuing
work. Schema validation guarantees supported shape only, not factual correctness.


JSON instance indexing is limited to100000 nodes/depth128. Numeric parsing uses
the existing serde_json supported range; out-of-range numeric inputs fail explicitly.


### exec owner-version guard

Before Thread creation/message send, exec makes a read-only end_message_only
probe with a random missing node. A current owner decodes the argument and reports
target_unavailable; old-style argument rejection exits6 before work. A refusal
at answer read also exits6 (owner_version_mismatch), retaining known answer IDs
and recommending ordinary history get --node END_MESSAGE_NODE_ID without the new flag.
Do not resend completed work as if this were an input/schema error. Closed-wire
support failures are owner/capability errors6, not input2. This is detection only,
not an old-Core format shim or automatic answer/protocol downgrade.


`index status` and `config check` include `auto_history` (default true). Setting
`indexing.auto_history=false` only disables automatic conversation embedding;
it does not remove existing vectors or disable standalone RAG. Semantic history
RAG reports disabled coverage in its existing degraded/scope_status/warnings.

### Bounded explicit summaries (#73)

`compact --summary --from FIRST --to LAST [--thread ID]` also accepts
`--max-chars N` and `--max-percent P` (summary mode only). Each overrides its
corresponding `[compaction]` setting; omitted overrides preserve the configuration.
Defaults are the smaller of 20,000 characters and 5% of original projected input,
floored at 2,000. Explicit summaries consolidate parts into one bounded text,
with independent map/reduce budgets (default 128/16) and four reduce rounds,
or fail without adoption. A map budget overrun known from uncached segment count
is rejected before any model request. Changing a size override reuses map cache.
The same limits apply to automatic stage 2; failure follows normal recovery wait.
The downstream `result.compact` adds `source_characters`, `summary_characters`,
`summary_limit` and `model_requests`; size/budget failure details expose the same
facts. Read [configuration](configuration.md#explicit-summary-size-limits) for
single-limit settings, retry caching, and omitted-history caveats.


`index status` reports `held_auto_history` separately from `pending`: automatic
pending source authorizations/bootstrap/segment jobs held while auto_history is
false. Explicit pending work, running/failed/done and space selection retain
their existing meaning. Re-enable or explicitly reauthorize to release applicable
held entries. No saved job state is changed by this status projection.


## Saved compaction and shell observations

These commands require matching v0.5.0 owner and CLI; v0.4.0 alone does not
provide them. They do not start/stop work, read shell output, call a
provider or allocate operation receipts.

```sh
shellboy compact list --thread THREAD_ID --limit 20
shellboy compact list --thread THREAD_ID --since 2026-10-31T00:00:00Z --until 2026-11-01T00:00:00Z
shellboy usage get --thread THREAD_ID --since 2026-10-31T00:00:00Z --until 2026-11-01T00:00:00Z
shellboy shell list --command-contains "rag search" --state exited --exit-code 0 --limit 50
shellboy shell list --thread THREAD_ID --cursor CURSOR
```

Replace example IDs/times. Times are RFC3339 with an explicit offset and four-digit
year. Intervals include since and exclude until; either bound may be omitted.
Usage selects model requests and shells by saved start, Runs by saved creation,
and a Turn's own lifetime by its creation. Requests and Runs crossing a boundary
belong wholly to their start side, without splitting. Selected durations are whole, not
prorated overlap. Unknown start times are excluded only when time filtering; saved
usage states/unknowns/overflow still apply. Totals are not invoices or prices.

Compaction observations merge known operation results, automatic records
and retained replacements, newest first. Automatic records carry `reason`:
`failure` (failure-driven reduction) or `proactive_threshold` (#125, with
`trigger.auto_at_tokens` and the reported `trigger.input_tokens`). Replacement creation is the timestamp
when available; otherwise operation completion or attempt end/start is identified
by timestamp_basis. Each summarizing event has `metrics.summary_usage`:
`requests`, `map_requests`, `reduce_requests` and `input_tokens`,
`cached_tokens`, `cache_creation_tokens`, `output_tokens` as
`{known_sum, unknown_count, overflow}` (provider-reported only; `requests: 0`
when every part came from the cache; null for older events). Restore is a distinct operation, not its old omission. A
coalesced shared replacement has one representative operation ID and the saved
operation count. Missing legacy counts/sizes remain null; after receipts expire,
restores and operation associations may be unavailable. Coverage indicates the
bounded source-candidate limit; narrow the time range if it was reached. thread get
includes its latest retained observation, including failed/no-target observations
with their state, rather than claiming every observation changed input.

Shell list defaults to all saved conversations; --thread restricts ownership.
Filters are literal/case-sensitive command substring, native state and signed
exit code. Pages return at most 200 rows and inspect at most 2,000 candidates;
filtered pages can be empty with next_cursor. Follow that cursor until absent,
keeping filters unchanged (page size may change). Cursors bind the owner instance,
normalized filters and insertion watermark. Each page sees current metadata;
restart after owner/database replacement or to see new work. This is not a frozen
audit snapshot under concurrent state changes, deletions or SQLite rowid reuse.

Only IDs, times, state, exit code and a 512-character command excerpt are returned.
Controls/bidi formatting are removed; obvious credential shapes suppress the
whole snippet, with a redaction flag. This conservative convenience cannot detect
arbitrary obfuscated secrets: keep results private. No stdout/stderr/body is
returned; explicitly use shell get/read for the chosen execution ID and raw output.
All IDs/metrics describe retained facts; collected history cannot be reconstructed.

## Owner memory

`status --memory` asks the running owner for JSON cache accounting and native
process memory. It is owner-wide and cannot be combined with activity filters.
`caches` contains body, layout, attachment and replay retained bytes, limit bytes
and entry counts/units. These reuse existing budget accounting, not a new heap
measurement. Body working-set exclusions and pinned/live shared allocations mean
cache bytes are not a complete partition of process memory. Counts are views for
body, layout-or-unit entries for layout, paths for attachments and Threads for
replay. The existing cache limits do not change.

`process.metrics` reports byte values with an unavailable_reason when unknown:
macOS footprint/RSS, Linux RSS/PSS and Windows working set/private bytes. Missing
UI owners yield null body/layout values, not invented zeros. Snapshots are taken
on request, are not atomic across owners, and must not be added to estimate total
heap. SQLite/allocator/other untracked allocations are not estimated. No periodic
sampling/logging, cache purge or provider request is performed. F8 display remains
separate #41 work.

### Thread wait continuation and latest messages (#74)

`wait --conditions-json JSON [--after CURSOR] [--timeout-ms N]` accepts
`{"type":"thread_settled","thread_ids":["A","B"],"states":["completed","recovery_wait","failed","stopped"]}`.
The nonempty states filter is explicit; omit unwanted states. At most64 Thread
IDs total (each at most128 bytes) and four distinct states are accepted; repeated
IDs within a condition are errors. Other existing wait conditions can coexist.
The result's `snapshot.changes` contains bounded Thread/Run/state/at and optional
Turn identities; `at=null` means not recorded, never a guessed observation time.
`cursor` is the Thread continuation cursor when these conditions are present,
otherwise the existing shell-output cursor. `snapshot.more` announces another
saved matching change. Keep the cursor on timeout too and pass it to the next
wait with unchanged conditions. It is independent of owner instance. Cursor
anchors invalidated by data removal require a fresh observation; external SQL
editing/VACUUM is not a supported continuation path. Self/owning-ancestor/composed
model waits remain cycle checked. Human wait observation does not scan the model
ownership graph. No-after reports current settled states, not arbitrary history.

`thread list/get` keep `excerpt` and add `last_end_message` and `last_input` objects
(or null when absent). The answer includes `node_id`, `turn_id`, `at`, `excerpt`
and `body_available`; the input includes `message_id`, `node_id`, `at`, `state`,
`sender`, `excerpt`, `body_available`. Excerpts include at most512 characters
from at most64 natural-text fragments, excluding tools/reasoning/opaque bodies.
These are the Thread's own saved facts, not expansion of fork-only ancestors.
Existing list paging/limits remain unchanged. Held/cancelled input is still a
received fact, not proof that the model consumed it; sender labels do not grant
permissions. Reclaimed bodies retain identity/time with an empty excerpt.

### Known usage-limit resets (#76)

When a response explicitly reports `usage_limit_reached` with a valid future
`resets_at` (Unix seconds) or `resets_in_seconds`, the Turn enters recovery wait
without automatic resend. The same applies to HTTP 429 with a valid Retry-After
above 600 seconds. Generic 429 without these facts retains ordinary retry rules.
OpenAI-shaped error.type/code, one JSON error.message wrapper and LiteLLM's
`litellm.RateLimitError` plus complete JSON suffix are supported; arbitrary prose
is not interpreted as quota evidence.

`run get --run ID` exposes `context.recovery_reason.details.usage_limit`: kind,
`resets_at` epoch seconds, `resets_at_utc`, optional `limit_window_minutes`, and
`locally_blocked`. The existing reason string also shows the UTC reset. Wait
until that time, or use `/model` to choose another connection/model. There is
no automatic switch or restart at reset. Changing only the model on the same
connection does not bypass the connection-wide cooldown.

The owner remembers at most one fixed-size reset fact per configured connection
in memory. Other conversations on that connection fail locally without POST
until expiry; their saved failed attempt is marked locally_blocked and has no
HTTP observation. Owner restart forgets the memory; already-in-flight requests
are not cancelled. Input preparation is not skipped. Embeddings and model-list
requests are outside this first version. Short bottom-line TUI rendering is
tracked separately in #85; use run get/the existing recovery reason for now.

## Terminal-free owner startup (#5, v0.5.0)

`shellboy serve --data-dir DIR [--config FILE]` starts an owner without a terminal.
The data directory must be explicit and is protected by the normal owner lock;
an already-owned directory is an error, not an attach request. Configuration
lookup is the same as TUI startup. The process stays part of its caller's shell;
it does not daemonize and stdin EOF does not stop it.

Wait for the flushed stdout JSON row with `event: "ready"` before sending CLI
requests to that explicit data directory. It reports the absolute data path,
owner instance and protocol version, not a model connection check. Startup
failures are JSON on stderr, never a successful stdout row. A data directory
that cannot be represented in UTF-8 readiness JSON is rejected with cleanup.
Inherited owner socket/origin/data variables do not select this owner. Shells
started by it receive its own identity; an inapplicable inherited Unix socket
is cleared on Windows. API-key and normal proxy variables are not cleared.

Use `shellboy shutdown --data-dir DIR` to request cleanup of that serve owner.
The reply acknowledges saved acceptance, not successful termination. Observe the
owner process and its final stdout JSON (`event: "stopped"`, `ok` and `state`).
A normal lifecycle emits ready plus stopped, each flushed; failures after ready
use the final JSON and exit 1, not startup stderr. Missing final output after a
force-kill or closed pipe is not success. Unix SIGTERM/SIGHUP use the same cleanup;
Windows CTRL_BREAK is not a shutdown interface. No duration guarantee is made.

The directory is required for shutdown too; inherited endpoint variables cannot
select another owner. Requests are pinned to the observed instance, and a stale
instance cannot stop a restarted owner. TUI owners reject this command. Existing
local IPC/OS access is the authority, not sender-label or a human approval claim.
Owner replacement does not transfer work or reconnect a UI. Native ownership and
failure-boundary qualification is tracked in issue #5; use disposable directories
for experiments.

### Terminal-free ownership and interruption

On Unix, foreground Ctrl+C (SIGINT), TERM and HUP request the same cleanup.
Repeated TERM during drain is not an escalation: use external SIGKILL if forced
termination is necessary. Windows uses `shutdown --data-dir DIR`, not CTRL_BREAK.
Neither path promises completion within the parent's termination grace. A missing
final row can mean forced termination; reopen the data and inspect saved state.
Never assume a shell whose effect may have happened is safe to execute again.

A serve owner started by a model's background shell stays owned by that shell
and Run. Stop the child explicitly when finished; otherwise its parent remains
live. Parent shell/Run/owner termination also ends the nested owner and its tools
through the existing Unix lifeline / Windows Job chain. No daemonization or
breakaway is added. Restart does not automatically resume interrupted work.

Use `status --thread THREAD_ID` and `run get --run RUN_ID` to inspect recovery,
then explicitly choose `run retry --run RUN_ID`, new input, or `run stop --run RUN_ID`.
Read the saved error before retrying; generation/embedding requests have normal
costs. TUI drafts, submitted-draft restore, Turn interruption and TUI slash-only
operations have no headless UI. Existing CLI compact operations remain available.
A TUI cannot be attached later, and owner replacement does not transfer live work.

### Bounded usage breakdowns (#101)

```sh
shellboy usage get --group-by thread --since 2026-10-31T06:00:00Z --until 2026-10-31T12:00:00Z
shellboy usage get --group-by hour --timezone +09:00 --since 2026-10-31T00:00:00Z --until 2026-11-01T00:00:00Z
shellboy usage get --group-by day --timezone UTC --limit 24
```

Grouped reads without an explicit scope cover non-deleted conversations, so the
first example can identify conversations with mean input above 300,000 tokens.
An explicit `--thread`, `--run`, or `--turn` narrows that selection; children
require an explicit Thread. Ungrouped `usage get` keeps its existing behavior.

Each item has a group `key`, saved per-state `model_attempts` sums with known/
unknown/overflow fields, `input_per_request` (mean/max/nearest-rank p50/p95,
known/unknown counts, `max_attempt` with attempt/Turn/Run/Thread references), and
`cache` (paired cached/input ratio and coverage counts). Inspect a max request
with `turn get --turn TURN_ID`, then use history list/get for that Turn's saved
nodes; an attempt ID is not a history node ID. Input statistics use terminal
completed/failed/incomplete attempts. Missing usage is not zero; a zero cache
ratio denominator is null. Provider inconsistencies are not clamped or repaired.

The default period is the last 24 hours; the maximum is 31 days. Time buckets use
owner `local` time by default, or `UTC`/an explicit fixed offset (`+HH:MM` or
`-HH:MM`; use `--timezone=-05:00` for a leading minus). IANA zone names are not
accepted. Repeated local DST hours have distinct offset-bearing keys.

At most 10,000 candidate attempts plus one lookahead are read per scan window,
before time/scope/deletion filtering; at most 168 groups are returned (`--limit` lowers
that). Repeat the same command with `--cursor NEXT_CURSOR` while `more` is true,
even when a filtered page is empty. A cursor pages the current window's groups,
then continues remaining candidates. `scan_truncated` means statistics cover
only the current candidate window; a group can recur in another window. Do not
average page percentiles or cache ratios into whole-period estimates. Cursors
bind filters/limit/owner instance and exclude later inserts, but separate calls
are not one persistent snapshot. Current saved UTC `+00:00` timestamps are
selected by string range; missing, malformed and non-current timestamp formats
are excluded and counted in `excluded_time_count` for each scan window. Valid
UTC timestamps outside the period appear in `outside_period_count`.
`elapsed_ms` is an observation, not a pass/fail
threshold. Summary/embedding usage and monetary cost are not inferred.

## Compaction forecast (`compact --dry-run`, v0.6.0)

Use a matching current owner and CLI; published v0.5.0 does not expose this
additive command. Preview before changing a valuable conversation:

```sh
shellboy compact --dry-run --thread THREAD
shellboy compact --summary --from FIRST --to LAST --dry-run --thread THREAD
shellboy compact --maximal --dry-run --thread THREAD
```

`--dry-run` also supports `--spill`, `--trim` and `--omit` with their ordinary
whole-group endpoints and summary size overrides. It cannot be mixed with list
or restore. It reads one query-only snapshot on the existing presentation pool:
no model/embedding requests, mutation receipts, queue admission, saved markers,
spills, summary parts or path changes. Actual compact remains a separate request.

Inspect `predicted_failure` even when the CLI returns `ok:true`. It contains a
canonical code/message/hint for an anticipated boundary, protection, stopping,
indivisible-group or summary-map-budget refusal. A coverage limit instead returns
`input_limit` with `coverage_complete:false`; it is not evidence that compaction
would fail. A preview never reserves selection or permits adopting stale results.

`target_nodes` reports distinct affected original IDs by saved node kind (a bounded
sample, not a body dump). `source_nodes` counts stage inputs, including virtual
spill replacements consumed by default summary, matching compact's combined
accounting. Character counts are replay JSON Unicode characters, not tokenizer,
SQLite-byte, full fixed-prompt or wrapped-terminal-line counts.
`removed_original_messages` counts replay user/assistant message records.
Deterministic after-size is exact within coverage; summary `after_characters`
is null because no text is generated. `summary` reports source characters,
configured text bound and valid cached/uncached segment counts, not factual
quality, final text length or provider acceptance. Default mode plans virtual
spill then summary; real execution can save spill before a later summary fails.

Collection is limited to 4096 combined selected/hidden nodes, 512 Turns, 4096
receipt/tool/attachment/ancestry rows per family, 16 ancestry layers, 1MiB per
payload/cache entry and 8MiB copied input text. At most 28 indexed/batched SQL
statements are used. An oversized scope stays unknown rather than becoming an
empty successful forecast. Large context-marker provenance uses the canonical
short outbound notice; genuinely oversized visible groups remain refusals.

### Inspect One Attempt's Usage And Cache Diagnostics

```bash
shellboy usage get --attempt ATTEMPT_ID
shellboy thread request --thread THREAD_ID --attempt ATTEMPT_ID --body
```

The first command selects one saved model attempt instead of an aggregate
scope. Do not combine `--attempt` with Thread/Run/Turn, children, time-window,
grouping or pagination options. It returns IDs/state, standard usage scalars and
`prompt_cache_diagnostics` (bounded reported object, or JSON null). Aggregate
usage queries do not add a growing list of diagnostic bodies.

Cache diagnosis is opt-in in the Responses connection; see
[configuration](configuration.md#optional-responses-cache-diagnostics). The
second command is read-only/offline, shows the saved diagnostic observation and
reconstructs the admitted comparison identity, not a newer response's ID. An
unknown historical request witness or changed diagnosis setting is reported as
unknown rather than silently guessed. Neither command verifies provider cache
effectiveness.

## Explicit Reply Core (#194)

`message send --expect-reply` records a reply duty on the accepted message;
there can be at most 128 pending duties per recipient Thread. A recipient model
(or human) replies with `message reply --message MESSAGE_ID --text TEXT` or
`--text-stdin`. It must be applied and still pending. `message get` exposes
`expect_reply`, `reply` (state and optional timestamp, node, sender), or
`reply_to` for a delivered reply; it no longer borrows a Turn's end message.
Use `history get --node REPLY_NODE_ID` for the body.

`wait reply_received` uses a JSON condition
`{"type":"reply_received","message_id":"MESSAGE_ID"}`. Inspect the snapshot:
`satisfied` can mean replied, ignored, cancelled, or recipient deleted. Timeout
does not cancel the duty. `thread get` includes `awaiting_reply` (up to 128,
oldest first). Cancelling a queued request cancels both input and duty;
cancelling an admitted duty returns `reply_cancelled`, retains history, and
notifies the recipient without waking it.
