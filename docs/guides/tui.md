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

F8's `使用量 | 合計` reads the selected conversation's saved model usage and
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

Immediately after sending, local `送信受付中` (accepting submission) shows the frozen input. Once committed, one DB snapshot supplies both selected history and saved `反映待ち` (pending) rows above the composer. Saved input appears in one of these projections, never both. Pending rows include CLI/other-conversation senders and distinguish held/uncertain input and immediate delivery. `/queue` opens the full bounded list (128 rows, overflow count); Enter inspects delivery facts, O opens the original, D cancels one selected ID and A cancels only the displayed IDs, both at once with no Y step (see [Cancelling queued input](#cancelling-queued-input)); new arrivals are never included. Unknown historical admission is not automatically replayed.

The configured send key queues normally at the next boundary. Ctrl+Enter sends immediately unless it is already configured as ordinary send; Ctrl+X then Ctrl+S is always the immediate-delivery chord while the composer owns input. Immediate delivery releases a foreground shell wait and preserves its process; it does not interrupt response generation. Esc first closes popups, completions and selection; in the bare conversation it interrupts the exact observed Turn. Background shells and owned children continue. Ctrl+C or `/stop` stops the whole Run and its owned work. No automatic background timeout is enabled.

The footer status bar also reports slow or stalled DB-worker activity. After an
active operation exceeds 1 second it shows `DB <operation> <elapsed>`; when the
worker reports no progress it shows `DB! <operation> no progress`. This is a DB
activity indicator, not a percentage or estimate of overall task progress.

`準備中` (preparing), `モデル応答待ち` (awaiting model), `応答受信中` (receiving response), and `ツール処理中` (processing tools) use moving text brightness. `待機中` (waiting), `回答済み背景処理中` (answered, background work active), `実行継続中` (execution continuing), and `停止処理中` (stopping) use a slowly pulsing marker. Completed, interrupted, failed, and recovery-wait states are static; failure and recovery wait use an attention color.

When no explicit title is saved, the conversation header derives a short name
from a bounded prefix of the selected requests, then pending input. If earlier
source evidence falls outside the read bounds, `名称未設定（元本文の取得範囲外）`
means the header cannot safely derive the first request. It does not mean there
are no messages; inspect history or set an explicit title. This header read does
not rewrite the title or alter the CLI's title lookup.

The header orders state, conversation name, model, and working directory. Long names use at most half the width; when space runs out, trailing items are omitted first. Working directories favor their final components; inspect/copy the full value in F8 `会話 | 詳細`.

Display follows stored state and received events. Receiving response means actual text has arrived from the current model attempt. Unobservable details, such as the model's internal thought, are not guessed. The `抱えている作業` section shows saved wait targets when available and labels the timeout estimate as approximate. A Turn that has answered while owned shells or child Runs remain active shows answered/background-active. Use `/status thread` or F8 for details.

At most one thing moves in each region: the status line keeps its shimmer/pulse, the newest running foreground shell row (or, without one, the streaming answer row) shows a one-column spinner in place of its mark, and the `◉ 抱えている作業` heading mark changes brightness at most once per second. Animation never changes history text, wrapping, selection or scroll position; `[ui] motion = "off"` keeps every region on its static mark, and `NO_COLOR` does not blink. Animation redraws stop during selection/copy mode or overlays. Selection mode shows `表示固定` (display frozen), its UTC freeze-entry time when space permits, and exit keys at the top; F2/Esc resumes updates. The open conversation list updates saved row facts reactively; it has no running-state animation.

`観測経過` (observed elapsed time) uses a monotonic clock for the period during which this screen observed Turn activity. It excludes time before selection, before restart, and while state was unavailable; it stops at recovery wait or Turn termination. It differs from a tool header's saved execution duration.

The `◉ 抱えている作業` section above the composer shows the current Run's
owned shells, child Runs and saved pending waits, including after an answer.
`ui.live_work_rows` sets the oldest visible rows from 1 to 10 (default 3);
smaller terminals show a hidden count. Matching row numbers connect a wait to
its visible target. Shift+Tab focuses a row, Space opens a shell preview, F6
inspects it, and `o` opens a selected child. Unknown evidence remains unknown.

The heading is always shown, as `◉ 抱えている作業` plus `なし` (none) when there is no work
(no blink then). Alt+W or a click on the heading folds and unfolds the body; the
state is kept while the UI runs and starts open. An open body is always exactly
`ui.live_work_rows` rows, padded with blank rows, so the section height changes
only when you toggle it or resize the terminal. Alt+W is skipped if it is bound
as the send or newline key.

Each work row is drawn as a tree entry, with state and elapsed time at the
right edge. When the startup measurement confirms that ambiguous characters
are one column wide, branches use `├─` and `└─`, and metadata uses a spaced
middle dot (`·`). If the measurement reports two columns or is unavailable,
the branches use `+-` and `\-`, and metadata uses a spaced vertical bar (`|`).
Truncated work and tool rows use
`…` only when the measurement is one column; otherwise they use `.`. No
terminal width is guessed.
The one-column kind glyph is `$` for a shell, `↳` for a child Run, and `⋯`
for a saved wait. Selection uses reverse video and bold, without a caret.
These fixed-width glyphs do not depend
on color. An overflow line uses the last branch; blank padding has no connector.

The composer follows displayed line count, normally limited to one quarter of screen height and at most eight lines; small screens prioritize a three-line control area. Folded pastes become short numbered chips; expand/edit/remove with `/paste NUMBER show|edit|remove`.

## Recovery-wait notification bell

New recovery waits detected after startup trigger the terminal bell for background conversations as well as the current one. Existing startup recovery waits, rereads of the same state, conversation switches, and redraws do not ring. Bursts across conversations are limited to one notification per two seconds. Completion and retrying displays do not ring.

To disable the bell, add this configuration and restart the owner:

```toml
[ui]
bell = false
```

This uses terminal BEL. Sound, screen flash, or visual-bell behavior follows terminal settings. If inaudible, check those settings; multiplexers such as tmux also need bell forwarding/notification configuration. Shellboy does not change OS notifications or volume.

## Terminal program status

Shellboy reports saved activity using [OSC 7501 rev 0.4](https://www.superlogical.com/rex/docs/build/program-status)
on the same terminal, **between** cell frames. This does not alter the status row
or conversation renderer. It uses the existing activity classifier: active work
maps to working, recovery to blocked (auth/config failures may carry kind=auth),
failed work to error, unseen completion to done, and idle/stopped/viewed completion
to idle. The root summarizes every conversation, including ones outside the
32-child export cap; idle children are cleared. Normal exit sends a root clear.

Only final changed reports are written. Titles/messages have controls, bidi and
invisible-format characters removed, are UTF-8-byte bounded and standard-base64
encoded. Progress is not known and is omitted. Model/tool OSC bytes are stored
and sanitized as ordinary untrusted output, not forwarded as Shellboy reports.

Disable with `[ui] program_status = "off"` in TOML (put the field on a new line)
and restart. It then writes no OSC 7501, even at exit. There is no feature query,
receiver process or kitty/cmux/herdr-specific command/passthrough. Supporting
terminals decide how to present the records. Unknown OSCs should be ignored;
tmux may discard them. PTY/byte-capture tests do not qualify a physical receiver's
GUI: actual kitty/cmux/herdr and WezTerm/Terminal.app display remains separately
verified or explicitly unverified in the validation record.

With `program_status = "on"` (the default), older terminals, GNU screen and old Windows consoles
without VT mode may show raw control strings (untested); if this happens, set `program_status = "off"`.
Conversation titles and recovery-wait reason summaries are sent to the terminal, including raw
recordings (script/asciinema) and your local terminal when running Shellboy over SSH.
After forced termination, done/error records may remain by design; clear them in the terminal.
OSC 7501 rev 0.4 is young and its format may change.
Status changes trigger a read of all conversations, at most once per 0.5 seconds, which can be costly
with very many conversations; a separate read-only connection keeps these reads off the save queue.

## Input and basic keys

These are defaults. Change send/newline combinations with `ui.send_key` and `ui.newline_keys`. Shellboy requests terminal keyboard enhancement only when the terminal reports support. Shift+Enter is available only with that enhancement; on unsupported terminals, help omits it and Ctrl+J remains the fallback. Combinations producing the same terminal key code cannot be distinguished. While processing, the composer explains that additional instructions apply at the next boundary; during recovery wait, a new instruction resumes work.

| Key | Action |
| --- | --- |
| Enter | Queue draft at the next boundary, or execute a manually typed slash action |
| Ctrl+Enter / Ctrl+X then Ctrl+S | Immediate delivery; configured ordinary Ctrl+Enter keeps its normal role |
| Esc | Close the current popup/completion/selection first; otherwise interrupt the active Turn |
| Ctrl+J | Newline; available without keyboard enhancement |
| Shift+Enter | Newline when terminal keyboard enhancement is supported |
| Ctrl+U | Clear current draft text, pastes, and attachment references; preserve sent requests and external files (configured send/newline bindings take precedence) |
| Ctrl+L | Clear and redraw the whole screen; preserve draft, scroll, selection, and popup (configured send/newline bindings take precedence) |
| Left/Right / Backspace / Delete | Move/delete by character, accounting for combining characters and emoji |
| Up/Down | Select completion, recall sent text, or move between draft lines; see below |
| Tab | Complete the selected slash candidate |
| Alt+W | Fold or unfold the owned-work body (heading click does the same) |
| Shift+Tab | Focus visible owned work above the composer; Up/Down selects, Space toggles shell output, F6 inspects, `o` opens a child, Esc returns |
| Home / End | Start/end of the current draft line |
| Mouse wheel | Move history three display rows, measured after wrapping |
| PgUp / PgDn | Move history by viewport height minus one row |
| Ctrl+End | Refetch the latest history and resume following |
| Ctrl+T / Ctrl+N | Previous List/Watch mode / new draft |
| F1 | Task-oriented help, showing currently configured send/newline keys |
| F2 | Enter/leave terminal selection and copy mode |
| F3 / F4 / F5 | Inspect / edit / remove the latest paste block |
| F6 | Choose conversation content and inspect details/source |
| F7 | Links and complete URLs for the current history item |
| F8 | Action list and typed target/argument forms |
| Ctrl+C | Stop the current Run and its background/owned work; does not quit |
| Ctrl+B | Background the current foreground shell |
| Ctrl+Q | Begin normal shutdown |

With mouse capture enabled, clicking an expandable tool-call/result or grouped
notice row opens or closes it. Clicks on ordinary user/model message rows and
received-response rows do not select or toggle them. Use Tab/arrow keys and
Enter or Space to focus and open/close selectable history items.

Use Ctrl+L if stale characters or old link regions remain. It works in lists, details, popups, and frozen selection; repeated presses are coalesced within the render-rate cap. If it conflicts with send/newline bindings, redraw is disabled and F1 explains why. It does not repair underlying terminal character-width disagreement.

In selection/copy mode, Ctrl+L redraws the latest state once, including responses received after freezing; a following viewport may advance. Selection mode remains active and automatic redraw stops again afterward. This does not restore a snapshot of the screen at freeze time, request a new DB read, or change the recorded freeze-entry time.

Each Thread retains its draft, source-character anchor, and follow state. Movement is measured in display rows. At the end, history aligns to the last screen and follows; reading upward stops following. Wheels do not move the conversation behind help/details overlays. Offscreen bodies/layouts may be evicted from cache without deleting source history. `本文未キャッシュ` (body not cached) is fetched again according to scroll position.

At the older edge, wheel-up, PgUp and history-focus Up request earlier nodes even
when folded tools make the loaded page shorter than the screen. Closed tool
outputs are not expanded merely to move past their summary rows. Updates and
repaints keep manual reading paused; a zero-distance downward gesture on a
partial short page does not resume follow. Use Ctrl+End to return explicitly.


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

This observes saved state, not OS process liveness, in-flight searches, or standalone RAG stores. A completed Turn can still own background shells or child Runs; inspect Turn and Run states separately. Use F8 `作業 | 状態一覧` for filters such as failures, `/index status` for indexing, and CLI `shellboy rag status` for RAG.

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
| `/threads active` | Watch saved work across conversations |
| `/threads descendants` | Watch descendants of the selected conversation |
| `/switch THREAD_ID` | Switch by ID |
| `/model` | Current connection, model, and effort |
| `/replies` | Pending reply requests for this conversation; Enter selects an applied request for a text-only reply |
| `/queue` | Saved pending/held input; fixed-ID cancellation and original/delivery inspection |
| `/restore list` / `/restore draft ID` | Durable archived input and frozen submission recovery |
| `/models [CONNECTION]` | Live model list: type to filter, Up/Down select, Enter changes, Ctrl+R refreshes, Ctrl+Y copies selected ID, Esc returns |
| `/model CONNECTION MODEL_ID` | Change connection/model |
| `/effort VALUE`, `/effort none` | Set/clear effort |
| `/cwd`, `/cwd PATH` | Show/change working directory |
| `/fork [NODE_ID]` | New Thread sharing history through the specified node |
| `/edit [USER_NODE_ID]` | Load the original request into a separate editing draft; branch on send |
| `/rewind [USER_NODE_ID]` | Move this conversation back to just before an earlier request (same Thread, ID and title); the removed part stays as a child conversation, so opening that child undoes it. It asks for [confirmation](#rewind-from-the-history) first, like the `r` key. Conversation only: shell side effects are not undone. Refused while work is running or messages are queued. |
| `/branches [THREAD_ID]` | Open the branch panel: the tree of this conversation's branches (see [Branches](#branches)) |
| `/compact` | Request spill/summary at a safe boundary |
| `/search QUERY` | Conversation search in the active embedding space |
| `/index [status\|all\|thread\|threads ID...]` | Index management; `thread` means current Thread |
| `/index space SPACE_ID [status\|all\|thread\|threads ID...]` | Select management targets in a specified space |
| `/attach PATH` | Add a path reference to the draft as file input |
| `/attachments` | Draft attachments and saved send selections |
| `/detach ATTACHMENT_ID` | Exclude from draft or saved send selection |
| `/reload-attachments` | Explicitly reread saved Thread attachments, without API submission |
| `/image`, `/paste-image` | Add clipboard image to draft |
| `/copy [NODE_ID] [BLOCK_INDEX]` | With no arguments, copy the sole code block or choose a numbered block; explicit indices are zero-based |
| `/copy all` | Copy the focused/read-position item's whole canonical body |
| `/reasoning [NODE_ID]` | Expand/collapse returned reasoning text |
| `/select` | Toggle selection/copy mode |
| `/paste ID show\|edit\|remove` | Paste-block actions |
| `/restore` | Restore the input range of the newest currently selected omission marker; preserve subsequent conversation, without automatically reviving attachments |
| `/restore REVISION` | Restore failed/cancelled draft snapshot |
| `/cancel` | Interrupt the current Thread |
| `/background` | Background the foreground shell |
| `/quit` | Normal shutdown |

Omitting the node in `/fork` suggests the anchor or tail; `/edit` and `/rewind` suggest the anchor or latest user node. Invalid node kinds/boundaries are errors. Specify IDs for reproducible actions. `/edit` never overwrites the old node: sending branches immediately before the original request. If a Run is active or stopping, sending is refused; stop it or wait for it to finish, then retry the retained draft.

## Branches

A branch is a child conversation (a Thread) made by `/fork`, `/edit` or `/rewind` that shares the start of its parent's history. Nothing is stored for the tree itself: it is read from the saved conversations (`shellboy thread branches` returns the same facts as JSON).

**In the conversation.** Where branches diverge, one muted row follows the last shared message: `↳ 2/3 ⋯ current "make CI pass"`. `2/3` is the alternative shown (the order is by when its first message was written); `current` marks the one this conversation follows, `branch` another (Japanese: `今の会話`, `別の会話`; the panel uses the same words, plus `parent` / `親`) and the quoted text is that alternative's last request (for the current one, this conversation's own last request). A conversation that was rewound shows itself ending at the point next to the removed part. Select the message in history focus (Tab, then Up/Down); `←` / `→` move through the alternatives (previewing never switches), and Enter opens the previewed one when it is not the current one (on the current alternative Enter keeps its usual meaning). The marker belongs to the node where the paths diverge: the arrow keys and Enter act when that node is the selected history item (a node folded into a grouped item, such as a notice group, is reached through its own item). Markers are read with each history page of up to 128 nodes; a branch made after the page was loaded appears when the page is read again.

**Branch panel.** `/branches` or F8 `会話 | 分岐の木` opens it (F11 and F12 are not used). Rows are drawn with `├─ └─ │` (`+- \- |` when the terminal does not draw them as single-width): each conversation, then its divergence points in path order (the quoted request at the point), then the branches that diverged there; only points and tips are listed. A row shows the state (`current`, `parent`, `branch`; `running`; `pruned` when the originals were released with `thread prune`), the last request, and, at 64 columns or more, the time and number of messages. Below 64 columns those move to the detail lines under the list. Keys: Up/Down/PgUp/PgDn/Home/End select, Enter opens the selected conversation, F forks from its tip (`thread fork`), P opens the prune form (`会話 | 原文の解放`) with that conversation and its tip filled in and the dry run selected, N reads the next page, Esc closes. At most 256 conversations are listed per family, read in pages of 200; when the limit cuts the tree the panel says so. Comparing branches is not part of the panel yet.

## Rewind from the history

In the conversation, press Tab and Up/Down to select one of your own requests, then press **`r`** (shown as `r Rewind` in the key help and listed in F1 under the rewind entry). The conversation goes back to just before that request: the same as `/rewind` and `shellboy thread rewind --exclude-node`. `r` does nothing on other items (the status says to select a request). No other key of the history focus uses `r`.

Nothing changes yet. The owner reads the facts with `shellboy thread rewind --dry-run` (the same rewind run and rolled back, so refusals such as a live Run or unknown path admission appear here too) and Shellboy shows a confirmation:

```
Rewind this conversation to just before this request?

Removed range: 2 turns, 14 nodes
Not undone:
  Shell executions after this request: 3
  File changes are not undone
  Child conversations started after it: 1
  Pending replies to sent messages: none
  Attachment selections removed from this conversation: 0 (the branch keeps them)
  Background shells and replies may still arrive here later
Undo: the removed part stays as a branch conversation. Right after rewinding, Alt+U opens it (so does /branches)
```

The Japanese screen shows the same facts (`外れる範囲`, `戻らないもの`, `取り消し`). Y rewinds; N or Esc cancels without writing anything. Counts come from the owner; the confirmation is not shown if the owner's answer lacks one of them. Shell executions and child conversations are counted from the time of the request; file changes made by shell commands are never undone.

After Y, the footer notice reads `Rewound to before the request. Alt+U undoes it (opens the removed branch)`, and `Alt+U` appears in the key help while the input is empty. **Alt+U** opens the branch conversation that holds the removed part (the same conversation `/branches` lists); it works once, only with an empty draft, and only "right after" the rewind: the offer ends at the next submission in that conversation, when its selected path changes, or when you leave it. It does not work when the configured send or newline key is Alt+U. If the rewind result arrives after you left the conversation, no offer is made; if the removed range differs from the one you confirmed, the notice says so. Plain letters cannot be used there because they would be typed into the draft.

## Overlays and selection mode

Popups share a frame, heading, explanation, body, and bottom controls. `›` plus background color marks selection; controls remain visible while scrolling. `.` indicates omitted middle content when an item cannot fit, but source, copy values, and execution arguments remain intact. Read full content through details/source. Forms expose the end of input and submit its whole value.

Ordinary lists, detail fields, and forms use Up/Down for selection and PgUp/PgDn for viewport-sized selection movement. Source, help, and result bodies scroll one display row with Up/Down and one page with PgUp/PgDn. PgDn in the conversation list continues fetching the next page. Confirmation screens scroll long explanations with Up/Down/PgUp/PgDn; only Y executes, while N/Esc return.

| Screen | Keys |
| --- | --- |
| Help | Up/Down/PgUp/PgDn scroll; Esc/Enter/F1 close |
| Conversation list | Text/Backspace filter; Up/Down select; Left/Right collapse/expand/navigate parents and children; Home/End first/last; Enter switches; PgDn adds next page; Ctrl+R refreshes; F1 information; Delete/Ctrl+D deletion confirmation; Ctrl+A archives the selected conversation and its descendants (unarchives in the archived list); Ctrl+G switches between the list and the archived list; Esc returns |
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
| Queued input (`/queue`, held rows) | D / A cancel at once; the status line reports the outcome and `/restore list` holds the text |
| Shutdown grace exceeded | W/Enter wait; F force confirmation, then Y force quit |

### Typed operation forms

F8 uses one catalogue for names, meanings, CLI identities, effects and field help.
[変更] changes saved data; [制御] starts or controls processing. Group headings are
not actions and disappear while filtering. Enter says display for inspection,
execute for changes/control, and send only for messages. F1 explains the selected
field's type, initial-value source and how to find an ID. Run inspection reads
saved state; it does not retry or stop it. Docs/config/RAG/exec use the same typed
clients as the CLI. Inline input belongs to the form; the TUI never consumes
terminal stdin. Explicit confirmations retain their existing cost/target guards.

The wait form also supports thread_settled: enter Thread IDs separated by spaces
and the observed states in its state field. After changing a condition, specify
an ID of the matching type; the initial target is the Turn captured when opened.
The shell-output cursor and the previous wait's after cursor are different fields.

Wheel input affects only the top popup under its painted bounds. Outside, at an
edge or during F2 terminal selection it is a no-op. Read-only pages reuse wrapped
rows until their source revision or width changes; a cache limit does not change
the original source/copy target. Fetching a later page remains explicit.

The conversation list uses one row per conversation with name, working directory, state, and update time columns. Filter text appears immediately; fetching follows 150ms without typing. Ctrl+R refreshes immediately. An empty saved title displays a short name from the first user request, searchable by that name, without rewriting explicit titles or history. Branch lines show ancestry; `▾` / `▸` mean expanded/collapsed, `●` marks the open conversation, and `›` with background marks the selected item. Left collapses children or moves to the parent when already collapsed/a leaf. Right expands or enters the first child when already expanded. Up/Down traverse visible rows and keep selection in view.

In F8 `会話 | 設定` (conversation settings), set `タイトル操作` (title action) to `set` and enter a title to rename. `auto` clears the manual name and returns to conversation-derived display; `keep` preserves it. CLI: `shellboy thread set --thread THREAD --title 'Name'`; clear with `--title ''`.

Fixed bottom details show the selection's cwd, ID, parent, state, and a one-line excerpt of roughly at most 80 characters with Markdown/newlines normalized. Narrow screens omit columns; details or F1 show complete values. Enter opens the conversation; F6 reads source.

`[親は一覧外]` (English `[Parent not in this list]`) means the parent is not among the rows of this list: outside the fetched page or filter, or archived while the child is not (or the reverse in the archived list). Adding pages with PgDn groups children under newly fetched parents. Fetched rows group ancestry based on update order; this is not a guaranteed complete tree. At 2000 entries, narrow the filter. Ctrl+R retries failed fetches. Searching/browsing does not stop Thread work.

### Watch work and descendants

Tab switches the same conversation browser between List and Watch while keeping
its filter and selection when available. `/threads active` opens Watch directly;
`/threads descendants` or Alt+D scopes it to the selected saved conversation's
descendants. A configured send/newline binding takes precedence over Alt+D.
These are TUI slash commands, not CLI subcommands. Ctrl+T reopens the last
List/Watch mode in this UI process; a new process starts in List. It reopens
global Watch, not a previous descendant root or filter. `/threads` explicitly
opens List regardless of the remembered mode.

Watch shows the same conversations as List (same scope, filter, limits and
PgDn paging) and changes only the order and the row presentation. It is a flat
priority view: recovery, failed, unread completion, active work and background
work first, then stopped conversations, read completions and idle ones, with
saved details below each title. A stopped row reads `13:42 中断済み`, List's word for the state (the saved
stop time; `時刻不明` when none was saved). When the saved stop reason records
an app exit or an owner restart, the row says `中断済み（アプリ終了）`; a stop with
any other or no saved reason is never labelled that way. After an owner restart
the interrupted conversations therefore stay visible and can be reopened. Enter opens a
conversation. Ctrl+S confirms a full Run stop; Alt+R confirms retry of a recovery
Turn; Alt+O reads saved output. Confirmation retains the actual Run/Turn/generation:
a changed target is rejected rather than retargeted. Typing filters; deletion
stays in List. Esc closes the browser without stopping work.

Thread rows may include a compact completion-delivery summary: `↻N` counts
messages delivered at Run completion, and a trailing `+` means at least one
`--deliver complete` message is still waiting (`↻N+`). No badge is shown when
there is neither a delivered nor waiting completion-delivery message.

Finished rows (completed, failed, stopped, and recovery waits) show the saved
transition time in local time instead of a growing counter: `13:42 完了`, or
`10/31 22:05 完了` when it is not today. It never changes as time passes. When
the transition time was not saved (only observed in this UI), the row says
`時刻不明` rather than inventing one. A recovery wait uses the failure time
saved in its recovery reason when present. Running rows keep an elapsed time that
rolls up units (`59秒`, `4分12秒`, `1時間3分`). A completed row is always `完了`;
an unread one adds a mark (`完了 ●`). When only finished rows are visible, the
clock never redraws the screen.

Unread tracks completed Run/answer identities only in this UI process. Opening
a completion marks it seen; it then moves below stopped rows but is not
removed. A new process starts with no seen identities, so earlier completions
reappear as unread there. The bounded 2000-identity history may become uncertain after
eviction; changing a title does not create a new unread answer. Nothing expires
merely because time passed.

Descendant counts follow parent Threads, independently of Run ownership. Active
and recovery counts are separate; exact zeros are hidden. Depth8/discovery256
bounds produce an explicit lower bound or unknown status when truncated/cyclic.
Retained grandchildren can remain reachable through a deleted intermediate; a
missing/deleted root is unavailable. Counts and visible details update from saved
changes; local age labels do not poll the database. An observed age is not an
invented saved transition time.

PgDn requests another discovery page. A bounded-title or candidate-scan notice
can require continuation even when the current page has no matching rows; it is
not proof that no work exists. Large previews/commands stay explicitly unavailable
with a route to the conversation/detail; the preview limit does not delete source
history. Normal source availability/GC rules still apply.

### Deleting from the conversation list

In `/threads` List mode, select a saved conversation and press Delete or Ctrl+D for impact and confirmation. On macOS, fn+delete means Delete; ordinary delete/Backspace still removes filter characters.

Ctrl+A in List mode archives the selected conversation together with its descendants: they leave the list but nothing is deleted, history, search and usage are unchanged, and a Run in progress keeps running. A notice reports how many conversations were archived. Ctrl+G opens the archived list (titled `Archived conversations`), where Ctrl+A unarchives the selection and its descendants; Ctrl+G returns. A new Run on an archived conversation returns it to the list. To move a conversation under another one (or detach it), use F8 `会話 | 設定` and fill `親会話` with the new parent's conversation ID (`none` makes it independent); the list tree rebuilds with the whole subtree under the new parent. The status line shows the usual `Settings changed` notice (connection and model), which does not name the parent; read the parent in the list tree or F1. A running conversation can be moved; its running work keeps its owner. Watch is not filtered by archive state: an archived conversation with live work still appears there. Unsent drafts cannot be archived. Selections over 256 conversations are refused. The `shellboy thread archive` commands do the same from a shell with explicit IDs.

Deleting child conversations is off by default. Toggling D/Space requeries impact. Only Y with the latest impact permitting deletion executes; N/Esc cancel. Activity in other conversations never disables Y or discards the open review: the counts and `Reviewed at` time are as of the preview, and the owner checks the target set and live Runs again when Y writes. A truly stale target (changed set, or a Run that started) is refused with the reason on screen; a changed set re-previews and needs a new Y, and a live Run leaves Y disabled until you stop it and review again. Reasons explain live/stopping Run blockers and similar restrictions. State is checked again during execution even after confirmation, and failures remain in the list.

Deleting a background conversation selects an adjacent list row while leaving the open conversation intact. Deleting the open one discards its TUI draft state and moves to a new draft. This does not collectively delete shared source history or external attachment files; history reclamation follows existing GC. Unsaved draft rows are not handled by Thread deletion.

When deletion is confirmed but only subsequent cleanup fails, the UI distinguishes `削除しました。後片付けに失敗` (deleted; cleanup failed). It does not restore the deleted conversation or reinterpret the result as inability to delete. Failures/warnings arriving after the list closes remain in status. Duplicate deletion of the same in-flight target is blocked. If a long session reaches the retention limit, restart Shellboy as instructed.

Screen hints such as S/P/B use lowercase keys. See [history and search](history-and-search.md) for index target selection and preview.

### Read source text and links from details

F6 `会話の詳細` (conversation details) selects by item number, kind, and excerpt. Model answers, shell calls/results, and state changes are distinct; the selected content remains above the fields after Enter. O and C act on the Up/Down-selected target; returning to the list with F6 preserves selection. Enter `送信` (submit) in an action form sends a query/change request; required mutation confirmation appears separately.

1. In F6 (`/details`), select a history item and Enter its fields: node ID, tool arguments, state, before/after values, and more. Full IDs remain here rather than repeating in ordinary conversation rows.
   For a model answer, the fields include the request that produced it: `入力のバイト数（UTF-8）` (Input bytes (UTF-8)) gives the total and each nonzero source (system, instructions, Shellboy context, user, assistant, reasoning, tool calls, tool results, spill placeholders, summary, notices, attachments, other) measured from the body actually sent; `入力トークン（提供元の報告）` (Input tokens (provider-reported)) and `キャッシュ済み入力トークン（提供元の報告）` (Cached input tokens) are the provider's totals. Bytes and tokens are different units and are never converted into each other. `未記録` (Not recorded) means the bytes were not saved (requests before this feature); `未報告` (Not reported) means the provider gave no count. `shellboy usage get` shows the same facts.
2. Select fields with Up/Down and C to copy. A shortened value marked `[詳細は原文参照]` is not copied; proceed to source. Enter on Run/Turn/execution references opens the corresponding query form; node references open source. Use F8 `履歴 | 原文` for neighboring/source node IDs.
3. O refreshes the selected source; N requests the next source page and P the first. C copies **only the current source page**. This never loads all history or raw output at once. Pages are at most 64KiB, with continuation, neighboring, and source references. For full shell raw output, use F8 `shell | 保存出力` with execution, cursor, and encoding.
4. F7 (`/links`) or lowercase l in details selects HTTP(S) links for the current item. C copies the complete URL regardless of wrapping/short labels. Display does not connect or launch a browser. An empty link set for the current body window is stated explicitly.

Ordinary shell results separate stdout/stderr blocks only when verified from retained raw output; words such as “error” do not determine the stream. This is a bounded tail display preview; `出力の表示範囲` (output display range) in details reports remaining/uncollected portions. When streams cannot be identified because raw was reclaimed, execution reference is unknown, or display preparation exceeded its deadline, saved combined excerpts and the reason remain. This visual distinction never rewrites saved source from O or body copies.

F6 returns to the item list; Esc closes details without changing draft or original reading anchor. Reclaimed source shows a reason rather than an empty successful body.

### Open URLs and file paths with your terminal

Opening a URL or a `file://` file/folder path in displayed text is a **terminal
feature**, not a Shellboy action. In WezTerm, use Shift+click: Shellboy captures
mouse events, and Shift normally bypasses application mouse reporting
([`bypass_mouse_reporting_modifiers`](https://wezterm.org/config/lua/config/bypass_mouse_reporting_modifiers.html)).
WezTerm's default hyperlink rules recognize `file://` URLs. In iTerm2, use
Cmd+click; custom bindings and multiplexers such as tmux can change the gesture.
The terminal and OS decide which application opens the target and how it opens.
Shellboy does not check whether a path exists or launch a browser, editor or
file manager.

For bare paths without `file://`, add a suitable terminal rule. For example,
this minimal WezTerm Lua configuration keeps the default URL rules and adds
simple ASCII absolute POSIX paths such as `/tmp/report.md`:

```lua
local wezterm = require 'wezterm'
local config = wezterm.config_builder()
config.hyperlink_rules = wezterm.default_hyperlink_rules()
table.insert(config.hyperlink_rules, {
  regex = [[(?:^|[\s(])(/[A-Za-z0-9_./-]+)(?:$|[\s)])]],
  format = 'file://$1',
})
return config
```

Merge the rule into your existing configuration rather than replacing unrelated
settings. Paths with spaces, non-ASCII names, relative paths or Windows drive
paths need a complete `file://` URL or a rule adapted to their form. See
WezTerm's [`hyperlink_rules`](https://wezterm.org/config/lua/config/hyperlink_rules.html)
for terminal-side customization. These rules do not validate paths.

If mouse capture interferes, use F2 terminal selection to copy displayed text;
F7 can copy full HTTP(S) URLs, but its link list does not include file paths.
Terminal settings and OS behavior still apply; Shellboy does not verify opening
success.

### Recovery wait

Reasons, unapplied instructions, and saved tool results remain below the conversation separately from transient notifications. `/retry` retries the current recovery wait with the same content; typing and sending resumes with a new instruction; `/stop` or Ctrl+C stops. F8 Run retry/stop defaults to current work. See [resuming recovery wait](recovery-and-storage.md#resuming-recovery-wait).

### Management result fields and detail pages

Ordinary answer details emphasize known-format body, state, and references, simplifying single-array nesting and default internal values. V shows retained internal fields; O reads source. C preserves original text/ID values.

F8 actions, searches, attachment/index/storage management results use structured field names, values, and array counts rather than dumping full JSON. Tab / Shift+Tab select a field; Enter fetches its details. `すべての詳細を見る` (full result details) also exposes unknown fields beyond ordinary display limits.

C on a shortened `[詳細は原文参照]` value fetches details instead of copying; omission marks never become a purported complete clipboard value.

Inside details, N gets the next page, P the first, and Up/Down/PgUp/PgDn scroll the current page. C copies that page; Esc returns to fields, then Esc to the conversation. Fetch detail pages when a complete value is required rather than treating a preview as the full text. `この操作の受付ID` (`request_operation_id`) identifies that query/action itself and can be inspected with F8 `操作 | 受付結果`; it differs from message and Run IDs.

Only the most recent result's details are retained. A new result replaces old details; late requests for old results are discarded. If body-cache limits prevent retention, the UI reports it and explains refetching. Read source pages using the node/execution or narrow the query. This is not a route to credentials or opaque reasoning that was never returned.

Esc closes management display without cancelling an accepted operation. `/stop` stops the Run; `/cancel-wait` only stops observing a wait.

### Entering action forms

F8 (`/actions`, `/operations`) lists all 36 actions under `この会話で` (in this conversation), `よく使う` (common), and `すべての操作` (all actions). Type/paste up to 128 characters to filter human names, CLI names, and related slash actions. Choose with Up/Down and Enter, then enter target IDs/arguments per field. Conversation-state updates affect suggestions in the next form, not the target currently being edited; IDs opened from references also remain fixed. Suggestions do not guarantee acceptance: verify the target before submission. Shown slash actions are related shortcuts; `/attach` adds to the draft whereas F8 attachment-add is a saved operation.

`履歴 | 検索` accepts Thread, kind/excluded kind (space-separated), and creation from/to (offset-bearing RFC3339) filters. An empty Thread means global, never automatically current Thread. `/search QUERY` remains global. `履歴 | 関係` takes node, previous/next/sources/replacements, count, and cursor. Structured fields expose provenance, source revision, and indexed spans. See [history and search](history-and-search.md) for scope and relation semantics.

F8 `会話 | 削除影響` queries blocking Runs, reference counts, sharing, and update time for a Thread/descendant choice. It does not open actual deletion confirmation, and execution still requires a new deletion check. Reclaimable size is unestimated. Structured fields expose get-result context and operation targets/downstream. Distinguish acceptance from downstream completion.

Typing/paste appends to a field. Ctrl+U clears a suggested ID before replacement; Backspace removes its last character. Multiline request/stdin fields use Ctrl+J for newline. **Enter submits the action request, not a newline**: inspect pasted content before pressing it. Total form input is limited to 1MiB; overflow preserves existing values.

Use Left/Right/Space for booleans/choices. Empty-field semantics and required arguments vary per action; validation errors remain in the form. Thread deletion/storage reclamation require confirmation; index mutation requires target and cost preview. For other results too, distinguish acceptance from target completion. [CLI reference](cli-reference.md) explains the same conditions/states.

### Cancelling observation versus interrupting its target

In F8 `待機 | 条件を観測`, choose a condition kind and target ID. F9 adds it and clears the target field; F10 removes the last added condition. Submission includes any condition still in the target field and waits for any condition. Inspect result status and snapshot, including timeout and `recovery_wait`.

**Lowercase w** in management or `/cancel-wait` in conversation input cancels only observation of an explicit wait started by the TUI. Its Run/shell continues. Esc merely closes display. **Ctrl+C interrupts the current Thread**; do not use it merely to stop waiting.

### Terminal selection and body copy

F2 or `/select` freezes history rendering and releases mouse capture. Use terminal-native range selection/copy; F2 or Esc returns. OS/terminal copy shortcuts may intercept Ctrl+C, but if Shellboy receives it, it interrupts. Ctrl+Q begins shutdown even in selection mode.

`/copy` fetches DB source rather than a display placeholder. With no arguments it targets the focused item, then the reading position, then the latest displayed answer. A sole code block is copied directly; multiple blocks open numbered choices; no code copies the body. `/copy all` explicitly copies the whole body. With item focus, B opens the same numbered choices: press a digit 1-9 or select with Up/Down and Enter/C. Captions and choices use one-based numbers; explicit `/copy NODE_ID BLOCK_INDEX` retains zero-based indices. C on a focused item still copies its whole body.

Preparation inspects at most 1MiB, 64 canonical pages and 128 code blocks on existing workers. Every clipboard payload, including a single block or whole body, remains bounded to 64KiB. Incomplete/unavailable/over-limit source fails without copying a prefix; O and N or `history get` expose pages. A chosen source is reread and digest-checked before copying. Code indentation and order use the same parser as Markdown display, including EOF-ended code only when the entire source is fetched. Feedback reports a block number and Unicode character count. OSC52 reports only that a request was sent, not confirmed OS clipboard completion. See [configuration](configuration.md) for remote-terminal limitations.

## AGENTS.md and Agent Guide

A Thread's `AGENTS.md` supplies work instructions read at each new Turn. The Agent Guide teaches Shellboy's model how to use the CLI; it is not automatically injected. When requesting delegation, name a real path for the model to read, as in [User Guide](user-guide.md#5-delegate-to-a-child). Do not treat this repository's root AGENTS.md as a universal Agent Guide for all users.

Attachments appear at the draft cursor as `[画像: ファイル名]` / `[添付: ファイル名]` (image/attachment chips). Left/Right move around them, allowing “description → image → description” order. Delete before or Backspace after a chip removes that draft attachment without deleting sent provenance or external files.

During sent-text recall with Up/Down, current unsent attachments move to the recalled text's end. Moving past the newest restores the unfinished draft text and original attachment positions. Use `/edit` to edit in the original request's attachment order, or `/restore` to restore a send snapshot to original positions.

## Reading saved receive records

`/received` lists receive records for the current conversation newest first, 32 per page. Specify a page with `/received 2`; previous/next list-page entries also open with Enter. The list shows count/page; select an item and Enter to read its saved JSON in the existing source viewer. N continues source pages, P returns to the first, C copies the current page. No memorized ID is required.

Failed requests display readable received reasoning/answers as `途中で切れた応答（保存済み・会話には含めない） | /received 一覧` (interrupted response, saved but excluded from conversation). Differences between streaming and final content on successful requests are marked `ストリーム差分 | 最終応答は確定済み | /received 一覧` (stream difference, final adopted). Ordinary conversation retains display windows for the latest two records; older records and omitted portions remain accessible in the list. Attention color applies only while the original Turn is in recovery wait, becoming a normal notification after recovery/stop. Local time and the same Turn's request excerpt identify records. Reselecting a conversation or restarting reconstructs display from saved records.

These are receive-audit records, excluded from adopted history resent to the model. Display windows extract text/reasoning excerpts rather than showing source JSON itself. Source retains partial text, completed items, final response, and failure/difference information. Encrypted reasoning is never expanded as screen text.

### Minimal failure recovery (#50)

`/compact-max` performs maximum safe API-free reduction without node IDs; it
does not retry. `/retry` is a separate potentially charged request. The compact
recovery line shows saved facts; F8 Run/Turn context has retry count, failure
time/stage, protected replay sizes and attachment sizes (null when unknown).
Only applicable edit/detach/restore choices are listed, without recommendations.
No recovery dashboard is added ahead of #41/#58.

### Conversation row legend

The help legend and conversation renderer share one mark table. `▸` is an
answer, `▹` a request, `⏵` a running tool, `◌` an unavailable observation,
`✓`/`✗`/`⊘` terminal outcomes, `⁂` a notification, `⬦` a summary and `>` a
spill. Input-source marks keep their explicit human/parent/model labels.
Background shell rows say so and show the saved background reason when known;
live seconds use the saved shell start, not the latest model Turn start.
Select a notification with Tab/arrow keys and press Enter to follow its loaded
shell row or child conversation. A shell outside the bounded displayed history
reports that limit; F6 retains the full saved references. F7 remains HTTP links.
Unlike row groups have nonselectable spacing; consecutive tools stay dense.
The `◉` work-region heading is reserved for #86, not drawn by this change.

### 要約の区切り行（#230）

要約が採用された位置には `⬦` の1行が閉じた状態で出る。開く（Enter）と要約本文を読める。
例（日本語、続いて英語）:

```text
⬦ 自動要約：205K で発動 · 412 ノード → 要約 1 件（31K 字） · 退避 8 件 · 費用 in 30K / out 2K · 要約後の入力 32K
⬦ Auto summary: triggered at 205K · 412 nodes → 1 summary (31K chars) · 8 spilled · cost in 30K / out 2K · input after 32K
```

手動は `手動要約：` / `Manual summary:` で始まり、発動の入力量を持たない。
`費用` は要約の要求のトークン数（金額ではない）。報告されていない値は `不明`、
一部の要求だけ報告がないときは `30K+`。`要約後の入力` は採用後の最初の要求で提供元が
報告した入力で、まだ要求がなければ `次の要求待ち`、報告がない・まだ終わっていなければ `未報告または未完了`。
複数の要約が1回の要求の前に続いたときは同じ要求の入力を共有する（効果を足し合わせない）。
同じ値は `shellboy compact list` の `post_compaction_input_tokens` で読める。
自動要約が見送られた・採用されなかった試行は、その応答の直後に `⁂` の1行で出る
（理由・そのときの入力量と閾値・次に発動する条件）。次の条件は保存された判定の規則
（保存された履歴に新しい・増えた要約対象が現れるまで再試行しない。後の Turn や入力トークンの増加だけでは再試行しない）であり、割合の閾値ではない。
狭い幅では折り返す。印は既存の語彙（`⬦` `⁂`）だけを使う。

## Context size in the footer

The footer's first row ends with the current Thread's latest adopted model
attempt, for example `^140K/200K v20K キャッシュ86%`.

- `^` is the provider-reported input tokens, `v` the output tokens, and
  `キャッシュN%` is cached tokens divided by input tokens. Nothing is estimated:
  an unreported value shows `-`, the percentage is omitted when either side is
  unknown, and `^-` stays until the first response is adopted. After `/model`
  the value is `^-` again until the new model answers.
- `/200K` is the effective `auto_at_tokens` of the Thread (the Thread's own
  `thread set --auto-compact-at`, else the connection's setting, else
  `[compaction] auto_at_tokens`). When it is `0`
  (proactive compaction off) the denominator is not shown.
- Units: below 1000 as is, then `10.2K`, `140K`, `1.2M` (rounded down).
- At 80% or more of the threshold the text is drawn in the attention color; with
  `NO_COLOR` it is prefixed with `!` instead.
- Narrow terminals follow the footer's existing rule (other facts keep their
  space; this text uses what is left) and drop `キャッシュ%`, then `v`, then
  `/閾値`, then `^`.
- The value comes from the saved usage already read with each current-view
  snapshot (`context.last_successful_attempt`). It changes only when a response
  is adopted; no clock redraw, extra query or table is involved.

## Activity phase and elapsed time

The footer's request age belongs to the current model attempt, not the whole
Turn. Tool activity may be shown alongside request waiting/reception; the two
labels have separate ages. `観測` marks a local observation interval, not a
persisted phase-start fact, and unavailable timestamps are not reported as zero.
Reception means observed transport headers/body liveness, not proof of generated
assistant text. Saved recovery disappears after confirmed resume/completion/stop;
unrelated failed operations remain visible until their own handling or dismissal.

A saved usage-limit recovery shows its kind and reset time in UTC, plus waiting
and `/model` guidance. A passed reset time does not automatically resume work.
Choosing a different model on the same connection does not bypass that
connection's remembered limit; original reason/details remain available.

Elapsed labels share one formatter: `45秒`, `5分12秒`, then `1時間3分`.
Tool rows keep tenths below a minute (for example `0.4秒`). Numeric saved facts
and originals remain unchanged. Terminal activity labels show the saved Run end
time, for example 完了 | 21:16 (or a date on other local days), not a new observation time;
missing/invalid end facts are marked unavailable. Live/recovery phases do not
reuse a previous end time. An idle clock does not trigger a redraw just for dates.

An in-flight summary/context edit can hold the safe request boundary after a
foreground command has finished. The footer shows the saved boundary owner's
reason (for example 会話要約の完了待ち); `run get` supplies its operation ID/action
and acceptance time. Acceptance time is not a measured lock-wait start. Shell
completion can already be saved while its model-facing event waits for that next
request boundary. This does not mean the driver is waiting for a background shell
or that replay should be forced; inspect the protected operation's downstream
result. Finishing/failed edits remove this current reason without a new timer.

### Notification Reference Metadata

Notification headers can show a short saved shell command and its background
request flag, or a child conversation's explicitly labelled current name. Names
are not historical identity. Deleted or missing references remain unavailable.
Enter follows an exact loaded execution row; otherwise it opens the saved tool
result's original source when its exact group/call/node relation is available.
Child navigation uses the saved Thread ID, never a similar short name.

## Replying to a request

`/replies` reuses the pending-message popup and shows up to 128 pending requests
for the current conversation, with sender, time and original excerpt. Enter
selects an applied request; queued or held requests cannot yet be replied to.
The composer shows `Replying to` and the original excerpt. Type text and use the
configured send key; this sends `message reply`, not an ordinary message. Replies
cannot contain attachments or edit an existing message. Esc leaves reply mode
without discarding the draft. A failed or raced reply preserves the draft for
inspection; a successful acknowledgement clears only its submitted revision,
not newer edits or another selected reply target.

Option B retains existing sender glyphs. Original rows and queued requests show
`awaiting reply`, `replied`, `ignored`, `cancelled`, or `recipient deleted` as
saved state tags. A reply row shows `Reply:` and a width-limited original excerpt.
Japanese UI uses `返事待ち`, `返事済み`, `返事なし`, `取消`, `相手削除`, and `返事:`.
No new reply glyph is introduced.

## Held Input Region

The fixed `◉ Held input` region follows live work and precedes the composer.
Its heading persists as `None` when empty and never blinks. The body reserves
`ui.held_input_rows` rows (1..10, default 3), independently of live work; small
screens can show fewer rows with an explicit hidden count. Shift+Tab traverses
the displayed live-work rows, then held rows, then returns to the composer.
Alt+W folds the last focused region; clicking either heading folds that region.
A focused row uses reverse video and bold without a caret, even without colors.

Held marks use saved sender/completion facts: person `›`, external CLI/unknown
`⇥`, model `↳`, completion-scheduled `↻`. State and hold reason appear on the
right; time is omitted before the reason is shortened. Space opens a saved
original, F6/Enter requests status, and `d` / `a` cancels the
selected / actually displayed saved IDs immediately (no confirmation). Submitting local drafts have bounded
excerpts and no invented saved cancellation ID. `/queue`, `/replies` and draft
restore remain available for the wider saved-input and reply workflows.

## Cancelling queued input

In `/queue` and the held rows above the composer, D (selected) and A (displayed IDs) cancel immediately; there is no Y step. A mis-key is recoverable: each cancelled input is archived, the status line reports the outcome per message ("Saved in /restore list"), and `/restore list` / `/restore draft ID` bring the text back (an empty composer also receives it automatically). A reply duty is not restored: if the cancelled input asked for a reply (`ReplyCancelled`), the sender's duty is cancelled and restoring the text does not recreate it; send a new request instead. Input the model already admitted is not cancelled (`すでに会話へ反映済み`).


### Resuming a recovery-wait Thread from the operation form

The `message send` operation form has an explicit `明示的に復旧待ちを再開` checkbox.
Leave it off for ordinary queued input; turn it on only when this message itself is
the deliberate recovery instruction. It has the same effect as CLI
`message send --resume-recovery`. Human text entered in the normal conversation
input remains a separate explicit human-resume action.
