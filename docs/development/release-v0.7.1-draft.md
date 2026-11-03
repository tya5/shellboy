# Shellboy v0.7.1

Hotfixes for responsive input and database inspection.

## Breaking changes

- None identified in this draft.

## Before upgrading

- Stop the owner, back up the complete data directory, update the owner and CLI
  together, and restart the owner.
- On first launch, Shellboy builds a database index for history inspection.
  Index creation takes **[W3 measurement pending]** on **[dataset and hardware
  pending]**; allow startup to complete. This is a draft, not a measured claim.

## New features

- None planned for this hotfix.

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

- A new index speeds up `history list` and `history get`. Measured before/after
  latency, dataset and environment: **[W3 measurements pending]**.
- First-launch index build duration: **[W3 measurement pending]**.

## Downloads and validation

This is a draft. Candidate archives, checksums, upgrade validation and CI are
separate release gates. No version bump, tag, release or binary replacement is
made by this document. Final release contents depend on the hotfixes landing.
