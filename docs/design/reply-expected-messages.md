# Reply-expected messages (#194)

Status: **Implemented on the migration-26 integration branch (2026-11-03);
final checks, independent review and main landing remain gates.** This design turns the
user's decisions on #194 (comments marked "決定" and the answers to the memo's
questions, 2026-11-03) into a concrete design, lists every existing behaviour
it touches, records the answered questions under [Decisions](#decisions) and
ends with a landable [implementation plan](#implementation-plan). Points the
user did not decide explicitly are marked **Judgement** and are also in the
[decision log](../development/decisions.md).

## Summary

A sender can mark a message `--expect-reply`. The recipient Thread's model (or a
human) answers it once with `shellboy message reply --message ID`. The reply is
attached to the original message for every sender kind and, when the sender is
a model, is also delivered to the sender's Thread as an ordinary message.
`wait` gains one condition, `reply_received`. A Run that tries to complete with
an unanswered reply-expected message is woken once; the second time the message
becomes `ignored`. Delegation, `exec` and the leader's flow all use this one
mechanism. The Turn's last assistant text stops being treated as "the answer to
a message": `message get` loses `answer_node_id`, and Run/Turn/Thread
projections keep the same fact under the name "end message". `exec` chooses
what it returns with `--result reply|turn-end|run-end` (default `reply`).

## Decisions this design implements

| # | User decision (2026-11-03) | Where |
|---|---|---|
| D1 | Wake once at Run end; on the second detection record "ignored" as a stored notice, release waits, notify the sender | [Run-end rule](#run-end-rule) |
| D2 | `message send --expect-reply`, combinable with `--deliver now\|complete` | [CLI](#cli) |
| D3 | One reply per message; replying clears the pending state; progress uses ordinary messages | [Reply operation](#message-reply) |
| D4 | The recipient Thread's model replies; a human may also reply from the TUI if that is simpler | [Who may reply](#authority), [Human reply](#human-reply) |
| D5 | Recipient Thread deleted: release waits and notify the sender | [Outcomes](#outcomes) |
| D6 | External senders wait with `wait reply_received`; four release reasons; `--timeout-ms` bounds only that call; pending replies have no deadline; `exec` and "thread request" become expect-reply + `reply_received` | [Wait](#wait-condition-reply_received), [exec](#exec) |
| D7 | Delegation is unified onto this mechanism | [Delegation mapping](#delegation-mapping) |
| D8 | Replace every `answer_node_id` use without compatibility paths | [Implementation plan](#implementation-plan) |
| D9 | `exec --result reply\|turn-end\|run-end`, default `reply`; `turn-end`/`run-end` send an ordinary message with no reply duty | [exec](#exec) |
| D10 | An empty end message is a normal response: a successful empty result, exit 0 in every mode, also with `--schema`; validation and repairs apply only to non-empty text (refined by the user) | [exec](#empty-end-message-and-no-end-message) |
| D11 | A Turn ended by Esc produced no end message: treated like a stopped Turn, exit 4, in every `--result` mode that reads an end message | [exec](#empty-end-message-and-no-end-message) |
| Q1–Q17 | Answers to the memo's questions | [Decisions](#decisions) |
| Earlier comments | Sender may cancel; cancelled recipient is notified through the existing event mechanism; fail-safe count cap reusing an existing queue bound; reply carries the original ID; both messages are stored in the DB and read on demand | [Data model](#data-model) |

## Facts found while reading the code

These correct or sharpen assumptions in the issue thread.

1. **Message receipts are not collected after 24 hours.** The 24-hour /
   pressure collection applies to `operation_receipt` outcome bodies (CLI
   operation idempotency, `src/control/receipt.rs`). The message `receipt` table
   has no deletion path outside tests; a row lives as long as the database, and
   its message node is a GC root while the target Thread is not deleted
   (`src/history/reachability.rs`). Reply state stored on the message receipt
   therefore needs **no GC exemption**. The only GC question is how the reply
   node stays rooted; see [Reply node](#reply-node).
2. **`thread request` is not a send-and-wait command.** It is the standalone,
   read-only debug command that rebuilds a saved request
   (`src/request_inspect.rs`). Nothing in it reads an answer. The send-and-wait
   pattern the issue means is the delegation recipe "send, wait
   `turn_completed`, read `answer_node_id`". This memo redefines that recipe and
   leaves the debug command unchanged (decision Q1).
3. **`message get`'s `answer_node_id` is the answer of the Turn the message was
   assigned to**, not a reply to that message (`message_details` joins
   `turn.answer_node_id`). Several messages assigned to one Turn all report the
   same node; a message admitted mid-Turn reports whatever the Turn said last.
   This is the defect the reply link replaces.
4. **The model never sees message IDs or senders today.** A received message is
   replayed as plain `user` text. A model cannot reply to a message it cannot
   name, so admission must add a short notice carrying the ID (see
   [Model-visible notices](#model-visible-notices)).
5. **Existing windows of 128 are not a queue admission invariant.** The
   pending-message projection (`/queue`, 128 rows), `message cancel` (1..128 IDs)
   and the pre-integration interrupted-message recovery all used 128. There is
   no total cap on ordinary queued messages. The integration replaces the old
   recovery refusal with oldest-first progressing units; see the recovery
   decision at the end of this document. The pending reply-duty cap remains128.
6. **The #187 completion hook** is `driver::next`, in the branch where no Turn,
   pending input, event or owned work remains: `revisit::release_tx` runs and,
   if it found nothing, `state::complete_idle_run`. The shutdown gate already
   skips starting new work there.
7. **A completed Turn can have no end message.** A Turn that a human ends
   with Esc is set `completed` without an `answer_node_id` and ends through the
   normal `driver::next` completion (control.md #52). Only
   `state::complete_answer_turn` writes the node, and it keeps the Turn open
   while the Turn has unapplied accepted receipts.
8. **`history get --final-answer-only` is not redundant with ordinary reads.**
   Ordinary `history get` of an assistant node flattens reasoning and natural
   text; the projection returns only the last `final_answer` (or unphased)
   assistant message and has its own cursor scope. Today it fails with
   `target_unavailable` when the node holds no such message.
9. **Error codes.** `ErrorCode` has no `forbidden` or `not_found`; unknown
   wait targets and unknown messages are `target_unavailable`. `forbidden` and
   `not_found` exist only as `message cancel` per-item outcomes.
10. **exec waits on IDs fixed at send.** It reads `MessageAccepted.run_id` /
    `turn_id`; an unassigned send is exit 4 `held`. `thread_settled` without a
    cursor reports current settled states, so it would match an earlier Run of
    an existing Thread.

## Data model

All state lives in rows that already exist. No table, worker, queue or
in-memory registry is added.

### Saved fields

| Fact | Where | Shape |
|---|---|---|
| Reply-expected flag | original message's `receipt.result_json` | `"expect_reply": true` (absent = false). Also in the saved `MessageAccepted` acknowledgement and the operation fingerprint |
| Pending-reply state | same `receipt.result_json` | `"reply": {"state": "pending"}` written at send |
| Outcome | same object | `state` ∈ `pending` / `replied` / `ignored` / `cancelled` / `recipient_deleted`, plus `at` (saved time) for any terminal state |
| Reply link (forward) | same object | `"node_id"` of the reply node and `"by"` (credential-free `MessageSender` of the replier) when `replied` |
| Reminder fact | same object | `"reminded_at"`, `"reminded_run_id"` after the one Run-end wake |
| Cancellation actor | same object | `"cancelled_by"` (existing actor shape) and `"cancel_reason"` (`sender` or `sender_deleted`) when `cancelled` |
| Reply link (reverse) | the reply's own delivery receipt (model senders only) | `"reply_to": ORIGINAL_MESSAGE_ID` in its `result_json` |

`receipt.state` (`accepted`/`held`/`applied`/`cancelled`) keeps its current
meaning — admission of the message into the recipient's history — and is
independent of `reply.state`. A message can be `applied` with `reply.state =
pending`.

### Reply node

The reply body is one ordinary `user` node
(`{"role":"user","content":[{"type":"input_text","text":…}]}`), the same shape
`message send` writes, authorised for indexing the same way. One node serves
both deliveries:

- **Sender is a model:** the reply is also an ordinary message to the sender's
  Thread, so the node is that delivery receipt's `message_node_id`. The
  existing receipt root keeps it while the sender's Thread lives; the delivery
  assigns its `turn_id` as for any message.
- **Sender is `external_cli`, `human` or `unknown`:** there is no receiving
  Thread. The node's `turn_id` is set to the original receipt's assigned Turn
  in the recipient Thread. The existing Turn root then keeps it exactly as
  long as the original message (until the recipient Thread is deleted). This is
  why a reply requires the original to be `applied` (it then has a Turn).

`message get` follows `reply.node_id` for every sender kind. If the sender
Thread of a model-sent message is later deleted, its copy of the reply can be
collected; `message get` then reports the body as unavailable with the existing
`body_available=false` convention rather than inventing content.

History provenance for a reply `user` node identifies the replier using the
credential-free `reply.by` saved on the original receipt when the reply has no
ordinary delivery receipt (external CLI, human, or unknown sender). When a model
sender receives the same reply node through ordinary delivery, its receipt
sender is the same replier identity and is used as the provenance source. The
node remains an ordinary `user` node in the recipient Thread; sender identity is
provenance, not a distinct node kind.

### Outcomes

| Outcome | Trigger | Saved where |
|---|---|---|
| `replied` | `message reply` commits | original `reply` object; reply node; delivery receipt for model senders |
| `ignored` | second Run-end detection (D1) | original `reply` object; notice to a model sender |
| `cancelled` | sender (or a human) runs `message cancel` while pending; also when the sender's Thread is deleted (`cancel_reason: sender_deleted`, decision Q4) | original `reply` object; notice to the recipient if it had already received the message |
| `recipient_deleted` | `thread delete` of the recipient Thread while pending | original `reply` object, in the deletion transaction; notice to a model sender |

Every terminal transition happens in one DB transaction on the existing DB
worker, after which the existing wait `Notify` and `Changed` fire. Terminal
states never change again.

### Fail-safe cap

A send with `--expect-reply` is rejected with the existing `input_limit` error
when the target Thread already has **128** messages with `reply.state =
pending`. 128 is reused, not chosen: it is the existing bound of the pending
queue projection, `message cancel` and resume-time message recovery. Reusing it
also keeps every pending-reply list (`thread get`, the Run-end reminder) inside
one bounded response with no cursor. Verification will include a test at
exactly 128/129 and record the size of a 128-entry reminder notice; no timing
or size is assumed here. The cap is per recipient Thread because the recipient
is the one that must work through the list (decision Q15).

### Migration 26

Main is at schema 24 (`0024_revisit_messages.sql`). The Run-end check, the cap
check and the `thread get` list all ask "pending reply-expected messages of
Thread T". Without an index that scans every receipt the Thread ever received,
at every Run completion, which the hot-query rules forbid. Migration 26 adds
only:

```sql
CREATE INDEX receipt_reply_pending ON receipt(target_thread_id)
  WHERE json_extract(result_json,'$.reply.state')='pending';
INSERT INTO schema_migration(version,name) VALUES(26,'0026_reply_expected');
PRAGMA user_version=26;
```

It moves no data. Message lookups by ID use the existing
`receipt_message_node` index. `src/storage/current.sql`,
`scripts/check-release-upgrade.py` and `scripts/error_catalog.py` move to schema
25. The internal column `turn.answer_node_id` keeps its name (renaming it would
touch every SQL string and the #74 index for no behavioural gain); only public
names change.

## CLI

### `message send --expect-reply`

```
message send [--thread THREAD] [--deliver now|complete] [--expect-reply]
             [--sender-label TEXT] (--text TEXT | --text-stdin)
```

- Combines freely with every `--deliver` timing; admission timing and the
  reply state are independent.
- Rejected (`invalid_argument`) when a model targets its own Thread: it would
  wait on itself.
- Rejected (`input_limit`) at the cap above.
- The result adds `"expect_reply": true`; everything else is unchanged.
- Without the flag, behaviour is exactly today's.

### `message reply`

```
message reply --message ID (--text TEXT | --text-stdin) [--sender-label TEXT]
```

One transaction:

1. Load the original receipt. Unknown ID → `target_unavailable` (as
   `message get`). No `expect_reply` → `invalid_argument`.
2. Authority (see [Authority](#authority)); failure → `invalid_state` with
   `details.reason = "not_recipient"`. No new error code is added: the
   `ErrorCode` set has no `forbidden`/`not_found` (those exist only as
   `message cancel` per-item outcomes).
3. `receipt.state` must be `applied` (the recipient has received it), else
   `invalid_state` with `details.delivery_status`.
4. `reply.state` must be `pending`, else `invalid_state` with
   `details.reply_state` (covers double replies, cancelled, ignored and deleted).
5. Check `limits.draft_bytes`. Write the reply node.
6. Set `reply = {state: replied, at, node_id, by}`.
7. If the original sender is a model whose Thread is not deleted, deliver the
   same node to that Thread through the ordinary `assign_message` path
   (default timing), with `reply_to` in the delivery receipt. All ordinary
   rules apply: new Run, new or existing Turn, held while stopping or in
   recovery wait.

Result: `{reply_to, message_id, delivery}` where `message_id` is the reply
node ID and `delivery` is the ordinary `MessageAccepted` when step 7 ran, or
absent. It is a mutation: same-ID replay returns the saved result, a changed
body under the same operation ID is `operation_conflict`, and a re-issue under a
new ID after the operation outcome expired hits step 4, so a reply is never
duplicated.

### How a model lists pending reply-expected messages

No new list operation. `thread get` adds `awaiting_replies`: up to 128 unified items
`{message_id, sender, created_at, delivery_status, reminded}` for the Thread,
oldest first (the cap guarantees completeness). Bodies are read with
`message get` / `history get --node`. Besides that, every reply-expected message
is announced with its ID when admitted, and the Run-end reminder lists the
remaining ones, so a model that lost the ID to compaction can recover it.

### Human reply

Humans have no Origin, so they may reply to any message through the CLI
immediately (`shellboy message reply --message ID --text …` from another
terminal). The TUI adds a small reply mode in this issue (decision Q7, **Judgement**: landed last, after the CLI path):

- `/replies` opens a shared-framework popup listing the current Thread's
  `awaiting_replies` items (recipient kind/Thread, sender, excerpt, elapsed time).
- Enter on an item puts the composer into reply mode, shown as one line above
  the composer (`Replying to <sender>: <excerpt>`); Esc leaves the mode.
- The next submit sends `message.reply` instead of `message.send`, text only.

### `message get`

Removes `answer_node_id`. Adds, only when relevant:

- `expect_reply: true`;
- `reply: {state, at?, node_id?, by?, reminded_at?}`;
- `reply_to: ORIGINAL_ID` when the message is itself a reply delivered to a
  model sender.

Reading the reply body is `history get --node REPLY_NODE_ID`, as for any node.

### `message cancel`

Unchanged for ordinary messages. For a reply-expected message with
`reply.state = pending`:

- still queued (not yet admitted): cancelled exactly as today; `reply.state`
  becomes `cancelled` in the same transaction; the recipient never saw it, so
  it gets no notice;
- already admitted: new per-item outcome `reply_cancelled` (today this is
  `already_admitted`). History is untouched; `reply.state` becomes `cancelled`
  and the recipient gets a notice.

Authority is the existing cancel rule: a model may cancel only messages sent by
its own Thread; a human may cancel any. Cancellation still never wakes the
recipient.

### `wait` condition `reply_received`

```json
{"type":"reply_received","message_id":"MESSAGE_ID"}
```

- Registration errors: unknown ID → `target_unavailable` with
  `details.target_id` (the existing unknown-wait-target rule); message without
  `expect_reply` → `invalid_argument`.
- Satisfied when `reply.state` is not `pending`. `status` is `satisfied` for all
  four reasons; the reason is in the snapshot:
  `{"type":"reply","message_id","state":"replied|ignored|cancelled|recipient_deleted","reply_node_id"?,"at"}`.
  As with `turn_completed`, satisfied does not mean success.
- `--timeout-ms` bounds only that call (D6). A pending reply has no deadline.
- Model callers: the cycle check adds the edge caller shell → recipient
  Thread's live Run, the same edge `thread_settled` already uses, so a parent
  and child waiting on each other's replies is rejected as `wait_cycle`.
- Any number of waiters on one message are all released by the one state
  change; each re-reads the DB.
- A reply delivered as a message to a model caller that is waiting in a
  foreground shell would also set the existing `interrupted` condition. When the
  arriving receipt is the reply to a waited message, `satisfied` takes
  precedence.
- It combines with other conditions under the existing any semantics, for
  example `thread_settled` with `recovery_wait/failed/stopped` to stop waiting
  when the recipient stalls (the release reasons do not include those, D6).

## Run-end rule

Placed in `driver::next`, in the completion branch, immediately after
`revisit::release_tx` returns no Turn (so waiting `--deliver complete` messages
are always delivered first), and skipped once the shutdown start gate is closed
(as Revisit is):

1. `P` = this Thread's receipts with `reply.state = pending` and
   `state = applied` (index `receipt_reply_pending`). Queued or held
   reply-expected messages are not in `P`: the model has not seen them.
2. `P` empty → complete the Run as today.
3. Every message in `P` that already has `reminded_at` becomes `ignored`
   (one transition each; notice to model senders; waits released).
4. Messages in `P` without `reminded_at` get `reminded_at`/`reminded_run_id`
   and one `reply_reminder` event listing them is delivered to this Run; the
   existing event path creates a new Turn and the Run continues.
5. If step 4 found none, complete the Run.

Steps 1–5 run in the transaction that would otherwise call
`state::complete_idle_run`, so a reply or cancel committed concurrently is
ordered against them on the DB worker. This extends the "Run continue/normal
end" row of [Control](control.md#状態境界の規範表): the completion check order
becomes unfinished Turn, owned work, unprocessed notices, waiting `complete`
messages, then pending replies. The rule "an answer alone does not complete a
Run" is unchanged; a pending reply is one more reason to continue, bounded to
one extra Turn per message.

"Once" is per message, not per Run. A message reminded in a Run that is later
stopped is not reminded again in the next Run (decision Q9). A human Esc does
not suppress the reminder, matching #187's rule that Esc does not suppress
completion delivery: Esc ends the Turn through the normal `driver::next`
completion (control.md #52), so the rule above runs. Because the reminder is
once per message, a second Esc after the reminder Turn lets the Run complete and
marks the messages `ignored`. While a reminder Turn runs, the TUI shows it (the
`reply_reminder` event row, rendered like other wake events) so a human can see
why the Run continued (decision Q11). Stop, recovery wait and shutdown are not
completions: pending replies stay pending; the recipient may reply after
resuming and the sender may cancel (decision Q2).

## Delivery of the reply

- **Attached to the original, every sender kind:** the `reply` object on the
  original receipt (`message get`, `wait reply_received`).
- **Model senders additionally:** an ordinary message to the sender's Thread,
  with the replier as its verified sender and `reply_to` saved on its receipt.
  It wakes or starts work like any message from that sender.
- **No sender inbox is created** for external or human senders (D6).

### Model-visible notices

Admission already appends saved `event` nodes in the
`Shellboy event v1: {…}` developer form. One helper, called from
`message_delivery::mark_admitted_tx` (the common point where receipt nodes
enter a Thread's path), appends after each admitted node:

| Admitted message | Notice payload kind | Content |
|---|---|---|
| has `expect_reply` | `reply_expected` | `message_id`, sender kind/label, and the command form `shellboy message reply --message ID` |
| has `reply_to` | `reply` | `reply_to`, and the original's first line, bounded by the existing 512-character excerpt |

Other notices:

| Notice | Event | Wakes? |
|---|---|---|
| Run-end reminder | new kind `reply_reminder`, delivered to the completing Run | yes, by construction (it is the reason the Run continues). Payload: IDs, sender kinds and times only, no bodies |
| Sender cancelled an admitted message | `context_changed` with payload kind `reply_cancelled` | no; delivered at the recipient's next request boundary (existing rule) |
| `ignored` / `recipient_deleted` to a model sender | new kind `reply_outcome`, delivered to the sender Thread's live Run | yes, like `child_run_ended`. With no live Run it is a `context_changed` notice seen at the next Turn and does not start a Run (decision Q6) |

New event kinds `reply_reminder` and `reply_outcome` are bounded per message
(at most one reminder entry and one outcome each), owned by the existing event
and delivery rows, and retained/collected by the existing #170 event retention.

### TUI rendering

Option B is adopted (decision Q17): no new mark is introduced and no decided
mark changes (the shared row vocabulary `◉ ⁂ $ ↳ ⋯ ├─└─` is unchanged).

| Row | Rendering |
|---|---|
| Reply on the sender's Thread | ordinary user-style row with the sender glyph chosen by the existing source rules (`↳` for a parent/model sender, and so on), followed by a label and the original's excerpt cut to the row width: `↳ 子 · 返事: 「Task: read-only…」` (English UI: `Reply: "Task: read-only…"`) |
| Original on the recipient's Thread | the existing row, with a state tag after the existing sender label: `返事待ち` / `返事済み` / `返事なし` / `取消` / `相手削除` (English: `awaiting reply` / `replied` / `ignored` / `cancelled` / `recipient deleted`) |
| Queued reply-expected message (`/queue`, rows above the composer) | the same `返事待ち` / `awaiting reply` tag |

Tags and labels go through the typed UI message boundary (#139), so both UI
languages are covered. The assistant row label `回答` is unchanged
(**Judgement**, decision Q10): the rename to "end message" is limited to
public field names, and the memo gave no recommendation to change UI wording.

## End message (renamed answer)

The Turn's final assistant node remains a saved fact, renamed in public
contracts from "answer" to "end message" so it is no longer read as a reply:

| Today | Design |
|---|---|
| `turn get` / `run get` `answer_node_id` | `end_message_node_id` |
| `wait` turn snapshot `answer_node_id` | `end_message_node_id` |
| `thread list/get` `last_answer` | `last_end_message` (same fields: `node_id`, `turn_id`, `at`, `excerpt`, `body_available`) |
| `status` item `answer_node_id` | run/turn/attempt items: `end_message_node_id`; message items: `reply_node_id` (from the receipt, no longer the Turn's answer) |
| `child_turn_completed` / `child_run_ended` payload `answer_node_id` | `end_message_node_id` |
| interruption context `answer_node_id` | `end_message_node_id` |
| `message get` `answer_node_id` | removed; see `reply` |
| `history get --final-answer-only` / `HistoryGetArgs.final_answer_only` / cursor scope `history.get.final_answer` | `--end-message-only` / `end_message_only` / `history.get.end_message` (same projection; see [Decisions](#decisions) Q12) |
| `exec` diagnostics `answer_node_id` | `reply_node_id` or `end_message_node_id`, by `--result` mode |

Guides describe it as "the last assistant message of the Turn; not a reply and
not proof of success".

### exec

`exec` gains one option (decision Q14 / D9):

```
exec --schema FILE [--result reply|turn-end|run-end] [--thread ID] ...
```

`--result` is client-side only: it selects which saved fact exec reads and
validates. It adds no wire field and no owner behaviour. The default is
`reply`. All three modes keep the existing Schema, repair, timeout and
sender-label rules; repairs are sent in the same mode.

| Mode | Send | Wait | Result text |
|---|---|---|---|
| `reply` (default) | `message send --expect-reply` | `reply_received` on the message **and** `run_ended` on the Run the message was assigned to | the reply node (`message get` → `reply.node_id` → `history get --node`) |
| `turn-end` | ordinary `message send`, no reply duty | `turn_completed` on the Turn the message was assigned to | that Turn's end message (`turn get` → `end_message_node_id` → `history get --end-message-only`) |
| `run-end` | ordinary `message send`, no reply duty | `run_ended` on the Run the message was assigned to | that Run's end message (`run get` → `end_message_node_id` → `history get --end-message-only`) |

The composed request keeps today's "return ONLY one JSON value matching this
Schema" contract. In `reply` mode it says to put that value in the reply to
this message (the admission notice carries the message ID and the command
form); in `turn-end` / `run-end` mode it says to make it the final message, as
today. All modes keep today's checks: the send must be assigned (`run_id` and,
for `turn-end`, `turn_id` present), otherwise exit 4 `held`; the message must be
`applied` before its result is read.

**Which end message.** Both modes read the saved end message of a Turn: the
last assistant node of a Turn that completed normally. A Turn ended by Esc
has none (see below).

- `turn-end` reads the Turn the message was assigned to (`MessageAccepted.turn_id`,
  the same value `message get` shows). That is the Turn that processed the
  message. If the message joined an already running Turn (`existing_turn`),
  that Turn's end message can also respond to other input, as today. A Turn
  stays open while it has unapplied accepted receipts, so its end message is
  always produced after the message was applied. exec returns as soon as that
  Turn completes, even if the Run then continues.
- `run-end` reads the Run the message was assigned to and waits for that Run
  to end. The Run's end message is the end message of its **last** Turn, the
  value `run get` already derives (latest Turn by creation order). A Run can
  hold several Turns after the one that processed the message: Turns started
  by `--deliver complete` messages released at completion (#187), the Run-end
  reminder Turn for any pending reply-expected message in that Thread (from
  any sender), Turns for later messages or wake events, and the Run also waits
  for owned shells and child Runs. `run-end` therefore returns what the Thread
  said last before going idle; `turn-end` returns what it said when it
  finished this message. Neither mode creates a reply duty, so neither mode
  causes a reminder for its own message.

#### Empty end message and no end message

Two user decisions (D10, D11) separate these cases:

- **Empty end message (D10, refined by the user).** The Turn produced an end
  message whose text is empty, for example a node holding only reasoning. That
  is a normal response and a successful empty result: exec exits 0 with an
  empty stdout line in every mode that reads an end message, including when
  `--schema` is given. Schema validation and repairs apply only to non-empty
  text, so an empty end message is never a mismatch and never triggers a
  repair. Exit 0 therefore means "valid JSON, or an empty end message".
  `history get --end-message-only` returns empty text for such a node instead
  of today's `target_unavailable`.
- **No end message (D11).** The Turn completed without producing an end
  message at all: a Turn ended by Esc completes with `end_message_node_id`
  null (fact 7). There is no result to return, so it is treated like a stopped
  Turn: exit 4 `execution_unavailable` under the existing
  stopped/held/failed/unavailable rule, with `end_message_node_id: null` in the
  diagnostics. It is never turned into empty text.

Per mode:

| Mode | Empty end message | No end message (Esc) |
|---|---|---|
| `reply` | not applicable: the result is the reply, and reply text follows the non-empty `message send` text rule | not applicable: an Esc-ended Turn does not end the wait; the Run-end rule reminds once, then `ignored` → exit 4 |
| `turn-end` | the processing Turn's end message is empty → exit 0, empty result, no validation or repair (also with `--schema`) | the processing Turn was ended by Esc → exit 4 |
| `run-end` | the Run's last Turn's end message is empty → exit 0, empty result, no validation or repair (also with `--schema`) | the Run's last Turn was ended by Esc → exit 4, even if an earlier Turn of the Run had an end message |

A deleted (tombstoned) node is also exit 4, as today: the saved fact is
unavailable.

#### Exit codes

| Exit | `reply` | `turn-end` | `run-end` |
|---|---|---|---|
| 0 | valid JSON in the reply | valid JSON in the Turn's end message, or an empty end message (D10) | valid JSON in the Run's end message, or an empty end message (D10) |
| 2 | input error (unchanged) | same | same |
| 3 | mismatch after repairs (unchanged) | same, non-empty text only | same, non-empty text only |
| 4 | held at send; `recovery_wait`; Run `stopped` / `failed` with the reply still pending; reply `ignored`, `cancelled`, `recipient_deleted` | held at send; Turn `recovery_wait`, `stopped`, `failed`; Turn ended by Esc with no end message (D11); Thread deleted; node tombstoned | held at send; Run in recovery wait (the `run_ended` wait reports `recovery_wait`), `stopped`, `failed`; last Turn ended by Esc with no end message (D11); Thread deleted; node tombstoned |
| 5 | client deadline; the message stays pending and the Run-end rule later marks it `ignored` unless answered | client deadline (unchanged) | client deadline (unchanged) |
| 6 | owner/transport/protocol/output failure (unchanged) | same | same |

`turn-end` and `run-end` never produce the reply outcomes (`ignored`,
`cancelled`, `recipient_deleted`), because they create no reply duty.
**Judgement:** they keep exit 4 for the execution failures exit 4 already
covers today (held, recovery wait, stopped, failed, unavailable). The user's
statement that these modes "never exit with code 4" is read as "never exit 4
for an ignored reply"; giving held/stopped/failed a new code would change the
existing contract for no benefit. Recheck if the user meant the literal reading.

In `reply` mode, a reply that is committed wins over a later stop: exec reads
`message get` after the wait and succeeds if `reply.state` is `replied`, even
when the Run has since ended. When the Run completes normally the reply state
is already terminal (the Run-end rule marks unanswered messages `ignored`
before completion), so `run_ended` cannot leave exec without an outcome. exec
uses `run_ended` of the assigned Run rather than `thread_settled`, because
`thread_settled` without a cursor would match an earlier failed Run of an
existing `--thread`.

#### Diagnostics and owner guard

Diagnostics replace `answer_node_id` with `reply_node_id` (`reply`) or
`end_message_node_id` (`turn-end`, `run-end`), and add `result: MODE`. Usage
facts stay saved owner facts, not a bill: `turn-end` keeps today's Turn-scoped
`turn_usage`; `run-end` reports the ended Run's `usage_totals`; `reply` reports
the assigned Run's `usage_totals` as observed when the reply was read (the Run
may still be live, so this is a snapshot). **Judgement:** the memo and issue
did not specify usage per mode.

The owner-version probe changes from a `final_answer_only` read to a
`wait reply_received` on a random ID, made before any work in
every mode: a current owner rejects it with `target_unavailable` (unknown
target, as today's probe expects), an older owner
rejects the unknown condition (exit 6 `owner_version_mismatch`). `reply_received`
and the end-message rename land before the exec change (see the plan), so one
probe covers all modes. A refusal at the end-message read still exits 6 with
the known IDs and the advice to use ordinary `history get --node`.

Behaviour change of the default: the model must call `shellboy message reply`.
A model that only writes a final text gets one reminder Turn and then exec
exits 4 `ignored`. `--result turn-end` keeps today's "final text" behaviour
for callers that want it.

## Authority

| Action | Model caller | Human / external CLI (no Origin) |
|---|---|---|
| `--expect-reply` send | ordinary send authority; not to its own Thread | ordinary send authority |
| `message reply` | only when its Thread is the original's target Thread (any Run of it). A parent cannot reply for its child; a fork cannot reply for its source | any message |
| cancel the pending reply | only messages its own Thread sent (existing rule) | any message |
| `wait reply_received` | any message, cycle-checked | any message |

## Delegation mapping

Every flow in `docs/guides/delegation.md` and the Agent Guide.

| Current flow | Today | New | Lost? |
|---|---|---|---|
| Create child | `thread create --parent` | unchanged | no |
| Assign work | `message send --thread CHILD --text …` | `message send --thread CHILD --expect-reply --text …` | no |
| Check assignment / pending | `message get` (`assignment`, `held` reason) | unchanged; also shows `reply.state` | no |
| Wait for one child | `wait turn_completed CHILD_TURN` | `wait reply_received CHILD_MESSAGE` (+ `thread_settled` for recovery/stop if wanted) | no; waits on the specific request rather than whichever Turn answered |
| Wait for any of several | several `turn_completed` (any) or `thread_settled` with cursor | several `reply_received` (any); `thread_settled` + cursor unchanged | no |
| Get the result | `turn get` → `answer_node_id` → `history get` | the reply arrives in the parent's own conversation as a message; or `message get` → `reply.node_id` → `history get` | no; the result is now the child's deliberate reply, not its last text |
| Child finished without a result | parent sees an answer node or none | reminder once, then `ignored` notice wakes the parent; `last_end_message` still readable | no; failure is now explicit |
| Progress reports | child `message send --thread PARENT` (ordinary) | unchanged; ordinary messages (D3) | no |
| Wait for all owned work to drain | `wait run_ended` | unchanged | no |
| Recovery decision | `run get` / `turn get` / `run retry` / `run stop` | unchanged; the pending reply survives retry and is answered after it | no |
| Stop a child | `run stop --run CHILD_RUN` | unchanged; pending reply stays pending (decision Q2); parent may `message cancel` it | no |
| After restart | interruption context + stored answers + unprocessed messages | same, with `awaiting_replies` and reply states instead of answers | no |
| Delete child | `thread delete-preview` / `thread delete` | unchanged; pending replies become `recipient_deleted` and the parent is notified | no |
| Overview | `thread get` `last_answer` / `last_input` | `last_end_message` / `last_input`, plus `awaiting_replies` | no |
| "Make sure you report before finishing" via `--deliver complete` | leader's workaround | unnecessary: the Run-end rule is that check | removed |

### Leader's operational flow

| Step | Today | New |
|---|---|---|
| Send instruction | `message send --thread T --text …` (external CLI) | `message send --thread T --expect-reply --text …` |
| Check completion | `wait thread_settled` / `status`, then `thread get` `last_answer` and history | `wait reply_received` (re-issue after timeout), then `message get` → reply node; `thread_settled` / `status` unchanged for stalls |
| Model ended without reporting | leader inspects the last answer and guesses | explicit `ignored`; `last_end_message` and history remain for diagnosis |
| Withdraw an instruction | `message cancel` while still queued | same, and also after admission (`reply_cancelled`, the model is told) |
| Progress | read history / `status` | unchanged (no external inbox) |

Nothing that can be done today becomes impossible; the only removed step is the
manual "did you report?" Revisit.

## Gap analysis

Each existing behaviour touching messages, answers, delegation, wait, exec or
"thread request", checked against this design. **Decided** = covered by a user
decision (D1–D11 or an answered question); **Design** = settled by this
adopted design (explanation given); **Judgement** =
decided here without an explicit user answer (see [Decisions](#decisions)).

| # | Existing behaviour | Status | Treatment |
|---|---|---|---|
| G1 | Operation-ID idempotency of `message send` | Design | `expect_reply` joins the request fingerprint; same ID + different flag = `operation_conflict` |
| G2 | Idempotency of `message reply` | Design | ordinary mutation receipt; replay returns saved result; expiry is safe because `reply.state` blocks a second reply |
| G3 | Idempotency of `wait` | Design | unchanged; `reply_received` is stored in `wait.pending` metadata like other conditions and survives restart as an `interrupted_wait` node |
| G4 | `held` with `stopping` | Design | reply state is independent of delivery; the message is replyable only once `applied`; sender may cancel meanwhile |
| G5 | `held` with `recovery_wait` | Decided | stays pending; not a release reason (D6, Q2); exec adds `run_ended` of the assigned Run, guides add `thread_settled` |
| G6 | `held` with `previous_instance_interrupted` | Design | stays pending across restart; admitted on explicit resume; the 128 resume bound is unchanged |
| G7 | `--deliver now` | Decided | combinable (D2); no reply-specific effect |
| G8 | `--deliver complete` and Run-end ordering | Design | waiting `complete` messages are delivered first; the reminder is checked only when none wait, so a `complete` message can still be answered before any reminder |
| G9 | Forks (`thread fork`) | Decided | receipts stay with the original target Thread; the fork shares the node but cannot reply and has no reminder (Q13) |
| G10 | Compaction replacing original or reply nodes | Design | node rows stay (receipt and Turn roots), so `message get` and `history get` still work; the model regains IDs from `awaiting_replies` and the reminder |
| G11 | Attachments in replies | Decided | text only, like `message send`; a human can send attachments as an ordinary message (Q8) |
| G12 | Who may reply | Decided | recipient model (D4) and humans; parent-for-child not allowed (Q5) |
| G13 | Who may cancel | Decided | sender may cancel (decided); authority reuses the existing cancel rule |
| G14 | Human reply in TUI | Decided | CLI works at once; TUI reply mode in this issue, last step (Q7) |
| G15 | Receipt GC and capacity pressure | Design | message receipts are not collected (fact 1); reply node rooted by receipt or Turn; operation-outcome expiry handled by G2; reply bodies count toward logical history like any message |
| G16 | Recipient Thread deleted | Decided | `recipient_deleted` in the deletion transaction, waits released, model sender notified (D5) |
| G17 | Sender Thread deleted | Decided | pending messages it sent become `cancelled` (`cancel_reason: sender_deleted`) in the deletion transaction and admitted recipients are notified (Q4) |
| G18 | Two replies racing | Design | single DB lane; first commit wins; the second gets `invalid_state` `reply_state=replied` |
| G19 | Reply vs cancel vs Run-end races | Design | same serialisation; whichever commits first decides the outcome |
| G20 | Reply to a cancelled / ignored / deleted message | Decided | rejected with `details.reply_state`; the model can send an ordinary message instead (Q3) |
| G21 | Child Run ownership | Design | reply delivery uses ordinary send rules; replying to a parent never creates ownership; an owning parent stays live while its child works, so the reply reaches the live parent Run |
| G22 | `run stop` cascade | Decided | descendants stop; pending replies on them stay pending; the parent's waits end with the stop as today (Q2) |
| G23 | Esc Turn interruption | Decided | reminder still applies once, as #187 delivery does; shown in the TUI (Q11) |
| G24 | App shutdown | Design | reminder skipped once the start gate closes; nothing becomes `ignored` at shutdown |
| G25 | TUI pending queue | Design | "reply expected" tag on queued reply-expected rows; admitted pending replies appear under `/replies` |
| G26 | Search and history indexing | Design | reply node indexed like any message node; scoped to the sender Thread (model senders) or the recipient Thread (others). `history list` provenance of a reply without a delivery receipt shows sender `unknown`; `message get` of the original gives `reply.by` |
| G27 | `exec` exit codes and timeouts | Decided | `--result` modes and exit table in [exec](#exec) (Q14, D9, D10, D11) |
| G28 | Multiple waiters on one message | Design | all released by one transition |
| G29 | Messages sent without `--expect-reply` | Design | unchanged except `message get` drops `answer_node_id` |
| G30 | Wait `interrupted` from an arriving receipt | Design | `satisfied` wins when the arriving receipt is the awaited reply |
| G31 | Wait cycles | Design | Thread → live Run edge, as in `thread_settled` |
| G32 | Model sending `--expect-reply` to its own Thread | Decided | rejected (Q16) |
| G33 | `child_turn_completed` wake plus reply message | Design | both still arrive; they usually land in one parent Turn. Not changed here |
| G34 | Thread title derived from first user body | Design | a reply node on a sender Thread is a user node like any message; no special case |
| G35 | `thread request` debug command | Decided | unchanged; it shows the new notice nodes in rebuilt requests (Q1) |
| G36 | `history get --final-answer-only` | Judgement | kept and renamed `--end-message-only`, because `exec --result turn-end\|run-end` needs it (see Q12) |
| G37 | Watch views (`/threads active`) using the answer for "completed / unseen" | Design | same fact, renamed end message; no behaviour change |
| G38 | Logical capacity at send | Design | reply text uses the existing `draft_bytes` limit and capacity admission |
| G39 | Sender label on replies | Design | `--sender-label` accepted, self-asserted as today |
| G40 | Cross-data-directory sends | Design | sender is `external_cli`; reply attached to the original only; the remote side waits with `reply_received` against this owner |

## Implementation plan

No aliases (pre-1.0, D8). Each step lands on `main` on its own, with
`sh scripts/check.sh`, its own tests on temporary stores and the local HTTP
mock, and the guide/design changes for what it ships (guides are embedded by
`src/docs.rs`, so guide edits travel with the code). No step needs a paid API.
Protocol version stays 2 (new commands and conditions are rejected by an older
owner, as with #187). The internal column `turn.answer_node_id` keeps its name.

### Step 1 — Rename "answer" to "end message" (no behaviour change)

Public names only: `answer_node_id` → `end_message_node_id` on Run, Turn, wait
turn snapshot, `status` run/turn/attempt items, `child_turn_completed` /
`child_run_ended` payloads, interruption context; `last_answer` →
`last_end_message`; `history get --final-answer-only` → `--end-message-only`
(`HistoryGetArgs.end_message_only`, cursor scope `history.get.end_message`);
exec diagnostics `answer_node_id` → `end_message_node_id` and its probe/refusal
text. `message get` `answer_node_id` stays until step 2.

- Protocol/Core: `src/protocol.rs` (RunDetails, TurnDetails, ThreadSummary,
  ThreadDetails, HistoryGetArgs), `src/control/db.rs`,
  `src/control/db/thread_summary.rs`, `src/control/state.rs`,
  `src/control/wait.rs`, `src/control/interruption.rs`, `src/status.rs`,
  `src/status/observation.rs`, `src/thread_observation.rs`,
  `src/thread_observation/latest.rs`, `src/history.rs`, `src/cli.rs`,
  `src/exec_client.rs`, `src/usage_totals.rs`.
- Frontend: `src/frontend/watch_page.rs`, `src/frontend/watch_read.rs`,
  `src/frontend/inspection_saved.rs`, `src/frontend/inspection_lifecycle.rs`,
  `src/frontend/inspection_lists.rs`, `src/frontend/inspection_read.rs`,
  `src/frontend/header.rs`, `src/frontend/browser.rs`,
  `src/frontend/management.rs`, `src/frontend/notices.rs`, `src/frontend.rs`,
  `src/presentation/events.rs`, `src/presentation/messages.rs`,
  `src/presentation/projection/envelopes.rs`, `src/ui.rs`, `src/ui/threads.rs`,
  `src/ui/presentation.rs`, `src/presentation/fields.rs` (the `回答node ID`
  field label follows the field to `終了メッセージnode ID`; the `回答` row label is
  unchanged, Q10).
- Tests: `tests/exec_client.rs`, `tests/thread_observation.rs`,
  `tests/work_status.rs`, `tests/ui_thread_deletion.rs`,
  `tests/message_recovery.rs`, `tests/ui.rs`, `tests/thread_browser.rs`,
  `tests/docs_proxy.rs`, `tests/docs_cli.rs`, `tests/cache_diagnostics.rs`,
  `src/frontend/inspection_tests.rs`, `src/control/driver/preparation_tests.rs`,
  `src/control/driver/revisit_tests.rs`, `src/frontend/client_operations.rs`
  tests.
- Scripts: `scripts/check-observability-cli.py`, `scripts/bench/footprint.py`.
- Guides: `cli-reference.md`, `delegation.md`, `agent-guide.md`,
  `recovery-and-storage.md`.
- Designs: `thread-observation.md`, `protocol.md`, `cli.md` (#65 sections and
  owner guard), `conversation-redesign.md`, `effective-spec.md` (#65 and #74
  paragraphs).

### Step 2 — Schema 26 and the reply core

`message send --expect-reply` (flag in `MessageSendArgs`, `MessageAccepted`,
the operation fingerprint), `message reply` (`Command::MessageReply`,
`"message.reply"`), the reply node and its two deliveries, `message get`
`expect_reply` / `reply` / `reply_to` (removes `answer_node_id`), `status`
message items `answer_node_id` → `reply_node_id`,
`wait reply_received` (condition, snapshot, cycle edge, precedence over
`interrupted`), `message cancel` outcome `reply_cancelled` and its
`context_changed` notice, `thread get` `awaiting_replies`, the admission notices
(`reply_expected`, `reply`), the 128 cap, and model-self-target rejection.
Pending replies have no Run-end effect yet; nothing uses them until step 4/5.

- Schema: `migrations/0026_reply_expected.sql`, `src/storage/current.sql`,
  `src/storage/migrate.rs`, `src/storage.rs`, `scripts/check-release-upgrade.py`,
  `scripts/tests/test_release_upgrade.py`, `scripts/error_catalog.py`.
- Core: new `src/control/reply.rs` (state transitions, reply operation,
  notices); `src/protocol.rs` (MessageSendArgs, MessageAccepted,
  MessageReplyArgs, MessageDetails, MessageCancelOutcome, WaitCondition,
  ThreadDetails), `src/cli.rs`, `src/control/db.rs` (dispatch,
  `message_details`, thread details), `src/status.rs`,
  `src/status/observation.rs` (message items), `src/control/message_delivery.rs`
  (`mark_admitted_tx` notice helper), `src/control/pending_messages.rs`,
  `src/control/wait.rs`, `src/operations.rs` (operation targets for the new
  mutation).
- Tests: new `tests/reply.rs`; `src/control/pending_messages/tests.rs`,
  `src/control/wait/thread_tests.rs` additions.
- Guides: `cli-reference.md` (send flag, `message reply`, `message get`,
  `wait`, cancel outcome, `thread get`).
- Designs: `protocol.md`, `cli.md`, `wait.md`, `pending-messages.md`,
  `storage.md`, `effective-spec.md` (schema 26 in "Current conversation
  storage").

### Step 3 — Run-end rule and terminal outcomes

The reminder/`ignored` rule in the completion branch after
`revisit::release_tx`; event kinds `reply_reminder` and `reply_outcome`
(rendered as wake/notice rows, the reminder visibly, Q11); `recipient_deleted`
and sender-deletion `cancelled` in the Thread deletion transaction (Q4);
`reply_outcome` as a `context_changed` notice when the sender has no live Run
(Q6); retention of the new events by the existing #170 rules.

- Core: `src/control/reply.rs`, `src/control/driver.rs`,
  `src/control/revisit.rs`, `src/control/state.rs`, `src/deletion.rs`,
  `src/gc.rs` (only if the retention query enumerates kinds).
- Frontend: `src/presentation/events.rs`, `src/frontend/notices.rs`,
  `src/ui/presentation.rs`.
- Tests: `tests/reply.rs` (reminder then `ignored`, Esc then reminder then
  second Esc, ordering with waiting `complete` messages, stop/recovery keep
  pending, deletion both ways, multiple waiters, restart with a pending wait),
  `src/control/driver/revisit_tests.rs`.
- Guides: `agent-guide.md` (how to see and answer reply-expected messages).
- Designs: `control.md` (Run continue/complete row), `revisit.md` (ordering
  note), `gc.md` if kinds are listed.

### Step 4 — `exec --result`

`--result reply|turn-end|run-end` (default `reply`) as in [exec](#exec): the
per-mode send/wait/read, the empty-end-message rule in the
`--end-message-only` projection (`src/history.rs` returns empty text instead
of `target_unavailable`) and in exec (empty text exits 0; no end-message node
exits 4, D11), the exit
table, per-mode diagnostics and the `reply_received` owner probe.

- Code: `src/exec_client.rs`, `src/history.rs`.
- Tests: `tests/exec_client.rs` (each mode's success; reply `ignored`,
  `cancelled`, recovery and timeout exits; turn-end vs run-end with a
  `--deliver complete` message and with a reminder Turn; empty end message →
  exit 0 with empty stdout and no repair message sent, with `--schema`; Esc-ended Turn (turn-end) and Esc-ended last Turn
  (run-end) → exit 4; probe against a refusing owner).
- Guides: `cli-reference.md` (exec section and owner guard).
- Designs: `cli.md` (#65 exec section, owner guard), `effective-spec.md`
  (#65 paragraph).

### Step 5 — Delegation and leader flow on replies

No code. Rewrite the delegation recipe as in
[Delegation mapping](#delegation-mapping): `--expect-reply` assignment,
`wait reply_received` (+ `thread_settled` for stalls), the reply as the result,
`ignored` as explicit failure; remove the "did you report?" `--deliver complete`
workaround.

- Guides: `delegation.md`, `agent-guide.md`, `recovery-and-storage.md`,
  `user-guide.md` and `user-guide.ja.md` only if they describe delegation
  results.
- Tests: `tests/docs_cli.rs`, `tests/docs_proxy.rs` if they pin guide text.

### Step 6 — TUI (option B, reply mode)

Reply row label and excerpt, state tags on the original row and in `/queue`
(Q17, option B), `/replies` popup and composer reply mode (Q7).

- Code: `src/frontend/message_metadata.rs` (`reply_to`, reply state),
  `src/presentation/messages.rs`, `src/frontend/message_queue.rs`,
  `src/ui/message_queue.rs`, `src/ui/composer.rs`, `src/ui/help.rs`,
  `src/ui.rs` (slash command and popup), `src/frontend/client_operations.rs`
  (`message.reply` submit), the typed UI message catalogue for both languages.
- Scripts/tests: `scripts/check-row-kinds.py` if row kinds are enumerated,
  `tests/ui.rs`, VT-simulator tests for the popup and the mode line.
- Guides: `tui.md`, `user-guide.md` and `user-guide.ja.md` (reply mode).

## Size

Rough, to be replaced by the real diff: step 1 mechanical; step 2 about +600
lines with one index; step 3 about +200; step 4 net small (the Turn-polling
loop becomes one wait per mode); step 5 docs only; step 6 about +350.

## Simplicity estimate

Removed (2): the "answer of a message" (message → Turn answer) and the
leader's "did you report?" Revisit workaround. exec's Turn-polling loop is
replaced by one wait per mode. The final-answer projection stays (renamed),
because `--result turn-end|run-end` uses it. The "answer" vocabulary becomes
"end message" (rename, no new concept).

Added (7): the expect-reply flag, the per-message reply state (five values), the
`message reply` operation, the `reply_received` wait condition, the Run-end
reminder rule, two event kinds (`reply_reminder`, `reply_outcome`) with three
notice payload kinds on existing paths, and the client-only `exec --result`
option.

Net, the design adds concepts, but every request/result flow (delegation, exec,
leader) goes through one path instead of three, and "did the recipient answer
this request" becomes a saved fact instead of an inference from Turn text.

## Verification plan

Integration tests on temporary stores with the local HTTP mock: send with each
`--deliver` timing; reply by the recipient model through the shell CLI; reply by
a human (no Origin); delivery to a model sender (new Run, existing Turn, held);
rejection paths (double reply, not applied, foreign Thread, fork, self target,
cap 128/129); Run-end reminder then `ignored` with sender notice and wait
release; ordering with waiting `complete` messages; cancel before and after
admission; recipient deletion; restart with a pending wait; multiple waiters;
cycle rejection; exec in each `--result` mode (success, `ignored`, timeout,
recovery, empty end message (exit 0) and Esc-ended Turn (exit 4), turn-end vs run-end with a
`--deliver complete` message and a reminder Turn); EXPLAIN QUERY PLAN for the
pending-reply index. No timing assertions; no paid API.

## Decisions

The memo's 17 questions, as answered by the user on #194 (2026-11-03). Q2, Q11
and Q14 were answered individually; the user accepted the memo's
recommendation for every other question, and chose option B for Q17. Where
the memo gave no single recommendation, the choice made here is marked
**Judgement** and logged in the [decision log](../development/decisions.md).

| # | Question | Decision |
|---|---|---|
| Q1 | What "thread request" means | The send-and-wait delegation recipe. The `thread request` debug command stays unchanged |
| Q2 | Stopped or stuck recipient | User: messages stay awaiting reply when the recipient's work stops (Ctrl+C, `run stop`, app exit) or waits for recovery. The recipient may reply after resuming; the sender may cancel |
| Q3 | Late reply after `ignored` / `cancelled` | Rejected with `details.reply_state`; the model can send an ordinary message instead |
| Q4 | Sender's Thread deleted | Its pending messages become `cancelled` (`cancel_reason: sender_deleted`) and recipients that already received them are notified |
| Q5 | Who may reply | Only the receiving Thread's model, or a human. A parent cannot reply for its child |
| Q6 | `ignored` when the asking model is idle | Notice only; it is seen at the asker's next Turn and starts no work |
| Q7 | TUI reply mode | Included in this issue (`/replies`, composer reply mode). **Judgement:** the memo asked "now or later" without a recommendation; the mode is in scope but lands last (step 6), after the CLI path that humans can already use |
| Q8 | Attachments | Replies are text only |
| Q9 | Reminder count | Once per message for its whole life, not per Run |
| Q10 | Names | Public names `end_message_node_id`, `last_end_message`. **Judgement:** the TUI assistant row label `回答` is unchanged (the memo gave no recommendation; UI wording is not changed without approval). The detail field label `回答node ID` follows its field to `終了メッセージnode ID` |
| Q11 | Esc | User: after Esc ends a Turn, an unanswered message is still reminded once at Run end. The reminder is once per message, so a second Esc lets the Run end; the TUI shows that a reminder is running |
| Q12 | Remove `history get --final-answer-only` | **Judgement:** not removed. The recommendation assumed exec would stop using it, but the user's Q14 answer (`--result turn-end\|run-end`) needs the end-message projection. It is renamed `--end-message-only` with the rest of the "answer" vocabulary |
| Q13 | Forks | A fork does not inherit the duty to reply; only the original Thread can reply |
| Q14 | exec strictness | User: replaced by `exec --result reply\|turn-end\|run-end`, default `reply`. `reply` reminds once and then exits 4 `ignored`; `turn-end` and `run-end` send an ordinary message with no reply duty and never produce the reply outcomes. See [exec](#exec) |
| Q15 | Cap | 128 pending reply-expected messages per receiving Thread; the 129th send is refused with `input_limit` |
| Q16 | Asking yourself | A model cannot send a reply-expected message to its own Thread |
| Q17 | TUI glyph | Option B: no new mark; a label after the existing glyph. See [TUI rendering](#tui-rendering) |
| D10 | Empty end message (user, after Q14; refined) | A successful empty result: exit 0 in every mode, also with `--schema`; Schema validation and repairs apply only to non-empty text. Exit 0 means valid JSON or an empty end message |
| D11 | Turn ended by Esc, no end message (user, correcting an earlier judgement) | Not the same as empty: no result, treated like a stopped Turn, exit 4 |

Other judgements made while finalizing (not covered by a user answer):

- **exec exit 4 in `turn-end` / `run-end`.** These modes keep exit 4 for the
  execution failures it already covers (held, recovery wait, stopped, failed,
  unavailable); only the reply outcomes cannot occur. Recheck if the user's
  "never exit 4" was meant literally.
- **exec `reply` mode waits on `run_ended` of the assigned Run**, not
  `thread_settled`, so an earlier failed Run of an existing Thread does not end
  the wait.
- **Usage diagnostics per mode** (Turn usage, Run usage, observed Run usage).
- **Error codes** follow the existing set: unknown targets are
  `target_unavailable`, authority failures `invalid_state` with
  `details.reason`; no `forbidden`/`not_found` error code is added.

### Recovery queue units after integration with main (2026-11-03)

The 128 pending reply-duty cap is not a general held-message admission bound.
Ordinary sends can accumulate more than 128 held receipts; UI/readback windows
also do not enforce admission. Per the user's decision, recovery must not refuse
an oversized queue. Explicit send/retry releases the oldest 128 per pass and
retains the remainder. The Turn stays in recovery_wait while work remains, so a
fresh send/retry can progress after interruption/restart. New input is held
behind the older queue. Each unit commits atomically, and a stale generation
cannot release a second unit. Existing status retains the remaining held queue;
only after the final unit does the Turn become preparing. Global startup repair
is not part of this foreground unit.

`thread get`'s existing `recovery_reason` and `run get`/`run retry`'s existing
`recovery_hint` include `remaining_held` while the Turn is recovery_wait. This is
an exact status-readback count over the recipient's held index range, not a
count or scan inside reply admission. No separate queue or worker is added.

The ordered unit merges current-Turn recovery_wait input with terminal-Run
interrupted input before LIMIT, rather than allocating separate source quotas.
Older Turns of the same still-live Run remain isolated: retrying the current
Turn is not permission to revive stale/imported assignments from another Turn.


## Human decision requests (#262, 2026-11-07)

A model may ask the user for a decision with `shellboy message ask --text ...`.
The question reuses a reply-expected receipt; no table, schema migration,
worker, queue, timeout or cross-Thread question browser is introduced. The
pending receipt records `to_human=true`; reply state remains `pending` until a
human reply, sender cancellation or recipient Thread deletion. Questions do not
create a Run, deliver to a model, trigger Run-end reminders, or become ignored.
They count toward the same 128 pending-reply cap.

`thread get` exposes unified pending duties for that Thread; `thread list` exposes
only their count. The TUI renders the same per-Thread list in its `Pending replies`
region; existing person/model marks distinguish recipient kind. Its fixed body
height is configured by `ui.awaiting_human_rows` (1..=10, default 1). It reuses
the established `◉` region heading and `›` human mark; no new glyph is added. Overflow is summarized
and remains accessible through existing `/replies` actions. Answering uses the
existing `message reply` flow, including the existing reply mark and reply notice.

On answer, the receipt resolves and the answer is admitted to that Thread as an
ordinary human message, starting a Run if none is live. Cancellation is terminal
without a wake; deletion resolves as `recipient_deleted` through existing receipt
collection. Asking an archived Thread unarchives it with reason
`question_asked`, consistent with the reply-resume unarchive behavior.

A model that has an outstanding reply duty while asking the human must tell its
requester it is blocked before ending its Run; the human question itself is not
a reason to pause or abandon that reply duty.

## Thread-scoped reply inbox (#262, unified for web/TUI)

There is no cross-Thread inbox endpoint. The existing `thread.get` read is the
public, bounded per-Thread inbox: `ThreadDetails.awaiting_replies` is the unified
page of pending duties for that one Thread, ordered by saved creation time and
bounded to 128. Each item has `message_id`, `recipient_kind` (`human` for
`message ask`, otherwise `thread`), `recipient_thread_id` (the target Thread),
`excerpt` (bounded first-line question), `created_at`, `elapsed_ms` (derived
from saved creation time and observation time), `delivery_status`, and
`reminded`. `thread.get` is already refreshed by `events.wait --thread THREAD_ID`;
clients reload only that Thread after a change hint. Answering uses existing
`message.reply` with `message_id` and text. No new queue or read operation is
needed. The caller must observe `recipient_kind`, not infer it from sender or
Thread identity. A thread-targeted duty is not a human notification.

The TUI `Pending replies` region is the same unified per-Thread list. It uses the
existing human mark for `recipient_kind=human`; Thread-targeted duties retain the
existing model mark. `thread list` exposes the count of all pending duties only.
Overflow remains reachable through `/replies`. Human questions alone are
suppressed from Run-end reminders and `ignored`; other-Thread duties keep the
existing rules.
