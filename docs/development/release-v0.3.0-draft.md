# Shellboy v0.3.0

More reliable long conversations, recoverable messages, saved usage totals,
and clearer guides for model-directed shell work.

## Upgrade notes — breaking before 1.0

- **Stop the owner and back up the complete data-dir before upgrading.** The
  conversation DB advances from schema 11 to 14 at the first write-capable start.
  v0.2.0 cannot open it afterward. To roll back, restore your backup into a
  separate data directory; do not use an old binary on the upgraded store.
  Standalone RAG remains schema 3.
- **Remove `connections.<name>.compact_hint_tokens` before the first start.**
  The key is no longer supported. If it remains, startup reports a configuration
  error rather than ignoring it.
- **Use the same version for owner and CLI, and restart the owner.** The local
  protocol stays at version 2, but old CLIs do not know the new commands.
  Replacing the executable does not update an already-running owner.

## Long conversations and recovery

- Reduction follows actual failures, not token estimates or a fixed 75k limit.
  There is no automatic reduction before sending. The first eligible temporary
  failure retries unchanged; the second can reduce safe history without a model
  call. Explicit context-limit errors reduce immediately. If no further reduction
  is available, work waits for your decision instead of repeatedly resending it.
- **Interrupted sends now use the normal retry budget**, instead of stopping
  after two partial uploads. The default is eight resends, including reduced
  requests. Successful responses reset that budget, and a long Turn can reduce
  again if it grows. An interrupted send may not have reached the server.
- `/compact-max` or `compact --maximal` reduces safe history without asking a
  model to summarize it. `/retry` is separate and may incur another charge.
  The current request and unfinished work stay protected; `/restore` can recover
  omitted text. Stopping or restoring does not undo commands already executed.
- Missing tool results after a crash are reconciled without executing tools
  again. Unix SIGTERM/SIGHUP use orderly shutdown. Optional Revisit can request
  a bounded follow-up when work would otherwise finish; explicit stop wins.

## Messages, instructions and display

- **Messages left behind after stopping or a forced exit are visible and can
  be restored for review and sending** (#53). They are not silently discarded
  or automatically sent again. Pending input can also be inspected, cancelled
  by fixed message IDs and restored to a draft without sending it.
- `message send --now` releases the foreground shell wait so input can be
  delivered sooner; the shell continues and model generation is not cancelled.
  Esc interrupts the current Turn after closing open UI surfaces; Ctrl+C stops
  the Run and its background work.
- **The conversation list and conversation view now agree on work status**,
  including background work still running after an answer (#44).
- Choose or change a conversation's AGENTS.md reference. Its source is shown;
  changes take effect on a new Turn, while retries keep the saved instructions.
  Changing the working directory alone does not change that reference.
- Already-painted messages and completed tool results remain visible during
  reloads and next-response waits. Reading positions and tail following are more
  stable, without filling logs with ordinary refresh records.

## Usage, indexing and guides

- `usage get` and F8 show saved usage for a Turn, Run, conversation or its child
  conversations. Unknown values stay unknown. Run, model-request and shell times
  are separate and overlap: these totals are neither a bill nor effective work
  time. Summary/embedding usage and arbitrary period filters are not included.
- Standard usage stays readable even with large provider extras. New records
  keep only standard numeric fields. Old large rows are not reclaimed by this
  release and still cost SQLite parsing time when read.
- Index pause/retry messages are clearer. Embedding requests can be batched, and
  dimension changes have explicit probe/rebuild controls. Preview the scope and
  possible API costs before indexing, probing or rebuilding.
- Guides cover current messaging and usage, explain the safety model and prompt
  injection risks, and include an English/Japanese User Guide. Model commands
  still use the owner's OS access by default; this is not a new sandbox.

## Downloads and verification

Windows x64, macOS Apple Silicon and Linux x64 archives contain the executable,
LICENSE, configuration example and offline guides. Extract and verify them
against `SHA256SUMS` before use.

Source: `8fb885d843a39069aac80fa63708b5c70df0531c`.
[Main CI](https://github.com/tya5/shellboy/actions/runs/41051520063) passed on all
three OSes. [Candidate builds](https://github.com/tya5/shellboy/actions/runs/41052577239)
also passed, including launching each extracted archive's binary.

The public checksum-verified v0.2.0 macOS binary created synthetic conversations,
indexes, pending input and recovery-wait work. The packaged 0.3.0 binary upgraded
that copy to DB14 and passed conversation/search/usage/RAG checks; RAG stayed3.
All API calls used a local mock, not user data or a paid provider. This is not
physical-terminal or real-provider qualification on every OS. Sandbox recipes
remain deferred until tested on their respective OSes.
