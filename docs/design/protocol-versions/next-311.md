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
