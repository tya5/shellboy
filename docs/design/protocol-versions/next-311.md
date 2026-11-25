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
