# Protocol next (#311)

## Commands

`history.recall` reads one Thread's selected path in the given direction. An
optional anchor carries an exact ordinal and node ID; a continuation also gives
`expected_revision` from the previous result. A displaced anchor or changed
continuation revision returns `cursor_expired`.

## Results

`found` returns one saved input's exact text and identity. `not_in_window`
returns a boundary and selected-path revision; `end` means the search reached
the path end. Each command examines at most 200 candidate ordinals and one
lookahead. It loads only one eligible payload, bounded to 8 MiB before decoding,
and refuses text beyond the draft limit or the native frame limit without
truncation. The TUI allows only two windows per keypress (one continuation),
then reports a coverage-limit notice if more path exists. No schema or index.

## Bounded Thread display header

`thread.get` accepts `display: true` for a bounded, single-Thread header. The
ordinary request and response are unchanged. The display projection returns
`ThreadDetails` with `display_coverage`: the optional title fallback is
`limited`, `untitled`, or `not_sent`; `recovery_limited` identifies a saved
recovery reason outside the 64 KiB allowance. Clients can localize these
outcomes without guessing from display strings. No continuation. This is not
an inspector or full context-usage read.

## Received history page

`history.received` reads one literal live Thread, in descending Turn creation
order (Turn rowid breaks ties) and ascending node ID within a Turn. The optional
opaque continuation seeks after the last examined position, even on a page
with zero matches. It is scoped to this Thread and read route, not a frozen
snapshot. Its presence means there is more candidate coverage, not that a
further matching record is already known. No global count or OFFSET scan.

One request examines up to 32 Turns and 200 node candidates, with one lookahead
per index seek, and returns up to 32 saved records. The entire page checks at
most 128 Thread receipt rows. The owner merges the four ordered state ranges
of the existing `receipt_target` covering index by creation time and rowid,
with one extra key as a sentinel and at most three other lookahead keys. A
proven first eligible request is shown even when the prefix is incomplete.
Unresolved Turns carry `request_limited`; their saved responses stay reachable.
The full per-Turn seek is deferred to #412. Before extracting either
80-character excerpt, the owner gates
source JSON at 64 KiB and returns explicit limit flags. Raw provider payloads
and received reasoning are not included. Open an exact returned node through
the existing original-source reader. No new schema or maintained index.
