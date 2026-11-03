# Shellboy Release Notes (Draft)

Safer long-running conversations, clearer progress, and more consistent terminal
behavior.

**Version:** to be decided (`0.7.0` suggested: breaking CLI changes).
**Scope:** Changes on `main` since the `v0.6.1` release tag (`93e527c`, tag
commit `45c34c75e266c5bfde3d28e07a303bdea93e137a`), through
`18103ce70f4a9a1940e06da0087613685e63f78e` (2026-11-03). The package version
has not been bumped; this is not a release candidate or publication record.

## Breaking changes

- Public Turn-end fields use “end message” instead of “answer”: for example,
  `answer_node_id` is now `end_message_node_id`, and `last_answer` is now
  `last_end_message`. `history get --final-answer-only` is renamed
  `history get --end-message-only`. These are breaking public-name changes with
  no compatibility aliases; the internal database column is unchanged (#194).
- `revisit set/get/cancel` and the `message send --now` option are removed. Use
  `message send --deliver now` or `message send --deliver complete` instead.
  `complete` waits while the recipient Run is live. At normal completion,
  waiting messages are delivered together, in send order, as one new Turn of
  that Run. A waiting message never outlives its Run: if it stops, is shut
  down or enters recovery wait instead, the message is released then as
  ordinary held input, and the usual resume or retry rules decide when it is
  admitted. Later default or `now` messages overtake waiting `complete`
  messages. `now` releases a foreground shell wait for urgent delivery (#187).

## Before upgrading

- **Stop the owner and back up the complete data directory.** Conversation
  storage advances from schema 19 in v0.6.1 to schema 24. Standalone RAG remains
  schema 3 and the local protocol remains version 2. Compatibility with earlier
  releases is not guaranteed before 1.0.0.
- **Update the owner and CLI together, then restart the owner.** Replacing an
  executable does not update an already-running owner.

## New features

- The TUI shows long-running database work in the existing status/footer area:
  after one second it displays the operation and elapsed time, or a no-progress
  warning. The labels are localized (#190).
- Startup displays bounded progress through startup and migration stages rather
  than appearing idle during that work (#191).
- In the composer, Shift+Enter inserts a newline when keyboard enhancements are
  enabled (#175).
- More operation forms, activity/live-work labels, recovery diagnostics and
  received-item details follow the selected English or Japanese display language
  (#150, #152). This expands coverage; it does not claim every UI string is
  translated.
- A new [Shell guide](../guides/shell.md) covers noninteractive Git use and
  diagnosing stuck shell processes.

## Changed behavior

- Long conversations are compacted proactively at a safe request boundary once
  reported input reaches the configured threshold (200,000 tokens by default).
  During a long Turn, compaction can also summarize an eligible older segment
  while preserving the current request, later user instructions and recent work.
  Incomplete or protected ranges are not compacted (#180, #181, #189).

## Fixes

- Conversation-history row clicks now expand only eligible tool/notice rows;
  ordinary message rows do not trigger disclosure, and command details appear
  only while their tool row is expanded (#177).
- Narrow conversation panes preserve wide glyphs and wrap nested rows without
  clipping or losing indentation. Fallback body hints remain on their own rows
  (#179).
- Overflow recovery trims retain every user input from the active Turn (#185).
- Watch results retain their order and separators in English; more recovery,
  receipt and operation diagnostics display in the selected UI language
  (#150, #152).
- Startup integrity checks remain pending after an interrupted startup and are
  skipped only after a clean shutdown has recorded completion (#192).
- Usage-limit retries no longer continue past the provider's reported reset
  time (#188).

## Performance

- The compaction changes reduce retained prompt context, but this draft makes no
  measured latency or cost claim for compaction (#180, #181, #189).
- Requests now append the Turn context to saved history instead of regenerating
  it at the end of every request, allowing provider prompt caches to reuse the
  full preceding request as a prefix (#183). In the small paid before/after
  measurement, cached input excluding the first request rose from 42.8% to 79.4%
  and uncached input fell 55% (25,657 to 11,524 tokens). Total input rose 5%
  because older Turn context is retained. These are observed results, not a
  guarantee: the report records a single back-to-back pair, provider-cache
  variance, and a synthetic workload. See the
  [2026-11-03 benchmark report](../validation/bench-report-2026-11-03.md).

## Downloads and validation

This is a release-notes draft only. No candidate archives, checksums, upgrade
verification or release publication are claimed here. Those checks belong to the
release candidate built from the final approved main commit.
