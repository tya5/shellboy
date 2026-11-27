# The TUI as a client of the headless owner

## Non-goals

- No Core OS registration, service manager, crash restart loop, or Studio supervisor. Starting one `serve` when none exists is not supervising it.
- No second owner shutdown path, embedded-owner fallback, compatibility shim for old in-process TUI calls, or public partial TUI mode.
- No TUI-only wire protocol, generic SQL or snapshot RPC, raw engine-event feed, secret or provider-body export, or unbounded scans. New facts belong to the existing native protocol and must also make sense for other clients.
- No machine-wide singleton, remote owner transport, crate split, or change to explicit stop semantics. No runtime or protocol changes are authorized by this draft.

Status: **Proposed for user approval** (#311). Source baseline: Core `aea8b57e5c79ff68b6b336b3c43eb46a1c8f41b0`. This is a static design, not a timing, memory, or runtime result. The intervention contract supplied by #402 is a prerequisite, not work to repeat here.

## One path: start, attach, leave, return

1. The user types `shellboy` in a terminal. Resolve CWD and the chosen `--data-dir` using the existing CLI path rules. Try that profile's native endpoint first, using the peer and exact-version/instance handshake (`src/ipc.rs:539`, `src/chat_client.rs:173`). Read the starting Thread page through `thread.list`; do not open SQLite. A running `serve` started by the user or by Studio is equally valid. `--config` affects a new owner only; it is not an instruction to change a running one.
2. If there is no owner, start the same executable as `serve` once, with the resolved absolute data dir and applicable explicit config path, without going through a shell. Detach it from terminal hangup and console control on the host platform; detach its standard streams too. Use the headless path in `src/serve.rs:43`, not a second startup implementation. Do not use a TUI-owned stdout readiness pipe: `run_prepared` closes the owner when publishing its ready line fails (`src/serve.rs:92`). Use bounded endpoint readiness plus a successful native handshake; check a retained child handle for early exit while attaching, then drop the handle without killing the child. Surface a bounded local startup failure rather than looping on spawn; retain existing `serve` diagnostics and allow explicit `serve` for diagnosis. Never print credentials.
3. An attach error is not proof of absence: `io_error` currently collapses different connect failures into `app_not_running` (`src/ipc.rs:96`). Retain the internal failure category before that translation, not a new wire command. Missing/stale unreachable endpoint with verified unowned profile permits spawn; busy or unclassifiable ownership, malformed/denied access, mismatched protocol or peer, and a live startup do not. Use the existing per-data-dir owner lock as authority; any read-only ownership assessment is advisory, and the new child must acquire the lock itself. Two concurrent launchers can both observe absence, but only one `serve` may initialize ownership; the loser re-reads the selected profile endpoint and attaches to the winner, or reports failure after the same bounded readiness window. Never steal the lock or delete endpoint metadata from the client. Preserve `serve`'s current supported path/identity rules; report unsupported paths rather than selecting another profile.
4. Quit, lost terminal, client signal, and local force-close stop only terminal input, client tasks and subscriptions. Restore the terminal even if transport is lost. Before graceful close flush the newest queued draft saves and report accepted, refused, or unknown in-flight writes via operation receipts; close must never revoke a durable accepted write. Cancel a readiness wait locally even if a newly launched owner is now shared. Never send `owner.shutdown`, stop a Run or kill children as part of client close. Owner shutdown remains the headless admission-and-drain path (`src/serve.rs:76`, `src/owner_session.rs:23`), including explicit `owner.shutdown` and the existing owner stop sources.
5. If the owner goes away while the TUI is open, leave the screen open and disconnected. Show the typed reason when available (for example owner shutdown or fatal storage failure), and use a typed connection-lost reason for an abrupt close with no authoritative cause; do not infer a crash from EOF. Keep the current local composer text. Mark unconfirmed saves and sends unknown with their operation/receipt IDs, distinct from refused or accepted writes. Offer Reconnect and Quit. Reconnect is user-triggered and runs steps 1-3 for the same profile: attach to a running owner, or start one detached `serve` if absence is confirmed. Do not restart automatically or run a reconnect retry loop; the bounded readiness wait after a user-requested launch is not a retry loop. On reconnect, reconcile unknown receipts before any user-requested resend; do not replay them automatically.
6. After quit, the user types `shellboy` again. Resolve the same profile and attach to the same owner instance if still running. Read fresh saved Thread state and draft revisions; resume view from a valid saved anchor, or use the regular bounded rebase when it has expired. Do not reissue a mutation whose outcome is unknown: look up its operation ID first. An owner restart invalidates in-flight tickets, event cursors and cached live state. Explicit owner stop and another profile's owner remain independent.
7. The Core management view shows the selected owner version, instance ID, uptime, #398 startup stage, and live Run and shell counts. Read those facts through the native handshake and existing status surfaces; extend their typed readbacks if needed, without a new command. Label a saved startup stage as last observed, not as proof of liveness, and never show unavailable work counts as zero. Stop opens a confirmation listing the live Runs and shells that would stop, with their IDs and observed counts. If the list cannot be verified, do not present it as complete. Confirm against the displayed instance, then send the existing receipt-backed `owner.shutdown`. Stay disconnected after Stop. Restart is also explicit: confirm the interruption, send the same instance-fenced shutdown, and after the old instance exits use the same detached `serve` path if absence is confirmed. If another compatible owner has already started, attach without starting another. Resolve an uncertain shutdown receipt before taking further action; no supervisor, automatic restart or retry loop after either action. Quit remains client-only.

## Facts and operations at the boundary

The owner writes execution, selected-path, durable draft and receipt state. The client owns UI layout, terminal/clipboard integration, input buffering, and display selection. Public reads return facts, not widgets. Every new reader must name its scope, revision or continuation, omission behavior, and row, byte and examined-work limits before adoption. A request generation plus selected Thread/path revision protects the client from late reads; it does not substitute for owner snapshot or mutation fences.

| In-process use (baseline source) | Existing wire use, remaining gap, or removal | What breaks if omitted |
| --- | --- | --- |
| Loaded config, owner and terminal width (`src/main.rs:659`, `src/frontend.rs:945`) | Use handshake and redacted `config.get` for public settings/limits; expose a typed public fact only if it is actually absent. Keep terminal preferences and measurement client-side; discard the owner-bound persistent width writer, not fresh terminal probing. | Wrong profile/locale/limits, or terminal rendering that depends on a DB owner lock. |
| Browser, header, branch list, lineage and watch (`src/frontend/browser_read.rs:46`, `src/frontend/header.rs:139`, `src/frontend/lineage_observer.rs:76`, `src/frontend/watch_read.rs:20`) | Use `thread.list/get/branches`, including exact visible-ID refresh. Need bounded first-retained input for the title fallback and exact-ID watch witnesses only where not in summaries. Retained rooted lineage needs a root-scoped page with count/truncation; `parent_id` is not enough to reconstruct bounded rooted counts. | Lost title/watch cues or a scan of all Threads to draw one tree. |
| Selected history, body/copy and annotation enrichment (`src/frontend/history_reader.rs:205`, `src/frontend/copying.rs:44`, `src/frontend/message_metadata.rs:96`, `src/frontend/tools.rs:238`, `src/frontend/recovery.rs:101`) | Use `history.window/list/get/relations`, `thread.branches`, `compact.list`, `operation.get` and attempt `usage.get`. Page full bodies only for explicit detail/copy. Need exact visible-node/revision annotation facts absent from these responses, notably historical recovery association and source-to-tool-result pairing across replacements. Move queries to the owner; retain presentation. | A stale anchor yields the wrong text or pairs a tool result with the wrong call. An excerpt cannot substitute for copied original text. |
| Received-response inspection and previous/next input (`src/frontend/received.rs:13`, `src/frontend/input_history.rs:35`, `src/frontend.rs:849`) | Need bounded received-attempt listing and safe bounded detail for responses off the selected path. Need one-result, direction/anchor/path-revision-scoped input recall, and exact source attachment reconstruction where `history.read` and `attachment.list` lack it. No raw provider body, hidden reasoning export, or paging until a match. | An unadopted response becomes undiscoverable; recall reconstructs the wrong text/attachments after a path change. |
| Queue, pending work and inspections (`src/frontend/message_queue.rs:9`, `src/frontend/receipt_delivery.rs:6`, `src/frontend/live_work.rs:114`, `src/frontend/inspection_read.rs:74`, `src/frontend/inspection_saved.rs:65`) | Use `message.list/get`, `operation.get`, `turn.get`, `run.get`, `status`, bounded history/shell/usage/storage/index reads and attachment/management operations. Need typed queue order/restore association, wait-to-shell relations and compaction/reduction stage where missing; retain user-visible inspection coverage/availability facts only where not derivable. Narrow each read to the selected Thread or visible exact IDs. Use `status` for program activity; do not replace the TUI program-wide scan with an endpoint that scans. | Held input may look cancelled, or a displayed shell may be mistaken for the one blocking a wait. An inspector must distinguish missing source from omitted coverage. |
| Durable drafts and attachment fences (`src/frontend/draft_saver.rs`, `src/frontend/message_queue.rs:148`, `src/frontend/draft_images.rs:138`, `src/frontend.rs:1656`, `src/frontend.rs:2308`) | Keep the owner as the only writer of saved editing, archived, submitting and submitted rows and attachment pins. Expose bounded exact-revision draft read and restorable page and receipt-backed save/restore/discard/submit. Submit uses the existing transactional revision, selection and message/operation fence; not a read followed by unrelated `message.send`. Preserve managed attachment staging/pins and expiry by ID with bounded content reads/writes where a path alone does not prove access. | Quit can lose accepted input; a retry can send twice; a replacement draft can be removed or its images unpinned while send is in flight. |
| Saved view and multi-section read (`src/frontend/view_read.rs:126`, `src/frontend/refresh.rs:199`, `src/frontend/snapshot.rs:30`) | Keep durable existing per-Thread anchor/follow-tail with a bounded typed read and matching write scoped to that Thread and revision. No new state solely to mirror UI views. Use separate domain reads normally; preserve one owner snapshot for any specific view whose acceptance witness requires coherent facts, e.g. visible annotations and selected-history revision. Keep previously persisted values readable. | A reopen loses position, or mixed revisions put an annotation on a different history item. |
| Saved changes, live stream and output (`src/frontend.rs:955`, `src/frontend.rs:3729`, `src/frontend/live_output.rs:179`, `src/frontend/streams.rs:66`) | Use `events.wait` as invalidation and bounded `shell.get/list/read` offsets for committed output. Need selected Thread/attempt scoped text and timing deltas for retained live preview, plus exact visible execution markers if current reads cannot show fresh bytes. Give scoped streams bounded cursors and gap/reset handling against durable readback; never bridge `EngineEvent` wholesale. Client-only bell from saved recovery transition; drop callbacks, raw GC/DB telemetry, and private notices when typed command/receipt/outcome already supplies the fact. | Streaming can silently leave holes or display stale output after a gap; raw private events would couple the TUI to the executor again. |
| Ctrl+B, Esc, explicit stop (`src/frontend.rs` action/dispatch loop; #402) | Call `shell.background` for the exact visible foreground execution and `turn.interrupt` for the exact active Turn; read back existing status/outcomes. Preserve exact identity and typed stale refusals. Run/owner stop are distinct commands, never close hooks. | A key can release or interrupt the next execution instead of the one the user saw. |

## Implementation order after approval

Each unit is independently reviewable and can land without a user-visible partial attach mode. Each unit removes its replaced direct path in the same change that switches its callers. There is one authoritative path per migrated call site and no fallback to SQL on a protocol refusal. The existing top-level composition is removed in the final unit, not offered as a second mode.

1. **Client close and a real browser slice.** Separate TUI close from owner teardown; until cutover only the existing outer launch composition retains its existing stop policy. Use the native protocol browser adapter for `thread.list`, including exact-ID refresh. Until Unit 5, the production embedded TUI uses that native adapter against its own existing listener, not its former direct browser reader. A synthetic independent `serve` test drives that production adapter, selects the requested data dir, renders the page and closes the adapter; a subsequent read can still reach the same owner. This validates the seam without shipping a second TUI. Check that explicit owner shutdown still works.
2. **Saved reads and bounded inspection.** Migrate header, history/window, browser watch and inspectors onto typed native reads. Add the minimum missing domain facts from the table, one bounded surface per reviewable patch. Verify revision mismatch, continuation/omission, exact source copy and received/recall cases. Keep no direct TUI path for a migrated surface.
3. **Durable compose over the wire.** Move draft save/restore/submit and attachment pin ownership behind native requests, migrate saved anchor/follow-tail, and remove direct mutations. Witness stale revisions, restoration after refused send, lost response after accepted send, and one receipt when the same operation is replayed. Keep local composer text on transport loss and mark unconfirmed saves/submits unknown with their operation/receipt IDs. A user-triggered reconnect reconciles those IDs without replaying writes. Existing draft and view state remain readable; no runtime legacy shim.
4. **Live input/updates and intervention.** Use instance-fenced native event/readback with gap recovery and route mutations, including #402, through the native dispatcher. Move remaining snapshot queries to owner-side domain readers and remove private subscriptions in independently landable slices. Witness that a gap reloads bounded data. Do not expose a new disconnected mode while startup is still fused; the frontend should no longer import owner DB or execution objects before cutover.
5. **Default startup, attach, and typed disconnect cutover.** Attach first, launch detached `serve` only on confirmed absence, and delete the old fused composition and owner shutdown/force hooks. In the same cutover, keep the screen, typed reason, local input, and unknown receipts on owner loss, cancel old-instance subscriptions, and allow only user-triggered reconnect or quit. Witness that stale Turn and execution IDs cannot affect replacements. Test absent/existing/concurrent starts in disposable profiles, quit and reattach to the same instance with a still-running owned shell, terminal loss, explicit stop, mismatch/permission refusal without spawning, and two independent data dirs. Verify platform detachment on each supported OS before adopting the default. Do not install an owner or migrate real user data as part of the witness.
6. **Core management view (separate slice).** Test owner observations, Stop confirmation with live work, exact-instance Stop, and explicit Restart through the same detached launch path. Stop stays disconnected, Restart does not target a replacement instance, and neither action enables supervision or retry.

## Deletion ledger

These are inclusive lines in the pinned source, not a promised net diff. Replacements, tests and migrated owner-side queries are not counted as deletions. Mixed regions list what is removed and what survives; do not sum overlapping or replacement ranges. No runtime measurements were made.

| Pinned file and inclusive lines | Deleted in unit | Removal or rewrite | Keep or move |
| --- | --- | --- | --- |
| `src/main.rs:586-658` (73 lines), `659-693` (35 lines) | 5 | Replace the 108-line embedded owner startup/handoff region, including cancellation that shuts down a `Prepared`, the owner width-cache writer, terminal owner diagnostics/shell binding and the local RPC dispatcher. | Native attach/readiness, terminal handling, local cancellation and restoration; preserve the headless initializer in `owner_session`. |
| `src/frontend.rs:865-944` (80 lines) | 5 | Remove the owner-bearing frontend entry signatures and plumbing; `run_with_delivery` no longer accepts `DataOwner`, `LocalServer` or `Control`. | Keep clipboard injection for client tests and the client action/update loop. |
| `src/frontend.rs:3861-3934` (74-line mixed drain) | 1, 4, 5 | Delete listener stop, interruption capture, owner tasks/control/server/owner close and frontend owner shutdown tracing from this block. Move the stop policy out of the client loop in Unit 1, remove the private live subscription in Unit 4, and delete the outer embedded listener and owner drain in Unit 5. Affected adjacent ranges include `945-960` (16 lines), `1364-1392` (29 lines), `1477-1480` (4 lines); these are mixed spans. | Keep client cancellation, draft and accepted send fences, read worker cleanup, receipt delivery and terminal restoration; do not delete the headless shutdown sequence. |
| `src/frontend.rs:739-755` (17 lines), `3729-3785` (57 lines), `3826-3839` (14 lines) | 2, 4 | Remove the TUI-originated storage-failure escalation, direct engine-event match and raw worker diagnostics observation. | Owner diagnoses storage failures; typed readbacks and live deltas replace the retained UI facts. |
| `src/frontend/snapshot.rs:1-321` (whole file, 321 lines) | 4 | Remove frontend-only WAL reader and its private snapshot fixture from the client. | Move any still-needed owner query/coherence witnesses to the owning domain; preserve stale guards on the client. |
| `src/frontend/program_status.rs:38-60` (23 lines) | 4 | Delete the program-wide SQL read from the frontend. Retire or migrate its SQL-specific tests rather than cloning the scan. | Bounded status responses and local completion/seen presentation (`:7-37`, `:61` onward). |
| `src/ui/actions.rs:102-106` (5 lines), `233-235` (3 lines); `src/ui/termination.rs:134-183` (50 lines), `261-289` (29 lines); `src/app.rs:291-293` (3 lines) | 5 | Remove both Quit-to-owner gate binders (8 lines plus shared gate field/dispatch references), terminal-to-owner diagnostic/shell-kill/owner deadline wiring (79-line mixed region), and the owner width-cache guard accessor if no reader remains. | Keep local control mailbox priority, OS signal handling and client force-close, and the existing headless owner controls. The Unit 5 Core management view invokes `owner.shutdown` over the wire, independently of terminal Quit. |

### Direct-reader modules

For mixed modules, each range is the full pinned file under review (including retained code and tests), not a line-deletion estimate. The named unit removes the corresponding direct path; presentation-only files and separate test-only modules are not counted as production readers.

| Pinned file and inclusive lines | Deleted in unit (direct path) | Disposition |
| --- | --- | --- |
| `src/frontend/activity.rs:1-865` (865 lines) | 4 | Rewritten, not deleted. Replace private attempt and activity reads. |
| `src/frontend/activity/reduction.rs:1-65` (65 lines) | 4 | Rewritten, not deleted. Replace direct reduction-stage reads. |
| `src/frontend/branch_markers.rs:1-36` (36 lines) | 2 | Rewritten, not deleted. Replace direct branch-source reads. |
| `src/frontend/browser.rs:1-1344` (1344 lines) | 2 | Rewritten, not deleted. Replace direct enrichment, preserving paging state. |
| `src/frontend/browser_read.rs:1-373` (373 lines) | 1, 2 | Rewritten, not deleted. Browser summaries and exact-ID reads in Unit 1; watch/lineage enrichment in Unit 2. |
| `src/frontend/compaction_markers.rs:1-290` (290 lines) | 2 | Rewritten, not deleted. Replace saved marker queries. |
| `src/frontend/copying.rs:1-321` (321 lines) | 2 | Rewritten, not deleted. Read source bodies over the wire; retain local copy formatting. |
| `src/frontend/draft_images.rs:1-196` (196 lines) | 3 | Rewritten, not deleted. Move draft pin and durable attachment work to the owner. |
| `src/frontend/draft_saver.rs:1-647` (647 lines) | 3 | Rewritten, not deleted. Keep client coalescing; route durable saves through owner receipts. |
| `src/frontend/header.rs:1-1108` (1108 lines) | 2 | Rewritten, not deleted. Thread header/title reads; retain presentation. |
| `src/frontend/history_reader.rs:1-1232` (1232 lines) | 2 | Rewritten, not deleted. History windows and bodies; retain rebase and stale-result checks. |
| `src/frontend/input_history.rs:1-367` (367 lines) | 2 | Rewritten, not deleted. Replace local recall queries; keep directional recall behavior. |
| `src/frontend/inspection_attachments.rs:1-493` (493 lines) | 2, 3 | Rewritten, not deleted. Replace direct inspection access; preserve the typed client projection or navigation. |
| `src/frontend/inspection_deletion.rs:1-31` (31 lines) | 2 | Rewritten, not deleted. Replace direct inspection access; preserve the typed client projection or navigation. |
| `src/frontend/inspection_history.rs:1-157` (157 lines) | 2 | Rewritten, not deleted. Replace direct inspection access; preserve the typed client projection or navigation. |
| `src/frontend/inspection_index.rs:1-74` (74 lines) | 2 | Rewritten, not deleted. Replace direct inspection access; preserve the typed client projection or navigation. |
| `src/frontend/inspection_lifecycle.rs:1-861` (861 lines) | 2 | Rewritten, not deleted. Replace direct inspection access; preserve the typed client projection or navigation. |
| `src/frontend/inspection_lists.rs:1-847` (847 lines) | 2 | Rewritten, not deleted. Replace direct inspection access; preserve the typed client projection or navigation. |
| `src/frontend/inspection_operation.rs:1-577` (577 lines) | 2 | Rewritten, not deleted. Replace direct inspection access; preserve the typed client projection or navigation. |
| `src/frontend/inspection_physical.rs:1-271` (271 lines) | 2 | Rewritten, not deleted. Replace direct inspection access; preserve the typed client projection or navigation. |
| `src/frontend/inspection_read.rs:1-421` (421 lines) | 2 | Rewritten, not deleted. Replace direct inspection access; preserve the typed client projection or navigation. |
| `src/frontend/inspection_retained.rs:1-1470` (1470 lines) | 2 | Rewritten, not deleted. Replace direct inspection access; preserve the typed client projection or navigation. |
| `src/frontend/inspection_saved.rs:1-856` (856 lines) | 2 | Rewritten, not deleted. Replace direct inspection access; preserve the typed client projection or navigation. |
| `src/frontend/inspection_state.rs:1-2434` (2434 lines) | 2 | Rewritten, not deleted. Replace direct inspection access; preserve the typed client projection or navigation. |
| `src/frontend/inspection_storage.rs:1-51` (51 lines) | 2 | Rewritten, not deleted. Replace direct inspection access; preserve the typed client projection or navigation. |
| `src/frontend/lineage.rs:1-629` (629 lines) | 2 | Rewritten, not deleted. Replace rooted descendant queries, retaining tree presentation. |
| `src/frontend/lineage_observer.rs:1-395` (395 lines) | 2 | Rewritten, not deleted. Replace direct lineage observation, retaining selection checks. |
| `src/frontend/live_output.rs:1-252` (252 lines) | 4 | Rewritten, not deleted. Replace direct output reads; retain bounded preview and offset state. |
| `src/frontend/live_work.rs:1-815` (815 lines) | 4 | Rewritten, not deleted. Replace direct wait/compaction/live work queries; keep display projection. |
| `src/frontend/message_metadata.rs:1-864` (864 lines) | 2 | Rewritten, not deleted. Replace direct provenance and annotation reads. |
| `src/frontend/message_queue.rs:1-1189` (1189 lines) | 2, 3, 4 | Rewritten, not deleted. Replace saved receipt, draft and live queue reads in their respective units. |
| `src/frontend/notices.rs:1-385` (385 lines) | 2 | Rewritten, not deleted. Use saved typed notice facts. |
| `src/frontend/received.rs:1-1012` (1012 lines) | 2 | Rewritten, not deleted. Received-response discovery and detail; retain inspection handling. |
| `src/frontend/received/list.rs:1-396` (396 lines) | 2 | Rewritten, not deleted. Replace direct page query with an owner reader. |
| `src/frontend/recovery.rs:1-479` (479 lines) | 2, 4 | Rewritten, not deleted. Replace saved recovery association in Unit 2 and live observation in Unit 4. |
| `src/frontend/refresh.rs:1-999` (999 lines) | 2, 3, 4 | Rewritten, not deleted. Retain refresh sequencing; replace direct saved, draft and live reads in their respective units. |
| `src/frontend/sql_trace.rs:1-50` (50 lines) | 4 | Retire the client module after moving any retained diagnostic test. Retire the client-side SQL tracing path; owner-side tracing is unaffected. |
| `src/frontend/streaming.rs:1-1099` (1099 lines) | 4 | Rewritten, not deleted. Replace private stream reads; retain rendering logic. |
| `src/frontend/streams.rs:1-822` (822 lines) | 4 | Rewritten, not deleted. Replace direct observations; retain client stream state. |
| `src/frontend/thread_deletion.rs:1-1010` (1010 lines) | 2 | Rewritten, not deleted. Use typed deletion preview and operations. |
| `src/frontend/tools.rs:1-823` (823 lines) | 2, 4 | Rewritten, not deleted. Replace historical pairing in Unit 2 and live/output observations in Unit 4. |
| `src/frontend/usage.rs:1-184` (184 lines) | 2 | Rewritten, not deleted. Replace direct visible attempt usage enrichment. |
| `src/frontend/view_read.rs:1-549` (549 lines) | 2, 3 | Rewritten, not deleted. Saved view reads in Unit 2; anchor and draft access in Unit 3. |
| `src/frontend/watch_action.rs:1-61` (61 lines) | 2 | Rewritten, not deleted. Retain watch actions; remove direct reader access. |
| `src/frontend/watch_page.rs:1-1001` (1001 lines) | 2 | Rewritten, not deleted. Retain watch paging; replace owner-coupled reads. |
| `src/frontend/watch_read.rs:1-764` (764 lines) | 2 | Rewritten, not deleted. Replace exact-ID watch queries. |

The final source check is structural: no owner initialization, listener, supervisor handle, direct owner shutdown, DB reader, or raw executor subscription in the TUI call graph. The explicit Core management command is a protocol client, not a local teardown path. The user-visible proof is a terminal disappearing while its owner and accepted work stay reachable, then a new terminal attaching to that same instance.

### Embedded intervention migration (Unit 4)

Ctrl+B and `/background` use only the most recent foreground execution in the
current conversation projection. No observed execution means local failure
feedback, restored slash input, and no owner request.
Esc uses the displayed active Turn ID. These IDs go through the selected
listener as receipt-backed `shell.background` and `turn.interrupt` commands.
Neither a stale refusal nor a lost reply is retried against another identity.
The typed acknowledgment is not completion; saved readback still determines
what happened to the shell or Turn.

### Recurring work mutations and reads

Schedule and Service mutations from the TUI management surface go through
the selected native listener. Saved target IDs and receipts keep their
existing meaning. A lost reply retains the operation ID for lookup and is
not retried or presented as a refusal.

The management Schedule and Service lists use the existing native list
commands, fenced to the connected owner instance. Their exact Thread scope,
64-row cap per kind, and existing saved output tails are unchanged. A missing
or replacement endpoint does not fall back to the embedded owner. Cancellation
drops the outstanding read; there is no new subscription or polling path.

### Saved change delivery (Unit 4)

The embedded client consumes owner-wide saved-change metadata from one
instance-fenced `events.wait` at a time. The list and status need changes to
Threads not yet visible, so this wait deliberately does not filter Thread IDs;
it never requests text or reasoning, and it requests `output` entries. Changes
feed existing view scopes and receipt observers. A gap resets the current
browser generation and schedules normal bounded first-page and selected reads,
not a walk through old pages. The first baseline also refreshes any view
already shown before it arrived.

This one wait is the client's only owner wake (4d slice 3):

- `index_changed` rereads the index pause, the selected Thread and an index,
  storage or retained-source inspection.
- `retention_changed` rereads a visible inspection and the selected detail
  sources.
- `output` re-reads the shown execution: the shell inspection, a Watch row's
  detail, or the open live preview. The live work output age is computed from
  the entry's saved `output_at` (the same time `shell.get` reports as
  `last_output_at`).
- An owner notification for one Thread arrives as an ordinary change for it.
  An operation inspection rereads on its receipt's Thread; one whose saved
  facts name no Thread rereads on any change and on any owner mark, since an
  owner-level receipt settles through its mark and a shell receipt through the
  shell's Thread.
- A baseline or gap rereads everything the marks and output entries cover.

No worker or queued event buffer is added; dropping the pending wait drops its
connection. All wait errors, including typed wait-class admission refusal, stop
the wait and use the existing reconnect/quit gate without a client retry;
explicit reconnect must validate the same instance before resuming the cursor.
This slice does not remove the remaining private live preview, recovery bell,
or diagnostics paths, or implement reattachment to a replacement owner.

### Unit 4a live reader inventory

| Private producer and client use | Native basis and parity constraint |
| --- | --- |
| `EngineEvent::Stream` and `streaming::Frame::deliver`: selected-Thread provisional preview, offsets, reasoning windows and suppression after attempt adoption | `events.wait` can already supply selected-Thread answer deltas and attempt ends. It omits reasoning text intentionally. A cursor gap must reset the transient window and schedule a bounded saved read; do not fill an omitted range with fabricated text or ship reasoning bodies through the existing feed. The current frame's SQL liveness check also needs a native fence. |
| `EngineEvent::ModelReceive`: current-attempt last receive time, including transport keepalives | `events.wait` reasoning activity includes attempt ID, reasoning elapsed time and optional bytes. Those facts are not last receive time. Using local event arrival time would mislabel delayed calls and reconnect baselines. |
| `EngineEvent::Changed { recovery }`: bell for a new saved recovery transition, including nonselected Threads | The owner-wide saved-change hint already wakes the client; `thread.get` and `turn.get` can explain current recovery. Their current state does not distinguish a new transition from a baseline or ordinary change while already recovering. |
| `EngineEvent::Notice` and `Error`: release/cancellation and transient status text | Project release/cancellation from bounded saved tool results, deduplicated by saved identity. A generic raw error or notice event is not a native replacement; check each unsaved status call site for an existing typed result. |
| DB activity, GC watches and UI-memory callback: slow/no-progress footer, Help diagnostics and `status --memory` UI cache contribution | The adopted design drops raw worker telemetry and callbacks. Existing native status does not reproduce those UI-only diagnostics. Keep slow-job reporting on the owner rather than depending on a terminal to sample it. |
| `DisplayOutput` wake plus `committed_*_output_markers`: exact visible execution refresh, output-age indicator and retained live tail | `wait` already has `shell_output` for an exact read cursor and `shell.read` can return decoded segments. Establish cursor coverage for the visible set, same-timestamp commits, retained-tail baseline, truncation and terminal edges before replacing the hint. `shell.get.last_output_at` is not a lossless output cursor. Replaced in 4d slice 3 by `events.wait` `output` entries. |

Approved user-visible changes: live preview shows answer text only; saved
reasoning is unchanged. The private DB/GC footer, Help diagnostics and UI-memory
callback are retired rather than exported. Carry these changes into the Core
release notes when the cutover lands.

Unit 4b owns saved attempt settlement and tool-notice projection, after which
the remaining private notice/error subscription is removed. Neither needs a
third event fact in 4a. The frontend delivers saved pages, preview frames and
exact native attempt ends through the existing ordered outbox. A single answer
chunk can display before the first saved read. If a frame is ahead of the
installed saved tail, the UI retains the latest frame for that attempt without
displaying it until a matching saved install; an exact end can retire it first.
No additional read or delta is required. The UI checks the window's
navigation revision, installs the eligible window, and reconciles the preview
against it in the same step. A superseded window cannot change the preview.
When a replacement retires the preview anchor, the installed saved window
supplies the anchor without a second read. Exact attempt ends follow any
in-flight frame and do not require a second attempt-end ledger.


Exact output is the separately reviewed Unit 4a-out, stacked on this unit.
Extend the existing `shell.read` (including the CLI) with a bounded indexed
tail and opaque continuation that does not replay the retained prefix. Keep
`wait`'s `shell_output` condition unchanged. Do not change the lossless read
mode or add an output timer, event fact, or private fallback.

The existing owner-wide saved-change wait remains metadata-only. No broadening
of the live text audience, periodic polling, or compatibility path follows
from this inventory.

### Unit 4e/4f selected window client

The selected current view, history pages, positioned windows, rebases, the
saved-message install and node hydration all read one `history.window` with
`include_observation` and assemble each kept node's display extent from fenced
`history.read` display pages. Layout is client-side. The observation supplies
the mutable tool, notice, recovery and usage facts, committed attempts and the
pending rows; its `read_sequence` orders every install. Pinned tool nodes
outside the current window join it through `observe_node_ids`.

The installed queue is the observation's pending rows (saved sender, reply
duty, path-admission uncertainty, run-completion schedule and the typed
`held_by` hold), this Thread's human duties from `message.list` with
`to=human` and `asked_by_thread_id` (named asks are saved applied), and the
client's own unsettled draft submissions, at most 128 per Thread, settled from
their exact `operation.get` receipts. Tracked receipts use `message.get` and,
when applied, `turn.get`.

A node hydration reads the extent by ID and the one-node window at the same
source revision; a received response, never on a path, takes the tail's
observation for ordering only. Any other node no longer selected is refused
with `cursor_expired`. A saved payload group selects the exact owner call and is
shown only when the owner confirms it. Outline fields carry path shapes for
their labels. A window recovery reason is a 256-character excerpt; when it is
cut short the event points at the F6 source node. Reasoning ranges are clipped
to each display page (recorded gap, not yet witnessed).

Still private and separate from these reads: message metadata, branch and
compaction markers, the received-response snapshot, activity and index pause,
context usage, and the detail panel's current tool executions.

### Unit 4d cutover gaps

`frontend::run` cannot stop owning `Control` until every remaining private
read or wake below goes through the owner protocol. Recorded at the 4d
checkpoint (4a–4g stacked); not a mechanism proposal.

Existing reads cover these (migration only): the branch list
(`thread.branches`), the
in-process command dispatch (`Thread` create/set, the management fallback) over
the selected endpoint,
compaction markers (`compact.list` plus node provenance `attempt_id`), the
detail panel's current executions (`history.window` `observe_node_ids`;
parity of its has-output bit is unchecked), and
the received snapshot (`history.received` with positioned `history.read`).

No protocol read or wake fact exists for these:

- Revisit (run-completion delivery) source of a saved input. Node provenance
  and `message.get` carry sender and reply state but not the delivery reason.
- Branch markers for the visible nodes. `thread.branches` pages the family at
  most 200 Threads per page, so a marker for one visible node can need every page.

Management (slice 4, done): F8 shows the result of the command it ran, read
once from the owner, as the CLI prints it. The in-process substitutions and
the saved-inspection observation (its readers, refresh tickets, retained and
list re-observations, and the Storage physical sample) are deleted; there is
no F8 refresh on saved changes, so F8 no longer consumes the index
saved-change or management owner-state wakes. The index coverage window is
the owner's `index.status` `coverage` opt-in (`IndexStatus.coverage_window`,
`selection` echo and `next_cursor`). F8 Storage shows `storage.status`; the
UI-only history/raw/index byte split, temporary-directory bytes, per-figure
completeness labels and separate sample time are retired (user decision,
2026-11-26).

### Live work and header facts (4d slice 2)

The selected activity takes each running shell's wait from `shell.list`
(`wait`, written by the owner only while its live registration matches the
saved pending record) for the selected Thread, at most 128 running shells in
one page; a page that does not cover them leaves the wait count unknown. The
activity snapshot keeps the waits of the selected Run and replaces each waiting
shell's row with its wait. Only running shells report a wait: a wait whose
shell is starting, stopping or already exited is not shown, and a shell
running several waits shows its earliest accepted one.

The index pause footer uses `index.status` `pause_kind`, the owner's
classification of the saved pause (endpoint family before profile). The
selected view reads it on each activity refresh, an O(embedding jobs) owner
read, until an `events.wait` index change wake lets it read only on change.
Context usage comes from the header's `thread.get` `context_usage`. All
three are owner reads on the read-only inspection lane, so the current view
still does not queue behind the writer.

The rest of the activity (Run, Turn, attempt, recovery counts, reduction and
automatic compaction) is the header's `thread.get` `activity`, so the
current view reads it with the header and no usage aggregation; a selection
or progress refresh reads the display `thread.get` itself. Live work keeps
the owner's counts and lists the Run's running shells from one `shell.list`
page, with `shell.get` for each kept shell row's last output and for a
shell wait target's command. A Run, Turn or Thread wait target is labelled
from the display `thread.get` of the wait's `target_thread_id`. At most ten
rows are kept, so at most ten of these reads. Starting and stopping shells
are counted but not listed. No activity read uses a private snapshot.

### Draft save and selected view

`draft.save` checks the live Thread and editing draft revision, then commits
the draft and view in one transaction. A history append does not invalidate
a save. A follow-tail view needs no selected node; a named reading anchor
must still occur on the selected path. A displaced anchor returns
`cursor_expired` without changing either row. The client keeps input and uses
the bounded history rebase to display a fresh anchor; it does not replay the
refused write. A save of a revision already consumed by an accepted send
returns `operation_conflict` with reason `draft_revision_submitted`; neither
row changes. The client drops that obsolete autosave without treating it as
a shutdown failure. Newer, unconsumed snapshots still use the normal save
fence. `draft.submit` keeps its separate selected-revision fence.
