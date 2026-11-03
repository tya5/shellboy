# Reply-expected messages (#194)

Status: **Proposed — awaiting user approval.** Nothing here is implemented.
This memo turns the user's decisions on #194 (comments marked "決定",
2026-11-03) into a concrete design, lists every existing behaviour it touches,
and ends with the questions that still need the user's answer. Implementation
starts only after approval.

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
projections keep the same fact under the name "end message".

## Decisions this design implements

| # | User decision (2026-11-03) | Where |
|---|---|---|
| D1 | Wake once at Run end; on the second detection record "ignored" as a stored notice, release waits, notify the sender | [Run-end rule](#run-end-rule) |
| D2 | `message send --expect-reply`, combinable with `--deliver now\|complete` | [CLI](#cli) |
| D3 | One reply per message; replying clears the pending state; progress uses ordinary messages | [Reply operation](#message-reply) |
| D4 | The recipient Thread's model replies; a human may also reply from the TUI if that is simpler | [Who may reply](#authority), Q7 |
| D5 | Recipient Thread deleted: release waits and notify the sender | [Outcomes](#outcomes) |
| D6 | External senders wait with `wait reply_received`; four release reasons; `--timeout-ms` bounds only that call; pending replies have no deadline; `exec` and "thread request" become expect-reply + `reply_received` | [Wait](#wait-reply_received), [exec](#exec) |
| D7 | Delegation is unified onto this mechanism | [Delegation mapping](#delegation-mapping) |
| D8 | Replace every `answer_node_id` use without compatibility paths | [Consumers](#consumers-to-update) |
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
   leaves the debug command unchanged (Q1).
3. **`message get`'s `answer_node_id` is the answer of the Turn the message was
   assigned to**, not a reply to that message (`message_details` joins
   `turn.answer_node_id`). Several messages assigned to one Turn all report the
   same node; a message admitted mid-Turn reports whatever the Turn said last.
   This is the defect the reply link replaces.
4. **The model never sees message IDs or senders today.** A received message is
   replayed as plain `user` text. A model cannot reply to a message it cannot
   name, so admission must add a short notice carrying the ID (see
   [Model-visible notices](#model-visible-notices)).
5. **Existing bound 128.** The pending-message projection (`/queue`, 128 rows),
   `message cancel` (1..128 IDs) and resume-time interrupted-message recovery
   (`message_delivery::recover_on_resume_tx`, rejects >128) all use 128. There
   is no total cap on queued messages today.
6. **The #187 completion hook** is `driver::next`, in the branch where no Turn,
   pending input, event or owned work remains: `revisit::release_tx` runs and,
   if it found nothing, `state::complete_idle_run`. The shutdown gate already
   skips starting new work there.

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
| Cancellation actor | same object | `"cancelled_by"` (existing actor shape) when `cancelled` |
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

### Outcomes

| Outcome | Trigger | Saved where |
|---|---|---|
| `replied` | `message reply` commits | original `reply` object; reply node; delivery receipt for model senders |
| `ignored` | second Run-end detection (D1) | original `reply` object; notice to a model sender |
| `cancelled` | sender (or a human) runs `message cancel` while pending; also proposed for sender-Thread deletion (Q4) | original `reply` object; notice to the recipient if it had already received the message |
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
is the one that must work through the list (Q15).

### Migration 25

Main is at schema 24 (`0024_revisit_messages.sql`). The Run-end check, the cap
check and the `thread get` list all ask "pending reply-expected messages of
Thread T". Without an index that scans every receipt the Thread ever received,
at every Run completion, which the hot-query rules forbid. Migration 25 adds
only:

```sql
CREATE INDEX receipt_reply_pending ON receipt(target_thread_id)
  WHERE json_extract(result_json,'$.reply.state')='pending';
INSERT INTO schema_migration(version,name) VALUES(25,'0025_reply_expected');
PRAGMA user_version=25;
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

1. Load the original receipt. Not found → `not_found`. No `expect_reply` →
   `invalid_argument`.
2. Authority (see [Authority](#authority)); failure → `forbidden`.
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

No new list operation. `thread get` adds `awaiting_reply`: up to 128 items
`{message_id, sender, created_at, delivery_status, reminded}` for the Thread,
oldest first (the cap guarantees completeness). Bodies are read with
`message get` / `history get --node`. Besides that, every reply-expected message
is announced with its ID when admitted, and the Run-end reminder lists the
remaining ones, so a model that lost the ID to compaction can recover it.

### Human reply

Humans have no Origin, so they may reply to any message through the CLI
immediately (`shellboy message reply --message ID --text …` from another
terminal). The TUI option is a small reply mode (Q7):

- `/replies` opens a shared-framework popup listing the current Thread's
  `awaiting_reply` items (sender, time, excerpt).
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

- Registration errors: unknown ID → `not_found`; message without
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

"Once" is per message, not per Run. A message reminded in a Run that is later
stopped is not reminded again in the next Run (Q9). A human Esc does not
suppress the reminder, matching #187's rule that Esc does not suppress
completion delivery (Q11). Stop, recovery wait and shutdown are not
completions: pending replies stay pending (Q2).

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
| `ignored` / `recipient_deleted` to a model sender | new kind `reply_outcome`, delivered to the sender Thread's live Run | yes, like `child_run_ended`. With no live Run it is a `context_changed` notice seen at the next Turn and does not start a Run (Q6) |

New event kinds `reply_reminder` and `reply_outcome` are bounded per message
(at most one reminder entry and one outcome each), owned by the existing event
and delivery rows, and retained/collected by the existing #170 event retention.

### TUI rendering

The reply appears on the sender's Thread as an ordinary user-style row whose
sender glyph is decided by the existing source rules (`↳` parent/model etc.).
On the recipient's Thread the original message row gets a short state tag.
Options for the user to choose (marks are East Asian Width N; no existing
decided mark is changed):

| Option | Reply row heading (sender Thread) | Original row tag (recipient Thread) |
|---|---|---|
| A — new mark | `↩ 返事` / `↩ Reply` plus the original's excerpt cut to the row width: `↩ 返事 ← 「Task: read-only audit…」` | `返事待ち` / `返事済み` / `返事なし` / `取消` / `相手削除` |
| B — no new mark | keep the sender's existing glyph and append a label: `↳ 子 · 返事: 「Task: read-only…」` | same tags, after the existing sender label |
| C — compact ID | `⤶ 返事 #a1b2c3` (short message ID, full ID in details F6) | `#a1b2c3 返事待ち` and so on |

The pending queue (`/queue`, rows above the composer) adds the same
"reply expected" tag to queued reply-expected messages. The assistant row label
`回答` is separate (Q10).

## End message (renamed answer)

The Turn's final assistant node remains a saved fact, renamed in public
contracts from "answer" to "end message" so it is no longer read as a reply:

| Today | Proposed |
|---|---|
| `turn get` / `run get` `answer_node_id` | `end_message_node_id` |
| `wait` turn snapshot `answer_node_id` | `end_message_node_id` |
| `thread list/get` `last_answer` | `last_end_message` (same fields: `node_id`, `turn_id`, `at`, `excerpt`, `body_available`) |
| `status` item `answer_node_id` | run/turn/attempt items: `end_message_node_id`; message items: `reply_node_id` (from the receipt, no longer the Turn's answer) |
| `child_turn_completed` / `child_run_ended` payload `answer_node_id` | `end_message_node_id` |
| interruption context `answer_node_id` | `end_message_node_id` |
| `message get` `answer_node_id` | removed; see `reply` |

Guides describe it as "the last assistant message of the Turn; not a reply and
not proof of success".

### exec

`exec` becomes: create or select the Thread → `message send --expect-reply`
(the composed request asks for "only the JSON value as the reply") → one `wait`
with `reply_received` **and** `thread_settled` (`recovery_wait`, `failed`,
`stopped`) on the target → `message get` → `history get --node REPLY_NODE_ID` →
local validation. A repair is another `--expect-reply` message in the same
Thread.

| Exit | Today | Proposed |
|---|---|---|
| 0 | valid JSON in the Turn's final answer | valid JSON in the reply |
| 2, 3, 6 | unchanged | unchanged |
| 4 | held, recovery_wait, stopped, failed, no answer | held at send, recovery_wait, stopped, failed (via `thread_settled`), `ignored`, `cancelled`, `recipient_deleted` |
| 5 | client deadline | unchanged; the message stays pending (the Run-end rule eventually marks it `ignored`) |

Diagnostics replace `answer_node_id` with `reply_node_id`. The owner-version
probe changes from a `final_answer_only` read to a zero-timeout
`wait reply_received` on a random ID: a current owner returns `not_found`, an
older owner rejects the unknown condition, before any work. `history get
--final-answer-only` then has no user and is removed with its separate cursor
scope (Q12). Behaviour change: the model must call `shellboy message reply`; a
model that only writes a final text gets one reminder Turn and then exit 4
`ignored` (Q14).

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
| Stop a child | `run stop --run CHILD_RUN` | unchanged; pending reply stays pending (Q2); parent may `message cancel` it | no |
| After restart | interruption context + stored answers + unprocessed messages | same, with `awaiting_reply` and reply states instead of answers | no |
| Delete child | `thread delete-preview` / `thread delete` | unchanged; pending replies become `recipient_deleted` and the parent is notified | no |
| Overview | `thread get` `last_answer` / `last_input` | `last_end_message` / `last_input`, plus `awaiting_reply` | no |
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
decision; **Proposed** = covered by this memo (explanation given);
**Open** = needs the user (numbered questions below).

| # | Existing behaviour | Status | Treatment |
|---|---|---|---|
| G1 | Operation-ID idempotency of `message send` | Proposed | `expect_reply` joins the request fingerprint; same ID + different flag = `operation_conflict` |
| G2 | Idempotency of `message reply` | Proposed | ordinary mutation receipt; replay returns saved result; expiry is safe because `reply.state` blocks a second reply |
| G3 | Idempotency of `wait` | Proposed | unchanged; `reply_received` is stored in `wait.pending` metadata like other conditions and survives restart as an `interrupted_wait` node |
| G4 | `held` with `stopping` | Proposed | reply state is independent of delivery; the message is replyable only once `applied`; sender may cancel meanwhile |
| G5 | `held` with `recovery_wait` | Proposed / Open | stays pending; not a release reason (D6); exec and guides add `thread_settled`. Q2 |
| G6 | `held` with `previous_instance_interrupted` | Proposed | stays pending across restart; admitted on explicit resume; the 128 resume bound is unchanged |
| G7 | `--deliver now` | Decided | combinable (D2); no reply-specific effect |
| G8 | `--deliver complete` and Run-end ordering | Proposed | waiting `complete` messages are delivered first; the reminder is checked only when none wait, so a `complete` message can still be answered before any reminder |
| G9 | Forks (`thread fork`) | Proposed / Open | receipts stay with the original target Thread; the fork shares the node but cannot reply and has no reminder. Q13 |
| G10 | Compaction replacing original or reply nodes | Proposed | node rows stay (receipt and Turn roots), so `message get` and `history get` still work; the model regains IDs from `awaiting_reply` and the reminder |
| G11 | Attachments in replies | Proposed / Open | text only, like `message send`; a human can send attachments as an ordinary message. Q8 |
| G12 | Who may reply | Decided / Open | recipient model (D4) and humans; parent-for-child not allowed. Q5 |
| G13 | Who may cancel | Decided / Proposed | sender may cancel (decided); authority reuses the existing cancel rule |
| G14 | Human reply in TUI | Decided / Open | CLI works at once; TUI reply mode proposed. Q7 |
| G15 | Receipt GC and capacity pressure | Proposed | message receipts are not collected (fact 1); reply node rooted by receipt or Turn; operation-outcome expiry handled by G2; reply bodies count toward logical history like any message |
| G16 | Recipient Thread deleted | Decided | `recipient_deleted` in the deletion transaction, waits released, model sender notified (D5) |
| G17 | Sender Thread deleted | Open | proposal: pending messages it sent become `cancelled` (reason `sender_deleted`) and admitted recipients are notified. Q4 |
| G18 | Two replies racing | Proposed | single DB lane; first commit wins; the second gets `invalid_state` `reply_state=replied` |
| G19 | Reply vs cancel vs Run-end races | Proposed | same serialisation; whichever commits first decides the outcome |
| G20 | Reply to a cancelled / ignored / deleted message | Proposed / Open | rejected with `details.reply_state`; the model can send an ordinary message instead. Q3 |
| G21 | Child Run ownership | Proposed | reply delivery uses ordinary send rules; replying to a parent never creates ownership; an owning parent stays live while its child works, so the reply reaches the live parent Run |
| G22 | `run stop` cascade | Proposed / Open | descendants stop; pending replies on them stay pending; the parent's waits end with the stop as today. Q2 |
| G23 | Esc Turn interruption | Proposed / Open | reminder still applies, as #187 delivery does. Q11 |
| G24 | App shutdown | Proposed | reminder skipped once the start gate closes; nothing becomes `ignored` at shutdown |
| G25 | TUI pending queue | Proposed | "reply expected" tag on queued reply-expected rows; admitted pending replies appear under `/replies` |
| G26 | Search and history indexing | Proposed | reply node indexed like any message node; scoped to the sender Thread (model senders) or the recipient Thread (others). `history list` provenance of a reply without a delivery receipt shows sender `unknown`; `message get` of the original gives `reply.by` |
| G27 | `exec` exit codes and timeouts | Proposed / Open | see [exec](#exec). Q14 |
| G28 | Multiple waiters on one message | Proposed | all released by one transition |
| G29 | Messages sent without `--expect-reply` | Proposed | unchanged except `message get` drops `answer_node_id` |
| G30 | Wait `interrupted` from an arriving receipt | Proposed | `satisfied` wins when the arriving receipt is the awaited reply |
| G31 | Wait cycles | Proposed | Thread → live Run edge, as in `thread_settled` |
| G32 | Model sending `--expect-reply` to its own Thread | Proposed | rejected (Q16) |
| G33 | `child_turn_completed` wake plus reply message | Proposed | both still arrive; they usually land in one parent Turn. Not changed here |
| G34 | Thread title derived from first user body | Proposed | a reply node on a sender Thread is a user node like any message; no special case |
| G35 | `thread request` debug command | Open | unchanged; it shows the new notice nodes in rebuilt requests. Q1 |
| G36 | `history get --final-answer-only` | Open | no user after exec changes; remove. Q12 |
| G37 | Watch views (`/threads active`) using the answer for "completed / unseen" | Proposed | same fact, renamed end message; no behaviour change |
| G38 | Logical capacity at send | Proposed | reply text uses the existing `draft_bytes` limit and capacity admission |
| G39 | Sender label on replies | Proposed | `--sender-label` accepted, self-asserted as today |
| G40 | Cross-data-directory sends | Proposed | sender is `external_cli`; reply attached to the original only; the remote side waits with `reply_received` against this owner |

## Consumers to update

No aliases (pre-1.0). Every `answer_node_id` / `last_answer` /
`final_answer_only` consumer:

- Protocol and Core: `src/protocol.rs` (MessageDetails, RunDetails, TurnDetails,
  ThreadSummary, ThreadDetails, HistoryGetArgs), `src/control/db.rs`
  (`message_details`, `run_details`, `turn_details`, thread details/list),
  `src/control/state.rs` (`child_turn_completed`, `child_run_ended` payloads),
  `src/control/wait.rs` (turn snapshot), `src/control/interruption.rs`,
  `src/control/db/thread_summary.rs`, `src/status.rs`,
  `src/status/observation.rs`, `src/thread_observation/latest.rs`,
  `src/thread_observation.rs`, `src/history.rs` (final-answer-only), `src/cli.rs`
  (help text, flag), `src/exec_client.rs`.
- Frontend: `src/frontend/watch_page.rs`, `src/frontend/watch_read.rs`,
  `src/frontend/inspection_saved.rs`, `src/frontend/inspection_lifecycle.rs`,
  `src/frontend/inspection_lists.rs`, `src/frontend/inspection_read.rs`,
  `src/frontend/header.rs`, `src/frontend/browser.rs`,
  `src/frontend/management.rs`, `src/frontend.rs`, `src/ui.rs`,
  `src/ui/threads.rs`, `src/presentation/fields.rs` (`回答node ID` label).
- Tests: `tests/exec_client.rs`, `tests/thread_observation.rs`,
  `tests/work_status.rs`, `tests/ui_thread_deletion.rs`,
  `tests/message_recovery.rs`, `tests/ui.rs`, `tests/thread_browser.rs`,
  `tests/docs_proxy.rs`, `tests/docs_cli.rs`, `tests/cache_diagnostics.rs`,
  `src/frontend/inspection_tests.rs`, `src/control/driver/preparation_tests.rs`,
  `src/frontend/client_operations.rs` tests.
- Scripts: `scripts/check-observability-cli.py`, `scripts/bench/footprint.py`.
- Guides: `delegation.md`, `agent-guide.md`, `cli-reference.md`,
  `recovery-and-storage.md`; `tui.md` for `/replies` and tags.
- Designs: `thread-observation.md`, `protocol.md`, `cli.md` (exec,
  final-answer-only), `pending-messages.md` (cancel outcome), `revisit.md`
  (ordering note), `wait.md`, `storage.md`, `conversation-redesign.md`,
  `effective-spec.md` (schema 25, #194 entry).

## Size and files

Rough, to be replaced by the real diff:

| Area | Files | Change |
|---|---|---|
| Schema | `migrations/0025_reply_expected.sql`, `src/storage/current.sql`, migrate ledger, two scripts | one index |
| Core | new `src/control/reply.rs` (state transitions, notices, Run-end rule, cancel and delete hooks, reply op); `db.rs`, `driver.rs`, `message_delivery.rs`, `pending_messages.rs`, `wait.rs`, `protocol.rs`, `cli.rs` | about +600–800 lines |
| exec | `src/exec_client.rs` | net smaller: the Turn-polling loop and applied check go; one wait and one message get come in |
| Removal | `final_answer_only` in `history.rs`, protocol, cli | about −150 lines |
| Renames | the consumer list above | mechanical |
| Frontend | `message_metadata.rs` (reply_to), presentation rows/tags, `message_queue.rs` tag; optional `/replies` popup + reply mode | about +150, plus about +200 for the TUI reply mode |
| Tests | new `tests/reply.rs` with temporary stores and a local HTTP mock | — |

### Simplicity estimate

Removed (4): the "answer of a message" (message → Turn answer), the
final-answer-only read projection and its cursor scope, exec's own Turn-polling
completion logic, and the leader's "did you report?" Revisit workaround. The
"answer" vocabulary becomes "end message" (rename, no new concept).

Added (6): the expect-reply flag, the per-message reply state (five values), the
`message reply` operation, the `reply_received` wait condition, the Run-end
reminder rule, and two event kinds (`reply_reminder`, `reply_outcome`), with
three notice payload kinds on existing paths.

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
cycle rejection; exec success, `ignored`, timeout and recovery exits; EXPLAIN
QUERY PLAN for the pending-reply index. No timing assertions; no paid API.

## Open questions

Phrased for a reader who does not know the internals.

1. **"thread request".** The existing `shellboy thread request` command only
   shows what a Thread would send to the model; it does not send or wait. I
   read "thread request" in the issue as "send a request to a Thread and wait
   for its result" (the delegation recipe). Is that right, and should the debug
   command stay as it is?
2. **Stopped or stuck recipient.** If the recipient's work is stopped (Ctrl+C,
   `run stop`, app exit) or is waiting for a recovery decision, should its
   unanswered messages stay "awaiting reply" (proposal: yes; it may still reply
   after resuming, and the sender can cancel), or become "ignored" right away?
3. **Late reply.** After a message is marked ignored or cancelled, the model
   tries to reply. Reject it (proposal; the model can send a normal message
   instead) or accept it as a normal message?
4. **Sender's Thread deleted.** If the asking Thread is deleted while its
   question is still unanswered, should the question be cancelled and the
   recipient told to stop (proposal), or stay open?
5. **Who may reply.** Proposal: only the receiving Thread's model, or a human.
   A parent model cannot reply on behalf of its child. Agree?
6. **"Ignored" when the asking model is idle.** If the asking model has no
   running work, should an "ignored" notice start new work for it (a reply
   does, like any message), or just wait until it next runs (proposal)?
7. **Replying from the TUI.** Humans can reply with the CLI immediately. Do
   you want the small TUI reply mode (`/replies` list, then type the reply) in
   this issue, or later?
8. **Attachments.** Replies are text only (like `message send`). Acceptable?
9. **Reminder count.** The model is reminded once per message for its whole
   life, not once per run. If a person stops the run right after the reminder,
   the next normal finish marks it ignored. Acceptable?
10. **Names.** Proposed public names are `end_message_node_id` and
    `last_end_message`. Should the TUI's `回答` label on the model's text rows
    also change, or stay?
11. **Esc.** After you press Esc to end a Turn, should the "you still owe a
    reply" reminder still wake the model if the run would otherwise finish
    (proposal: yes, like `--deliver complete`), or should Esc suppress it?
12. **Remove `history get --final-answer-only`.** Only `exec` uses it, and
    replies make it unnecessary. Remove it?
13. **Forks.** A fork of a Thread does not inherit the duty to reply; only the
    original Thread can reply. Agree?
14. **exec strictness.** With this change the model must run
    `shellboy message reply`. If it only writes its answer as normal text, it
    is reminded once and then exec fails with exit 4 ("ignored"). Acceptable?
15. **Cap.** At most 128 unanswered reply-expected messages per receiving
    Thread (the existing queue bound); the 129th send is refused. Agree with the
    number and with counting per receiving Thread?
16. **Asking yourself.** A model cannot send a reply-expected message to its own
    Thread. Agree?
17. **TUI glyph.** Choose option A, B or C in [TUI rendering](#tui-rendering)
    (or propose another).
