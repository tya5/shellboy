# Shellboy v0.9.0

This draft tracks changes after [v0.8.3](https://github.com/tya5/shellboy/releases/tag/v0.8.3). It is not a release announcement.

## Breaking changes

None. Every change is additive: new commands and flags, new optional fields in existing results, and two migrations that add nullable columns. No setting or option is removed or changed. The minor version marks the size of the feature set, not an incompatibility.

## Before upgrading

- Stop the owner, back up the complete data directory, update the owner and CLI together, and restart the owner.
- Conversation schema upgrades from 29 to 31 through migrations `0030_thread_auto_compact_at.sql` and `0031_thread_summary_parameters.sql` (nullable columns on `thread`; no row is rewritten). Standalone RAG schema remains 4.
- Verify with `python3 scripts/check-release-upgrade.py --old OLD_BINARY --new NEW_BINARY --root EMPTY_TEMP_DIR --old-version 0.8.3 --start-schema 29 --expected-version 0.9.0 --expected-new-schema 31`.
- A newer `shellboy chat` on a terminal sends an optional `human` field that an older owner rejects; update the owner and CLI together.

## Changes since v0.8.3

| Change | User-visible effect | Breaking | Schema migration | Evidence |
| --- | --- | --- | --- | --- |
| Thread prune ([#235](https://github.com/tya5/shellboy/issues/235)) | `thread prune --thread T --before NODE [--dry-run]` releases this Thread's hold on originals already replaced by adopted summaries and spills; the existing history GC reclaims them, and originals held by other live Threads survive. Explicit only, never automatic. Reading or restoring a pruned original fails clearly. | No | None | Commits `f3c6a1d9af`, `90c388adfc` |
| Per-Thread compaction threshold ([#229](https://github.com/tya5/shellboy/issues/229)) | `thread set --auto-compact-at N|0|''` overrides the proactive compaction threshold for one Thread without a restart, from the next boundary check; `thread get` shows the override and the effective value. | No | `0030_thread_auto_compact_at` | Commits `9e8739edab`, `c505316bd2` |
| Storage vacuum ([#238](https://github.com/tya5/shellboy/issues/238)) | `storage vacuum [--dry-run]` shrinks the conversation database file while no owner runs. It reports the guaranteed reclaimable bytes, the temporary space needed and the free space, refuses when space is short or a reader holds the WAL, and runs `quick_check` afterwards. | No | None | Commits `4dd800495b`, `16f39d3805` |
| Faster history search ([#240](https://github.com/tya5/shellboy/issues/240)) | One vector scan per search instead of repeated rescans, and a cheaper visibility check with identical results. On a copy of one real database (44,038 vectors, 3,072 dimensions) an all-history ranking took 0.66–0.86 s instead of 2.6–3.1 s with the file in the OS cache; uncached timing is not measured. | No | None | Commit `8b5fc23822` |
| Summary segment size and reduce rounds ([#231](https://github.com/tya5/shellboy/issues/231)) | The summary segment size (default 64,000) and maximum reduce rounds (default 4) can be set in `[compaction]`, per connection, per Thread with `thread set --segment-chars/--reduce-rounds`, or per `compact --summary` call. Each compaction records the values it used in `compact list`. | No | `0031_thread_summary_parameters` | Commits `647839fda4` |
| Chat client ([#242](https://github.com/tya5/shellboy/issues/242)) | `shellboy chat [--thread T] [--until turn-end|run-end|reply] [--timeout-ms N] [--json]` talks to a running owner (for example `shellboy serve`) from a plain terminal, printing progress lines and the end message; `--json` emits one JSON object per line. Exit codes match `exec`. Terminal input is recorded as a human sender, like the TUI. | No | None | Commit `41f3a66642` |
| Thread rewind ([#236](https://github.com/tya5/shellboy/issues/236)) | `thread rewind --node NODE [--exclude-node]` and `/rewind` move a Thread back to an earlier node in place; the removed part stays available as a child Thread. Conversation only: shell side effects are not undone, and the result lists what happened after the node. | No | None | Commit `d0c8638693` |
| Branch view ([#237](https://github.com/tya5/shellboy/issues/237)) | `thread branches` returns a Thread family as a tree of divergence points and tips. The conversation marks divergence points with `↳ 2/3 ⋯`, and `/branches` (or the F8 entry) opens a branch panel to open, fork or prune a branch. | No | None | Commit `d1f7eb210c` |

## Unverified / pending

Synthetic compaction measurements do not establish real-provider latency, retention quality, or physical terminal behavior.
