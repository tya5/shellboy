# Shellboy v0.7.2

Explicit replies, more predictable context reduction, and bounded startup recovery.

## Breaking changes

- Message completion is no longer presented as a per-message answer: `message`
  results no longer expose `answer_node_id`, and thread projections use the
  end-message terminology. Before 1.0, no compatibility alias is provided.

## Before upgrading

- Stop the owner, back up the complete data directory, update the owner and CLI
  together, and restart the owner.
- First launch upgrades conversation schema 25 to 27. Migration 26 adds the
  pending-reply index; migration 27 adds the startup index-recovery index.
  Migration timing: `MIGRATION_TIMES`.
  Standalone RAG remains at schema 4.

## New features

- Request a reply with `message send --expect-reply`; respond with
  `message reply`, and wait for `reply_received`. `thread get` shows pending
  requests in `awaiting_reply`. A Run-end reminder calls attention to requests
  that still need a reply.
- In the TUI, `/replies` opens pending requests for the current conversation.
  Selecting one enters reply mode; leaving reply mode preserves the draft.
- Choose the result returned by `exec` with `--result reply|turn-end|run-end`.
  The modes distinguish an explicit reply from the end message of a Turn or
  Run.

## Changed behavior

- Configure `[shell] unknown_arguments = "warn"|"strict"` to control how
  unknown model-supplied shell argument keys are handled. Malformed, duplicate,
  and other invalid arguments receive precise errors for the individual call;
  valid sibling calls in the same group can still run.
- Proactive compaction targets 80% of `auto_at_tokens`, calibrated from paired
  provider-token and serialized-input measurements for the same request. Fresh
  usage verifies whether the target was reached; if not, a later attempt widens
  the safe source scope instead of repeating the same shallow reduction.
- Startup index recovery runs in bounded indexed pages and yields the database
  worker between units. Recovery can continue through oversized legacy work
  without triggering an automatic full rebuild (#172).
- Japanese and English UI feedback is more consistently localized across
  inspection, recovery, setup diagnostics, usage and elapsed-time displays
  (#152).

## Fixes

- Reply duties and their reminders survive recovery and terminal Run outcomes
  (#194); deleted-thread receipts are reclaimed in bounded batches (#197).
- Startup recovery preserves committed progress when a later unit fails, and
  reports recovery state to users without losing queued work (#172).
- The TUI keeps submitted input and status visible while database work is in
  progress.

## Performance

- Proactive context reduction uses a measured target to avoid repeatedly
  accepting reductions that are too shallow (#209).
- Startup index recovery uses bounded work units rather than loading the entire
  recovery queue into one operation (#172).

## Downloads and validation

Run `scripts/check-release-upgrade.py` from the independently verified public
v0.7.1 binary (conversation schema 25) to the exact v0.7.2 candidate (schema
27), as described in [publication validation](publication.md).
Candidate archives, checksums, upgrade validation and three-OS CI remain
separate release gates. These notes do not themselves create a tag, publish a
release or replace an installed binary.
