# TUI controls

[User Guide](user-guide.md) · [Guide index](README.md)

The current UI calls a Thread `会話` (conversation), a Run `作業` (work), and a Turn `応答処理` (response processing). CLI names, identifier keys, and source text retain their original spelling. Japanese captions quoted below identify the unchanged current UI.

The top shows the conversation, connection, and model; the center shows history; the bottom holds the draft and activity status. History distinguishes messages, commands, execution results, state changes, and summaries. Answers, summaries, and expanded visible reasoning use Markdown; requests and command output use source text. Routine observations appear as concise facts at the end of their conversation section; actionable failures remain visible, with individual source facts available in details.

A new conversation without history shows send/newline keys and working directory. `AGENTS.md: 未確認` means the screen has not received an observation of the file's presence, not that it is absent.

Simplified display does not delete saved source. Details and typed action forms provide full IDs, before/after values, remaining source, provenance, and complete URLs. Use `/cwd` or `/model` to inspect current settings as well.

## Read times, refresh and frozen display

Bounded output previews and original pages show their read time in UTC. That
time is captured once when the read succeeds, not advanced by drawing or the
animation clock. It identifies the display observation, separately from a saved
message's creation time. A completed preview is a snapshot: close and reopen its
row to read again. This only rereads retained output; it does not rerun the shell.
In F6's original page, O rereads the selected source, N requests the next page,
and P requests the first page. Each accepted page has its own read time.

Mutable fields and links for the visible selected-history F6/F7 item update after
relevant saved changes, including CLI changes from another terminal. Changes
coalesce for 500 ms; the target in an unloaded local environment is visibility
within 1 second. This is not an API/network or busy-database freshness guarantee. A failed
refresh labels retained facts stale/unavailable instead of silently presenting
them as current. Source identity and selection remain distinct from refresh.
This does not promise that every other popup is already reactive: management
usage/reference behavior is described separately below, and browser/watch/live
work/form improvements belong to subsequent increments.

F2 records a UTC **freeze-entry time** once and stops automatic repainting.
F2/Esc resumes normal updates. Ctrl+L redraws currently available UI data once
while remaining frozen; it neither performs a DB refresh nor changes the entry
time. Therefore the freeze-entry label is not the read time of every value on
the repainted screen. The compact banner may shorten the timestamp or omit it
when the terminal is too narrow; the full-width banner includes the UTC date.

## Saved usage details

F8's `使用量 · 合計` reads the selected conversation's saved model usage and
separate Run/model/shell time; enable children to include its bounded descendants.
Conversation, Run and Turn detail results also include these saved totals. Missing
usage is marked unknown and unrecorded summary/embedding coverage is explicit.
These values are not a bill, provider-receipt proof or a sum of effective work.

Explicit F8 usage/get detail forms query the existing DB worker on open.
For those forms, Changed bursts become one pending
refresh, eligible three seconds after the previous result and checked by the
existing500ms UI tick (normally within3.5s plus worker/DB backlog). There is one
in-flight refresh; displayed values are snapshots. Rendering uses cached fields only. Esc, a new
operation or conversation selection stops that refresh; stale generations cannot
replace another view. A refreshed result changes its detail identity, so links
selected from an older result must be selected again. F6 field pages inspect the
cached response, not a new query. Reference navigation from a result
(`FollowManagement`) performs one query and opens a one-shot snapshot; it does
not subscribe to Changed or inherit the previous form's refresh. To obtain newer
values, explicitly submit the F8 usage/get form again or follow the reference
again from a current result. F6 paging alone does not refresh that snapshot.
No freshness deadline is promised for either surface. Ordinary conversation
selection reads lightweight metadata only, without usage aggregation.
No per-frame database read or permanent footer total is added.

In `<data_dir>/logs/shellboy.log`, filter `event=usage_totals_slow` and inspect `usage_slow.trigger`, `scope`, `elapsed_ms`.
Only DB-worker aggregations taking at least the fixed100ms threshold are logged; fast reads are silent, and this is not render/IPC time.
Each actual reference query uses trigger `open` once if it crosses that threshold;
F6 paging and Changed notifications on the reference snapshot do not re-read or
log an aggregation. Closing the view or switching drafts leaves no hidden usage
refresh/subscription.

## Activity display

Immediately after sending, local `送信受付中` (accepting submission) shows the frozen input. Once committed, one DB snapshot supplies both selected history and saved `反映待ち` (pending) rows above the composer. Saved input appears in one of these projections, never both. Pending rows include CLI/other-conversation senders and distinguish held/uncertain input and immediate delivery. `/queue` opens the full bounded list (128 rows, overflow count); Enter inspects delivery facts, O opens the original, D cancels one selected ID, and A confirms cancellation of only the displayed IDs. New arrivals are excluded from an existing confirmation. Unknown historical admission is not automatically replayed.

The configured send key queues normally at the next boundary. Ctrl+Enter sends immediately unless it is already configured as ordinary send; Ctrl+X then Ctrl+S is always the immediate-delivery chord while the composer owns input. Immediate delivery releases a foreground shell wait and preserves its process; it does not interrupt response generation. Esc first closes popups, completions and selection; in the bare conversation it interrupts the exact observed Turn. Background shells and owned children continue. Ctrl+C or `/stop` stops the whole Run and its owned work. No automatic background timeout is enabled.

`準備中` (preparing), `モデル応答待ち` (awaiting model), `応答受信中` (receiving response), and `ツール処理中` (processing tools) use moving text brightness. `待機中` (waiting), `回答済み背景処理中` (answered, background work active), `実行継続中` (execution continuing), and `停止処理中` (stopping) use a slowly pulsing marker. Completed, interrupted, failed, and recovery-wait states are static; failure and recovery wait use an attention color.

When no explicit title is saved, the conversation header derives a short name
from a bounded prefix of the selected requests, then pending input. If earlier
source evidence falls outside the read bounds, `名称未設定（元本文の取得範囲外）`
means the header cannot safely derive the first request. It does not mean there
are no messages; inspect history or set an explicit title. This header read does
not rewrite the title or alter the CLI's title lookup.

The header orders state, conversation name, model, and working directory. Long names use at most half the width; when space runs out, trailing items are omitted first. Working directories favor their final components; inspect/copy the full value in F8 `会話 · 詳細`.

Display follows stored state and received events. Receiving response means actual text has arrived from the current model attempt. Unobservable details, such as the model's internal thought or the target behind `待機中` (waiting), are not guessed. A Turn that has answered while owned shells or child Runs remain active shows answered/background-active. Use `/status thread` or F8 for details.

Animations affect only the status line, never history text or scroll position. Animation redraws stop during selection/copy mode or overlays. Selection mode shows `表示固定` (display frozen), its UTC freeze-entry time when space permits, and exit keys at the top; F2/Esc resumes updates. The conversation list is a snapshot and has no running-state animation.

`観測経過` (observed elapsed time) uses a monotonic clock for the period during which this screen observed Turn activity. It excludes time before selection, before restart, and while state was unavailable; it stops at recovery wait or Turn termination. It differs from a tool header's saved execution duration.

The composer follows displayed line count, normally limited to one quarter of screen height and at most eight lines; small screens prioritize a three-line control area. Folded pastes become short numbered chips; expand/edit/remove with `/paste NUMBER show|edit|remove`.

## Recovery-wait notification bell

New recovery waits detected after startup trigger the terminal bell for background conversations as well as the current one. Existing startup recovery waits, rereads of the same state, conversation switches, and redraws do not ring. Bursts across conversations are limited to one notification per two seconds. Completion and retrying displays do not ring.

To disable the bell, add this configuration and restart the owner:

```toml
[ui]
bell = false
```

This uses terminal BEL. Sound, screen flash, or visual-bell behavior follows terminal settings. If inaudible, check those settings; multiplexers such as tmux also need bell forwarding/notification configuration. Shellboy does not change OS notifications or volume.

## Input and basic keys

These are defaults. Change send/newline combinations with `ui.send_key` and `ui.newline_keys`. Combinations producing the same terminal key code cannot be distinguished. While processing, the composer explains that additional instructions apply at the next boundary; during recovery wait, a new instruction resumes work.

| Key | Action |
| --- | --- |
| Enter | Queue draft at the next boundary, or execute a manually typed slash action |
| Ctrl+Enter / Ctrl+X then Ctrl+S | Immediate delivery; configured ordinary Ctrl+Enter keeps its normal role |
| Esc | Close the current popup/completion/selection first; otherwise interrupt the active Turn |
| Ctrl+J / Shift+Enter | Newline |
| Ctrl+U | Clear current draft text, pastes, and attachment references; preserve sent requests and external files (configured send/newline bindings take precedence) |
| Ctrl+L | Clear and redraw the whole screen; preserve draft, scroll, selection, and popup (configured send/newline bindings take precedence) |
| Left/Right / Backspace / Delete | Move/delete by character, accounting for combining characters and emoji |
| Up/Down | Select completion, recall sent text, or move between draft lines; see below |
| Tab | Complete the selected slash candidate |
| Home / End | Start/end of the current draft line |
| Mouse wheel | Move history three display rows, measured after wrapping |
| PgUp / PgDn | Move history by viewport height minus one row |
| Ctrl+End | Refetch the latest history and resume following |
| Ctrl+T / Ctrl+N | Conversation list / new draft |
| F1 | Task-oriented help, showing currently configured send/newline keys |
| F2 | Enter/leave terminal selection and copy mode |
| F3 / F4 / F5 | Inspect / edit / remove the latest paste block |
| F6 | Choose conversation content and inspect details/source |
| F7 | Links and complete URLs for the current history item |
| F8 | Action list and typed target/argument forms |
| Ctrl+C | Stop the current Run and its background/owned work; does not quit |
| Ctrl+B | Background the current foreground shell |
| Ctrl+Q | Begin normal shutdown |

Use Ctrl+L if stale characters or old link regions remain. It works in lists, details, popups, and frozen selection; repeated presses are coalesced within the render-rate cap. If it conflicts with send/newline bindings, redraw is disabled and F1 explains why. It does not repair underlying terminal character-width disagreement.

In selection/copy mode, Ctrl+L redraws the latest state once, including responses received after freezing; a following viewport may advance. Selection mode remains active and automatic redraw stops again afterward. This does not restore a snapshot of the screen at freeze time, request a new DB read, or change the recorded freeze-entry time.

Each Thread retains its draft, source-character anchor, and follow state. Movement is measured in display rows. At the end, history aligns to the last screen and follows; reading upward stops following. Wheels do not move the conversation behind help/details overlays. Offscreen bodies/layouts may be evicted from cache without deleting source history. `本文未キャッシュ` (body not cached) is fetched again according to scroll position.

F1 (`/help`) groups common controls, conversation navigation, details/activity, models, and attachments by purpose. Up/Down scroll one row, PgUp/PgDn one page, and Esc/Enter/F1 close. Return to the conversation with Esc before entering commands. `/model`, `/cwd`, and F1 in the conversation list show model settings, working directory, and selected-conversation information respectively.

## Completion and input history

Typing a command name beginning with `/` shows candidates and descriptions above the composer. Choose with Up/Down and complete with Tab or Enter. Enter only completes an unfinished name; when the name already matches exactly, the send key executes it. Esc closes only the candidates. No candidates appear while entering arguments or for pasted slash text.

Without candidates, Up on the first draft line recalls sent text from the current conversation, newest first. Up/Down then move to older/newer text; Down beyond the newest restores the unfinished draft. Recall includes text after restart and outside the display cache. A new unsaved conversation has no history. Within ordinary multiline drafts, Up/Down move the cursor between lines.

Recalled text can be edited and sent. If the send key is pressed immediately after Up, sending waits for the recalled text. Editing or switching conversations during loading cancels that pending send. Current draft attachments remain; historical attachments are not automatically restored. Recalled text starting with `/` remains conversation text. Unsent slash actions and other conversations are excluded. Oversized historical text produces an error without truncation and preserves the current draft.

## Pasting long text

Bracketed paste received in the conversation composer enters the draft including newlines and an initial `/`; a paste event alone never sends or executes slash actions. By default, over 1000 characters or over three lines fold into a block while retaining the full text. Enter sends the full folded content.

Non-input screens such as help, management, details, confirmation, and the conversation list reject pastes and show a bottom notification. Return with Esc and paste again. Pasting during selection/copy mode is also not applied: a notification explains the mode was left and repasting is needed. Nothing enters the draft or a field behind the screen.

Address a block by number:

```text
/paste 1 show
/paste 1 edit
/paste 1 remove
```

Both `show` and `edit` expand the block and move the cursor to its start. While folded, Left/Right jump between block boundaries; Backspace/Delete at a boundary removes the whole block. Expand first to edit characters. F3/F4/F5 target the latest block. Copying only the folded display does not recover source text. Pasting beyond `limits.draft_bytes` rejects the entire paste rather than adopting a prefix.

Restore failed/cancelled send snapshots with `/restore REVISION`. Unaccepted or failed slash actions also restore their input; if other editing has begun, that new input is preserved and the old action's restore revision is displayed. Status hints supply the revision. After acceptance, restoration uses immutable message source; a late restore result is not adopted if you edit while waiting. Durable `/restore list` also lists cancelled messages and unconfirmed frozen submissions; `/restore draft ID` restores one source. Argumentless `/restore` retains its context-range meaning. Cancellation preserves separate archives; newly cancelled inputs merge in receipt order only if the captured composer remains empty and unchanged and the result fits its byte budget. Otherwise use the individual archives. A successful send's old revision is suppressed after restart, while newer edits remain editable.

## Slash commands

`/status` lists stored work across all Threads. `/status thread` limits it to the current Thread; `/status all` includes terminal work. Runs, Turns, background shells, unapplied requests, and model attempts each occupy one row. Up/Down select, Enter opens ownership/model/timestamp details, O queries the target, S reads shell source output, R refreshes, and N fetches the next list page. Within a detail page, N continues that detail. Esc returns to the conversation, retaining the draft and reading position.

This observes saved state, not OS process liveness, in-flight searches, or standalone RAG stores. A completed Turn can still own background shells or child Runs; inspect Turn and Run states separately. Use F8 `作業 · 状態一覧` for filters such as failures, `/index status` for indexing, and CLI `shellboy rag status` for RAG.

Replace `ID` and connection names below with real values. `/attach` and `/cwd`, which use their argument as a path, allow quoted paths with spaces.

| Action | Purpose |
| --- | --- |
| `/help` | Task-oriented help, including memory-retention settings in reference information |
| `/status [all\|thread]` | Stored work globally/current Thread; `all` includes terminal states |
| `/details` | Select conversation content and inspect details |
| `/links` | Links for the current history item |
| `/actions`, `/operations` | Typed action forms |
| `/cancel-wait` | Cancel observation of a TUI-started wait, without stopping its target |
| `/new` | New draft |
| `/threads [FILTER]` | Threads and unsent drafts |
| `/switch THREAD_ID` | Switch by ID |
| `/model` | Current connection, model, and effort |
| `/queue` | Saved pending/held input; fixed-ID cancellation and original/delivery inspection |
| `/restore list` / `/restore draft ID` | Durable archived input and frozen submission recovery |
| `/models [CONNECTION]` | Live model list: type to filter, Up/Down select, Enter changes, Ctrl+R refreshes, Ctrl+Y copies selected ID, Esc returns |
| `/model CONNECTION MODEL_ID` | Change connection/model |
| `/effort VALUE`, `/effort none` | Set/clear effort |
| `/cwd`, `/cwd PATH` | Show/change working directory |
| `/fork [NODE_ID]` | New Thread sharing history through the specified node |
| `/edit [USER_NODE_ID]` | Load the original request into a separate editing draft; branch on send |
| `/compact` | Request spill/summary at a safe boundary |
| `/search QUERY` | Conversation search in the active embedding space |
| `/index [status\|all\|thread\|threads ID...]` | Index management; `thread` means current Thread |
| `/index space SPACE_ID [status\|all\|thread\|threads ID...]` | Select management targets in a specified space |
| `/attach PATH` | Add a path reference to the draft as file input |
| `/attachments` | Draft attachments and saved send selections |
| `/detach ATTACHMENT_ID` | Exclude from draft or saved send selection |
| `/reload-attachments` | Explicitly reread saved Thread attachments, without API submission |
| `/image`, `/paste-image` | Add clipboard image to draft |
| `/copy [NODE_ID] [BLOCK_INDEX]` | Copy source body or zero-based complete code block |
| `/reasoning [NODE_ID]` | Expand/collapse returned reasoning text |
| `/select` | Toggle selection/copy mode |
| `/paste ID show\|edit\|remove` | Paste-block actions |
| `/restore` | Restore the input range of the newest currently selected omission marker; preserve subsequent conversation, without automatically reviving attachments |
| `/restore REVISION` | Restore failed/cancelled draft snapshot |
| `/cancel` | Interrupt the current Thread |
| `/background` | Background the foreground shell |
| `/quit` | Normal shutdown |

Omitting the node in `/fork` suggests the anchor or tail; `/edit` suggests the anchor or latest user node. Invalid node kinds/boundaries are errors. Specify IDs for reproducible actions. `/edit` never overwrites the old node: sending branches immediately before the original request. If a Run is active, confirm stopping it with Y before proceeding.

## Overlays and selection mode

Popups share a frame, heading, explanation, body, and bottom controls. `›` plus background color marks selection; controls remain visible while scrolling. `…` indicates omitted middle content when an item cannot fit, but source, copy values, and execution arguments remain intact. Read full content through details/source. Forms expose the end of input and submit its whole value.

Ordinary lists, detail fields, and forms use Up/Down for selection and PgUp/PgDn for viewport-sized selection movement. Source, help, and result bodies scroll one display row with Up/Down and one page with PgUp/PgDn. PgDn in the conversation list continues fetching the next page. Confirmation screens scroll long explanations with Up/Down/PgUp/PgDn; only Y executes, while N/Esc return.

| Screen | Keys |
| --- | --- |
| Help | Up/Down/PgUp/PgDn scroll; Esc/Enter/F1 close |
| Conversation list | Text/Backspace filter; Up/Down select; Left/Right collapse/expand/navigate parents and children; Home/End first/last; Enter switches; PgDn adds next page; Ctrl+R refreshes; F1 information; Delete/Ctrl+D deletion confirmation; Esc returns |
| Management/search results | Tab / Shift+Tab select fields; Up/Down/PgUp/PgDn scroll; Enter field details; C copies value; A action form; W cancels explicit-wait observation; Esc returns |
| Conversation details (F6) | Up/Down select item, Enter opens fields; Up/Down select fields; V concise/internal view; O source; F7/L links; C copies value; A source-fetch form; Esc closes |
| Source | PgUp/PgDn scroll current page; O refreshes selected source; N next page; P first page; C copies current page source |
| Link list | Up/Down select; C copies full URL; recognition/display alone never connects |
| Action list (F8) | Type/paste to filter; Backspace removes; Ctrl+U clears; Up/Down/PgUp/PgDn/Home/End select; Enter opens form; Esc clears filter then closes; F8 closes |
| Action form | Tab/Up/Down fields; Shift+Tab previous; Left/Right/Space choose; type/paste; Backspace deletes at end; Ctrl+U clears field; Ctrl+J newline in multiline bodies; Enter submits; Esc returns to list |
| Wait form | F9 adds condition; F10 removes last added condition; waits for any condition; lowercase w in subsequent management view cancels observation |
| Attachment management | Additionally lowercase `s` saved selections, `r` explicit reread |
| Index management | Lowercase `s` status, `p` cost preview, `b` backfill, `t` retry, `r` rebuild; target and preview required before execution |
| Conversation deletion confirmation | D/Space toggles descendants and requeries; Y executes when allowed; N/Esc cancel |
| Action confirmation | Y confirms; N/Esc cancel |
| Shutdown grace exceeded | W/Enter wait; F force confirmation, then Y force quit |

The conversation list uses one row per conversation with name, working directory, state, and update time columns. Filter text appears immediately; fetching follows 150ms without typing. Ctrl+R refreshes immediately. An empty saved title displays a short name from the first user request, searchable by that name, without rewriting explicit titles or history. Branch lines show ancestry; `▾` / `▸` mean expanded/collapsed, `●` marks the open conversation, and `›` with background marks the selected item. Left collapses children or moves to the parent when already collapsed/a leaf. Right expands or enters the first child when already expanded. Up/Down traverse visible rows and keep selection in view.

In F8 `会話 · 設定` (conversation settings), set `タイトル操作` (title action) to `set` and enter a title to rename. `auto` clears the manual name and returns to conversation-derived display; `keep` preserves it. CLI: `shellboy thread set --thread THREAD --title 'Name'`; clear with `--title ''`.

Fixed bottom details show the selection's cwd, ID, parent, state, and a one-line excerpt of roughly at most 80 characters with Markdown/newlines normalized. Narrow screens omit columns; details or F1 show complete values. Enter opens the conversation; F6 reads source.

`[親未取得]` means the parent is outside the fetched page/filter scope. Adding pages with PgDn groups children under newly fetched parents. Fetched rows group ancestry based on update order; this is not a guaranteed complete tree. At 2000 entries, narrow the filter. Ctrl+R retries failed fetches. Searching/browsing does not stop Thread work.

### Deleting from the conversation list

In Ctrl+T's list, select a saved conversation and press Delete or Ctrl+D for impact and confirmation. On macOS, fn+delete means Delete; ordinary delete/Backspace still removes filter characters.

Deleting child conversations is off by default. Toggling D/Space requeries impact. Only Y with the latest impact permitting deletion executes; N/Esc cancel. Reasons explain live/stopping Run blockers and similar restrictions. State is checked again during execution even after confirmation, and failures remain in the list.

Deleting a background conversation selects an adjacent list row while leaving the open conversation intact. Deleting the open one discards its TUI draft state and moves to a new draft. This does not collectively delete shared source history or external attachment files; history reclamation follows existing GC. Unsaved draft rows are not handled by Thread deletion.

When deletion is confirmed but only subsequent cleanup fails, the UI distinguishes `削除しました。後片付けに失敗` (deleted; cleanup failed). It does not restore the deleted conversation or reinterpret the result as inability to delete. Failures/warnings arriving after the list closes remain in status. Duplicate deletion of the same in-flight target is blocked. If a long session reaches the retention limit, restart Shellboy as instructed.

Screen hints such as S/P/B use lowercase keys. See [history and search](history-and-search.md) for index target selection and preview.

### Read source text and links from details

F6 `会話の詳細` (conversation details) selects by item number, kind, and excerpt. Model answers, shell calls/results, and state changes are distinct; the selected content remains above the fields after Enter. O and C act on the Up/Down-selected target; returning to the list with F6 preserves selection. Enter `送信` (submit) in an action form sends a query/change request; required mutation confirmation appears separately.

1. In F6 (`/details`), select a history item and Enter its fields: node ID, tool arguments, state, before/after values, and more. Full IDs remain here rather than repeating in ordinary conversation rows.
2. Select fields with Up/Down and C to copy. A shortened value marked `[詳細は原文参照]` is not copied; proceed to source. Enter on Run/Turn/execution references opens the corresponding query form; node references open source. Use F8 `履歴 · 原文` for neighboring/source node IDs.
3. O refreshes the selected source; N requests the next source page and P the first. C copies **only the current source page**. This never loads all history or raw output at once. Pages are at most 64KiB, with continuation, neighboring, and source references. For full shell raw output, use F8 `shell · 原文` with execution, cursor, and encoding.
4. F7 (`/links`) or lowercase l in details selects HTTP(S) links for the current item. C copies the complete URL regardless of wrapping/short labels. Display does not connect or launch a browser. An empty link set for the current body window is stated explicitly.

Ordinary shell results separate stdout/stderr blocks only when verified from retained raw output; words such as “error” do not determine the stream. This is a bounded tail display preview; `出力の表示範囲` (output display range) in details reports remaining/uncollected portions. When streams cannot be identified because raw was reclaimed, execution reference is unknown, or display preparation exceeded its deadline, saved combined excerpts and the reason remain. This visual distinction never rewrites saved source from O or body copies.

F6 returns to the item list; Esc closes details without changing draft or original reading anchor. Reclaimed source shows a reason rather than an empty successful body.

Open links using standard terminal controls; modifiers, tmux, and mouse capture follow terminal settings. While TUI mouse capture is active, WezTerm normally bypasses it with Shift+click ([`bypass_mouse_reporting_modifiers`](https://wezterm.org/config/lua/config/bypass_mouse_reporting_modifiers.html) default); a user confirmed this on macOS. Other terminals or customized bindings may differ. If capture interferes, use F2 terminal selection or copy the full URL from F7. Shellboy does not claim to verify browser launch success.

### Recovery wait

Reasons, unapplied instructions, and saved tool results remain below the conversation separately from transient notifications. `/retry` retries the current recovery wait with the same content; typing and sending resumes with a new instruction; `/stop` or Ctrl+C stops. F8 Run retry/stop defaults to current work. See [resuming recovery wait](recovery-and-storage.md#resuming-recovery-wait).

### Management result fields and detail pages

Ordinary answer details emphasize known-format body, state, and references, simplifying single-array nesting and default internal values. V shows retained internal fields; O reads source. C preserves original text/ID values.

F8 actions, searches, attachment/index/storage management results use structured field names, values, and array counts rather than dumping full JSON. Tab / Shift+Tab select a field; Enter fetches its details. `すべての詳細を見る` (full result details) also exposes unknown fields beyond ordinary display limits.

C on a shortened `[詳細は原文参照]` value fetches details instead of copying; omission marks never become a purported complete clipboard value.

Inside details, N gets the next page, P the first, and Up/Down/PgUp/PgDn scroll the current page. C copies that page; Esc returns to fields, then Esc to the conversation. Fetch detail pages when a complete value is required rather than treating a preview as the full text. `この操作の受付ID` (`request_operation_id`) identifies that query/action itself and can be inspected with F8 `操作 · 受付結果`; it differs from message and Run IDs.

Only the most recent result's details are retained. A new result replaces old details; late requests for old results are discarded. If body-cache limits prevent retention, the UI reports it and explains refetching. Read source pages using the node/execution or narrow the query. This is not a route to credentials or opaque reasoning that was never returned.

Esc closes management display without cancelling an accepted operation. `/stop` stops the Run; `/cancel-wait` only stops observing a wait.

### Entering action forms

F8 (`/actions`, `/operations`) lists all 36 actions under `この会話で` (in this conversation), `よく使う` (common), and `すべての操作` (all actions). Type/paste up to 128 characters to filter human names, CLI names, and related slash actions. Choose with Up/Down and Enter, then enter target IDs/arguments per field. Conversation-state updates affect suggestions in the next form, not the target currently being edited; IDs opened from references also remain fixed. Suggestions do not guarantee acceptance: verify the target before submission. Shown slash actions are related shortcuts; `/attach` adds to the draft whereas F8 attachment-add is a saved operation.

`履歴 · 検索` accepts Thread, kind/excluded kind (space-separated), and creation from/to (offset-bearing RFC3339) filters. An empty Thread means global, never automatically current Thread. `/search QUERY` remains global. `履歴 · 関係` takes node, previous/next/sources/replacements, count, and cursor. Structured fields expose provenance, source revision, and indexed spans. See [history and search](history-and-search.md) for scope and relation semantics.

F8 `会話 · 削除影響` queries blocking Runs, reference counts, sharing, and update time for a Thread/descendant choice. It does not open actual deletion confirmation, and execution still requires a new deletion check. Reclaimable size is unestimated. Structured fields expose get-result context and operation targets/downstream. Distinguish acceptance from downstream completion.

Typing/paste appends to a field. Ctrl+U clears a suggested ID before replacement; Backspace removes its last character. Multiline request/stdin fields use Ctrl+J for newline. **Enter submits the action request, not a newline**: inspect pasted content before pressing it. Total form input is limited to 1MiB; overflow preserves existing values.

Use Left/Right/Space for booleans/choices. Empty-field semantics and required arguments vary per action; validation errors remain in the form. Thread deletion/storage reclamation require confirmation; index mutation requires target and cost preview. For other results too, distinguish acceptance from target completion. [CLI reference](cli-reference.md) explains the same conditions/states.

### Cancelling observation versus interrupting its target

In F8 `待機 · 条件を観測`, choose a condition kind and target ID. F9 adds it and clears the target field; F10 removes the last added condition. Submission includes any condition still in the target field and waits for any condition. Inspect result status and snapshot, including timeout and `recovery_wait`.

**Lowercase w** in management or `/cancel-wait` in conversation input cancels only observation of an explicit wait started by the TUI. Its Run/shell continues. Esc merely closes display. **Ctrl+C interrupts the current Thread**; do not use it merely to stop waiting.

### Terminal selection and body copy

F2 or `/select` freezes history rendering and releases mouse capture. Use terminal-native range selection/copy; F2 or Esc returns. OS/terminal copy shortcuts may intercept Ctrl+C, but if Shellboy receives it, it interrupts. Ctrl+Q begins shutdown even in selection mode.

`/copy` fetches DB source rather than a display placeholder. Whole-body copies over 64KiB fail with instructions to page via `history get`. Code-block indices/indentation use the same parser as Markdown display. An EOF-ended block is eligible if the entire source has been fetched. With an intermediate page, an unfinished trailing block is not copied; fetch more source first. See [configuration](configuration.md) for OS clipboard/OSC52 selection and remote-terminal limitations.

## AGENTS.md and Agent Guide

A Thread's `AGENTS.md` supplies work instructions read at each new Turn. The Agent Guide teaches Shellboy's model how to use the CLI; it is not automatically injected. When requesting delegation, name a real path for the model to read, as in [User Guide](user-guide.md#5-delegate-to-a-child). Do not treat this repository's root AGENTS.md as a universal Agent Guide for all users.

Attachments appear at the draft cursor as `[画像: ファイル名]` / `[添付: ファイル名]` (image/attachment chips). Left/Right move around them, allowing “description → image → description” order. Delete before or Backspace after a chip removes that draft attachment without deleting sent provenance or external files.

During sent-text recall with Up/Down, current unsent attachments move to the recalled text's end. Moving past the newest restores the unfinished draft text and original attachment positions. Use `/edit` to edit in the original request's attachment order, or `/restore` to restore a send snapshot to original positions.

## Reading saved receive records

`/received` lists receive records for the current conversation newest first, 32 per page. Specify a page with `/received 2`; previous/next list-page entries also open with Enter. The list shows count/page; select an item and Enter to read its saved JSON in the existing source viewer. N continues source pages, P returns to the first, C copies the current page. No memorized ID is required.

Failed requests display readable received reasoning/answers as `途中で切れた応答（保存済み・会話には含めない）` (interrupted response, saved but excluded from conversation). Differences between streaming and final content on successful requests are marked `ストリーム差分 · 最終応答は確定済み` (stream difference, final adopted). Ordinary conversation retains display windows for the latest two records; older records and omitted portions remain accessible in the list. Attention color applies only while the original Turn is in recovery wait, becoming a normal notification after recovery/stop. Local time and the same Turn's request excerpt identify records. Reselecting a conversation or restarting reconstructs display from saved records.

These are receive-audit records, excluded from adopted history resent to the model. Display windows extract text/reasoning excerpts rather than showing source JSON itself. Source retains partial text, completed items, final response, and failure/difference information. Encrypted reasoning is never expanded as screen text.

### Minimal failure recovery (#50)

`/compact-max` performs maximum safe API-free reduction without node IDs; it
does not retry. `/retry` is a separate potentially charged request. The compact
recovery line shows saved facts; F8 Run/Turn context has retry count, failure
time/stage, protected replay sizes and attachment sizes (null when unknown).
Only applicable edit/detach/restore choices are listed, without recommendations.
No recovery dashboard is added ahead of #41/#58.
