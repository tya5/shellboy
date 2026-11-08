# Shellboy v0.11.0 release notes (draft)

## Before upgrading

- The conversation database migrates from schema 32 to 35. Migrations 33-35 add index quantization metadata and lookup indexes, Thread tags and cwd lookup, and bounded reclaim indexes. Back up data and stop the owner before upgrading; older binaries cannot use the upgraded schema.
- Recovery-wait Turns now hold non-human messages by default. Use `--resume-recovery` to resume intentionally; human replies still resume (#263).
- Several non-safety restrictions on messaging and execution are removed (#259). Live-Run protection and data-integrity checks remain.
- Index removal reports `removed` for the logical removal decision; physical reclamation is bounded and may continue later (#267). Do not interpret it as proof all bytes have already been reclaimed.

## Changes since v0.10.0

- Unsaved model text is exposed through `events wait` with explicit gaps (#243 E3); Thread tags and cwd discovery organize conversations (#222).
- Owner configuration can be read and changed with `config get` and `config set` (#266); capacity and usage inspection also support external clients (#265). Disk free-space admission and the 50 GiB default are updated (#261).
- Connection setup and live connection generations improve configuration and keep each Run on its selected generation (#269). Live defaults do not overwrite saved Thread choices.
- Human reply handling, delivery after rewind, and recovery behavior are more reliable (#262, #263). Editing a request now refuses while its Run is active instead of stopping it (#258).
- Compaction avoids repeated oversized-summary retries and repeated spill near the trigger (#251, #271); Thread inspection labels retained compaction facts as incomplete when an expired restore may be missing (#264).
- `usage get --group-by model` groups saved attempts by model; summary usage without a saved model is not attributed (#272). Capacity reporting shows configured thresholds (#268).
- History exposes execution descriptions, and the core guides no longer point to Studio Apps. Other documentation and bounded-read fixes improve inspection.

## Validation and limitations

A synthetic in-memory SQLite fixture (two Threads, one node) migrated 32 to 35 in 3.017 ms on macOS arm64, SQLite 3.51.0, one run. This measures SQL migration only, not a public-binary upgrade or production-sized database. The exact release candidate upgrade, installation and release artifacts remain to be qualified. Native Windows hardware, real provider behavior and production data were not tested in this preparation. CI results must be checked on the final candidate SHA.
