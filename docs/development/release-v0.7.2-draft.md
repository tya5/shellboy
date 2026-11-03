# Shellboy v0.7.2

Explicit replies, more predictable context reduction, and bounded startup recovery.

## Breaking changes

- None identified. The reply commands add capabilities without removing existing commands.

## Before upgrading

- Stop the owner, back up the complete data directory, update the owner and CLI
  together, and restart the owner.
- First launch upgrades conversation schema 25 to 27: migrations 26 and 27
  build the pending-reply and running-index-recovery indexes. On one 3.16 GiB `VACUUM INTO` copy of a live schema-25 database,
  applying both migrations in one transaction took 0.043475 seconds on macOS
  (Apple M1); this single measurement excludes copying and is not a startup
  time guarantee. The temporary copy was removed.
  Standalone RAG remains at schema 4.

## New features

- Request a reply with `message send --expect-reply`; respond with `message
  reply` and wait for `reply_received`. `thread get` exposes `awaiting_reply`,
  and a Run ending with pending requests receives a reminder (#194).
- In the TUI, `/replies` selects a pending request and enters reply mode. The
  composer retains its draft when leaving that mode.
- `exec --result reply|turn-end|run-end` selects explicit replies or the relevant
  end message. End-message wording distinguishes completion from a reply (#194).

## Changed behavior

- Shell tool `unknown_arguments = warn|strict` controls unknown keys; malformed
  or duplicate arguments and other preflight failures receive precise per-call
  errors without discarding valid sibling calls.
- Proactive compaction calibrates its target to measured request sizes and widens
  insufficient reductions rather than repeating a shallow plan (#208, #209).
- Startup index recovery processes bounded pages, releases the worker between
  pages and resumes outstanding work without an automatic full rebuild (#172).
- Japanese and English presentation coverage is expanded for recovery,
  inspection, usage, context reduction and related feedback (#152).

## Fixes

- Recovery retains committed progress and notifications when a later page fails,
  and oversized work can continue through bounded recovery paths (#172).

## Performance

- Startup recovery uses an index over running jobs and bounded page sizes rather
  than scanning the entire queue (#172). Workload-dependent timings are not
  implied by the migration measurement above.

## Downloads and validation

Run `scripts/check-release-upgrade.py` from the independently verified public
v0.7.1 binary (conversation schema 25) to the exact v0.7.2 candidate (schema 27),
as described in [publication validation](publication.md#v072-synthetic-upgrade-check).
Candidate archives, checksums, upgrade validation and three-OS CI remain
separate release gates. These notes do not themselves create a tag, publish a
release or replace an installed binary.
