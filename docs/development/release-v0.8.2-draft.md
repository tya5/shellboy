# Shellboy v0.8.2

This draft tracks changes after [v0.8.1](https://github.com/tya5/shellboy/releases/tag/v0.8.1). It is not a release announcement.

## Breaking changes

None. Version 0.8.2 follows the compatible-change rule: #215 removes or changes no settings or CLI options, and introduces no migration.

## Before upgrading

- Stop the owner, back up the complete data directory, update the owner and CLI together, and restart the owner.
- Conversation schema remains 28; no new migration. Standalone RAG schema remains 4.
- Verify with `python3 scripts/check-release-upgrade.py --old OLD_BINARY --new NEW_BINARY --root EMPTY_TEMP_DIR --old-version 0.8.1 --start-schema 28 --expected-version 0.8.2 --expected-new-schema 28`.

## Changes since v0.8.1

| Change | User-visible effect | Breaking | Schema migration | Evidence |
| --- | --- | --- | --- | --- |
| Automatic compaction ([#215](https://github.com/tya5/shellboy/issues/215)) | Automatic compaction works toward the remaining removal target in four stages: closed-turn spill, closed-turn summary, active-turn spill, then active-turn summary. Bounded spills and summaries preserve safety fences and report already-adopted spill if a later stage is cancelled. Readable older reasoning can inform summaries; reduce parts stay intact. On one synthetic fake-provider fixture, a 156,310-character removal need was met by spilling 36 groups (158,852 replay characters), avoiding 10 map and 1 reduce requests that the no-spill fixture used. This is not a real-world speed or latency measurement. | No | None | Commit `fad3a6d686`; issue #215 synthetic fixture report |
| Shared shell-output drain progress | stdout and stderr pumps sharing a sink recognize shared progress during drain; stalled streams remain bounded without increasing timeouts. | No | None | Commit `96d9c07240` |

## Unverified / pending

Synthetic compaction measurements do not establish real-provider latency, retention quality, or physical terminal behavior.
