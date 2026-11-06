# Event subscription and the external RPC contract (#243, #244)

Status: study, not adopted. Nothing here changes the current contract until the
user adopts it. Evidence refers to `origin/main` at `b0d9c94284` (v0.9.0 RC).

Context (user decision 2026-11-06): the Web UI is a separate project
(`shellboy-web`, a Rust bridge on the same machine that speaks the existing
local IPC to the owner and serves HTTP/WebSocket to a browser). Core adds only
stable external surfaces: change notification without polling (#243) and the
RPC as a documented external contract (#244).

## 1. Findings from the current code

### 1.1 How the owner notifies observers today

All notification is in-process and carries no saved data. Observers always
re-read saved facts.

| Mechanism | Where | Shape | Consumers |
|---|---|---|---|
| `EngineEvent::Changed{thread_id, recovery}` | `Control::changed_with_recovery` (`src/control.rs:393-402`), via `publish_event` (`src/control/ui_events.rs:84-89`) | `tokio::broadcast`, capacity 256 (`control.rs:254`), plus a separate 256-slot UI broadcast | TUI frontend; tests |
| `changed: Notify` | same call, `notify_waiters()` | wake-only, no payload | every `wait` loop; also woken by request completion (`RequestGuard::drop`), shutdown |
| `shell_output_changed: Notify` | after raw chunk + `saved_sequence` commit (`wait.rs:1508`) | wake-only | `shell_output` waits; TUI live output |
| `EngineEvent::Stream{thread_id, node_id(attempt), text, stream_char_offset}` | `src/control/driver/request.rs:336` | text deltas, never saved | TUI only (selected Thread) |
| `EngineEvent::ModelReceive / Notice / Error` | driver | ephemeral | TUI |

`Changed` is published after the commit that changed a Thread: every mutation
reports `changed_threads` and `apply_effects` emits them (`control.rs:995-1025`);
thread create (`control/db.rs:1263`), delete (`db.rs:2708` – all deleted
targets), rewind, message cancel/reply, compaction (`compact.rs:173,206`),
driver transitions (≈40 call sites in `src/control/driver*`). The adopted wait
design already relies on "commit, then Changed" (wait.md C6).

The TUI treats `Changed` as an invalidation hint: it marks the Thread dirty
and reads from the DB worker (`src/frontend.rs:3732-3756`). On
`RecvError::Lagged` it performs an explicit resync of every projection
(`frontend.rs:3791-3810`). It also has a 500 ms tick that refreshes the
selected Thread while progress is running (`frontend.rs:1017, 3838-3849`).
So "hint + bounded re-read + explicit resync on gap" is the established
pattern; #243 should expose that pattern, not invent a second model.

`wait` is the existing external observation: enable `Notify` → read DB snapshot
→ evaluate → await (`src/control/wait.rs:188-230`). It is not a cheap poll:
an external wait keeps an operation receipt (protocol.md R3-3) and the first
pass writes a `wait.pending.<id>` row to `app_meta` (`wait.rs:212-217`).

### 1.2 Transport

- Framing: 4-byte big-endian length + UTF-8 JSON, 8 MiB max (`protocol.rs:840-870`).
- One connection = `Hello{protocol_version}` → `Welcome{protocol_version,
  instance_id}` → exactly one `Request` → exactly one `Response`, then close
  (`src/ipc.rs:72-145`, client `call_stream` `ipc.rs:313-381`). The reply is a
  `oneshot`; while waiting, the server reads one byte to detect peer
  disconnect and cancels `Invocation.disconnected`.
- Admission: 32 connections; permits ordinary 24 / stop 2 / status 2
  (`src/ipc/server.rs:18-21, 38-51`). A long `wait` holds one ordinary permit for
  its whole duration. The 5 s read deadline covers only the request frame, not
  the processing time, so long-held responses are already supported.

Consequence: a long-poll operation (block until there is something after the
cursor, or timeout) fits the current transport with **no** transport change,
no worker and no queue. A multi-frame streaming response would need a new
reply channel (`mpsc` instead of `oneshot`), a new frame kind on the client,
and a policy for a permit held indefinitely; it is not needed to remove
polling and is deferred (stage E).

### 1.3 There is no durable, global change order

Tables use TEXT IDs and wall-clock `created_at`/`ended_at`; state changes
update rows in place (no change sequence). Thread observation deliberately
refuses wall-clock ordering and uses per-stream anchors (thread-observation.md).
Durable tail cursors exist only per kind: `thread_settled` (`wait --after`),
shell raw output, and paged reads bound to a watermark. A DB-replayable
"node appended / turn state / tool start" event stream would therefore need a
new change-sequence column or table written in every relevant transaction.
That conflicts with #243's "no new table" direction and is not required:
clients' state is saved facts, so a missed hint is repaired by re-reading.

### 1.4 How chat (#242) polls

`ChatSession::follow` (`src/chat_client.rs:258-397`) loops: `run.get` (Run
modes) + `turn.get` + `shell.list(limit 50)` (`tool_progress`, `:505-573`) +
`wait{turn_completed|run_ended, timeout ≤ 1000 ms}`. Every pass is up to four
connections; a running tool yields roughly that per second, and tool
start/finish lines appear up to ~1 s late because `wait` is not woken by a
tool start (no condition matches it). Nothing was measured; see §3.

### 1.5 Versioning and decode strictness (relevant to #244)

- `PROTOCOL_VERSION = 2` (`protocol.rs:10`); owner and client both reject any
  other version at endpoint metadata and Welcome (`ipc.rs:85, 235, 332`).
  protocol.md (PR1-10, Issue #33) states "Use the same release for CLI and
  owner … No capability negotiation". `shellboy-web` breaks that assumption.
- Every `args!` type has `#[serde(deny_unknown_fields)]` (`protocol.rs:376-383`),
  and the **same macro defines result records** (`ThreadDetails :922`,
  `RunDetails :934`, `TurnDetails :935`, `MessageAccepted :762`,
  `WaitResult :783`; 67 `args!` uses). `Hello`, `Welcome` (`ipc.rs:25-35`) and
  `Endpoint` (`app.rs:128-135`) are strict too. So today an added field is
  not compatible in either direction for a Rust client: an old owner rejects a
  new request field; an old client rejects a new result field.
- `Request` has a hand-written `Deserialize` (`protocol.rs:262-284`); an
  unknown command or unknown args field fails inside `read_frame`, and
  `serve_connection` returns via `??` **without writing a reply**. The client
  then reports "Connection ended before reply; the operation result is
  unconfirmed" (`ipc.rs:359-366`). Concretely: a new `chat` (sending
  `message.send.human`, #242) against an older protocol-2 owner gets an
  "uncertain mutation" message instead of "owner does not support this field".
  (Code reading; add a regression test when fixing.)
- Results are partly untyped: record types contain 16 `Value` fields
  (`snapshot`, `last_end_message`, `last_input`, `awaiting_reply`,
  `context_usage`, …), several operations build results with `json!` in
  `control/db.rs` and the client side receives `Response<Value>`. There is no
  code-level map from command to result type.
- `shellboy` is a lib + bin crate with `pub mod protocol` (`src/lib.rs:36`),
  `publish = false`. No JSON Schema dependency exists (`Cargo.toml:32-61`).

## 2. Event model (#243)

### 2.1 Principle

Events are **invalidation hints with a resumable cursor**, not a second copy
of the data. Facts are read with existing RPCs. This keeps one source of
truth (the DB), needs no new table, and makes "no silent loss" provable:
either the client sees every changed Thread after its cursor, or it is told
`resync_required` and re-reads.

### 2.2 Change clock (the only new core state)

`Control` owns one `ChangeClock` behind a `std::sync::Mutex` (memory only,
held for map updates, never across `.await`, never on the UI OS thread):

- `seq: u64` incremented per `Control::changed(thread)` call, inside the
  existing function before `publish_event` and `notify_waiters` (one hook,
  all ≈40 producers already go through it);
- `by_thread: HashMap<ThreadId, u64>` and `by_seq: BTreeMap<u64, ThreadId>`:
  only the latest seq per Thread is kept (coalescing);
- `floor: u64`: when entries exceed `MAX_CHANGE_ENTRIES` (proposal 4096 Threads),
  the oldest entry is evicted and `floor` rises to its seq.

Bounds: entry count is fixed; memory is entries × (two map nodes + one Thread
ID string). Measure the actual bytes in a test rather than estimating.
Owner: `Control`; dropped with it; no task, worker or queue. Shutdown:
`begin_shutdown` already calls `changed.notify_waiters()` and cancels `stop`,
which ends pending long-polls.

Burst behaviour: 10 000 changes to one Thread occupy one entry. Overflow only
happens when more than `MAX_CHANGE_ENTRIES` distinct Threads change between two
polls of one client; that client gets `resync_required`, never a silent gap.

### 2.3 Operation `events.wait` (receipt-free long-poll)

Request `EventsWaitArgs`:

- `after: Option<EventCursor>` — opaque base64url JSON `{v:1, instance_id, seq}`,
  a dedicated type like the shell raw cursor, not `PageCursor`.
- `thread_ids: Vec<String>` — optional filter, 0..64 (0 = all Threads),
  same ID bounds as `thread_settled`.
- `timeout_ms: u64` — default 30 000, 0 allowed (same as `wait`).

Result `EventsWaitResult { status, cursor, changes: [{thread_id, seq}], more }`:

| status | when | client action |
|---|---|---|
| `baseline` | `after` absent | read current state; keep `cursor` |
| `changed` | ≥1 matching entry with seq > after | re-read those Threads; continue with `cursor` |
| `timeout` | none before deadline | continue with `cursor` |
| `resync_required` | `after.instance_id` ≠ current instance (owner restart), or `after.seq < floor` | re-read everything the client shows; continue with the new `cursor` |

`seq > head` for the current instance is a forged/corrupt cursor →
`invalid_argument`. Changes are ascending by seq, at most 64; `more=true`
means further matching entries already exist and `cursor` is the last
returned seq; otherwise `cursor` is the head observed in the same lock.

Algorithm (same lost-wakeup discipline as `wait`): enable
`control.changed.notified()` → lock clock, compute → if non-empty or
deadline reached return → `select!` on notified / deadline /
`Invocation.disconnected` / `control.stop`. No DB access at all; spurious
wake-ups (request completion also notifies) cost one map lookup.

Properties:

- Ordering: by owner-local seq, which follows `changed()` call order; each
  entry follows its commit. Across Threads this is "notification order", not
  a causal order of saved facts — clients must not infer cross-Thread causality.
- Coalescing: intermediate states can be skipped (running→completed between
  polls yields one entry). Clients that need every transition read durable
  facts (history, `shell.list`, `wait thread_settled --after`).
- Back-pressure: pull-based; the owner keeps no per-client buffer. A slow
  client only receives larger coalesced batches or `resync_required`.
- Auth: identical to other read operations (peer-uid check on Unix, optional
  Origin). No new authority. A model Origin is not given ownership edges and
  does not enter Turn `waiting`; whether models should use it at all is open (Q3).
- Admission: ordinary IPC permit, like `wait` (24 shared). A bridge must
  multiplex one long-poll for all browser tabs.
- No receipt, no `app_meta` row: it is an observation (fresh re-observation on
  lost reply, protocol.md Issue #3), unlike `wait`.

Why not a `wait` condition: `wait` keeps receipts, writes `wait.pending`
metadata, participates in the wait-cycle graph and Turn `waiting` state. All
of that is wrong for a high-frequency hint poll.

What `changes` deliberately omits in stage B: node IDs, run/turn states, tool
start/finish, delivery accepted/held, compaction adoption. Those are read
with existing RPCs after a hint (`thread.get` gives run/turn IDs and
`last_input`, `turn.get`/`run.get` states, `shell.list` tools, `history.list`
nodes, `message.get` delivery). A typed per-Thread digest in the response is
stage E1, only if measurement shows follow-up reads dominate.

Shell output bytes are not Thread hints (#28 intentionally uses the separate
output `Notify`); clients use `shell read` / `wait shell_output` for live output.
Model text deltas (`EngineEvent::Stream`) are never saved and are not covered;
see E3.

CLI: `shellboy events [--after CURSOR] [--thread ID]... [--timeout-ms N] [--follow]`
prints one JSON result per line; `--follow` loops client-side. Also usable by
scripts and as the reference consumer for the contract tests.

### 2.4 Correctness risk: hint completeness

The scheme is only lossless if every commit that changes a client-visible
saved fact of Thread T is followed by `changed(T)`. The TUI's 500 ms progress
refresh can mask a missing call today. Stage B must add a test per mutation
family (send/held/cancel, run/turn transitions, tool start/end, compaction,
create/fork/rewind/delete, reply, recovery) asserting the clock advanced for
the affected Thread(s), and treat a gap as a bug in the producer.

## 3. Chat and TUI

Chat switch (stage C), still a thin client:

1. Before `message.send`, `events.wait{after: None}` → baseline cursor (so a
   change between send and follow is not missed).
2. Loop: read state as today (`run.get`/`turn.get`/`shell.list`), emit lines,
   return on terminal state; then `events.wait{after, thread_ids:[thread],
   timeout_ms: remaining-or-30000}` raced with Ctrl+C exactly like the current
   `select!`. `resync_required` = just read again (chat is already level-triggered).
3. Remove the 1 s clamp. Reads happen only after a hint or timeout.

TUI: do not switch. It is in-process and already receives push hints with
Lagged resync. Possible later use only: count, in a test build, periodic
refreshes that observe a change no hint announced; zero over the
hint-completeness suite would justify removing the periodic refresh for
saved facts (elapsed-time display still needs a tick).

What to measure (record as facts in validation, no bounds asserted):

- Latency from a saved fact (tool start, Turn end) to the chat line, before vs
  after, p50/p95, fake provider, synthetic Thread, same machine.
- IPC connections and DB-worker calls per followed Turn and per idle minute
  while a long tool runs, before vs after.
- `events.wait` wake-ups vs returned changes with N concurrent long-polls
  (spurious wake cost of the global `Notify`).
- Per-poll connect + handshake time on Unix socket and Windows named pipe, to
  decide whether a persistent stream (E2) is worth it.

## 4. External contract (#244)

### 4.1 Covered (clients may rely on)

Framing and limits; endpoint discovery (`endpoint.json`, `SHELLBOY_SOCKET`);
Hello/Welcome; Request/Response envelope; the closed `ErrorCode` set and
`details.request_may_have_been_applied`; every operation name with its args;
typed result records; opaque cursors (never parse them); operation-ID replay
and receipt semantics; sender recording (`sender.kind`, self-asserted labels,
`human` is an unverified claim); wait semantics; `events.wait`.

Explicitly not covered: DB tables and `app_meta` keys, logs and diagnostics
files, TUI behaviour, in-process `EngineEvent`s, field contents typed as
`Value` (marked "opaque, may change" in the schema until typed), message
text formats of errors and hints.

Schema-valid is not accepted: limits and cross-field rules stay in Rust
validation and are documented in prose.

### 4.2 Version and compatibility policy (proposal)

- Requests stay strict (owner rejects unknown fields). A client must not send
  a new optional field or operation unless the owner advertises it.
- Results become tolerant: clients ignore unknown fields. Split the `args!`
  macro into `args!` (strict, requests) and `record!` (no
  `deny_unknown_fields`, results); also make client-side `Welcome` and
  `Endpoint` decoding tolerant.
- Additive change (new operation, new optional request field, new result
  field, new enum value only in a result field documented as open): no version
  bump, add a capability name if it is a request-side addition.
- Breaking change (remove/rename/retype, change meaning, make optional
  required, framing/handshake change): bump `PROTOCOL_VERSION`. Removal goes
  through one release where the capability is no longer advertised.
- The owner keeps accepting exactly one version (current-only, Issue #33);
  supporting a range is open (Q2).

### 4.3 Capability advertisement

`Welcome` gains `capabilities: Vec<String>` (names like `events.wait`,
`message.send.human`). Welcome arrives before the request, so a client can
decide before sending anything. Because current clients decode `Welcome`
strictly, adding the field is itself breaking: do it once, together with the
tolerant-record split, as **protocol 3**. Everything present at protocol 3 is
the baseline. The list holds only additions after that and starts empty or
nearly empty. The list is a `const` slice checked by a test against an
exhaustive match over `Command`, not a registry.

Old owner + new client:
- owner on protocol 2 → existing clear version-mismatch error before sending
  (the `human` case is solved this way, since `human` is in the v3 baseline);
- owner on protocol 3 without a capability → client omits the field or
  reports "owner does not support X" before sending.

Independently, and without a version change (stage A1): `serve_connection`
should parse the frame as `Value`, extract a non-empty `operation_id`, and
on unknown command/field reply `invalid_argument` with
`request_may_have_been_applied=false`. Today it closes without a reply, which
is reported as an uncertain mutation. This matches protocol.md P5 ("decodable
request rejected → reply with its own ID").

This supersedes "No capability negotiation" in protocol.md PR1-10 and needs
the user's approval (Q1).

### 4.4 JSON Schema generation

Requirement: generated from the Rust types, not hand-written, with a drift check.

Options:

| Option | Cost | Verdict |
|---|---|---|
| `schemars` derive (respects serde `rename`, `tag/content`, `default`, `deny_unknown_fields` → `additionalProperties:false`) | one dependency + its derive crate; derive added in 2 macros (`args!`/`record!`, `states!`) plus ≈16 plain derives, `MessageSender`, `ExecutionContext`; manual schema for `Request` (custom Deserialize, flattened `Command`) and `#[schemars(with)]` on `ShellToolArgs.description` (or exclude the model tool type) | recommended |
| Infer schema from fixture JSON in `tests/protocol.rs` | no dependency; but misses optional fields, enums, and is not "from types" | rejected |
| Own proc-macro / serde reflection crate | more code or a less complete dependency for adjacently tagged enums | rejected |

Dependency record (AGENTS.md): need — #244's "generated from source types"
requirement; bound — compile-time derives, runtime use only in the export
command; pin an exact 1.x version at least two weeks old, recorded in the
decision log; validation — 3-OS build/clippy/tests and measured release
binary size delta.

Command → result mapping: one exhaustive `match` in `protocol` that returns
the result schema per `Command` variant (consistent with R3-3: exhaustive
matches, no metadata registry). Operations whose results are still `json!`
values are marked opaque in the schema; typing them is follow-up work, one
issue per family, ordered by what `shellboy-web` actually reads.

Export and drift: `shellboy protocol schema` prints one JSON document
(envelope, handshake, per-operation args and result, capabilities, version).
Commit it as `docs/reference/protocol.schema.json`; a test in
`tests/protocol.rs` compares generated output with the committed file and
fails with the regeneration command. It runs under the existing `cargo test`
in `scripts/check.sh` and CI, so no new CI step. Whether an edit is additive
or breaking stays a review decision.

## 5. Staged plan

| Stage | Content | New mechanism |
|---|---|---|
| A1 | Reply typed `invalid_argument` for decodable unknown command/field instead of closing; regression test (done, D-244-1) | none |
| B | `ChangeClock` + `events.wait` + `shellboy events`; tests: ordering, coalescing, `more`, filter, baseline, resume, floor → resync, instance change → resync, forged cursor, timeout 0, disconnect, shutdown, hint completeness per mutation family; protocol/CLI docs | one bounded in-memory map in `Control`, one operation |
| C | chat uses `events.wait`; before/after measurements (§3) recorded in validation | none |
| A2 | Protocol 3: `record!` split (tolerant results), tolerant `Welcome`/`Endpoint` decode, `Welcome.capabilities`, version policy section in protocol.md; supersede PR1-10 | handshake field |
| D | `schemars`, command→result match, `shellboy protocol schema`, committed schema + drift test, contract section | one dependency |
| E (measure first) | E1 per-Thread digest in `changes`; E2 persistent multi-frame stream; E3 live text deltas for Web (in-memory, lossy, explicit gap, falls back to saved output); E4 durable typed change log (schema change) | each needs its own decision |

A1 and B are the smallest valuable first steps: B alone removes chat polling.
A2 and D must land before `shellboy-web` depends on the RPC.

## 6. Risks

- Missing `changed()` producers (masked by the TUI tick) would make hints
  incomplete. Mitigation: the per-family test in B.
- Global `Notify` wakes every `wait` and long-poll on any change; cheap
  (memory only for `events.wait`) but measure with several clients.
- Long-polls share the 24 ordinary permits with `wait` and mutations; many
  chats plus a bridge reduce capacity. A dedicated small class would change the
  documented 28-permit budget; decide after measurement (Q6).
- Coalescing hides intermediate states; documented, durable facts remain.
- Without E3, the Web UI cannot show token streaming like the TUI.
- Protocol 3 breaks every mixed-release pair once (acceptable pre-1.0 under
  the current same-release rule, but it is a visible break).
- Schema truthfulness is limited by `Value` fields until they are typed.

## 7. Open questions for the user

1. Approve replacing "no capability negotiation" (protocol.md PR1-10) with
   `Welcome.capabilities` and a one-time bump to protocol 3?
2. Keep the owner current-only (one protocol version), or accept a small
   range so a bridge can lag one release?
3. May models use `events.wait` (Agent Guide), or is it for humans and
   external clients only?
4. Is live token streaming required in the first Web milestone? If yes, E3
   needs a design (bounded per-attempt buffer, explicit gaps) before the Web
   project starts.
5. How should `shellboy-web` obtain types: generate from the JSON Schema,
   depend on this crate by git (pulls the TUI stack), or split a small
   protocol crate?
6. Long-polls on the ordinary permit pool (simplest) or a dedicated class?
7. Name: `events.wait` (describes long-poll semantics) or `events.subscribe`
   as in the issue?
8. Is a durable typed change log (E4, new column/table) ever wanted, or are
   hints + saved-fact reads the permanent model?
