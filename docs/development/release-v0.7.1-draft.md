# Shellboy v0.7.1

Hotfixes for responsive input and database inspection.

## Breaking changes

- None identified.

## Before upgrading

- Stop the owner, back up the complete data directory, update the owner and CLI
  together, and restart the owner.
- On first launch, migration 25 builds one database index for history
  inspection. Building it took 0.322 seconds on a 2.8 GiB database copy; allow
  startup to complete. Standalone RAG remains at schema 4.

## New features

- None.

## Changed behavior

- The database activity item remains available as a debug display; it is not a
  signal that input submission must wait for a database operation (#198).

## Fixes

- The input box clears from local state when submitted, and the status name is
  no longer dropped while database work is in progress (#198).
- Inspection reads no longer run on the database writer lane. A slow
  `history list` therefore cannot freeze other workers or the TUI by occupying
  that lane. Issue #164 recorded three incidents lasting 127, 179 and 332 seconds.

## Performance

- On a 2.8 GiB database copy, the same `history list --limit 40` operation
  took 39.10 seconds before and 0.124 seconds after the index. The index used
  1.8 MB; its build took 0.322 seconds. A 5,000-row `tool_call` insert
  microbenchmark measured 7.81 before and 6.14 microseconds per row after.
- In a three-minute read-only snapshot test with concurrent writes, the WAL
  grew from 2.0 MB to 9.2 MB; a passive checkpoint succeeded afterwards
  (`busy=0`). A separate harness measured a write waiting 45.43 seconds behind
  a slow writer-lane read, versus a maximum 58.97 milliseconds during an
  isolated read. These are single measurements, not latency guarantees;
  caches and workloads were not controlled across runs.

## Downloads and validation

Run `scripts/check-release-upgrade.py` from the independently verified public
v0.7.0 binary (conversation schema 24) to the exact v0.7.1 candidate (schema
25), as described in [publication validation](publication.md#v071-synthetic-upgrade-check).
Candidate archives, checksums, upgrade validation and three-OS CI remain
separate release gates. These notes do not themselves create a tag, publish a
release or replace an installed binary.
