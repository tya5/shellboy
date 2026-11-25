# Positioned history source

`history.read` with `display.position` (`absolute`, `from_end`, or `tail`)
returns a bounded UTF-8 source slice instead of original text. `display.page`
is the slice's half-open Unicode scalar range, `display.extent` names the full
extent, and `display.total_chars` describes the whole source. `display.reasoning`
is clipped to the page. `output_limited` and `received_recovery_wait` remain
separate saved observations. Keep the selector unchanged when supplying
`next_cursor`; when it expires restart without the cursor.

Only the first display page contains `display.outline`. The outline describes
the full named extent, which may be larger than the first page:

- `node`: tagged saved classification. Spill and Summary include bounded source
  node IDs; Summary also reports whether its saved structure was recognized.
  Trim includes saved omission count, reason, and boundary node IDs if present.
- `blocks`: ordered, extent-clipped `range` and `role`; applicable blocks can
  include saved `tool` facts, `event_kind`, and bounded envelope `fields`.
  Tool facts include exact call, group, and execution IDs when available, a
  call ordinal, and saved command, description, status, and exit code. These
  do not describe the current execution.
- `fields`: bounded, redacted saved details as `path`, `value`, and `omitted`.
- `links`: recognized URL, label, and source `range` within the extent.
- `limited`: more structural metadata existed than the projection retained.

Continuations omit the outline. They cannot be used across payload or requested
selection revisions. The outline carries no raw payload, Markdown layout, or
live tool, hold, and attempt state; those live facts belong to the selected
window observation. The source byte ceilings remain 8 KiB per page and 64 KiB
per extent. Outline metadata has the existing independent projection bounds.
