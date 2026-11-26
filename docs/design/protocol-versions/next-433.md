# Protocol next (#433)

## Reply duties awaiting an own ask

A reply duty has a new open state, `awaiting_own_ask`. It is saved at Run-end
settlement when the owing Thread has an open ask it sent after the duty was
admitted; `reply.blocking_message_id` names that ask. Open states are `pending`
and `awaiting_own_ask`; `replied`, `ignored`, `cancelled` and
`recipient_deleted` stay terminal. `message reply` and `message cancel` accept a
duty in either open state, and a `reply_received` wait is satisfied only by a
terminal state. `MessageReplyState` in history provenance gains
`awaiting_own_ask`.

When the blocking ask ends, the duty returns to `pending` and saves
`reply.owner_run_id`, the asker's Run that receives the answer or outcome; its
delivery `run_id` and `turn_id` are unchanged. A pending duty also saves
`reply.admitted_at` when it enters the Thread's history.

## Read surfaces

`message list --awaiting` items carry the saved `reply` object, so they show the
state and blocking ask. `message get` shows the same `reply` object.
`thread get.pending_replies` adds `awaiting_own_ask`: up to 16
`{message_id, blocking_message_id}` pairs, omitted when empty. The
`owed_by` and `asked_by` counts and the Thread summary reply counts include
both open states.
