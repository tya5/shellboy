# Shellboy v0.8.1

This draft tracks user-visible changes merged to `main` after [v0.8.0](https://github.com/tya5/shellboy/releases/tag/v0.8.0) (`6a0827d`). It is not a release announcement. Entries are included only after their changes appear in `git log v0.8.0..origin/main`.

## Breaking changes

None. Version 0.8.1 is appropriate because the changes are additive or compatible, including migration 0028.

## Before upgrading

- Stop the owner, back up the complete data directory, update the owner and CLI together, and restart the owner.
- Conversation schema upgrades from 27 to 28 through migration `0028_tool_group_recovery_candidates.sql`. Standalone RAG schema remains 4.
- Run the synthetic upgrade check with public v0.8.0 and this exact candidate in a fresh disposable directory: `python3 scripts/check-release-upgrade.py --old OLD_BINARY --new NEW_BINARY --root EMPTY_TEMP_DIR --old-version 0.8.0 --start-schema 27 --expected-version 0.8.1 --expected-new-schema 28`. This exact-binary check remains pending.

## Changes since v0.8.0

| Change | User-visible effect | Breaking | Schema migration | Evidence |
| --- | --- | --- | --- | --- |
| Interruption notice indentation ([#220](https://github.com/tya5/shellboy/issues/220)) | Wrapped and subsequent lines in interruption notices now align with the notice body, in English and Japanese, including at narrow widths. | No | None | [Commit `44acf9555b`](https://github.com/tya5/shellboy/commit/44acf9555bdc4d3b5c5c6f4bf82ed398c9aa34b3) |
| Shell output drain completion ([commit `86b9f9b`](https://github.com/tya5/shellboy/commit/86b9f9b23b01dc5f740762e8a6e888ff4542e643)) | Shell output continues draining through ongoing progress until EOF; a stalled open stream still has a bounded no-progress timeout. This avoids prematurely truncating output during normal drain while retaining bounded completion for streams that do not close. | No | None | [Commit `86b9f9b`](https://github.com/tya5/shellboy/commit/86b9f9b23b01dc5f740762e8a6e888ff4542e643) |
| Optional clean redraw ([#216](https://github.com/tya5/shellboy/issues/216)) | Setting `[ui] clean_redraw = true` clears and redraws the screen when overlays close and rewrites moved history rows, reducing visible terminal residue. It is off by default; Ctrl+L remains available. Windows hardware behavior is **unverified**; synthetic terminal tests do not verify font pixels. | No | None | [Implementation `958b1df`](https://github.com/tya5/shellboy/commit/958b1dfb09ea5b7476e05f4d13b66b83e02fd4ab), [scroll coverage `8781185`](https://github.com/tya5/shellboy/commit/8781185b15), [approved default `291f298`](https://github.com/tya5/shellboy/commit/291f298f03da3f6c77aa51c64e09f23b77936b4a) |
| Grouped summary usage ([#217](https://github.com/tya5/shellboy/issues/217)) | Hourly, daily, and per-thread usage breakdowns now include compaction summary counts and token totals separately from model attempts, alongside combined totals. | No | None | [Implementation `16784db`](https://github.com/tya5/shellboy/commit/16784db9668774814b1f773c58f49a45f396b3bf), [CLI reference at merge](https://github.com/tya5/shellboy/blob/16784db9668774814b1f773c58f49a45f396b3bf/docs/guides/cli-reference.md) |
| Interrupted tool-group recovery startup scan ([#219](https://github.com/tya5/shellboy/issues/219)) | Startup now keeps an indexed set of groups that may need interrupted-work recovery, rather than searching all completed tool groups each time. On one 3.4 GiB database snapshot, candidate enumeration fell from a 0.354 s warm median to 9 microseconds; this is a candidate-query measurement, **not an end-to-end startup timing**. | No | 28 | [Implementation `24b45af`](https://github.com/tya5/shellboy/commit/24b45af3cf2fbb453db78c967eab08fed4ae1bf9), [migration `0028`](https://github.com/tya5/shellboy/blob/24b45af3cf2fbb453db78c967eab08fed4ae1bf9/migrations/0028_tool_group_recovery_candidates.sql), [measurement decision](https://github.com/tya5/shellboy/blob/24b45af3cf2fbb453db78c967eab08fed4ae1bf9/docs/development/decisions.md) |
| Held input regions ([#218](https://github.com/tya5/shellboy/issues/218)) | Held input now has a fixed, separately configurable region and shared navigation with live work. | No | None | Commit `f18e228e5d` |
| Markdown fallback ([#221](https://github.com/tya5/shellboy/issues/221)) | A bounded readable fallback preserves text when Markdown rendering metadata is unavailable. The original field cause remains unconfirmed. | No | None | Commit `2ccbe02906` |
| Startup context queries ([#219](https://github.com/tya5/shellboy/issues/219)) | Reuses recovery candidates to avoid completed-history scans; results are workload-specific. | No | None | Commit `bded17ecae` |
| Thread deletion preview ([#204](https://github.com/tya5/shellboy/issues/204)) | On one snapshot, three-run median preview time fell from 2,693.8 ms to 115.0 ms for a large tree, and from 2,052.4 ms to 7.7 ms for a leaf; these are snapshot-specific measurements. | No | None | Commit `78cc8349f7` |
| Development build profile | Development and test builds omit debug information by default; an environment override restores line tables. | No | None | Commit `8ce66fb452` |

## Unverified / pending

The Windows hardware behavior for clean redraw remains unverified; the synthetic evidence does not substitute for that check. Other requested follow-ups are not listed until their commits appear in `git log v0.8.0..origin/main`.
