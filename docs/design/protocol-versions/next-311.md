# Protocol next (#311)

## Submitted draft snapshots (slice 4c)

`draft.list` (CLI `draft list`) lists restorable or submitted draft records
with the effective `filters` and `limit`, and an opaque `next_cursor` when
more records are available. `draft.get` with an exact operation ID and Thread
returns the submitted draft record. Submitted records include the owner-saved
operation, message and source draft/revision binding, even when the frozen body
was collected. No event fact or mutation is added.
