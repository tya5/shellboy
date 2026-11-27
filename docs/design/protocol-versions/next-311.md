# Protocol next (#311)

## Positioned history source

`history.read` accepts an optional `display` projection. Its position is tagged
`absolute`, `from_end`, or `tail`; the first two take a character offset.
The optional selected Thread (`display.selection.thread_id`) fences access to a
node still on that live Thread's current selected path: an append does not
expire a read of an earlier node, while rewind, compaction or deletion that
removes it does (`cursor_expired`). The selection carries no revision. An
original-body cursor is not a display cursor. Repeat the same display selector
with `next_cursor`; a payload change invalidates continuation. Display pages carry their half-open character range,
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

## Thread attachment reload (slice 4g)

`attachment.reload` (`shellboy attachment reload --thread THREAD_ID`) forces
an owner reread of the Thread's selected attachments, even if file size and
mtime match a cached entry. It reports the reload manifest, selection revision,
notices, exclusions and `sent: false`. Attachment listing and draft reads are
unchanged.

Like `shell.read` with transient raw-output pins, this bounded owner observation
creates no operation receipt. Its worker can temporarily pin the selection and
refresh its cache, so owner shutdown and context capture fence it like other
context-changing requests. Reload does not send to a model, change the saved selection or
successful-send baseline, or publish a new event fact. The TUI's explicit reload
calls the same owner operation.

## Shell waits and selected header facts (4d slice 2)

`shell.list` items and `shell.get` add an optional `wait`: the earliest
accepted `wait` that this shell is running now, with its `operation_id`,
`accepted_at`, `timeout_ms`, `condition_count` and `first_condition`. The owner
writes it only while its in-memory registration of that operation matches the
saved pending record for the same Run and shell; the field is absent otherwise,
including after cleanup or a failed cleanup that left the saved record behind.
Registration and cleanup already publish a saved change for the caller's
Thread. Waits are admitted IPC observations (at most 32 at once), so the proof
read is bounded; it runs on the read-only inspection lane.

`thread.get` with `display` now also returns `context_usage`, read in the same
read-only transaction as the header, and `context_usage.last_successful_attempt`
adds the attempt's provider-reported `output_tokens`. `index.status` adds
`pause_kind` (`transport`, `authentication`, `configuration`, `dimension`,
`capacity` or `other`): the owner's classification of the saved pause it
reports, endpoint family before profile. It is absent when no pause is saved,
including a pause caused only by an owner storage stop, and text appended to
`reason` does not change it.

These owner reads, the shell wait proof, and `index.status` run on the
read-only inspection lane (`control::db::read_only`, the existing bounded
inspection workers), so a reader never queues behind the writer; `index.status`
reads its saved state in one transaction. No operation, event fact or CLI
option is added.

## Thread activity (4d slice 2b)

`thread.get` (with or without `display`) adds `activity`, absent when the
Thread has no Run. The owner reads it in the same read-only transaction as the
rest of the result, with no usage aggregation:

- `run`: the latest Run's `run_id`, `state`, `stop_reason`, `ended_at`,
  `owned_shells` (starting, running or stopping, at most 128), `revisit` and
  `waiting_reason`.
- `turn`: that Run's latest Turn with `turn_id`, `generation`, `state`, the
  saved `recovery_reason`, `tools` (`running_since`, `spawned`, `results`,
  `cancelled`, `capped`), `recovery_counts` (`receipts`, `results`,
  `active_shells`, `uncollected_shells`, `capped`) whenever the Turn has a
  saved recovery reason, and `context_reduction` (the automatic reduction's
  character and item counts at the current generation).
- `current_attempt`: that Turn's latest attempt with `attempt_id`, `state` and
  `started_at`. Only when it belongs to the Turn's current generation does it
  carry `automatic_retry`, `retry_exhausted`, the attachment flags, and
  `error_kind` (`model_request` or `model_context_limit`) with `error_message`.
  The owner sets those two only when the Turn's saved recovery reason is
  exactly this attempt's model error; `error_message` is the model's own
  message.
- `automatic_compaction`: `input_tokens`, `threshold` and `started_at` while an
  automatic compaction runs for that live Run and Turn.

Counts stop at 128 and `capped` means "128 or more". The `history.window`
observation `activity` IDs come from the same owner function.

`ShellWait` adds `target_thread_id`: the saved Thread of the first
condition's Run, Turn or first Thread target, so a client can label the wait
without a usage-aggregating `run.get` or `turn.get`.

## Owner marks and output entries on `events.wait` (slice 4d-3)

`EventsWaitResult` adds required `index_changed` and `retention_changed`, marks
like `config_changed`: the newest index change (saved state or pause) and the
newest GC availability change (tombstoned source, raw-output deletion or new
raw retention generation, managed-file deletion) after the cursor. They have no
Thread and ignore `thread_ids`.

`include_output` (CLI `events wait --output`) adds `output`, at most 64
`{thread_id, execution_id, output_at, seq}` entries per page: each execution's
latest committed output batch after the cursor, filtered by `thread_ids`.
`output_at` is the batch's saved receive time (`shell.get` `last_output_at`). Output bytes
stay on `shell.read` and `wait`. Evicting one of the 4096 retained executions
raises the existing floor, so an older cursor gets `resync_required`.

Owner notifications that concern one Thread are now ordinary `changes`
entries for it. A runtime receipt's reservation and settlement are announced
through the matching mark (`storage.collect`: retention; `config.set` and
credentials: config; index operations: index) or through its target's Thread.
No operation, field or capability is added for owner state.

## Index coverage window (unit 4d slice 4)

`index.status` takes its own `IndexStatusArgs` (`scope`, `space_id`,
`thread_ids`, `source_ids`) instead of the shared index selection; it never
accepted `kinds` or `dimensions`. The opt-in `coverage: true` adds
`IndexStatus.coverage_window`: the owner's per-source coverage of one bounded
window of the selection (at most 32 explicit sources per window), with
`coverage` counts, `evaluated_sources`, `inspected_memberships`,
`unverified_sources[{source_id, reason}]` (`body_over_budget`,
`malformed_source`, `unprovable`), `window_complete`, `scope_exhausted`, the
normalized `selection` and `read_at`. `next_cursor` continues through
`coverage_cursor`, which requires `coverage` and the same selection and
embedding profile (otherwise `invalid_argument`). Windows are separate reads,
not one snapshot, and are never summed. CLI: `index status --coverage
[--coverage-cursor CURSOR]`. Without `coverage` the result is unchanged.
