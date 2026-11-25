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
scoped to that extent; continuations omit it. Each outline field carries
`shape`, its path through the saved payload as `key`, `index` and `count`
segments (`count` follows a collection), so a client renders the field label
in its own language. No TUI layout, localized label or raw payload is added.

## Scoped live event facts

`events.wait` with `include_text` can also report `receive_age_ms` for a live
attempt, including provider keepalives, without exposing provider payloads or
reasoning text. The age is measured by the owner in milliseconds at readback.
Receive timing alone neither advances the event cursor nor completes a wait;
it accompanies the normal response, including a timeout.

`recovery_transitions` contains committed recovery notifications since the
cursor, identified by Thread and event sequence. Baselines and gaps do not ring
historical bells; a gap requests fresh saved state instead. A page returns at
most 64 recovery transitions and may have more even when changes to the same
Thread coalesced.

## Selected history observation

`history.window` accepts `include_observation: true`, and with it an optional
`observe_node_ids` list of at most 128 other saved node IDs whose tool facts
(calls, limits and notices) join the same observation and `read_sequence`. The result adds
`observation` even if its selected path is empty. The default response does
not include this field. The owner observes the window, its pending-message
prefix and typed holds, the selected Thread's activity IDs, and exact saved
attempt-to-node bindings in one read transaction. Ambiguous bindings are not
returned. `revision` still describes the selected path; `read_sequence`
orders observation admissions within the selected owner instance, not across
owners or independent header and sidebar calls.

A pending observation returns up to 128 items ordered by saved receipt
creation and row identity. Each returned row includes its creation time, a
512-character message preview with a truncation indicator, attachment count,
the saved `sender`, `expect_reply` for an open reply duty, `admission_uncertain`
when path admission was not confirmed, `completion_scheduled` for a saved
`complete` delivery, and the typed `held_by` from #423 as its hold reason.
The owner examines at most 129 rows per pending
receipt state before applying the selected-path exclusion. If that prefix
cannot prove completeness, `total_exact` is false and `total` is a lower
bound; unavailable items are not evidence of rejection or delivery.

Opt-in reads accept at most 128 window positions. `tools` reports exact
node/group/call correlations with saved result and current execution facts;
`limited_node_ids` reports per-node call coverage limits. `notices` joins each
saved `shell_ended` notification in the window to its exact execution, call
and result: `execution_available` is false when the execution is missing, and
`source_node_id` is absent when the result is missing or reclaimed. `recovery` describes
saved resume evidence without raw diagnostic records, and `usage` reports
bounded candidate attempt identities and measured usage. These facts cover
only items in the returned window. A response that cannot fit the frame
budget fails with `InputLimit`; it is not returned as a complete observation.

## Combined reply-duty filters

`message.list` combines `asked_by_thread_id` with either `to` or
`owed_by_thread_id` using AND. `owed_by_thread_id` selects unnamed duties and
still conflicts with a named `to`. Continuations bind to the whole filter;
the typed response filter echo still omits the named recipient itself.

## Saved hold kinds

`held_by` (on `message.get` and window pending rows) also reports the saved,
verified hold reasons `previous_instance_interrupted`,
`terminal_before_path_admission` and `uncertain_path_admission` as their own
kinds, like `run_completion`
and `recovery_wait`; neither carries an execution ID. `unknown` remains only
for unresolved live evidence.

`message.get` now reads on the owner's read-only inspection lane, so it never
waits behind the writer.

## Submitted draft snapshots (slice 4c)

`draft.list` (CLI `draft list`) lists restorable or submitted draft records
with the effective `filters` and `limit`, and an opaque `next_cursor` when
more records are available. `draft.get` with an exact operation ID returns the submitted draft record, including its
accepted Thread. An optional Thread selector constrains that exact lookup. Submitted records include the owner-saved
operation, message and source draft/revision binding, even when the frozen body
was collected. No event fact or mutation is added.
