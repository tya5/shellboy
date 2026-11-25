# Protocol next (#311)

## Positioned history source

`history.read` accepts an optional `display` projection. Its position is tagged
`absolute`, `from_end`, or `tail`; the first two take a character offset.
The optional selected Thread/revision fences access to a node still in that
selection. An original-body cursor is not a display cursor. Repeat the same
display selector with `next_cursor`; a payload or selection revision change
invalidates continuation. Display pages carry their half-open character range,
the full extent's range, total characters, reasoning ranges, and saved node
projection facts. Each page contains at most 8 KiB of source bytes; the named
extent preserves the original source geometry and contains at most 64 KiB.

The first page also carries `display.outline`, typed immutable source facts
scoped to that extent; continuations omit it. No TUI layout or raw payload
is added.

## Scoped live event facts

`events.wait` with `include_text` can also report `receive_age_ms` for a live
attempt, including provider keepalives, without exposing provider payloads or
reasoning text. The age is measured by the owner in milliseconds at readback.

`recovery_transitions` contains committed recovery notifications since the
cursor, identified by Thread and event sequence. Baselines and gaps do not ring
historical bells; a gap requests fresh saved state instead. A page returns at
most 64 recovery transitions and may have more even when changes to the same
Thread coalesced.
