# Next Release Notes (Draft)

This draft tracks user-visible changes merged to `main` after [v0.8.0](https://github.com/tya5/shellboy/releases/tag/v0.8.0) (`6a0827d`). It is not a release announcement. Entries are included only after their changes appear in `git log v0.8.0..origin/main`.

## Changes since v0.8.0

| Change | User-visible effect | Breaking | Schema migration | Evidence |
| --- | --- | --- | --- | --- |
| Interruption notice indentation ([#220](https://github.com/tya5/shellboy/issues/220)) | Wrapped and subsequent lines in interruption notices now align with the notice body, in English and Japanese, including at narrow widths. | No | None | [Commit `44acf9555b`](https://github.com/tya5/shellboy/commit/44acf9555bdc4d3b5c5c6f4bf82ed398c9aa34b3) |
| Shell output drain completion ([commit `86b9f9b`](https://github.com/tya5/shellboy/commit/86b9f9b23b01dc5f740762e8a6e888ff4542e643)) | Shell output continues draining through ongoing progress until EOF; a stalled open stream still has a bounded no-progress timeout. This avoids prematurely truncating output during normal drain while retaining bounded completion for streams that do not close. | No | None | [Commit `86b9f9b`](https://github.com/tya5/shellboy/commit/86b9f9b23b01dc5f740762e8a6e888ff4542e643) |
| Optional clean redraw ([#216](https://github.com/tya5/shellboy/issues/216)) | Setting `[ui] clean_redraw = true` clears and redraws the screen when overlays close and rewrites moved history rows, reducing visible terminal residue. It is off by default; Ctrl+L remains available. Windows hardware behavior is **unverified**; synthetic terminal tests do not verify font pixels. | No | None | [Implementation `958b1df`](https://github.com/tya5/shellboy/commit/958b1dfb09ea5b7476e05f4d13b66b83e02fd4ab), [scroll coverage `8781185`](https://github.com/tya5/shellboy/commit/8781185b15), [approved default `291f298`](https://github.com/tya5/shellboy/commit/291f298f03da3f6c77aa51c64e09f23b77936b4a) |
| Grouped summary usage ([#217](https://github.com/tya5/shellboy/issues/217)) | Hourly, daily, and per-thread usage breakdowns now include compaction summary counts and token totals separately from model attempts, alongside combined totals. | No | None | [Implementation `16784db`](https://github.com/tya5/shellboy/commit/16784db9668774814b1f773c58f49a45f396b3bf), [CLI reference at merge](https://github.com/tya5/shellboy/blob/16784db9668774814b1f773c58f49a45f396b3bf/docs/guides/cli-reference.md) |

## Unverified / pending

The Windows hardware behavior for clean redraw remains unverified; the synthetic evidence does not substitute for that check. Other requested follow-ups are not listed until their commits appear in `git log v0.8.0..origin/main`.
