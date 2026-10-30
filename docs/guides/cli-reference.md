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

Global flags may also follow subcommands. `--data-dir` selects owner storage, not work cwd; `--config` selects TUI startup settings or standalone RAG embedding connection settings. CLI connection resolution is `--data-dir` > `SHELLBOY_DATA_DIR` > OS default; it does not read TOML. See [OS paths](configuration.md#locations-and-precedence).

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

`message send` `delivery_status=accepted|held|applied|cancelled` describes acceptance/application. `assignment=new_run|new_turn|existing_turn` includes both run_id and turn_id; saved assignment never changes. `assignment=pending` has neither; query `message get` later for initial assignment. Successful send does not mean an answer is complete. Use `answer_node_id` for source retrieval when available.

### Saved execution context

`run get`, `turn get`, `message get`, and `history get` require `context`. `thread get` can omit it for a fork. `history get` requires `provenance`; every `history list` item requires `provenance`, `context`, and `selection`. It includes meaningful state, saved created_at/updated_at/ended_at, thread_id/run_id/turn_id/node_id, Thread parent_thread_id versus Run owner_run_id, and structured recovery_reason. Unrecorded started_at and Thread created_at are omitted. Run/Turn `lifetime_ms` is wall-clock creation-to-end time including preparation/waits and is absent before termination. A node's Thread is its origin, not a fork's selected path.

`operation get` `state/outcome` is the saved acceptance-operation result. `replay_policy=return_saved_outcome` returns the original result; `do_not_reexecute` means pending effects are not restarted. `result_uncertain=true` is not evidence of nonexecution. `targets` includes only IDs in the saved result; `downstream` shows compaction processing or current message delivery. Otherwise follow targets to get commands; inspect indexing per space with `index status`. The query itself creates no receipt and rereads current values under one query ID. `result_expired=true` / `replay_policy=operation_expired` means the original outcome body was collected and that mutation ID will not execute again.

## Operation dictionary

Brackets mean optional; `|` means alternatives. Add common global flags and `--help` to the operation-specific flags listed here.

### Work status (1 operation)

| Operation and all specific flags | Main result and conditions |
|---|---|
| `status [--thread THREAD] [--run RUN] [--since RFC3339] [--state STATE]... [--all] [--limit N] [--cursor C]` | `items`, `filters`, `observed_at`, `scope`, `consistency`, `fingerprint`, `next_cursor`; default 50, maximum 200. Saved state from one DB read transaction; no new execution, API request, or operation receipt |

### Saved usage (1 operation)

| Operation and all specific flags | Main result and conditions |
|---|---|
| `usage get [--thread THREAD] [--run RUN] [--turn TURN] [--include-children]` | All-time saved model usage and separate Run/model/shell durations. At most one scope ID. Default is the authenticated caller's Thread; external callers supply an ID. Children apply only to Thread scope. No provider call, receipt or period flags. |

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

`thread list` / `thread get` fill empty saved `title` from the first user body, preferring the selected path then accepted/held messages in the same Thread if no body exists. `thread list --query` can match that display name. Stored title/history are unchanged; no model API naming occurs.

Set with `thread set --thread THREAD --title 'Investigation and fixes'`. `--title ''` or whitespace-only clears to conversation-derived display; omission preserves saved title. Trimmed titles have at most 512 characters and no newlines/control characters. Title-only updates do not change model, effort, or reasoning epoch. External CLI requires `--thread`; omission from an agent targets itself.

Root cwd defaults to owner startup cwd; child cwd to the parent's. Relative cwd resolves against the creation/change target's base. Changing cwd does not change an existing AGENTS reference. See [configuration](configuration.md#startup-settings-and-thread-settings).

### Message / Operation (4 operations)

| Operation and all specific flags | Main result and conditions |
|---|---|
| `message send [--thread THREAD] [--now] (--text TEXT | --text-stdin)` | message_id/thread_id/delivery_status/assignment/run_id/turn_id; acceptance differs from completion. Active work takes additions at safe boundaries; stopping can yield held/pending. Ordinary machine messages never silently resume recovery_wait |
| `message get --message MESSAGE` | Original immutable assignment, current delivery_status, and answer_node_id when available |
| `message cancel --message MESSAGE [--message MESSAGE ...]` | Cancel 1..128 fixed queued message IDs. Each item reports cancelled/already_cancelled/already_admitted/not_found/forbidden/unavailable and an archive_draft_id when available; admitted history cannot be cancelled. Does not wake the model |
| `operation get --operation ID` | operation_id/state/outcome, instance_id/created_at/current_instance, result_uncertain/result_expired/replay_policy/targets, target-dependent downstream. Global `--operation-id` identifies this query; `--operation` identifies its target |

Ordinary sends queue for the next safe boundary. Add `--now` only to request
urgent delivery by releasing a foreground shell wait: that shell continues under
its execution ID, and an in-progress model response is not interrupted.
Acceptance still does not mean delivery or completion.

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
| `run get --run RUN` | run_id/thread_id/state/owner_run_id/turn_id/answer_node_id, owned_run_ids/owned_execution_ids, recovery_hint. Owned IDs reference current live resources, not a ledger of all historical children |
| `run retry --run RUN` | Recovery Run/Turn references; explicitly reprepare a recovery_wait Turn in a live Run, reusing the Turn/original message without blindly rerunning executed shells. Human or direct parent decides |
| `run stop --run RUN` | Confirmed stopping/Run state; stops own Run and owned descendants without affecting independent Runs. Models limited to ownership, humans may explicitly target. Check get/wait for completed resource cleanup |
| `turn get --turn TURN` | turn_id/run_id/thread_id/state/answer_node_id/recovery_hint. Background owned work may keep the Run live after answering |

### Revisit (3 operations)

| Operation and all specific flags | Main result and conditions |
|---|---|
| `revisit set [--run RUN] (--text TEXT \| --text-stdin)` | Replace one pending completion instruction, 1..8192 UTF-8 bytes, nonblank. Returns pending/setter/count/time, `replaced_other` and `previous_text_preview` |
| `revisit get [--run RUN]` | Read pending instruction/setter, consumed_count, last_fired_at and optional limit; no operation receipt |
| `revisit cancel [--run RUN]` | Remove pending instruction; returns `cancelled`, false when nothing was pending. Does not stop the Run |

Humans must supply `--run`. Authenticated models may omit it for their own live
Run or name a directly owned child's live Run; unrelated Runs and non-direct
descendants are denied. All operations reject non-live Runs. Revisit is delivered
as a developer event at normal Run completion, after owned shells and children
finish; it never fires during recovery wait, stop or restart. One reservation is
consumed once, while ordinary request/recovery and external-effect semantics
remain unchanged. Re-registration is unlimited unless the owner configures
`limits.revisit_max_per_run`. Cancel or rewrite before ending with a question
that requires the user. Re-query `get` after uncertain mutations, using the same
operation ID to replay the saved outcome.

### Wait (1 operation)

```text
wait --conditions-json JSON [--timeout-ms MS]
```

Conditions are a typed array with **any** semantics (at least one satisfied). Default timeout 30000ms; 0 observes immediately. Empty arrays are invalid. Satisfaction, timeout, observer disconnect, or cancellation never stops other target work.

Bash:

```bash
shellboy wait --conditions-json '[{"type":"turn_completed","turn_id":"TURN"},{"type":"run_ended","run_id":"RUN"}]' --timeout-ms 30000
shellboy wait --conditions-json '[{"type":"shell_ended","execution_id":"EXECUTION"}]'
shellboy wait --conditions-json '[{"type":"shell_output","execution_id":"EXECUTION","cursor":"CURSOR_FROM_SHELL_READ"}]'
```

PowerShell native JSON argument passing differs from Bash. Reading a UTF-8 file into a string alone may not fix quoting. See [PowerShell arguments](shell.md#powershell-arguments) and match your version's argument mode.

`turn_completed` observes an answer unit; `run_ended` the lifetime including owned resources; `shell_ended` shell termination; `shell_output` output after a shell-read cursor. Cyclic self/ancestor waits yield `wait_cycle`. `result` contains `status`, satisfied conditions `matched`, target-state `snapshot`, and optional cursor/reason/hint. Status is `satisfied|timeout|interrupted|recovery_wait|unavailable`. Even satisfied can mean stopped/failed/nonzero exit; inspect snapshots, then run/turn/message get and history get as appropriate.

### History (4 operations)

| Operation and all specific flags | Main result and conditions |
|---|---|
| `history list --thread THREAD [--excerpt-chars N] [--run RUN] [--since RFC3339] [--status STATE] [--limit N] [--cursor C]` | `items` (node_id/kind/excerpt/payload_chars/provenance/context/selection/executions), next_cursor. Status filters saved related-shell state. Selected path, default 50/max 200. excerpt-chars default 512/range 0–4096; zero retrieves no body and returns empty excerpt. payload_chars counts stored JSON characters, null if reclaimed. Up to eight execution links with total/truncation |
| `history get --node NODE [--limit-bytes N] [--cursor C] [--encoding UTF-8]` | node_id/kind/text/provenance/context, source_node_ids/previous_node_ids/next_node_ids, next_cursor, tombstone_reason; display text excludes encrypted reasoning |
| `history search --query TEXT [--limit N] [--thread THREAD] [--kind KIND]... [--exclude-kind KIND]... [--from RFC3339] [--to RFC3339]` | Array of node_id/excerpt/distance/spans/provenance and optional thread_scope; default 10/max 50. Filters before active-embedding-space ranking; no active space yields empty array |
| `history relations --node NODE --relation previous\|next\|sources\|replacements [--limit N] [--cursor C]` | `items` (node_id/provenance and, for replacements, replacement_id/source_revision/source_hash), next_cursor; direct relations, default 50/max 200 |

Without `--thread`, search is global and never implicitly fills in the model's Thread. With it, eligible nodes are the Thread's current selected path plus recursively followed completed summary/spill replacement sources, not every branch of a fork origin. A global hit need not be on the currently selected path. `--kind` values combine with OR; Thread/time/exclusion combine with AND. Kinds identify nodes (`user` / `assistant` / `tool_result`); exclude tools with `--exclude-kind tool_result`. Include/exclude lists each allow at most 32 kinds, each at most 64 characters. `--from` inclusive and `--to` exclusive filter node creation time using offset-bearing RFC3339; together require from < to.

`provenance` includes kind/created_at/source_revision and only recorded turn_id/run_id/origin_thread_id associations. Origin means creation, not every sharing fork. `spans` contains source_revision/content_hash/body_kind/start/end; start/end are half-open Unicode scalar offsets in extracted text. These are selected semantic index-chunk spans, not keyword matches. Distance is L2; no threshold option exists. Chunks group per node; different nodes with identical content remain distinct. Only `--thread` adds thread_scope (thread_id/on_current_path): true on the current adopted path, false if eligible solely as a completed-replacement source. Global results omit it rather than inferring membership.

Relations previous/next are conversation edges; sources goes from replacement output to source; replacements from source to output. The latter two return only completed replacements. Relation-item source_revision/source_hash identify source at replacement time, while provenance.source_revision is the returned node's current revision. No bulk ancestor/descendant traversal or history editing occurs. Pass next_cursor to the same node/relation, never another target. Inspect adopted paths with `history list --thread`.

Get defaults/maxes to 65536 bytes, minimum 1, paginating without splitting UTF-8 characters. Follow neighboring branches and summary-source references with get; list does not enumerate the whole DAG.

### Shell (5 operations)

Shell startup uses the model shell tool; there is no CLI `shell start`.

| Operation and all specific flags | Main result and conditions |
|---|---|
| `shell get --execution EXECUTION` | execution_id/run_id/state/output_complete/exit_code/reason, saved command/cwd/thread_id/turn_id |
| `shell read --execution EXECUTION [--cursor C] [--encoding NAME] [--limit-bytes N]` | execution_id/text/encoding/cursor/raw_bytes/output_complete/decode_errors; default/max 65536 bytes, minimum 1; omitted encoding uses the saved execution encoding |
| `shell write --execution EXECUTION (--text TEXT | --text-stdin) [--encoding NAME]` | bytes_written/status/stdin_open/reason; status `complete|timeout|closed|stopped|failed`; bytes_written is actual bytes, including partial writes. Encoding defaults UTF-8. Only live shells started with stdin_open; stdin/write-deadline limits apply |
| `shell eof --execution EXECUTION` | accepted and related fields; closes stdin, separate from stop; shell can continue afterward |
| `shell stop --execution EXECUTION` | accepted and related fields; starts stop, while termination/pipe drain are observed separately. Self-stop gives the CLI reply bounded grace |

Write/EOF/stop validate model Run ownership and reject side effects on independent Runs. Read/get remain queries. Shell termination differs from complete raw collection; check output_complete for the full tail. See [shell guide](shell.md).

### Attachment (3 operations plus alias)

| Operation and all specific flags | Main result and conditions |
|---|---|
| `attachment add [--thread THREAD] --path PATH --kind image\|file` | **Array** of attachment details (attachment_id/source_node_id/delivery_status/thread_id/path/kind/selected/managed/unavailable_reason). Even one addition uses `result[0].attachment_id`. Selects external path references, read at model-request time |
| `attachment list [--thread THREAD]` | Attachment-detail array; inspect saved selection IDs |
| `attachment remove [--thread THREAD] --attachment ATTACHMENT` | attachment_id/removed; deselects without deleting external files |

`shellboy attach PATH --kind image|file [--thread THREAD]` aliases add with positional path; kind remains required. Relative CLI attachment paths become absolute against CLI startup cwd. See [attachments](attachments.md) for image/PDF/file support, model capability, changes/missing files, and compaction exclusion. Successful add does not mean API delivery completed.

Add/list elements return `source_node_id` and `delivery_status` (accepted/held/applied/cancelled). A separate attachment-add node is saved; shell-origin additions enter the path at a safe boundary after the complete tool group. Add acceptance JSON is neither a human-message receipt nor proof the model read it. Resending unfolds at the original position instead of moving attachments to the latest message. Same-ID/same-request replay checks saved outcomes before current Thread/file preflight, returning the original result even if the Thread was deleted after acceptance. Initial request checks and caller authentication still apply. See [attachment guide](attachments.md).

### Compact (1 operation)

`compact [--thread THREAD]` saves compaction for the next safe boundary and returns `thread_id/status=queued/operation_id`. Normally it includes model summarization and API cost. Queued is not adopted completion. Same-ID replay returns original queued acceptance even after processing. Inspect `operation get --operation ID` `downstream.state` (queued/running/completed/failed), terminal outcome/ended_at. Original nodes are not overwritten: a replacement path retains source references. See [history and search](history-and-search.md).

### Index (6 operations)

Status/preview/backfill/retry/rebuild support `[--scope threads|all] [--space SPACE] [--thread THREAD ...] [--source NODE ...]`; repeat thread/source flags.

| Operation | Main result and conditions |
|---|---|
| `index status` | Work counts and saved pause reason/time; bounded spaces with connection/model/dimensions/segment count/vector payload bytes, plus recorded probes; no automatic backfill |
| `index preview` | sources/segments/characters/already_indexed/unavailable; read-only candidates, no embedding requests/jobs/candidate creation |
| `index backfill` | sources/segments/characters/already_indexed/unavailable; accepts explicit selection into index jobs, with API costs separate from conversation |
| `index retry` | accepted; explicit retry of failed jobs/pause, not a new all-history backfill |
| `index rebuild [--dimensions N]` | Same acceptance-size shape as backfill; first real input validates an explicit dimension, preserving the active space until complete catch-up; refusal stops the candidate |
| `index probe [--dimensions N]` | One potentially charged fixed-input request; saves outcome/dimension per endpoint/model/request mode; no automatic fallback |

Backfill/rebuild require **explicit targets** via scope, thread, or source. `--scope threads` alone adds no IDs: also specify Thread IDs. `--scope all` strongly authorizes all targets. `--space` alone is not an explicit selection. Omitted space means current configuration's space. Unsupported nonactive-space changes or inconsistent combinations fail; successful status does not authorize arbitrary space mutation. Without a source selection, preview reports currently authorized retry-eligible work (failed admissions count as unavailable when their input size is unknown). It never authorizes new history. Dimensions are 1..8192 and are accepted only by rebuild/probe, not every endpoint. A probe does not authorize a rebuild. Returned characters are not a monetary estimate. First preview the same selection, check scope and your contracted unit price, then execute. Preview reserves nothing; later additions/deletion/configuration can change work volume. The CLI is noninteractive and backfill/rebuild do not pause for confirmation.

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
| `revisit_limit_reached` | Configured per-Run consumption cap reached; inspect `revisit get` and owner configuration |
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
