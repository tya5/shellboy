# Shellboy Core v0.17.0

## Interface consistency

- **Record pages (#416):** Owner lists report `items`, the effective `limit`, typed `filters`, and `next_cursor` when more coverage is available. Even an empty page can have a continuation; repeat the read with the same selectors. `compact list` is a fixed report with `more`, not a cursor-based list. Schedule and service status report the latest execution, not an execution page.
- **Work and history observations (#416):** Work `status` reports `limit` at the top level and no longer echoes `filters.cursor` or `filters.memory`. History execution counts state their exactness independently of excerpt or history coverage. `history read` returns the same body and projections as the former `history get`.
- **Message sender attribution (#420):** Use `--sender-label` on Send and Ask to supply a single-line display label, up to 64 Unicode scalars. A label does not authenticate a sender. A reply to a named duty takes its label from the saved addressee. A Thread reply has model provenance only when its caller is authenticated for that Thread; an originless Thread reply is external CLI. Replies to `human` retain human attribution.
- **Ask acceptance (#421 Unit 3):** `message.ask` now declares its existing flat `MessageAccepted` result, shared with `message.send`. Acceptance records the request and its initial delivery state; it is not execution or an answer. For a named Ask, the result's `thread_id` identifies the asking Thread.
- **Reply result (#422):** `message.reply` now declares its existing `MessageReplied` result. The outer `message_id` identifies the answer and `reply_to` identifies the Ask. The optional nested `delivery` records acceptance for routing and can initially be held; standalone replies omit it. Use `message get --message MESSAGE_ID` with the answer ID to check current delivery.
- **Branch revisions (#418):** Each `thread branches` row has `selection_revision`, the same value as `history window` reports in `revision` for that Thread. Compare the revisions before using branch path positions; a rewind followed by an append can restore the same length without restoring the same path. When present, `divergence.parent_selection_revision` identifies the parent path used for the comparison.

## Terminal UI

- **Recurring work (#409):** `/schedule` and `/service` use the CLI's verbs and flags. For `/schedule add`, put the shell program after `--` as one quoted slash argument so its inner shell quotes reach the program unchanged.
- **Settings (#413):** `/config get`, `/config set`, and `/config check` use the corresponding CLI verb flags. To edit settings, pass the current file hash from Get to Set as `--expected-hash`. Check uses the current session's configuration and can contact its model catalog.
- **Ask (#414):** `/ask --thread ID --text 'Question'` opens one reply duty; ordinary Send opens none. Named and default human targets still require an authenticated asking model Thread. Use `shellboy message ask --text-stdin` with the intended CLI target for piped text; the TUI does not read piped bodies.
- **Owner connection and drafts (#311):** The TUI reads selected history through bounded owner queries and uses native change notifications to refresh the view. Draft text, attachment references, and the saved view are coordinated with the owner. If a send outcome is uncertain after disconnect, confirm the original operation before sending again. The TUI does not automatically replay it.

## Guides and reliability

- **Getting started (#410, #417):** The README opens with a quickstart for the downloaded binary. Offline guide verification follows continuation pages and checks both link targets and complete guide bodies.
- **Delivering results (#424):** The Agent Guide shows how to register an output as a Thread artifact, update that artifact on revision, and mention it in the final reply.
- **Recovery guidance (#400):** Saved indexing failures use the same model transport guidance as conversation failures. The guidance does not rerun work.
- **Replay budget (#385):** A selected resend that exceeds the assembly budget is refused before provider admission. The history retained for future resend is not reduced by this check.

## Upgrade notes

- **Backup and compatibility:** Back up the entire data directory before upgrading. Upgrade the owner, CLI, and RPC clients together. There are no owner database migrations in this release. After upgrading, restart history body reads without a cursor; cursors from `history.get`, `history.get.parts`, and `history.get.end_message` are not accepted at the new route.
- **Protocol:** **26 -> 34**. Versions 27-29 cover native draft uploads, bounded history reads, and draft persistence; 30 normalizes page and exactness fields; 31 updates message sender attribution; 32 renames selectors and Thread parent fields; 33 renames history body reads; 34 publishes branch selection revisions.

The renamed interfaces below have no aliases. Update scripts and client decoders before upgrading.

| Surface | Old | New |
| --- | --- | --- |
| History body read | `history get` / `history.get` | `history read` / `history.read` |
| `message list --awaiting` flags | `--owed-by`, `--asked-by` | `--owed-by-thread`, `--asked-by-thread` |
| `message.list` request fields | `owed_by`, `asked_by` | `owed_by_thread_id`, `asked_by_thread_id` |
| Thread create and set flag | `--parent` | `--parent-thread` |
| Thread create and set args; public Thread summary and detail | `parent_id` | `parent_thread_id` |
| History list flag | `--state` | `--shell-state` |
| `history.list` args and page filters | `state` | `shell_state` |
| Total reply count exactness | `awaiting_replies_exact` | `awaiting_replies_count_exact` |

`thread set --parent-thread ''` still detaches a Thread. History's `shell_state` is the state of a saved linked shell execution, not a Run. Saved reply keys, saved parent-change facts, and the docs-corpus `--parent` selector did not change.
