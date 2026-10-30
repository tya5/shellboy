# Shellboy v0.3.0

Long-conversation recovery based on observed failures, durable pending messages,
working-instruction provenance, saved usage totals and more stable terminal history.

## Upgrade notes — breaking before 1.0

- **Stop the owner and back up the complete data-dir before the first start.**
  The conversation database advances from schema 11 to 14 on its first
  write-capable open. Older binaries, including v0.2.0, cannot reopen schema 14.
  Roll back by restoring a consistent backup into a separate data directory,
  not by pointing the old binary at the upgraded store. Keep any backup files
  and managed data together. Standalone RAG remains schema 3.
- **Remove `connections.<name>.compact_hint_tokens` from your configuration
  before upgrading.** It is removed, not ignored: a file still containing the
  key produces a configuration error and the owner does not start. The sample
  configuration and guides describe the new behavior.
- **Use matching owner and CLI binaries and restart the owner.** The local
  protocol remains version 2; an unchanged protocol number is not a promise
  that an old CLI knows every new operation. Replacing a file does not update
  a process that is already running.
- Stop/restart does not undo tools or automatically restart terminal Runs.
  Inspect retained work, interrupted reasons and pending input before resending;
  another model request may incur another charge.

## Long conversations and recovery

- Core no longer estimates tokens, calibrates a token/character ratio, uses a
  fixed 75k threshold, or reduces before POST. `thread get` shows the last
  successful measured usage and its time. Context edit sizes are characters,
  not token or prompt-cache estimates; the former estimate fields are removed.
- The first eligible transient failure retries unchanged. From the second
  consecutive eligible header/5xx/disconnect failure, safe context can be reduced
  without a summary API. Explicit context-capacity rejection reduces immediately;
  if no reduction can be adopted, it enters recovery instead of resending the
  same explicitly rejected input. The existing final spill/summary stage remains.
- **Interrupted uploads no longer have the old two-partial-upload stop rule.**
  They use the configured normal consecutive retry budget (eight resends by
  default), with eligible reduction from the second failure. Reduced resends
  consume that same budget; success resets it. A long Turn can reduce again
  after subsequent growth. Partial-output opt-out, stop and Retry-After limits
  remain. Source-byte consumption is not proof of provider delivery.
- `/compact-max` / `compact --maximal` requests maximum safe API-free reduction
  without node IDs; retry remains a separate explicit action. Protected work,
  immutable originals, restoration provenance and attachment caveats remain.
  Recovery shows saved failure/size/retry facts and applicable actions without
  predicting causes or ranking remedies.
- Interrupted tool groups recover retained results without repeating side
  effects. SIGTERM/SIGHUP use orderly shutdown on Unix. Interrupted latest Runs
  and factual cancellation reasons are visible. Optional Revisit supplies one
  bounded self-instruction when work would otherwise finish; explicit stop wins.

## Input, instructions and terminal history

- Inspect saved pending input, cancel fixed unread message IDs and restore its
  archived text without automatically sending it. `message send --now` requests
  foreground-wait release without cancelling the shell or model generation.
- Esc interrupts the current Turn after dismissing open UI surfaces; Ctrl+C
  stops its Run and owned background work. Saved/held input and received partial
  responses remain inspectable; cancelled or unreconciled tools are not invented.
- Select an AGENTS.md reference when creating/forking a conversation or change
  it later. Its source path is shown; each new Turn rereads it, while retries
  keep that Turn's snapshot. Changing cwd alone does not move the reference.
- Already-painted history and completed tool results remain available during
  asynchronous reloads and next-response gaps. Follow/reading anchors and bounded
  retention are preserved; history diagnostics avoid ordinary-refresh log spam.

## Usage, search and guides

- `usage get` and F8 show saved Turn/Run/conversation totals, optionally including
  bounded child conversations. Success and non-success usage, unknown counts and
  separate Run/model/shell time are explicit. These are not a bill or additive
  effective work time. Summary/embedding usage is not recorded in these totals;
  arbitrary period filters are not part of this first version.
- Standard usage survives provider appendices larger than 64KiB. New writes keep
  standard numeric fields only. Existing large rows remain stored and SQLite
  still parses them when read; this release does not rewrite/reclaim those rows.
- Shell observations expose start/duration/latest retained output time. Index
  operations give clearer pause/retry causes, bounded embedding batches and
  explicit dimension probe/rebuild selection. Embedding/probe/rebuild operations
  may incur API costs; inspect the target and preview first.
- Guides cover current message controls and usage, and explain the actual safety
  model: commands inherit the owner's OS access, with no built-in command sandbox
  or per-command approval. Use OS isolation where needed. This explanation is
  not a new security boundary. English guides include a Japanese User Guide.

## Downloads and known limits

The prepared archives target Windows x64, macOS Apple Silicon and Linux x64,
with `SHA256SUMS`, the executable, LICENSE, generic README, sample configuration
and offline guides. Verify the checksums and extract before running.

Synthetic upgrade validation uses the public checksum-verified v0.2.0 macOS
binary, owned PTYs and a loopback Responses/embedding server, never a user's DB
or paid API. It covers schema 11→14, retained conversation/index/pending facts,
new work and usage totals, history search, standalone RAG keyword search and the
removed configuration key. This is not real-provider or physical-terminal
qualification on every OS. Platform sandbox recipes remain deferred until
actually validated; the guides name tools and limitations only.
