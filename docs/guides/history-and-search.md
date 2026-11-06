# History, forks, and search

[User Guide](user-guide.md) · [TUI controls](tui.md) · [Guide index](README.md)

Original conversation content is stored as nodes; each Thread has an adopted path. Forks, summaries, and spills do not overwrite original nodes. They preserve access to original preceding/following nodes and sources. Search uses a separate embedding index: existence of original content alone does not mean it is searchable.

CLI examples below use Bash/zsh. Replace `THREAD_ID`, `NODE_ID`, `CURSOR`, and `SPACE_ID` with actual returned values. Human external terminals must connect to the same data directory as the TUI. For another location, add `--data-dir /path/to/shellboy-data` to each command.


Current `history list` items always include `provenance`, `context`, and `selection`; `history get` always includes `provenance` and `context`. Individual unknown associations inside these records remain omitted rather than inventing IDs or times. These required records are not optional older-owner fields.

## Finding and reading original content

In the TUI, use F6 to select a history item, O for an original-content page, N for the next page, and P to return to the beginning. Inspect preceding/following and source IDs. To specify another node, use F8's “履歴 | 原文” (History · Original); for paginated relationships, use “履歴 | 関係” (History · Relations). In management views such as search results, Tab selects a field and Enter opens its details. See [Management result fields and detail pages](tui.md#management-result-fields-and-detail-pages) for full results, originals, and copying.

```sh
shellboy thread list --limit 20
shellboy thread get --thread THREAD_ID
shellboy history list --thread THREAD_ID --limit 20
shellboy history get --node NODE_ID --limit-bytes 65536
```

Omit body excerpts when inspecting path structure or locating large nodes:

```bash
shellboy history list --thread THREAD_ID --excerpt-chars 0 --limit 20
```

`--excerpt-chars` counts Unicode scalar characters, ranges from 0 to 4096, and defaults to 512. At 0, `excerpt` is empty and the body is neither retrieved nor parsed. `payload_chars` is the character count of the entire stored JSON, not the extracted body length or token count; reclaimed bodies return null. Inspect node ID, kind, adopted order (`selection.ordinal`), and size, then get only needed nodes. Origin context and destination selection can differ after forks, so both remain present. Links and provenance also remain: 50 items do not always fit within 16KiB. If large, reduce `--limit` or read subsequent shell output. Supply the same `--excerpt-chars` when continuing with a cursor.

CLI output uses a JSON envelope. Check `ok` and `error`; read content from `result`. `history list` pages through node IDs, kinds, and excerpts on the selected path. If `result.next_cursor` exists, continue with the same Thread and conditions.

```sh
shellboy history list --thread THREAD_ID --limit 20 --cursor 'CURSOR'
```

The `text` returned by `history get` is a body page respecting UTF-8 character boundaries. `source_node_ids` identifies sources of summaries and similar content; `previous_node_ids` and `next_node_ids` reference original neighboring nodes. Call `history get` again for an ID you want to follow. Reading originals does not alter the Thread's adopted path. `provenance` returns node kind, creation time, current original-content revision, and originating Thread/Run/Turn only where corresponding stored records exist. Do not confuse membership in a forked Thread with the origin.

The `context` from `history get` contains stored creation time, originating Turn/Run/Thread, and present/tombstone state. Even after body reclamation, inspect tombstone_reason and origin. These values do not infer adoption into a fork or execution start time.

Page through direct relationships separately by type:

```bash
shellboy history relations --node NODE_ID --relation previous --limit 50
shellboy history relations --node NODE_ID --relation next --limit 50
shellboy history relations --node SUMMARY_NODE_ID --relation sources --limit 50
shellboy history relations --node SOURCE_NODE_ID --relation replacements --limit 50
```

previous/next represent conversation neighbors; sources represent sources of summary/spill output; replacements represent replacement outputs using a source. Only completed replacements are returned; unfinished or failed replacements are not treated as history. Get `node_id` from `items` to read the body; when `next_cursor` exists, continue with `--cursor` for the same node/relation. Default is 50 items, maximum 200. This does not expand unlimited ancestors/descendants. Distinguish relation items' source_revision/source_hash, recorded by the replacement, from provenance.source_revision, the returned node's current revision.

```sh
shellboy history get --node NODE_ID --encoding UTF-8 --limit-bytes 65536 --cursor 'CURSOR'
```

Continue using the `next_cursor` returned by that body. Cursors are opaque: do not edit or reuse them for another node/encoding. Current body pages are capped at 64KiB. For deleted/reclaimed bodies, inspect reasons such as `tombstone_reason`; do not reinterpret empty text as "there was originally no output."

A `history get` tool_result is the result/excerpt saved into the conversation. Retrieve stored shell raw output separately with paginated `shellboy shell read --execution EXECUTION_ID`. See [Shell](shell.md) for encoding changes and output retention. External attachments are path references; history cannot restore past external file contents.

In the TUI, the wheel moves by three display rows; PgUp/PgDn move by the history viewport height minus one row. The saved reading position uses the original node and source position. Ctrl+End returns to the bottom. If display cache is reclaimed, original pages are fetched again. `/copy NODE_ID` retrieves original content rather than screen truncation or a reasoning placeholder.

## Linking executions and history

```bash
shellboy history list --thread THREAD_ID --run RUN_ID --limit 20
shellboy history list --thread THREAD_ID --since '2026-10-25T00:00:00Z'
shellboy history list --thread THREAD_ID --status running
```

`--run` filters by the Run of the Turn that generated the node. `--thread` denotes a path currently adopting the node; selecting a forked Thread does not change the originating Run. `--since` inclusively compares node creation time against an absolute RFC3339 timestamp. It does not mean shell end time or state update time.

`--status` is an exact match on the **stored state of a related shell execution**: `starting`, `running`, `stopping`, `exited`, `failed`, or `stopped`. This differs from Run/Turn state. Nodes without a related actual shell are excluded. A background shell can remain running after the tool call returns.

List `provenance` and `context` describe node origin and storage state. `selection.thread_id` / `ordinal` describe the path being listed; `selection.current_cwd` is that Thread's current configuration. For the actual shell launch directory, inspect `executions[].cwd`. Do not infer historical execution cwd from current settings.

`executions` comes from stored relationships between tool groups/calls and shell executions, not guessed from ID-like body text or acceptance JSON. Follow `execution_id` with `shell get/read`, and `request_node_id` / `result_node_id` with `history get`. Records without a unique request node omit its ID. Distinguish `call_state` from shell `state`.

Each node lists at most eight executions. `execution_count` gives the total and `executions_truncated` indicates omissions. Calls cancelled or rejected without creating an execution are excluded from this count. `command_excerpt` is a saved command excerpt of at most 80 characters; inspect `shell get` or the original request for the full command.

```bash
shellboy shell get --execution EXECUTION_ID
shellboy shell read --execution EXECUTION_ID --encoding UTF-8
shellboy history get --node REQUEST_NODE_ID
shellboy status --run ORIGIN_RUN_ID --all --limit 200
```

Use the originating Run's status listing to follow executions beyond eight. When `next_cursor` exists, continue with the same conditions. History cursors bind filters; filtering by status also makes related shell state changes expire the cursor. Fetch again from current state; do not resend a request because a query failed.

## Forks and request editing

```sh
shellboy thread fork --thread THREAD_ID --node NODE_ID
```

This creates a new Thread sharing a prefix that includes the specified node. Use the returned new `thread_id` in TUI `/switch THREAD_ID`, then send a follow-up request. Through the CLI, use `shellboy message send --thread THREAD_ID --text 'Follow-up request'`. Original Thread content is unchanged. Specify `--cwd /path/to/project` at fork time if needed.

TUI `/fork NODE_ID` performs the same branch operation. Forking within an unfinished tool call/result group may be rejected. Follow the error guidance to select a safe group boundary.

To replace a request, use `/edit USER_NODE_ID` in the TUI. It reads the full original request and attachment references into a separate edit draft. Sending forks a Thread with a prefix ending immediately before the original request, then saves the new request there. It does not directly UPDATE the original user node. If the original Thread's Run is active, stopping it requires explicit confirmation. Check the edit and stop target, then press Y; cancel with N/Esc. There is no independent `shellboy edit` CLI.

## Proactive compaction during a long Turn

At a safe request boundary, Shellboy can compact from the latest
provider-reported input-token count when it reaches `auto_at_tokens` (200,000
by default; see [configuration](configuration.md), including the per-connection
override, the per-Thread override `thread set --auto-compact-at` and the `0` opt-out). Unknown or stale usage does not trigger it.
Shellboy summarizes the oldest content first: it runs the ordinary summary of
earlier Turns that are not yet summarized. Only when nothing but earlier
summaries is left there, or when a fresh measurement after that summary is
still at or above the threshold, does it summarize the active Turn: every tool
round of the Turn is replaced by one summary, and the latest tool round is
included in it as plain text. Earlier Turns and the Turn's request stay
unchanged, so the selected path is `[earlier Turns][request][summary]`. User
messages in the summarized work are given to the summarizer in their original
order; their substance goes in a "User instructions to keep" section, not as
verbatim copies. Every summary asks for that section. The originals stay
readable with `shellboy history get` and `shellboy history relations`. One
summary runs per attempt.

Reasoning follows one rule for every replacement: reasoning before the first
changed position is still sent unchanged, and reasoning saved after it is not
sent again (signed thinking is bound to the exact history before it). Spilling
tool results waits until the model's latest response requested no tool. If the
context overflows before then, Shellboy summarizes the active Turn instead.
Original history remains available subject to retention. This threshold policy
is distinct from failure-driven overflow recovery; it adds no setting beyond
`auto_at_tokens` and its per-Thread override.

## Compaction and spill

```text
/compact
```

The CLI is `shellboy compact --thread THREAD_ID`. This is an explicit compaction request, not automatic capacity prediction. Active model communication or unfinished tool groups defer it until a safe boundary. Inspect summary creation/reuse and path adoption results. Acceptance alone may not mean compaction has completed. Pass the returned operation ID to `shellboy operation get --operation OPERATION_ID` and inspect `downstream.state` and terminal outcome. Resending compact with the same ID returns the original queued acceptance even after completion; it does not start compaction twice. Downstream work cancelled by restart is reported as failed/result_uncertain; external side effects are not automatically re-executed.

Spill replaces long tool-result input bodies with placeholders referencing originals. Summaries save purpose, constraints, state, next work, references, and similar content into another node. Both preserve access to original content within retention rules, and neither silently changes other Threads' adopted paths. Attachments in the replaced interval leave send selection; [reattach](attachments.md) them if needed.

Failure-driven overflow recovery starts only on provider-explicit context overflow. Its bounded recovery sequence is: spill older results → normal request retry 1 → if still explicit overflow, spill including the latest group and summarize → normal request retry 2 → if still failing, recovery wait. While the model's latest response asked for a tool, spills and omission wait; an overflow then summarizes the active Turn instead (see [recovery](recovery-and-storage.md#failure-driven-reduction-50)). If no eligible targets exist, the intermediate retry is skipped. Unknown errors or mere HTTP/byte limits are not assumed to be context overflow. Already executed shells are not rerun for compaction.

Manual compaction during recovery wait does not automatically resume the normal request. Inspect results; a human or a model must explicitly resume with `shellboy run retry --run RUN_ID`. See [Recovery and storage](recovery-and-storage.md).

## Reasoning display and model changes

Returned reasoning text/summaries are collapsed by default in the TUI. Toggle them with `/reasoning NODE_ID`. Shellboy does not invent reasoning that was not returned or decrypt/display encrypted reasoning. Source character positions and node IDs do not change when toggling.

Changing connection or model advances the outgoing reasoning epoch and excludes prior-epoch reasoning/opaque continuation data from subsequent requests. Switching A→B→A does not automatically restore old A opaque data. Original nodes remain in the DB; history can retrieve displayable original text. `history get` does not display opaque bodies. Changing effort alone is distinct from switching models.

## Search requires a separate index

After configuring an embedding connection/model, newly finalized conversation content of the kinds listed in `indexing.auto_kinds` (default: user messages, summaries and the model's replies) is registered for automatic indexing when `indexing.auto_history=true` (default). Tool output, model replies and other kinds are not indexed automatically; see [Choosing which kinds are indexed](#choosing-which-kinds-are-indexed). Startup or configuration changes alone do not automatically backfill all history. Search results are not automatically injected into every model input either.

```text
/search error handling for the current task
```

CLI equivalent:

```sh
shellboy history search --query 'error handling for the current task' --limit 10
```

Search creates a query embedding in the active IndexSpace and returns excerpts grouped by original node, distances, node IDs, and related fields. Verify results through `history get` on the original node. Embedding API use may cost money. No matches do not necessarily mean the original is absent: check missing indexing, another space, individual failures, and similar causes. Local folders are not automatically ingested.

Filter the scope first through the CLI or F8's “履歴 | 検索” (History · Search):

```bash
shellboy history search --query 'error handling for the current task' --thread THREAD_ID --kind user --kind assistant --exclude-kind tool_result --from '2026-10-01T00:00:00+09:00' --to '2026-11-01T00:00:00+09:00'
```

`--thread` covers the current adopted path plus sources recursively reached through completed replacements from that path. Omitting it searches globally, without implicitly restricting to the current Thread or guaranteeing hits are on its adopted path. Kind means node kind. Values within repeated `--kind` use OR; other conditions use AND. Specify node creation timestamps as offset-bearing RFC3339; from is inclusive, to exclusive. Ranking follows filtering, avoiding missed matches caused by filtering only global top results in a shell.

Result provenance identifies origin and current original-content revision. Span start/end values are Unicode scalar positions `[start,end)` in the body extracted for each body_kind. They identify semantically selected chunk ranges, not search-term match positions. Use source_revision/content_hash too to identify the original. Distance is L2; no distance threshold option exists. Chunks are grouped by node, but distinct nodes with identical content are not merged. Only Thread-scoped searches return thread_scope: on_current_path=true means the current adopted path; false means a completed replacement source included in the scope. Global search omits thread_scope and does not infer membership. See [CLI reference](cli-reference.md) for fields and limits.

Which of these are indexed automatically depends on `indexing.auto_kinds` (below). Model replies are indexed as separate answer and displayable-reasoning bodies, and tool results (with retained shell raw output) as their own kind; each can be searched once its kind is indexed, and all of them by keyword search. Attachment binaries and encrypted opaque data are not sent to embedding. Reclaiming raw output also invalidates its derived vectors; late results cannot revive reclaimed content.

## Index management and cost previews

Specify a target in the TUI:

```text
/index status
/index thread
/index threads THREAD_ID_1 THREAD_ID_2
/index all
/index space SPACE_ID thread
```

`/index status` displays state. `thread` selects the current Thread, `threads` specified Threads, and `all` all retained sources. Specifying only `space` does not select a historical-processing scope. The management screen shows spaces, active/selected state, model/dimensions, and coverage definitions. A job's done count is not equivalent to "every source fully indexed." Interpret partial indexing and unverified raw coverage according to the displayed definitions.

| Lowercase key | Purpose |
| --- | --- |
| `s` | Refresh status |
| `p` | Preview source/segment/character counts and already indexed content for the selected scope |
| `b` | Backfill history |
| `t` | Retry failures / explicitly resume from pause |
| `r` | Rebuild |

`b/r` require an explicit source target and matching preview, followed by confirmation with Y that costs may occur. Without a target, `p` explains how to select one. `t` can also preview already-authorized failed work globally; it shows source/segment/character counts and asks for Y before retrying. It does not authorize untouched old history. Preview calls no API and reports counts as observed at that time. It does not assert a price when unit pricing is unknown, nor reserve/freeze later source updates. Obtain another preview after execution.

The CLI also supports explicit scope selection. It has no TUI confirmation screen; the mutation commands below are accepted directly. First read source/segment/character counts with `index preview`, then verify cost and scope before executing a mutation. Preview itself calls no embedding API and submits no jobs.

```sh
shellboy index status
shellboy index preview --scope threads --thread THREAD_ID
shellboy index backfill --scope threads --thread THREAD_ID
shellboy index retry --scope threads --thread THREAD_ID
shellboy index rebuild --scope all --kind all
```

Select targets with `--scope all`, `--scope threads --thread ID`, `--source NODE_ID`, and `--space SPACE_ID`. A space alone does not substitute for historical scope selection in backfill/rebuild. CLI `index retry` also accepts omitted targets, resuming failed jobs and similar work in that connection's active/candidate spaces. To limit targets, explicitly provide Thread/source as above. Omitting the scope on CLI `index preview` previews that retry-eligible work without sending an API request. The TUI performs this read before its retry confirmation. See [CLI reference](cli-reference.md) for detailed conditions.

`pending/running/done/failed` are job states; `held_auto_history` separately counts pending automatic entries held by `auto_history=false` (source authorizations or segment inputs, not unique unindexed nodes). Such entries are excluded from `pending`; explicit pending work remains eligible, while re-enabling or explicitly reauthorizing work moves it back into pending. An explicit space selection excludes pre-space source authorizations/bootstrap from both counts, as before. `paused` and reason describe why a space stopped. Individual temporary failures receive bounded retries. Shared failures such as authentication pause indexing without halting new conversations. Explicitly retry after fixing the cause. Already successful portions are not processed again.

A pause notice in conversation history is a saved **stop record**, not a current request waiting or a conversation failure. It leads to `/index` → `t`, shows the original `paused_at` and reason, and remains in history after recovery. Missing historical time stays unknown. Unchanged pause observations are not repeated every Turn. Fresh `s` status reflects resumption; there is no continuous background status poller. Fix the underlying authentication, credit, or provider problem before explicitly retrying. Existing vectors may still be searchable while updates are paused, and query embeddings can still incur charges.

### Inspecting and explicitly changing embedding dimensions

Read `shellboy index status` first. Each retained space reports its connection/model, measured `dimension`, optional `requested_dimensions`, resident segment count and `vector_payload_bytes`. The latter is only `segments × dimension × 4` bytes of float32 vector payload (`× 1` for an int8 space, see [Shrinking the index](#shrinking-the-index-without-api-calls)). It is not the SQLite file, WAL, allocation, reclaimable capacity, or an exact disk-space saving. Candidate rebuilds temporarily coexist with the active space. Old vectors are kept until you ask for them to be removed (paid indexes are never collected automatically); a smaller dimension may reduce search quality.

If the user authorizes a support test, run one explicit probe:

```sh
shellboy index probe --dimensions 768
```

A probe sends one small embedding request and may cost money. The result is an observation for that endpoint/model/requested dimension, not a capability catalogue. A model alias does not prove dimension support. On refusal/mismatch, inspect the saved failure; do not try other dimensions or native defaults automatically.

Before a paid dimension change, preview the intended source scope and obtain explicit consent for its count/characters, possible quality loss, API charges and temporary extra storage. For example, after authorization:

```sh
shellboy index preview --scope threads --thread THREAD_ID
shellboy index rebuild --scope threads --thread THREAD_ID --dimensions 768
shellboy index status
```

The dimension is frozen for the candidate and subsequent active-space ingestion/query requests; returned vector lengths must match. The first real authorized source validates the candidate without an extra hidden probe. Refusal stops that candidate, preserving the old active vectors. Rebuild without `--dimensions` retains the current matching active space's request mode; a changed endpoint/model defaults to native mode. Status includes up to 200 retained spaces and the newest 32 probe observations, so inspect total/truncation fields instead of assuming the lists are exhaustive.

Every embedding the provider returns (indexed text and search queries, for every model) is scaled to unit L2 length before it is stored or compared, so L2 ranking follows semantic closeness at any dimension and any vector length. This matters when you reduce dimensions: Gemini `gemini-embedding-001` returns unit-length vectors only at 3,072 dimensions, and reduced sizes (768/1,536) are not normalized by the provider. Existing unit-length spaces (such as the 3,072-dimension one) behave identically and need no rebuild. A zero-length or non-finite vector is a failure, never repaired silently. A space built before this change at a reduced dimension holds unnormalized vectors, so rebuild it to benefit.

### Shrinking the index without API calls

`index shrink` converts the active index to 8-bit values (int8), so a search reads about a quarter of the vector bytes. It calls no embedding API and reads no history: it re-encodes the vectors already saved. It never runs by itself.

```sh
shellboy index shrink --dry-run   # effect and sizes; changes nothing
shellboy index shrink             # start, or resume after a failure
shellboy index status             # progress in `shrink`, result in `spaces`
shellboy index shrink --cancel    # stop and keep the old index
```

| | `index shrink` | `index rebuild` |
|---|---|---|
| Embedding API | None, free | Every segment, charged |
| Source text | Not read | Re-read and re-split |
| Model, dimension, coverage | Unchanged | Can change |
| Precision | int8 | float32 |

What to expect:

- Search keeps using the old index until the new one is complete. A failure, `--cancel`, a restart or reaching the logical data limit leaves the old one in place, without pausing indexing or holding new work; a restart continues where it stopped, and after a failure `index shrink` continues (after the data limit it starts over, because the partial index was given back to keep room for conversations). While it runs, the database holds both, so it needs a quarter of the vector bytes extra (`temporary_extra_vector_payload_bytes` in the preview), plus its bookkeeping rows.
- Afterwards the old index is removed by storage collection, automatically or with `storage collect --category index`. Returning to full precision then needs a paid `index rebuild`. The database file does not shrink by itself; freed pages are reused, and `storage vacuum` returns them explicitly.
- The order of results changes slightly. On one real index of 32,779 distinct segments, 98.8% of a search's top 10 stayed the same when segments themselves stood in for queries; real queries were not measured. By the time you can search the new index, the old one is about to be collected or already gone, so a comparison afterwards cannot fall back on it. For a first conversion of a real index, copy the database while the owner is stopped, then check real searches you care about straight after the switch.
- New text and search queries are converted with the same fixed factor, chosen from your vectors when the conversion ran and shown as `int8_scale` in `index status`. An element beyond that range is clipped to ±127 (saturation) rather than failing. On that same index none was clipped when the factor came from the oldest half of the vectors and was applied to the newer half.
- `distance` is the L2 distance between the stored integers, so values are around hundreds instead of around 1. As before, it only orders results within one space. Do not compare it across spaces or with a threshold.
- A later `index rebuild` builds a float32 index again; shrink it again if you want.

`vector_payload_bytes` in `index status` is `segments × dimension × 1` for an int8 space.

IndexSpaces are separated by connection, embedding model, observed dimensions, and related settings. Configuration changes do not silently delete old spaces; failed rebuild candidates preserve the old active space. Historical content needs explicit backfill/rebuild to enter the new space. Automatically detecting a changed model behind an alias is not guaranteed.

Search excludes deleted nodes, reclaimed bodies, and stale index entries. Deleting a Thread differs from deleting a shared node; originals retained by other Threads or references can still appear in global search. provenance.origin_thread_id records origin, not necessarily a Thread still available for operations. Search does not pin originals: inspect tombstone_reason when getting content later too.

Relations cursors exclude relationships added after the first page but do not reserve a whole-DB snapshot or body retention. A target node revision change produces CursorExpired. Related-node metadata reflects page retrieval time. Re-searching or querying current values creates no receipt and observes again even with the same operation ID. Cursor snapshot conditions and expiration remain separately enforced. Repeating semantic search can incur another query embedding charge.

## Choosing which kinds are indexed

Tool output is about 96% of an index built from everything, so automatic indexing takes in only the kinds named by `indexing.auto_kinds`: by default `user`, `summary` and `assistant_response`. The words are the node kinds that `history search --kind` already uses: `user`, `summary`, `assistant_response`, `tool_result` (shell results, including retained full output), `spill` and `event`. Other node kinds carry no searchable text. The setting is read at owner start (restart to change it) and appears as `auto_kinds` in `index status` and `config check`.

Summaries are registered when they are created, like your messages. Verified against the code for #254: before this change summaries and spills were never registered automatically, and tool results, model replies and attachment notices always were.

What the setting does and does not do:

- It changes only what is registered from now on. Vectors that exist are never removed, and jobs already queued keep running; a registration written before a restart that no longer matches the list is dropped without embedding when its turn comes, and does not keep a rebuild candidate from becoming active.
- Search still finds the other kinds, without embeddings: `shellboy rag search --scope history --mode keyword --query TEXT` reads the retained originals of every kind. Semantic and hybrid results from `history search` / `rag search` say in `scope_status` (`auto_kinds`, `kinds_not_auto_indexed`) that those kinds can be missing; they never claim complete coverage.
- Explicit commands add kinds for the Threads you choose. `index preview`, `backfill` and `rebuild` accept `--kind WORD` (repeatable; `all` means every searchable kind) in addition to `--scope`/`--thread`/`--source`. `--kind` needs `--thread` or `--scope all` (with only `--source`, or no selection, it is rejected). A Thread or `--scope all` selection takes in `auto_kinds` plus the `--kind` words and nothing else; the preview counts exactly that selection and reports `kinds` and `excluded_sources` (approximate: sources of other kinds that were left out). `--source NODE_ID` names a node explicitly and is never filtered. `index status` and `retry` take no `--kind` (`retry` resumes work that was already authorized).

```sh
shellboy index preview --scope threads --thread THREAD_ID --kind tool_result
shellboy index backfill --scope threads --thread THREAD_ID --kind tool_result
```

Backfill into the active space only adds missing parts; segments already indexed show as `already_indexed`. `rebuild` builds a new space from the selected sources only, and activating it retires the old space, so a full rebuild must not silently lose vectors of kinds it does not take in. `index rebuild --scope all` without `--kind` is therefore refused (`invalid_argument`) when the active space holds vectors for sources of other kinds; the message gives the count per kind. Choose: `--kind all` keeps everything, or name the kinds you want with `--kind` to accept dropping the rest. `index preview` with the same arguments shows the same counts as `indexed_outside_selection` before anything is charged. The retired space itself is kept until you ask for it to be removed (paid indexes are not collected automatically). To change the stored precision without re-embedding anything, `index shrink` converts in place and keeps every kind; it is the safe route when you only want a smaller index (see [Index management](#index-management-and-cost-previews)). In the TUI, `/index all kinds all` then `r` rebuilds with every kind, `/index thread kinds tool_result` adds a kind for one conversation, and the index form has a "kinds" field; a refused rebuild is explained in the panel.

## Concurrent searches and reader recovery

`history search` permits one semantic search at a time per owner. The single slot is held from before embedding HTTP through ranking and result revalidation; concurrent child Threads share it. When busy, it immediately rejects another request with `InvalidState` before sending an additional query embedding; this rejection incurs no embedding charge. A slow embedding provider can therefore keep other searches busy until completion, cancellation, or the existing embedding timeout (60 seconds by default). Serialize child searches, or have the parent search once and share the results. Wait for the current search to finish before retrying; do not spin or launch parallel retries.

Search uses a separate readonly connection, allowing conversation and shell output writes during heavy ranking. Read-only connection/open, lock, query, or read failures return `IoFailed` with the underlying cause and do not stop the owner or other Runs. Fix the reported file-access, resource, or locking problem before retrying. Database corruption (`SQLITE_CORRUPT` / `NOTADB`) and missing-table errors (`no such table`) also return `IoFailed`; blind retries do not repair them. Diagnose the cause and restore or explicitly rebuild the affected index as appropriate before retrying. Each retry that reaches embedding HTTP may incur another charge even if the same database error follows. This does not change real writer failures, which still stop the owner for storage safety. Cancellation or disconnection returns `InvalidState` and stops observation only, not the target Run. Deletion/reclamation/selection changes during search are checked again before returning, so fewer results than the requested limit may be returned.

## Input size and range editing

Use `context_usage.last_successful_attempt` from `shellboy thread get --thread THREAD_ID` for measured input_tokens/cached_tokens, started_at and ended_at. next_request_pending means an edit occurred after that request; its effect is known on the next successful request. Core provides no token estimate or size threshold. When unavailable, for example while stopping, it returns `available:false` with a reason; other Thread details remain accessible.

```sh
shellboy compact --spill --thread THREAD_ID --from FIRST_NODE --to LAST_NODE
shellboy compact --summary --thread THREAD_ID --from FIRST_NODE --to LAST_NODE
shellboy compact --omit --thread THREAD_ID --from FIRST_NODE --to LAST_NODE
shellboy compact --trim --thread THREAD_ID --to LAST_OMITTED_NODE
shellboy compact --restore --thread THREAD_ID
shellboy compact --restore --thread THREAD_ID --from MARKER_NODE
```

`from/to` follow path order, include both endpoints, and select tool calls/results as complete groups. `spill` replaces result bodies with original references while retaining commands and exit state. `summary` uses the existing summary API. `omit` replaces a leading or interior range with one fixed notice, preserving surrounding content. `trim` omits the prefix through `to`. Omission cannot target the current Turn's first request, latest group, unfinished tool groups, or protected nodes. Invalid boundaries return candidates rather than silently adjusting the range.

All use the same queued acceptance as existing compact. Query downstream with the returned operation ID and confirm completion/outcome. Each edit executes in order at that Thread's safe boundary and affects subsequent model requests. Resending the same ID/request does not apply an edit twice.

Originals remain after omission. `restore` restores the original range at the latest marker on the selected path (or the marker specified by `--from`), preserving subsequent conversation. In the TUI, use argument-free `/restore`. Attachments do not automatically return to send selection; add them with new attachment IDs if needed. Restoring beyond the guideline produces a warning.

To restore omitted context in shorter form, use `--summary --from ... --to ...` with the notice's first/last original node IDs. This targets all omitted sources of one marker. Selecting only part of a hidden range is rejected with guidance to the full boundaries. Reading large amounts of originals through `history get` itself adds those read results to conversation input.

Edit infrequently and in coherent ranges. Prompt caching after the edited position may be invalidated. Result `before_characters` / `after_characters` / `changed_suffix_characters` count replay characters, not tokens or cache hits. Omission/restoration does not undo executed commands or file changes.

To select a restoration range from its notice, use `compact --restore --from FIRST_OMITTED_SOURCE_NODE`. `--from` uniquely identifies either a selected marker ID or the first omitted source node ID. Argument-free restoration restores the most recently created selected marker, not necessarily that particular notice. Explicit `--summary` can include the currently adopted request, so normally keep the current request outside the range to preserve important instructions.


## Keyword retrieval after trimming

`history search` uses the selected semantic index and query embedding. To search
the same Thread membership without embedding, use:

```sh
shellboy rag search --query 'earlier decision' --scope history:THREAD_ID --mode keyword
```

Both scopes include the current path and recursive sources of completed
summary/spill/omit replacements. A result with `thread_scope.on_current_path=false`
is an original retained through replacement provenance, not a currently adopted
node. This differs from `history list --thread`, which pages only the current
path. Global `--scope history` intentionally searches all history instead.
Read the original or summarize a selected range before restoring a large omitted
range, which can recreate the same input failure.

### API-free maximum reduction

Use `/compact-max` or `shellboy compact --maximal --thread THREAD_ID` to omit
all safe context outside protected work, without a model call or node IDs.
It does not retry or resume the Run. Inspect the saved edit outcome, then choose
`/retry` explicitly (another model request may be charged). `/restore` can bring
omitted text back. `/compact` without flags still requests a model summary.


## Automatic history indexing policy

`indexing.auto_history=false` holds automatic ingestion without removing existing
vectors or disabling standalone knowledge RAG. `indexing.auto_kinds` narrows which
kinds automatic ingestion takes in (see above) and is independent of it. Restart the owner after changing
it. Explicit `index backfill` / `rebuild` / `probe` remain available and may cost
money. `index status` and `config check` report `auto_history`.
`history search` returns an object with `items`, `auto_history`, `degraded` and
`scope_status`, not a bare hit array. With automatic ingestion off, it warns that
newer nodes may be absent and searches only retained indexed sources, even if
`items` is empty. This is not an exact missing-node count. Keyword search remains
available for retained originals; existing coverage is never a completeness claim.
`scope_status` also lists `auto_kinds` and `kinds_not_auto_indexed`: kinds that
semantic search finds only after an explicit backfill.
For storage cleanup, `storage collect --category index` removes eligible inactive
spaces; no existing CLI removes the active index alone while keeping all history.
