# User Guide

[Guide index](README.md) · [Configuration and installation](configuration.md) · [TUI controls](tui.md) · [日本語](user-guide.ja.md)

Shellboy is a terminal application for conversations and model-directed shell work. Start one TUI to own a conversation data directory; external `shellboy` CLI commands connect to that running application. Conversations require a compatible endpoint, model ID, and credentials: Responses by default, or explicit Chat Completions per connection. You can read guides with `shellboy docs list` before configuring anything, and use the document-search CLI `shellboy rag` independently.

Before granting model-directed shell access, read the [safety model](#safety-model).

## Conversations and work states

The UI currently uses Japanese captions. The following table explains the terms you see on screen.

| On-screen term | Meaning | Internal information in details |
|---|---|---|
| 会話 (conversation) | A saved container for history, working directory, and model | Thread. Parent/child conversations describe history lineage, separately from execution ownership |
| 依頼 (request) | The message you sent; once accepted, it is persisted | message. accepted/held/applied do not indicate whether an answer succeeded |
| 処理中 (working) | Preparation, model response, tool execution, or explicit waiting is in progress | Run is the work lifetime; Turn is the processing unit up to the next answer; attempt is one communication attempt |
| 背景処理 (background work) | Shell work or owned children continue after the answer | A completed Turn can coexist with a live Run. Work continues across conversation switches |
| 復旧待ち (recovery wait) | The reason is saved, awaiting a retry, new instruction, or stop decision | The Turn is in recovery_wait. The Run remains live; elapsed time alone does not resume it |

Selecting a conversation, stopping current work, and inspecting past results are different operations. “Accepted” means persistence succeeded; “answer complete” means the Turn completed, which need not mean that all work, including background work, has finished. IDs and receipts are diagnostic details. Usually you can read the state and reason, then open details when you need them.

In recovery wait, check the reason and target Run before resending a saved request. After addressing the cause, retry the same work with F8's Run retry operation or `shellboy run retry --run RUN_ID`. To stop it, use Ctrl+C or `shellboy run stop --run RUN_ID`. A new human instruction resuming the same recovery Turn differs from retrying the original work. Retry cannot revive a terminal Run after stopping or restarting. See [Recovery and storage](recovery-and-storage.md) for checking saved content and uncertain side effects.

Saved input waiting for the next boundary is visible above the composer and in `/queue`, including CLI/other-conversation input. Ordinary send queues; Ctrl+Enter (unless configured as ordinary send) or Ctrl+X then Ctrl+S requests immediate delivery without interrupting generation. Esc closes a popup or returns from history focus first; in the unfocused conversation it interrupts only the Turn; Ctrl+C stops the Run and owned background work. Cancel unread inputs by fixed IDs in `/queue`; recover archived inputs through `/restore list` and `/restore draft ID`. Newer composer edits are preserved. See [TUI controls](tui.md) for limits and confirmation details.

## 1. Start in a working directory

First-time setup, before starting a conversation:

```sh
shellboy config init
# Edit the reported config_path: API root, model and api_key_env variable name.
# Set that environment variable's value in this terminal; never put keys in TOML.
shellboy config check
shellboy
```

This works in PowerShell too. Existing config files are never overwritten. Use
identical `--config PATH` options throughout if you choose a custom path. Check
uses only the model catalog, not a conversation request; no owner is required.
For `base_url`, write the API root (`https://proxy.example/v1`), **not**
`.../v1/responses`. See [first setup](configuration.md#first-setup-without-an-owner)
for OS paths, credentials and a keyless local-endpoint example. If an owner was
already running, restart it after editing the file or its credential environment.


Use `/status` in the TUI or `shellboy status` in another terminal for an overview of current work. Saved IDs let you track background shells, child Runs, held requests, and model requests. Include terminal work with `/status all` or `shellboy status --all`. See [TUI slash commands](tui.md#slash-commands) and [CLI connection and common syntax](cli-reference.md#common-syntax-and-connection).

First choose your terminal, OS binary, and model shell combination in [Starting on each platform](configuration.md#starting-on-each-platform). Follow [Configuration and installation](configuration.md) to build or install Shellboy and configure the connection, then run the following in a terminal. These examples use Bash/zsh.

```sh
command -v shellboy
shellboy --version
cd /path/to/project
shellboy
```

Replace `/path/to/project` with an existing working directory. If the short command is not on PATH, add the installation directory to your user PATH or invoke the binary by absolute path. Shellboy configures PATH automatically for CLI discovery in the model's shell. See [Configuration](configuration.md) for endpoint and credentials, or [Development setup](../development/setup.md) to build a source checkout.

To choose configuration and data explicitly:

```sh
shellboy --config /path/to/config.toml --data-dir /path/to/shellboy-data
```

Starting without a subcommand requires an interactive terminal. You cannot run two owners against the same data-dir. Subcommands from another terminal use the running owner; they do not start one automatically. Choose a different data-dir for an independent environment.

During startup, the screen shows current preparation such as configuration, DB migration, or cleanup of previously interrupted work. Conversation input becomes available after startup completes. Esc / Ctrl+C / Ctrl+Q request exit; initialization and persistence already started proceed to a safe boundary before closing. This does not roll back a migration or immediately release the lock. If startup fails, read the reason and close with Enter or Esc.

A new root Thread uses the startup directory as its cwd. Opening a saved Thread uses that Thread's saved cwd without overwriting it with the current startup directory. Check it with `/cwd` or F8's “会話 · 詳細” (conversation details).

### Choosing a conversation format

Connections default to `api = "responses"`; set `api = "chat_completions"` for an
OpenAI-compatible Chat endpoint, then restart the owner. The [dialect guide](configuration.md#conversation-api-dialect)
contains an **unverified** Gemini example and the unavailable features. Responses
reasoning/encrypted replay is not sent to Chat, and vendor reasoning content is
unused. All real providers remain unverified: only synthetic SSE and loopback
mocks were used. Cache usage shown by `usage get` and F6 is reported fact, with
missing details unknown; a catalog check cannot prove caching. Anthropic's
OpenAI facade is excluded because prompt caching is unavailable there; use a
suitable native-API proxy rather than assume it supports long cached sessions.

## 2. Send your first request

Type into the composer and press Enter to send by default. Ctrl+J or Shift+Enter inserts a newline. For example: “Read this folder's README and explain its structure and possible next tasks.” The model has a shell tool, so a request to modify files may run in that working directory.

The first send persists the Thread and request. “Accepted” means DB persistence, separately from completion of an answer, shell, or Run. A saved request remains even if preparation or communication fails after acceptance. Address the reported failure and explicitly retry; see [Recovery and storage](recovery-and-storage.md).

Until acceptance completes, the submitted text and attachments remain visible. Edits during that interval do not change the submission snapshot, and a late acceptance response does not erase a subsequently edited draft. If the queue is full or persistence fails, check the text before trying again. Pasting a long passage does not send it automatically; inspect its [collapsed display](tui.md#pasting-long-text).

## 3. Switch conversations

- `/new` or Ctrl+N opens a new draft. It is not a saved Thread until the first send.
- `/threads` or Ctrl+T opens a one-row-per-item list. Select with Up/Down and Enter. Left/Right expand, collapse, or move through parent/child relationships; typing filters the list. Details and a short conversation excerpt appear below; F1 opens full values.
- If you know an ID, use `/switch THREAD_ID`.

Saved title, state, additions and deletions update the open list automatically while keeping selection and collapsed branches. A new row may be above your retained reading position; use Up to reach it. `再確認中` means a previously selected row is being checked after refresh. Ctrl+R explicitly reloads the list; lost notifications trigger an explained resynchronization.

Each Thread retains its own draft and reading position. Switching does not stop execution. Shells or child Runs in the previous Thread continue. Background work may keep a Run live after its Turn's answer completes. See [Delegation](delegation.md) for the Thread/Run/Turn distinction.

### Read the conversation

Your messages form full-width bands, with the saved time at the right when there
is room. Marks distinguish input from a parent, the model itself, another model,
or Revisit. The fixed header is separate from the conversation: title and state
on the left, connection/model/working directory on the right. Long paths keep
their final portion; the original value remains available in details.

For an automatically derived header title, `名称未設定（元本文の取得範囲外）` means
that earlier source evidence exceeded the header's bounded read. Messages still
exist; inspect history or assign an explicit conversation title. The display does
not rewrite the saved title.

Each shell call has one compact row containing its command and known outcome or
duration. IDs and per-response call numbers are left to details. Successful
output starts closed; a failed call opens a short tail preview, including stderr
when retained. Opening a row requests a bounded preview, rather than loading the
whole output into the screen. An abbreviated preview keeps the real end of the
retained output. A decoding warning means the byte boundary or encoding state
is uncertain; use the original-output view for further inspection. Closing or
opening the display does not run, retry or stop the command.

| Place | Key | Action |
|---|---|---|
| Composer, with no slash completion | Tab | Focus a visible history item |
| History focus | Up / Down | Select a message or shell-call row |
| History focus | Enter / Space | Open or close the selected item inline |
| History focus | `o` / `c` | Open the selected original / copy its source |
| History focus | Esc / Tab | Return to the composer |
| Conversation | F6 / F7 | Details / links for the selected item |
| Composer | Shift+Tab | Focus visible live work above the composer |
| Live work | Up / Down; Space | Choose work; open/close the selected shell output |
| Live work | F6; `o`; Esc | Inspect the selected work; open a child conversation; return to input |

With mouse capture enabled, click an item to open or close it. Empty-composer Up
still recalls sent messages. Opening details does not submit a new request.
Copies and originals refer to saved source content, which may contain more than
the visible preview. F6's original-text page also retains its own copy controls.

URLs and `file://` file/folder paths are opened by your terminal, not Shellboy.
In WezTerm, use Shift+click to bypass Shellboy's mouse capture; the default rules
recognize `file://` URLs. Bare paths need a `hyperlink_rules` entry; see the
[short terminal configuration example](tui.md#open-urls-and-file-paths-with-your-terminal).
In iTerm2, use Cmd+click. Bindings can vary, and the terminal and OS determine
what opens and how; Shellboy does not validate paths or launch applications.

Output previews and original pages show a UTC read time captured once per
successful read. Close/reopen a tool row to reread its output without rerunning
it. In F6's original page, O refreshes the selected source, N reads the next page
and P the first. Mutable facts and links in the visible selected-history F6/F7
item follow relevant saved changes automatically, coalesced for 500 ms with a
1-second target in an unloaded local environment. Refresh failures show stale/unavailable information; the
target is not a network/API or busy-database guarantee.

F2 freezes automatic repainting and records its UTC entry time. F2/Esc resumes
updates. Ctrl+L redraws available UI data once, while keeping F2 active and its
entry time unchanged; it does not request a fresh DB read. The displayed entry
time is therefore distinct from each preview's read time.

Routine observations appear as quiet facts at the end of the message section;
failures and recovery decisions remain prominent. The bottom rows show current
activity, pending input and concrete notices with a short help hint. Successful
operation notices clear on subsequent input; failures remain until the next
deliberate action. A completed answer can still have background work: use
`/status` or existing F8 Run/shell details to inspect it.

Choose `dark` (default) or `light` in the configuration file and restart the TUI:

```toml
[ui]
palette = "dark"
```

`NO_COLOR` selects plain marks and spacing; terminals without
advertised 256-color/truecolor support use reverse/separator fallback. No color
is required to distinguish input sources or select an item.

Explicitly reading older history or selecting a history item pauses tail
following. A new-content mark appears when appropriate; Ctrl+End resumes tail
following. Background refreshes do not pause following or erase already
displayed response content. [History and search](history-and-search.md) explains
forks, request editing, search filters, provenance and direct relationships.

F8 retains its current `この会話で` (in this conversation), `よく使う` (common),
and `すべての操作` (all actions) groups. Filter by human-facing name, CLI name or
related slash command; enter target IDs and arguments in separate fields.
Results use field/value presentation. [TUI controls](tui.md#read-source-text-and-links-from-details)
documents detailed keys and copy ranges. Watch
mode and operation-form redesign are later increments; they are not implied by
the conversation layout.

### Inspect saved usage

- Open F8 `使用量 · 合計` (usage totals), enabling children if needed, or use
  `shellboy usage get --thread THREAD_ID --include-children` (omit the last flag
  for this conversation alone).
- These are saved-record totals, not a bill or effective work time. The time
  categories overlap; unreported or unrecorded usage is shown as unknown, not zero.
- If it feels slow, look for `event=usage_totals_slow` in
  `<data_dir>/logs/shellboy.log`. Only aggregations taking at least 100ms appear.
  [TUI details](tui.md#saved-usage-details) explains refresh and snapshot behavior.

## 4. Change connection or model

`/model` alone shows the current selection. `/models` retrieves the current connection's model list. Type to filter, select with Up/Down, and press Enter to change the current conversation's model. Esc only closes the list. Use `/models CONNECTION` for another connection. To set values directly:

```text
/model CONNECTION MODEL_ID
/effort medium
/cwd /path/to/other-project
```

Replace `CONNECTION` and `MODEL_ID` with configured values. The list comes from the endpoint; Shellboy does not embed a model or supported-effort catalogue. If the endpoint also lists embedding models, they appear too; listing is not a guarantee of conversation API compatibility. If listing is unsupported, use `/model CONNECTION MODEL_ID` directly. `/effort none` clears the effort setting; it does not instruct Shellboy to always send `none` to the model. Changing a model preserves the existing effort, so change or clear it explicitly when needed.

Changes apply to subsequent requests, without rewriting communication already sent. Changing connection or model also changes the reasoning-send epoch; returning to the original model does not automatically revive old opaque reasoning. Conversation source text remains saved.

## 5. Delegate to a child

Simply saying "use subagents" does not guarantee that the model understands Shellboy's child-Thread operations. Guides are bundled with the executable; the model receives a short entry-point instruction and reads the guides itself when needed, not their full text in every prompt. No checkout or repository path is required. To explicitly request the procedures, name the guide IDs:

```text
First read shellboy docs get --id shellboy/agent-guide and
shellboy docs get --id shellboy/delegation using shell.
Follow those procedures to create an owned child Thread and delegate research only.
After sending its request, check assignment and completion, retrieve the original
answer, and summarize the results.
```

[Delegation](delegation.md) covers parent/child stopping scope, progress reports, and recovery-wait decisions.

A Thread's `AGENTS.md` reference points to working instructions. A new root Thread defaults to the cwd's `AGENTS.md`; it is read when preparing a new Turn, while a retry in the same Turn uses the saved snapshot. The Agent Guide explains product operations; a repository's root `AGENTS.md` supplies that repository's work instructions. They serve different purposes.

## 6. Interrupt and exit

Ctrl+C stops the current Run and its owned work. Esc closes an open overlay; otherwise, it interrupts only the current Turn. Ctrl+B moves its current foreground shell into the background: the target shell continues, while any unexecuted later calls in the same group are canceled. This differs from changing screens. After backgrounding, the screen reports the target shell and treatment of later calls, and the model receives the corresponding result. If no later calls exist, no nonexistent cancellation is shown. An interruption progresses from stopping to interrupted according to saved state, distinctly from backgrounding or completion.

Waiting to observe completion differs from interrupting the target. To cancel only an observation started in F8's wait form, press lowercase w on the result screen or use `/cancel-wait`. This does not stop the target Run or shell. In forms, Enter executes, Ctrl+J inserts a newline in multiline text, and Ctrl+U replaces a suggested value.

Exit with Ctrl+Q or `/quit`. Shellboy waits for persistence and for owned work to stop and be reaped. After the grace period, W/Enter continues waiting; F opens force-exit confirmation, then Y confirms. Force exit can leave unsaved state or unreaped work. See [Recovery and storage](recovery-and-storage.md).

Next, explore [Attachments](attachments.md), [History and search](history-and-search.md), and the [CLI reference](cli-reference.md).

Before deleting, use F8's “会話 · 削除影響” (deletion impact) to inspect live Runs, descendants, sharing, provenance, and attachment references. Preview does not delete anything; reclaimable-capacity estimates are unsupported. If you lose an acceptance or compaction result, query its operation using F8's “操作 · 受付結果” (operation receipt), then check downstream state. See [Recovery and storage](recovery-and-storage.md).

### Recovery without token guesses

After a failure, inspect Run/Turn context in F8. `/compact-max` reduces safe
context without a summary API; `/retry` separately resends and may charge again.
`/restore` can recover omitted text. Core no longer estimates tokens or uses a
75k threshold. Last successful measured usage and its time are in thread get.

## Storage and resources

Conversations, answers, reasoning and summaries are saved in the data directory
without automatic expiry. Raw shell output and managed clipboard images are separate:

- Terminal shell raw output: default seven days / total 1GiB (`retention.tool_raw_days`, `tool_raw_bytes`).
- Managed images: default seven days / total 256MiB (`retention.clipboard_days`, `clipboard_bytes`).

Live shells, pins, selected images and saved drafts protect content from collection.
Collection leaves history references and unavailability reasons, not recoverable raw content.
Managed PNGs live under `<data_dir>/managed/clipboard/`; their absolute `path` is not permanent storage. GC never deletes external originals.

While the owner runs, GC starts automatically and repeats about one minute after each pass;
raw/history DB work uses batches of up to 128 records, not a 128-per-minute quota.
SQLite pages freed by collection are reused; no automatic VACUUM shrinks the DB file.
To reduce logical storage, inspect `shellboy storage status`, run `shellboy storage collect`,
then preview unwanted conversations with `shellboy thread delete-preview --thread THREAD_ID`
before `shellboy thread delete --thread THREAD_ID`. Collection respects protection and sharing;
preview is read-only and does not estimate reclaimable bytes. See [Recovery and storage](recovery-and-storage.md).

For RAM, `[cache]` budgets are `body_bytes` (32MiB), `layout_bytes` (16MiB),
`history_bytes` (32MiB), and `attachment_base64_bytes` (64MiB) by default.
Lower them in the configuration file and restart the owner; `0` disables retained caching,
not active working memory or saved history. These are not a whole-process memory cap.
`shellboy status --memory` shows on-demand owner cache accounting and native process memory;
unknown values are null, not zero, and cache figures must not be added to footprint/RSS.
See [Configuration](configuration.md#all-sections-and-defaults) for settings and [CLI reference](cli-reference.md) for observation details.

## Safety model

**What you grant.** The model's only tool is a general-purpose shell. Commands
inherit the owner's OS account and environment by default. Shellboy does not
insert a per-command approval prompt. Shellboy supplies no filesystem/network sandbox
and no command denylist; a conversation's cwd is a starting directory, not a
fence. Separate conversations and worktrees do not isolate permissions.

**Why not an approval or prohibition layer?** Blocking command names while
leaving a general-purpose shell available can be bypassed by expressing the
same operation another way; it would imply protection it cannot provide.
Approving every command also conflicts with long autonomous work and encourages
clicking through without reading. Use OS boundaries rather than rely on a weak
application boundary. This does not authorize a model to ignore your instructions.

**What Shellboy does instead.** It saves admitted requests before execution and
records saved output, origins, cancellation reasons and instruction snapshots
so you can inspect what happened. Retention limits still apply, especially to
raw shell output: this is not a promise to retain everything forever. Origin
and ownership checks validate CLI callers and targets; they do not isolate OS
resources from a malicious model. Stop a Turn, Run or shell using the documented
controls and inspect saved facts after interruption. Stopping cannot undo file
changes or external effects, and restarting does not automatically resume terminated Runs.
Some TUI operations still have confirmations (for example deletion or paid index
work); those are not a per-command security gate.

**Your boundary.** For valuable environments, use a dedicated least-privilege
OS account, container or VM and restrict file/network access there. OS options
include macOS Seatbelt, Linux bubblewrap/Landlock, and a dedicated Windows user
with NTFS permissions and Windows Firewall. See [OS sandbox wrappers](shell.md#os-sandbox-wrappers)
for the tested macOS example and the explicitly unverified Linux/Windows remainder. Include the owner's privileges in your design: wrapping only the
model shell does not restrict owner-mediated reads such as `attachment add`.
Keep unrelated secrets out of the environment/readable files; assume commands
can inspect credentials accessible to that account. Conversations and selected
file/tool content can go to configured API endpoints (including embeddings when
enabled), and the data-dir stores conversation data; protect both endpoint
choice and local access.

**Prompt injection.** Instructions found in web pages, files, RAG hits or other
conversations/child results are data, not new user authorization. A model cannot
reliably block every such attack. Limit what it can reach using the OS boundary,
and use saved provenance/history to inspect actions; neither replaces the other.
See [Agent Guide](agent-guide.md#safety-boundary) and
[stopping and recovery](recovery-and-storage.md).


## Live work above the composer

Up to three oldest shells, owned child Runs and pending waits remain visible
while the selected Run owns work, including after its answer completes. Overflow
shows a count; missing saved timing or wait evidence is not reported as zero.
Elapsed time comes from saved start facts. Saved-output age and a newly observed
output commit are distinguished; local clocks do not poll the database.

Use Shift+Tab or click a work row to focus it. Space toggles a shell's bounded
output preview, F6 opens the matching inspection form, and `o` opens a selected
child conversation. Esc returns to the composer. Opening output neither runs
nor stops the command. An open preview refreshes at most twice per second;
large backlogs skip to a marked tail and uncertain decoding remains explicit.
Closed/covered views and conversation switching stop that preview subscription.
F2 keeps its existing frozen-screen behavior. Use `/status` or F8 when more than
three work items or full saved facts are needed.
