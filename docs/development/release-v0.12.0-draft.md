# Shellboy v0.12.0

Single-request automatic and manual compaction (#303). Protocol stays 5; the
conversation schema moves from 42 to 43 (migration 43, below).

## What changes for users

- A summary is now ONE provider request over the whole selected range. If the
  provider refuses for context length, the range is halved from its start and
  retried; success ends the attempt. Map/reduce, segment sizes, reduce rounds and
  the summary-part cache are gone.
- Proactive compaction is decided by the provider-reported `input_tokens` only.
  `#A` (the first input count reported after the last compaction; 0 if none) is
  compared with the target (`auto_compact_at` times `auto_compact_target_percent`,
  default 20%): at or below the target only spill runs; above it spill is followed
  by one summary (earlier Turns first, then the current Turn). One attempt per
  reported value; below the threshold nothing happens.
- A summary that is refused, empty or not smaller than its source is remembered for
  that exact range and settings (latest closed and latest active range per Thread)
  and is not tried again; rate limits, server errors and cancellation are retried at
  the next eligible report. A summary above the output limit is still adopted if it
  shrinks the source, and the overrun is recorded.
- New per-Thread settings: `shellboy thread set --auto-compact-target-percent N`
  (1-100) and `--summary-max-chars N`; `thread get` shows the effective values.
- The TUI "auto-compacting" indicator is kept.
- Fixed: a storage failure during automatic compaction is no longer recorded as an
  ordinary failure followed by the next request; summary retries are bounded by the
  connection's `response_max_retries`; a re-summary of an omitted range no longer
  feeds the omission notice to the summarizer; `compact list` shows the metrics of
  automatic attempts.

## Removed (configuration is strict)

`segment_chars`, `reduce_rounds`, `summary_map_max_requests`,
`summary_reduce_max_requests`, `summary_max_percent` and `--max-percent` are removed
and are rejected if present. Remove them from config files before updating the owner.

## Migration 43

Adds `thread.auto_compact_target_percent` and `thread.summary_max_chars` and drops
the retired `thread.segment_chars` / `thread.reduce_rounds`. No table is added and
saved history, receipts and usage records are not rewritten. As with every
migration, an older binary cannot open a migrated database: keep a backup of the
data directory before the first start of this version.

## Qualification

Exact-SHA hosted CI on Ubuntu, macOS and Windows (all-OS), independent review of the
whole change and of the review follow-ups, local full lib and integration runs.
Installation on a real data directory is a separate, explicitly approved step.
