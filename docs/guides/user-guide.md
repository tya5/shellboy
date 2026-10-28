# User Guide

[Guide index](README.md) · [Configuration and installation](configuration.md) · [TUI controls](tui.md) · [日本語](user-guide.ja.md)

Shellboy is a terminal application for conversations and model-directed shell work. Start one TUI to own a conversation data directory; external `shellboy` CLI commands connect to that running application. Conversations require a Responses-compatible proxy endpoint, model ID, and credentials. You can read guides with `shellboy docs list` before configuring anything, and use the document-search CLI `shellboy rag` independently.

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

## 1. Start in a working directory

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

## 2. Send your first request

Type into the composer and press Enter to send by default. Ctrl+J or Shift+Enter inserts a newline. For example: “Read this folder's README and explain its structure and possible next tasks.” The model has a shell tool, so a request to modify files may run in that working directory.

The first send persists the Thread and request. “Accepted” means DB persistence, separately from completion of an answer, shell, or Run. A saved request remains even if preparation or communication fails after acceptance. Address the reported failure and explicitly retry; see [Recovery and storage](recovery-and-storage.md).

Until acceptance completes, the submitted text and attachments remain visible. Edits during that interval do not change the submission snapshot, and a late acceptance response does not erase a subsequently edited draft. If the queue is full or persistence fails, check the text before trying again. Pasting a long passage does not send it automatically; inspect its [collapsed display](tui.md#pasting-long-text).

## 3. Switch conversations

- `/new` or Ctrl+N opens a new draft. It is not a saved Thread until the first send.
- `/threads` or Ctrl+T opens a one-row-per-item list. Select with Up/Down and Enter. Left/Right expand, collapse, or move through parent/child relationships; typing filters the list. Details and a short conversation excerpt appear below; F1 opens full values.
- If you know an ID, use `/switch THREAD_ID`.

Each Thread retains its own draft and reading position. Switching does not stop execution. Shells or child Runs in the previous Thread continue. Background work may keep a Run live after its Turn's answer completes. See [Delegation](delegation.md) for the Thread/Run/Turn distinction.

The conversation view distinguishes messages, execution results, and state changes. For full IDs or source values, use F6 to select an item and open its details, then O for the original-text page. F7 lists links and copies full URLs. F8 shows `この会話で` (in this conversation), `よく使う` (common), and `すべての操作` (all actions). Filter by human-facing name, CLI name, or related slash command; enter target IDs and arguments in separate fields to inspect history, Runs, shells, indexes, and storage. Results use field/value presentation; retrieve needed portions in a detail page. [TUI controls](tui.md#read-source-text-and-links-from-details) documents keys and copy ranges.

Reading older history pauses following the tail and shows “新着あり” (new content). Ctrl+End resumes tail following. [History and search](history-and-search.md) explains forks, request editing, search filters, provenance, and direct relationships.

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

Simply saying “use subagents” does not guarantee that the model understands Shellboy's child-Thread operations. The Agent Guide is not injected automatically. When using a checkout, provide actual absolute paths and ask the model to read them first:

```text
First use shell to read /path/to/shellboy/docs/guides/agent-guide.md and
/path/to/shellboy/docs/guides/delegation.md.
Follow those procedures to create an owned child Thread and delegate research only.
After sending its request, check assignment and completion, retrieve the original
answer, and summarize the results.
```

If you copied only the binary elsewhere, the docs need not exist alongside it. Replace these paths with the actual guide locations. [Delegation](delegation.md) covers parent/child stopping scope, progress reports, and recovery-wait decisions.

A Thread's `AGENTS.md` reference points to working instructions. A new root Thread defaults to the cwd's `AGENTS.md`; it is read when preparing a new Turn, while a retry in the same Turn uses the saved snapshot. The Agent Guide explains product operations; a repository's root `AGENTS.md` supplies that repository's work instructions. They serve different purposes.

## 6. Interrupt and exit

Ctrl+C interrupts the current Thread. Ctrl+B moves its current foreground shell into the background: the target shell continues, while any unexecuted later calls in the same group are canceled. This differs from changing screens. After backgrounding, the screen reports the target shell and treatment of later calls, and the model receives the corresponding result. If no later calls exist, no nonexistent cancellation is shown. An interruption progresses from stopping to interrupted according to saved state, distinctly from backgrounding or completion.

Waiting to observe completion differs from interrupting the target. To cancel only an observation started in F8's wait form, press lowercase w on the result screen or use `/cancel-wait`. This does not stop the target Run or shell. In forms, Enter executes, Ctrl+J inserts a newline in multiline text, and Ctrl+U replaces a suggested value.

Exit with Ctrl+Q or `/quit`. Shellboy waits for persistence and for owned work to stop and be reaped. After the grace period, W/Enter continues waiting; F opens force-exit confirmation, then Y confirms. Force exit can leave unsaved state or unreaped work. See [Recovery and storage](recovery-and-storage.md).

Next, explore [Attachments](attachments.md), [History and search](history-and-search.md), and the [CLI reference](cli-reference.md).

Before deleting, use F8's “会話 · 削除影響” (deletion impact) to inspect live Runs, descendants, sharing, provenance, and attachment references. Preview does not delete anything; reclaimable-capacity estimates are unsupported. If you lose an acceptance or compaction result, query its operation using F8's “操作 · 受付結果” (operation receipt), then check downstream state. See [Recovery and storage](recovery-and-storage.md).
