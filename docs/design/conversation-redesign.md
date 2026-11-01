# Conversation presentation and reactive Thread browser (#41)

Adopted increment plan after #58 main e8c1ff2 and successful three-OS CI41034892332.
This design covers the issue body and comments through6520544888; the complete
implementation ledger is [the delivery plan](../development/issue-41-plan.md).
Only the next complete B+C increment is starting; D/E/F/G remain planned.

## Current issue ownership (2026-10-31)

User decision[6539106715](https://github.com/tya5/shellboy/issues/41#issuecomment-6539106715)
limits #41 to the existing E/F/G main admissions, requirement ledgers and P2–P6
measurements. Later conversation marker/notification/spacing, pasted slash,
footer/seconds, owned-work presentation/configuration and detail/copy/image
convenience additions are owned by #83–#87. They are not new requirements for
this E/F/G admission. Coordination6541457457 allows #85–#87/#90 to start
before F/G; the later-admitted side reconciles overlapping changes. G uses
#87 when it is already on main at G startup. The delivery plan
retains an explicit handoff mapping; earlier original catalogue/forms/reactivity
and immutable source/copy identity contracts below remain applicable.

## Delivery and ownership

#58 must ship alone first. Each later main increment is usable independently, passes the local standard, and has green main CI on all three OSes before another main increment proceeds. Report delivered and remaining requirements after each admission; close #41 only when the complete ledger is satisfied. #35 follows #41. No new public protocol command, lifecycle state, schema, persisted UI state, business event, worker or queue is required. Existing Core facts may gain a narrow bounded in-process observation only where D explicitly requires it.

A is the adopted design commit: this text, effective-spec link, complete requirement ledger and decisions. A changes no product behavior and does not advertise features. B+C is one main-admission unit: conversation structure and working inline interaction must ship together. Internal commits may separate projection/read primitives, render/focus, and tests/guides, but a collapsed row may not ship without a working way to open it. D adds live work; E adds reactive browser rows; F adds watch and descendants. E, F and G are separate main-admission units, in that order, each with its completed requirement ledger and delegated reviewer approval. No unfinished screen/key or successful placeholder implementation enters main.

B+C is intentionally useful without D: existing activity and background controls remain functional while the new fixed live-work region is not yet present. Remove no existing background access until D supplies its replacement. Report the absent live region, background elapsed correction, browser patches/watch and descendant counts as remaining #41 work, not hidden completion claims.

## Common presentation model

Retain the canonical DAG, source offsets, saved tool results, immutable receipt assignment and #58 body/follow/read fences. The UI model is a projection, never a rewritten history.

Use internal identities:

- Message: Node(node_id).
- Tool: Tool(group_id, call_id), not call_id alone, ordinal, display label or execution ID. Pending/cancelled calls may have no execution.
- Live shell: Execution(execution_id); live child: Run(run_id); wait: saved wait operation ID.

Add group_id: Option<String> to ToolPresentation as display-only enrichment, populated by frontend/tools.rs from its already validated SOURCE_SQL correlation. No wire type is changed. For uncorrelated source data, retain a node-local tool key and the existing original/details route; never merge two uncertain calls by repeated #1/call_id labels. Extend the ordinary module rather than creating a general identity service.

A conversation item carries its stable identity, source node IDs, kind, canonical ordinal anchor, saved timestamp, relevant tool facts and expansion availability. A projected tool row merges call/result by the saved key even across source-window boundaries. Nonadjacent source members are not a contiguous ordinal span: suppress only the duplicate tool block, preserve intermediate prose, and keep each emitted row's actual node/source anchor. Saved result facts can enrich call-only rows through bounded exact-source metadata; do not fetch hidden raw output merely to correlate an item. When a result is outside the loaded source window, perform a bounded exact-source lookup; until then the row shows the known call state and an explicit unavailable/loading affordance, never an invented successful result. A call embedded with prose does not hide the surrounding prose.

Ui history focus is Option<ItemKey>; ordinary conversation bodies default open, while successful tool output defaults closed. Enter/Space operates on the selected item's available fold/preview; original/copy access remains independent.  expansion state is a bounded LRU of at most 128 identities per selected conversation, retained across body/layout eviction and cleared when its Thread view is discarded. UI source-window/cache budgets remain those of #58. No entire Turn is pinned. Geometry maps click regions and F6/F7 to the same ItemKey; the mapping is regenerated on layout and width changes. Late replies must match Thread/generation, ItemKey and per-item request revision.

## B+C: bands, rows, interaction and quiet chrome

Add the frontend configuration enum ui.palette = "dark" | "light", default dark, using the existing strict config Ui section and validation/sample/docs path. No runtime model setting or database migration. Palette resolution is immutable for one UI process. A small UiPalette value is passed to render functions rather than mutable global colors. Retain existing OSS Markdown parsing/rendering and paired code surfaces.

Capability resolution happens before the UI loop: NO_COLOR wins and uses plain text plus spacing; known 256/truecolor terminals use the chosen palette; other terminals use reverse/separator fallback. Do not probe terminal background using blocking terminal I/O. NO_COLOR may also need nonchromatic selection cues, so retain markers, labels and borders rather than relying on color.

Input bands have three structural parts: a left source mark, body spans, and
optional right metadata (source label, self-reported sender label, saved HH:MM).
Only the body participates in source anchors, selection/copy and F6 original;
metadata never enters canonical text. Keep existing parent/self/other/Revisit
marks and move their disambiguating labels to metadata. External/unknown input
gets a distinct width-one mark from the shared row table, not `入力 ·`.
The external mark is provisional pending user choice among ⤷ / ⇥ / ⤳; do not
integrate #91 on main until selected. All candidates must pass width_cjk == 1.

Bands cover every span and wrapped row, including code/link spans and padding:
override backgrounds only, preserving foreground/bold/underline/link identity.
Plain mode supplies no band background. Build metadata after wrapping the body;
drop the whole optional metadata group when it cannot fit without sacrificing
body, and never repeat it on continuation rows. Sender labels come only from
existing bounded receipt facts, remain unverified, and keep F6's original sender
inspection. Cache accounting and display projection stamps include that label.
No schema/protocol, DB query/worker/queue, model input or copy API is added.

The fixed header uses a different band or bottom rule from user bands. Title is prominent, activity has symbol/color, and connection/model/cwd are muted right-side fields. Cwd truncates from the beginning. Footer and composer have their own separators. Narrow width drops optional metadata instead of wrapping the fixed chrome unpredictably.

Render a tool as one row: symbol + sanitized command at left; saved exit/duration/known line count at right. Hide normal IDs and #N. Unknown line count is absent. Successful output defaults closed. Failure opens a bounded true-tail preview automatically; cancellation/error without an execution uses the saved structured reason and performs no raw read. An explicit user close overrides automatic failure expansion until that tool identity changes outcome.

Each stream remains visually distinct through existing block styling; no stdout/stderr title is needed. Omission captions use known numerical counts only; do not infer omitted line counts from bytes or a truncated prefix. Preserve exact original/copy/link targets even though the screen is abbreviated.

Group concrete quiet observations at the end of their user-message section. Deduplicate equal concrete facts inside the bounded loaded projection, showing the actual repeat count for that projection (never a whole-Thread total). Each generated row contains at most two distinct facts and at most two representative latest source IDs; all individual saved observations remain reachable through original DAG navigation. Its canonical frontier covers every coalesced observation, including repetitions. Do not emit one row per two repeated events: that turns quiet confirmations into hundreds of scrolling rows. Do not label them state changes/ordinary/other. Preserve important interruption/recovery/error instructions as visible actionable rows. A rendered section can begin before its user node is loaded: never fabricate a user band or move unrelated notices across canonical order.

Keyboard contract: Tab in composer with no completion enters history; arrows choose items; Enter/Space toggles selected inline content; o/c open/copy originals; Esc/Tab returns to composer. Empty-composer Up retains sent-message recall. F6 details and F7 links target the same selection, with a deterministic current/nearest visible fallback if focus has not been established. Clicking a row toggles it only with mouse capture. Item hints appear only in history focus. Explicit detail access remains available for every item, including nodes with no tool output.

Replace overloaded view.status with separate ephemeral OperationNotice {kind:Success|Failure, text} and existing factual activity state. No operation-prepared placeholder. Opening an overlay produces no artificial notice. A concrete success clears on the next input or completion of its command flow; a failure remains until a deliberate subsequent user action. Do not clear a failed send before its restored input can be understood. Footer uses short symbols/counts, ? help and conditional down/new count; remove composer key legends and generic follow/accepted prose. Preserve #52 sending/queued/error visibility with symbols and concrete error text.

## Output-read API, bounds and decoding

Successful closed tools on page load have zero raw-output DB reads. frontend/streams.rs continues metadata/recovery/usage enrichment but raw enrichment is invoked only for failed default previews or explicit expansion. Do not route display reads through ShellOutputReader::read_stream: it writes read pins and reconstructs decoders from byte zero for a cursor, so it cannot establish the required bounded independent read path.

Add narrow connection-based preview helpers in control/output.rs, shared decoder/sanitizer primitives where useful:

    preview_tail(db: &Connection, execution: &str, limits: PreviewLimits) -> PreviewPage
    // D adds incremental reading only when its live-output consumer exists.

These are crate-private in-process APIs, not new CLI/protocol semantics. For B+C, PreviewPage contains bounded stdout/stderr decoded segments, raw-byte/inspected-row counts checked at the frontend boundary, skipped/decode-uncertain facts and terminal/output-complete. Snapshot retention/encoding remain internal read facts. Continuation cursor and saved timing fields are deferred to D, which adds them only together with its concrete incremental caller; existing tool metadata supplies B+C timing. PreviewLimits is fixed call-site data, not another public config family. Query one SQLite read transaction so metadata/raw retention are consistent. Preview reads never override output_encoding or invalidate vectors.

Run the synchronous query/decoding on the existing finite frontend read/presentation blocking path, opening the existing DbWorker path with SQLITE_OPEN_READ_ONLY and query_only. Drop the connection/transaction when the bounded task ends; do not retain snapshots across sleeps. Existing read semaphore owns admission and existing cancellation/generation owns acceptance. No new DB thread or connection pool is introduced.

Query/copy limits bound returned rows and application bytes, not SQLite's internal filesystem I/O for giant BLOB pages. Use existing output_stream(execution_id,stream,stream_offset) to seek each stream's actual tail; use the primary key for sequence continuation. Use substr to copy only the bounded suffix from a large chunk. Bound both raw bytes and inspected/copied chunk rows (at most 128 per preview); short fragmented output may return fewer bytes. Head/tail presentation reads share the existing per-tool byte allowance rather than paying it twice. Failure automatic preview budget is2KiB TOTAL, with a bounded share reserved for stderr when present. Explicit completed expansion uses existing limits.tool_excerpt_bytes (currently16KiB) TOTAL and maintains the true tail when abbreviated.

Use encoding_rs and the existing encoding resolver. Keep separate stream decoder state for contiguous incremental reads in frontend memory. A tail jump resets decoder state; UTF8/UTF16 boundary alignment can be repaired with a small lookbehind charged to the same budget. Stateful encodings cannot promise exact arbitrary-tail decoding: display a fixed decode-uncertain fact and retain original access rather than scan from the start or claim exact output. Trim ANSI/OSC/cursor/control/invisible-format characters with existing sanitization. A preview-only bounded current-line normalizer handles carriage-return overwrite before sanitization (the global sanitizer only strips CR); do not change CLI raw semantics. Cap decoded preview text by the existing excerpt/body budget.

## D: owned live work and accurate output activity

Extend frontend/activity.rs with LiveWorkSnapshot captured in the same existing refresh as activity: current Run ID, observed wall/monotonic time, total counts and a configured number of oldest rows. The validated `ui.live_work_rows` range is 1 through 10 (default 3); the same bound applies to each SQL candidate lane, the final merge, displayed rows, and output-marker requests. Values outside the range are errors rather than silently clamped. The section has a blank separator, a dedicated-colour `◉ 抱えている作業` heading and indented `│` rows; every row names its state. Small screens retain the heading and indicate hidden rows. Wait rows replace their own caller-shell rows, but shell counts remain factual. Their target is derived from the first bounded saved condition, with an additional-condition count, and timeout is an approximate saved wall-clock observation (zero means immediate timeout). Unknown or malformed evidence remains unknown. A shared target identity marks visible matching rows; no OS probes or new persistence are introduced. Row enum Shell/ChildRun/Wait carries IDs, sanitized display text, saved start time, state, elapsed base and last_output_at where known. Shell commands come from saved shell_json, child titles/classification from the existing Thread projection. Wait rows reflect saved pending waits and do not count as extra child work. The query orders by saved start and ID, uses existing owner indexes and caps returned rows; counts are scalar queries. Completed work disappears on the next Changed refresh. The fixed region sits immediately above composer and remains after the Turn answer when the Run still owns work. Small screens collapse overflow before suppressing composer/history usability.

Background footer derives oldest-work elapsed and last real output from this same snapshot, never a stopped Turn observation clock. Local Instant advances saved elapsed without DB reads; switching away/back reloads saved timestamps. Unknown saved timing remains unknown. No animation tick performs DB work.

Precise per-shell output activity needs a narrow ephemeral extension to the existing LiveRegistry shell entry: Option<Instant> last_committed_output (or equivalent scalar). collect_shell updates that entry only after an output-batch commit, before the existing global shell_output_changed notification. The entry is installed before collection and removed with its existing handle, so storage is bounded by already-live shells and has the same owner/cleanup. A crate-private getter accepts at most the configured displayed execution IDs and copies those scalars under the existing async registry lock. It performs no I/O/DB read and creates no event/queue. Frontend listens to the existing Notify, looks up only displayed live executions, and coalesces repaint in the existing cadence. Never label another shell's output as activity of the selected shell. Initial/past output age is supplied by #45 saved last_output_at in the normal activity snapshot.

Only focused/open live shell rows read output. Reuse the frontend select loop with one pending deadline, not a spawned poller. On output notification mark the focused execution dirty; at most once/500ms consume at most4KiB total across its streams. If behind, use tail seek; otherwise use the retained decoder/cursor. One request in flight, cancellation on close/completion/Thread switch and identity/revision checks before delivery. No raw output bytes travel as EngineEvent or per-byte UiUpdate; only bounded prepared previews use existing update delivery. Closed live rows have zero output-driven DB reads.

Existing management handles shell detail/read, child navigation and wait inspection. Preserve full Run-stop and human Turn-interrupt distinction. A live work row never implies a tool was cancelled/stopped when only its foreground wait ended.

## E: reactive Thread rows

Extract a shared internal saved-summary projection from control/db.rs THREAD_LIST_SQL and row decoding; expose a bounded ID-restricted connection helper, not a new Command. Reuse activity::classify. Avoid calling ThreadGet or scanning a page for one Changed ID. Fetch excerpt/last-answer preview only when the target mode needs it; simple list patches do not load conversation bodies.

Frontend owns dirty Thread IDs, a fixed128-ID batch cap, existing read-slot admission and one500ms coalescing deadline. Only while browser visible are summary patches scheduled. A Changed ID absent from loaded rows is still read once, allowing matching new Threads to be inserted. Missing/deleted or nonmatching rows are removed. Dirty overflow sets a bounded resync-needed state rather than an unbounded set; use the existing lost-notification/full-refresh recovery only when exact IDs were lost and record that exceptional reason.

Patch DTO: BrowserPatch {browser_generation, read_sequence, rows: Vec<ThreadSummary>, missing_ids, optional_watch_details}. Each row has frontend request sequencing so an old patch cannot undo a newer patch, and an older full page cannot resurrect a deleted row. Full reload invalidates prior patch generation. Opening/filter/Ctrl+R reload pages; subsequent pagination stays bounded with ID deduplication and the existing cursor/watermark. Maintain newly inserted rows as a bounded overlay around loaded pages, not an excuse to fetch all history. Cap retained browser rows by the existing paging/view budget and refetch via explicit paging if a pruned row is needed.

ThreadBrowser applies changed rows only, then locally rebuilds the existing tree. Preserve collapsed IDs/filter/selected Thread ID and selected screen offset. An updated row moves to its existing updated_at/ID order; unchanged siblings retain their prior relative order. The screen position of the selected Thread is the anchor. Row removal chooses the nearest surviving visible row. Ctrl+R remains explicit and available.

State styles are shared with activity/watch/descendant markers: active LightCyan+bold, completed subdued Green/default, waiting/background/retry LightYellow, recovery/failure LightRed. Preserve state labels/symbols with selection and NO_COLOR.

## F: watch and descendants

ThreadBrowser has enum BrowserMode {List,Watch}; do not expose independent combinations of row height/state filter/order. Tab switches modes in the browser without consuming filter input; header names the mode. Retain selected Thread if present, otherwise choose first. /threads opens List; /threads active opens Watch; /threads descendants opens Watch scoped to selected Thread lineage. Add one conversation shortcut to descendant Watch (proposed Alt+D, with final collision test before adoption), and retain the previous mode only for this UI process.

List remains the existing tree and deletion surface. Watch is flat, two lines unless narrow: recovery, failed, unread completion, active, background. Include parent title. First line contains state/title/state age; second has active shell command+output age, model-request age, background counts/oldest work, recovery reason+hint, or last answer first line. All external strings are sanitized and width-bounded. Pending messages and Revisit counts are appended only when nonzero. Read details only for visible watch rows and reuse D snapshots/projections; no hidden-page per-row activity reads.

Unseen completion is keyed by latest completed Run/answer identity in session-only UI memory, not updated_at (settings do not create unread answers). Opening that conversation marks its completion seen. Read rows already visible in the current watch session remain until browser close; reopening removes idle/read rows. Never expire rows by time. Category transitions trigger priority reordering; local clock ticks and new output ages do not. Within active category, establish stale-output ordering at entry/category insertion, then retain stable order until category transition.

Watch actions: Enter opens, confirmed stop uses existing RunStop, retry uses existing RunRetry and saved recovery state, output opens existing shell management. No composer/send inside watch; deletion only List. Revalidate exact target Run/generation before a late confirmation can stop a replacement Run.

Descendants use parent_thread_id, not owner_run_id. Traverse breadth-first by parent index with explicit depth8 and discovered256 caps plus cycle deduplication; count live descendants using each latest Run and activity::classify. Recovery count is separate; hide zeros. Set truncated when depth/row frontier remains and render a lower-bound sign, never an exact total. Deleted nodes are not counted; traversal policy must still reach retained live grandchildren through a deleted intermediate if supported by current Thread deletion semantics. Changed IDs trigger a bounded ancestor-membership check so a newly created descendant outside the prior discovered set invalidates the parent count. Reuse the same coalescing deadline; no full-tree scan per unrelated Changed and no timer polls. Descendant Watch uses this same bounded scope, clearly showing truncation.

## Validation and files

Main implementation areas: config.rs/config validation + theme; presentation.rs/frontend/tools.rs; ui/presentation.rs + ui view/focus/layout; frontend/streams.rs + connection-based output preview; frontend/activity.rs/live region; control/live.rs and driver/shell_output.rs narrow ephemeral output marker only; shared control/db.rs summary projection; ui/threads.rs/browser frontend module. Keep ordinary Rust modules and meaningful stage commits.

Validate B+C stable identities across duplicate call IDs in different groups, node/page/cache boundaries, failure without execution, every focus/click/F6/F7/original/copy path, all body kinds, #52 pending sends, tiny body budget and #58 follow/stale/paid-text regressions. Count raw reads: closed success0; failed tail <=2KiB; explicit expansion <=existing limit; >8KiB fixtures contain the real final marker. Validate encoding boundaries, stdout/stderr, ANSI/OSC/CR sanitization and deleted/retained output.

D tests count output DB reads0 while closed, <=2/sec and<=4KiB while open, cancellation/stale reply on close/end/switch, correct notification attribution among hidden/displayed shells, elapsed persistence across Turn completion and switching, child/wait/overflow rows. Run sustained output and compare render/input latency with baseline; do not claim source-byte/queue limits alone prove responsiveness.

E/F tests assert one changed row queries only its IDs, bounded batch coalescing, additions/deletions/filter changes, reordered replies, stable tree/selection/page overlays, state-category ordering, session unread retention, confirmed action target CAS, narrow row reduction, queue/Revisit hints, unowned descendant Runs, stopped/deleted/depth/cycle/overflow cases, and update on newly introduced descendants. Local clock ticks cause no queries/reorders.

Before each main admission: meaningful focused tests, standard check, secrecy scan as appropriate, updated status/acceptance/decisions/guides, then all3OS main CI. Actual PTY captures80/wide, palettes/fallback/NO_COLOR, long conversation and sustained output accompany applicable stages. Physical Windows terminal UX is separate from native Windows CI and must be recorded unverified unless observed. Update English TUI/Agent guides and Japanese User Guide for each delivered stage, plus help/quoted-label tests. Do not migrate all product language under this issue.


## G: operation catalogue, forms and popup scrolling

This independent increment includes comments6519624389,6519666213 and6519745314.
Use one ordinary Rust catalogue for operation identity, complete one-line meaning,
effect, CLI path, typed text keys, fields, default-value origin/discovery and
existing confirmation policy. UI lists/forms/confirmation/results derive their
labels and operation meaning from that definition. Do not create a generic form
DSL, dynamic command interpreter or new Core dispatch layer.

Effects distinguish read-only inspection, saved-data changes and runtime control;
message sending alone uses the send verb. Enter says display for inspection and
execute for changes/control, send only for message send. The existing mutation
confirmation decision comes from the catalogue effect/policy, with exact target
preview guards retained. Read-only RunGet must plainly describe native state,
current Turn, ownership and saved recovery reason without implying a mutation.

Define Run/Turn terminology and complete messages centrally so #35 can apply a
typed localization boundary later. Do not assemble translated fragments. Field
help identifies its ID type, actual default source (current Thread/Run etc.) and
where an empty value can be discovered. The list explains that it runs the same
operations as the CLI and shows the selected CLI syntax; the form shows that
operation's explanation/effect, replacing generic prepared/request wording.
F1 inside an operation form opens that operation's field/ID explanation and
Esc/F1 returns to the unchanged form. General help may layer above the operation
list and other popups, retaining #87's escape route; its global handler must not
intercept the form-specific explanation promised by the form footer.
The shared bottom hint uses the same catalogue EnterVerb and dynamic effect as
the form, with that typed verb in its existing cache key. An open explanation
advertises help navigation, not field editing/submission. Keep form-owned F1
dispatch distinct from hint classification, so closing an explanation remains
possible while a client confirmation still retains layered general help.

The latest main adds headless process lifecycle commands serve/shutdown (#5).
These are launch/termination controls for a separate terminal-free owner, not
operations executable inside the current TUI: Core explicitly rejects `owner.shutdown` through its TUI adapter. Keep both CLI paths in a separate, explicit
lifecycle inventory with their global options, beside the launch/help/version
boundary; do not invent a successful F8 form or bypass that Core admission rule.
The UI operation catalogue remains complete for its applicable commands. Command
caption lookup returns an explicit no-form classification for OwnerShutdown;
future Command variants remain exhaustively matched. Review this choice if main
adds a supported TUI lifecycle operation or an explicit other-owner launcher.

Prove a complete mapping between catalogue operations and executable CLI paths,
including standalone docs/RAG, attach shorthand, and executable parents with
optional subcommands (compact itself as well as compact list). Enumerate these
from the current clap command tree; parser help/version and UI launch are not
operations. Do not hide missing commands by testing only the
old OpKind.ALL inventory. Introduce no successful stub for an unsupported form.

Standalone forms call existing docs/config/RAG/exec clients on the existing
bounded command admission and workers. They never parse CLI strings or read
process-global stdin. The CLI RAG wrapper alone owns stdin; the shared RAG
entrypoint accepts owned UTF-8 text or an explicit file. Keep its16MiB input
limit, form1MiB total budget, provenance/hash validation and explicit confirmation.
No new Core command, protocol action, worker, queue, table or job engine is added.
Immutable client results identify their completion time and explicit repeat route;
this does not exempt mutable saved owner inspections from G's reactive inventory.

Section headings are separate nonselectable rows, visually distinct from actions.
Selection, page movement and N/M count use actions only and never stop on a header.
Rename the residual category to Other operations; hide headings while filtering.

The top popup owns wheel input only while the pointer is over its current painted
bounds. Dispatch to that popup's existing row/reader scrolling; never move the
underlying conversation. Outside hits, edges and non-scrollable content are
no-ops and do not redraw. F2 terminal-selection mode ignores mouse events. Geometry
must match the current popup identity/epoch so a stale rectangle cannot operate
another overlay. Reuse the existing popup layout framework rather than adding
individual wheel exceptions or a new screen.

Validate every operation's effect/summary/Enter verb against the catalogue and
its CLI mapping; empty-ID help, RunGet read-only clarity, header nonselection,
filtered/narrow lists, and every popup's wheel/edge/outside/F2 behavior. Record
actual PTY forms/screens separately from TestBackend. G can contain reviewable
subcommits but must enter main as a complete usable independent increment.

### G saved inspections: domain ownership and complete coverage

The retained mutable-fact inventory below governs extraction of the archived
inspector. Its twelve initial target variants are not a complete coverage proof.
In particular, audit activity status, history/context/availability and the current
shell/compaction readback lists as well as the original Run/Turn/index targets.
Record per-field source, committed notification, finite readonly read and failure
behavior before claiming each route complete. A followed reference is not by
itself a reason to freeze mutable facts. Edited form inputs intentionally remain
captured; the read result and downstream mutable work remain separate.
The form's prefilled ID is the typed request target. ManagementContext identifies
where a reply belongs; it is not an implicit Thread/Run authority. Clearing a
required human target must retain canonical CLI validation, not silently restore
the viewing Thread. In particular, human Revisit get without --run remains an
error rather than following a changing current Run; attachment source references
keep their explicitly resolved foreign Thread and selection identity.

| Result class | Required treatment |
| --- | --- |
| Exact saved Thread/Run/Turn/message/operation/Revisit/shell/attachment facts | Relevant committed changes update the visible exact target; preserve source/selection and reject older generation/sequence results. Terminal receipt evidence stays distinct from downstream current work. |
| Activity status and saved readback/history lists | Audit actual mutable row/context fields and page membership; use bounded saved reads or an explicit source/time-scoped page contract, never a blanket all-F8 snapshot exception. Preserve immutable originals while mutable context/availability changes. |
| History originals and shell-output pages | Already obtained bytes retain source/hash/page/encoding/read time. Explicit next-page/read cursors do not advance automatically. Mutable lifecycle, current selection and availability need their own current/stale indication. |
| Usage and context totals | Relevant changes trigger the existing independent3s cadence; lightweight facts remain on500ms shared admission. No periodic heavy reread during quiet work. |
| Native memory, remote model catalog, standalone docs/config/RAG/exec results | These have explicit on-demand/external-source scope and observed/completion time plus refresh/repeat action. Do not start hidden OS/API/file polling or automatically repeat costly/mutating clients. This exception does not apply to saved owner lifecycle fields. |
| Deletion/index cost confirmations | Retain chosen target/source/time and existing stronger execution guards; changed facts cannot silently authorize a stale destructive/cost preview. |

An index cost preview refresh that fails retains the last successful result for
the same index scope as explicitly stale evidence, alongside the new failure.
It is not a valid confirmation: only a new successful preview can authorize a
costly operation. A changed source invalidates pending and completed previews;
scope changes and closing the management view discard the retained result. No
background preview, owner protocol, or Core persistence is added.

#### Remaining saved pages: captured evidence and current facts

History list/get/relations/search, shell output pages and completed waits retain
their obtained bytes, result time, original filters, IDs, hashes, encoding and
cursor. A live update must never replace those bytes, advance that cursor or
silently rerun a search embedding/wait. Extend the existing inspection scope
with a separately labeled current saved-source observation. Keep the retained
result through its existing bounded Arc/detail retention; do not copy it into
the UI target or add a second body cache. Explicit repeat/reload produces a new
result identity. Current facts have their own read sequence/time and error state.

History's private observation belongs beside canonical history semantics:
current selected-path membership, body revision/availability, tombstone reason
and captured Thread liveness/selection revision are domain facts. Read only
the returned page's exact IDs (at most200), using node/path primary-key lookups;
no payload/raw-output bytes enter this metadata read. Bound scalar copies before
String materialization (IDs512 bytes, kind64, hash128, reasons/timestamps512)
and the aggregate metadata channel to256KiB. A limit or missing row is explicit
unknown/unavailable, never an empty body or evidence of deletion by itself.
When GC removes a payload or a rewind changes the selected path, the original
result remains inspectable while these current facts change. Bind selection to
the result's captured Thread, not whichever Thread happens to be visible later.
Shell pages likewise retain their original retention generation/sequence and
observe current execution/retention facts without another raw-output read.


Retained history integration uses the existing management detail Arc for original
JSON/body pages and one separately bounded, last-presented inspection metadata
Arc for current facts. A later read cannot change which metadata a displayed
request ID resolves; publish the new metadata Arc with that exact presentation.
Current field pointers live under `/current_source`; detail/follow resolution
selects that metadata Arc without serializing or copying the original body again.
A retained inspection does not clear the management detail on each read. The UI
may retain at most256 original field descriptors (not body JSON) and combines a
bounded current prefix with them under the existing256-row field limit; omissions
remain explicit and original detail retrieval remains available. The current
snapshot's request identity still fences old detail replies and hide/cancel. Completed
original-detail pages retain their text, cursor and captured time across current
metadata updates; rebind their unchanged original source to the new display
request identity instead of labeling them stale or refetching them. Pending
original-detail reads still receive a fresh request fence when needed.

HistoryGet has no Thread selection argument: do not infer selection from the
viewer, node creation Thread or delivery owner. Show selection scope unknown
while observing exact node revision/body availability. HistoryList uses its
literal captured Thread, never a later viewer selection; a captured selection
revision unavailable in the wire result stays unknown, not invented. A page
revision match is not a guarantee of raw-file availability. Metadata captures
use one exact Thread query when scoped and one indexed batch for at most200
node IDs; input JSON is capped128KiB, scalar copies and metadata are capped
before materialization. Core's private history reader owns these path/tombstone
semantics because canonical payload readers materialize the original body and
cannot serve a body-free notification refresh. No new command/table/worker/queue.

Saved lists retain the chosen page/window and selected row identity. Reuse F's
Thread row reader for generic F8 ThreadList instead of creating a second summary
projection. Shell/compact rows and activity status use their native domain
identities and filter meanings; do not infer a common lifecycle. Within the
captured window, update mutable row facts and label rows which no longer match.
New results outside the retained watermark cause an explicit newer-results
indication; they do not advance a user's cursor. A changed selected-path revision
marks a history cursor noncurrent while preserving the obtained page. Explicit
refresh restarts from the captured filters without a cursor. The initial
canonical status scan is not a permanent periodic read: factor bounded native
candidate/point reads in its domain before G9 admission and record exact query,
source-copy and incomplete-result budgets. No full-owner scan or repeated
Control request is justified merely by moving it onto a readonly connection.

These targets use the same visible-scope dirty admission, shared snapshot,
generation/sequence fences and cancellation as the initial twelve groups.
Committed relevant Thread changes and existing GC availability hints invalidate
current facts; hiding all consumers admits zero work. Do not add a generic
observation engine, persisted watch table, public Core command, worker or queue.
Qualify retained-original preservation and actual CLI mutation/replay/rollback
behavior for each route; a grouped enum/test is not proof for omitted commands.

Inspection visibility has a UI-local monotonic revision, separate from saved
read sequence and request generation. One scalar hide revision reuses the
existing priority control mailbox, so hiding a popup retires observation even
when ordinary input admission is full. The frontend retains one latest desired
visibility scope/revision across target installation and rejects older queued
show actions. Reopening/manual refresh uses a newer revision; unchanged frames
do not emit observations. This adds no Core event, worker, timer or persistence.
The mailbox slot holds only one u64, is coalesced and drained/dropped with the
existing ActionReceiver. Initial explicit read outcomes retain existing input
acknowledgment semantics; subsequent observation work is cancelled on hide.
Initial scoped Run/Turn/Thread metadata and usage share one snapshot; following
usage observations keep the independent3s dirty cadence.

Attachment removal returns its existing active-selection change bit and uses
the existing committed changed_threads delivery only when that bit is true.
Duplicate removal/replay stays quiet; no extra selection query or new event is
needed. Runtime and ordered-edit reservations wake the existing owner completion
hint after a new pending receipt has committed, before waiting on external I/O.
Saved/pending replay branches do not publish a new reservation transition. These
hints expose pending facts without changing side-effect/reply-grace ownership.
Managed-file GC reuses its existing availability signal at each committed
ready/failed-to-deleting and deleting-to-deleted transition, including when
later unlink/storage work fails. No-op candidates stay quiet; metadata refresh
reads saved availability only and never probes external files.


The live saved-index consumer has one private coalesced Notify permit owned by
IndexService, independent of the existing indexing-worker wake. Publish only
after committed saved changes; read-only status/replay/rollback do not publish.
It has no payload, queue, worker or persistence, and follows IndexService/owner
shutdown lifetime. Frontend visibility gates waiting and dirty admission. The
existing owner completion hint and GC availability notification are reused;
neither is an authorization or proof that a mutation occurred. Exact reads and
sequence/generation checks still determine adopted facts.

Frontend owns visibility, cadence, cancellation, generation/sequence adoption,
retention and presentation. Reads join the existing finite presentation pool and
readonly WAL snapshot; no I/O, DB work, queue wait or join enters the UI OS thread.
A hidden/closed consumer causes no read. Failure keeps identified old facts stale
or marks unavailable; it never invents idle/zero/completed values.

The first saved-metadata extraction covers shell get, message get and explicit-Run
Revisit get. Only these three typed commands enter this reader. Identifiers are
validated before SQL (at most4096 bytes); point-read scalar cells are bounded to
64 bytes for native enums and512 bytes for other saved scalars, and JSON source
copy is at most64KiB (sender plus assignment share that budget). Required identity
overflow is an explicit error; optional overflow carries field-limit witnesses.
Decode runs after the readonly snapshot on finite presentation work. Revisit's
canonical saved-state decoder/view remains in its existing domain module; no
second business-state interpretation or authorization policy is introduced.
Initial explicit F8 reads use the existing admission/pool and report their read
scope/time. This intermediate route is not completion of live target subscriptions
or the final joint current/browser/inspector snapshot; those remain mandatory.

Saved Thread context usage shares one private compaction reader and formatter
with canonical context_metrics: latest completed attempt by saved rowid for the
current profile/model, unchanged replacement-time comparison, and unknown usage
remaining null. Read only the five normalized usage scalars, never copy the raw
usage JSON. Return the captured Thread/profile/model beside the prepared value
so a later3s observation cannot appear current under changed settings. A caller-
owned readonly snapshot may impose512-byte saved scalar cells before String
materialization; canonical CLI has no new scalar cap. An oversized display cell
makes the observation unavailable, never a fabricated empty identity or zero.
These initial shared helpers do not themselves connect live subscriptions or
prove constant query work; qualify latest-attempt/replacement SQL in G9. Keep the
existing latest-order/eligibility semantics, without a new schema or cache table.

Thread's latest input/answer reader owns its canonical rowid ordering, natural
text selection and sender whitelist in thread_observation. Factor its raw read
and preparation so display snapshots do not decode sender JSON, sanitize text
or construct public JSON while holding the readonly transaction. CLI wrappers
keep their existing unlimited scalar/source contract and512-character excerpt.
Display reads cap scalar cells at512 bytes before String copying and share a
64KiB source allowance across the two node payloads and sender JSON. A payload
which exceeds the remaining allowance is not JSON-walked; retain its saved body
availability but show excerpt/sender as unknown with exact limited-field
witnesses, not an empty known value. Two indexed point reads and at most two
exact-node excerpt queries use the caller's snapshot. This private domain model
shares semantics with canonical callers; no table, worker, queue or public API.
Last-compaction projection and overall Thread/header SQL bounds remain separate
G9 obligations rather than being hidden by these two bounded message facts.
For last-compaction, split the existing readback reader into saved row capture
and pure preparation. Reuse its receipt/automatic/replacement queries and spill
coverage semantics; metadata collection stays in the caller's readonly snapshot,
while JSON metric decoding, event merging, ordering and public result formatting
run afterward. Canonical CLI list remains a wrapper over those same two phases.
Derive spill-count admission in the existing SQL projection with the same
nonnegative-integer semantics, not an additional per-row query or a second
business-state decoder. Candidate caps alone do not prove bounded source copies
or total query work: G9 must still qualify/limit those queries before admission.

Thread's latest-compaction display uses the same reader with limit1 (at most5
receipt/automatic/replacement candidates each and25 spill IDs, at most9 SQL
statements). Its dedicated readonly connection temporarily applies SQLite's
256KiB string/BLOB/row limit and a2500-callback allowance at approximately100 VM
instructions per callback, including preparation. The callback count is the
resource gate, not a wall-clock or an exact VM/time promise. Scalar result cells
are512 bytes and SQL-to-Rust copied text shares256KiB, checked before String copying.
Restore connection limits/remove the callback on every exit; no hook is currently
installed by the owning snapshot helper. Future snapshot hooks must be composed
at that owner boundary, never silently overwritten.

An owned size/work limit makes only last_compaction unknown with an explicit
limited-field witness; current Thread lifecycle/configuration still updates.
It is not evidence that no compaction exists, nor a successful stub. Other SQL
failures retain their actual error path. CLI full readback does not install this
display envelope. Tests must prove small-result parity, owned interruption and
oversize behavior, unchanged stored source, restored limits/callback and a usable
same transaction after either limit. Explain/index and broader G9 checks remain
separate from this envelope. No table, worker, queue, timer or public API is added.

The next extraction covers saved Run/Turn metadata. Their initial explicit reads
must retain canonical saved usage totals in the same readonly transaction; normal
reactive refresh later separates metadata500ms and usage3s. Run metadata copies
at most128 owned Run IDs and128 active execution IDs, with per-state indexed
prefixes of129 before sorting. Recovery reason and Revisit share64KiB. Saved
scalar cells are bounded to512 bytes before materialization; an oversized scalar
makes this projection explicitly unavailable rather than silently changing an
identity, timestamp or lifecycle fact. This initial extraction adds no schema/index.
On main805efc9, Run details also contain #102's saved context-boundary wait
reason. Reuse the canonical readback helper in the same readonly snapshot;
do not silently omit it in the F8 projection. This adds one protected-key query
and, only when that witness exists, one exact operation-receipt query (Run
metadata total4 or5, Turn metadata1). G9 must also qualify the helper's copied
scalars/source work; the extra calls are not permission for repeated whole-owner
requests or a new observer/worker.
The canonical latest-Turn ordering uses(created_at,id); the existing Run/time
index does not cover the ID tie-breaker. G9 must measure tied timestamps and
qualify bounded lookup or extend the existing index before G admission. Initial
finite result copies alone do not establish bounded total SQLite work.

For that G9 qualification, prefer replacing the existing turns_by_run index with
(run_id,created_at,id), keeping its identity and current canonical ordering, over
adding a redundant parallel index. A local SQLite diagnostic found a temporary
sort and work proportional to the equal-time group with the current two-column
index; adding id removes that sort. This is design/preflight evidence only, not
qualification on bundled SQLite or a shipped schema change. Prove the complete
Run query, scalar bounds, fresh/upgrade schema equivalence and atomic index/
ledger/user_version rollback before adoption. Recheck main's migration slot just
before coding/admission; current main is17. Document a closed-owner backup of
the complete data directory before any schema upgrade; never run the validation
on user data or automatically migrate their installed binary.

Operation inspection uses the exact operation-receipt primary key, never an ID
prefix or the viewing Thread as authority. A receipt outcome may legally reach
8MiB; inspect its length before copying or invoking SQLite JSON functions. The
outcome and compact-result/pending/inflight JSON witnesses share a64KiB copy
budget; oversized witnesses leave explicit unavailable facts, not empty targets
or guessed queue state. Scalar identity/time/state witnesses are capped512 bytes.
Pending sends can resolve their actual Thread through the unique message receipt
operation key; finalized sends resolve current downstream delivery independently
of their immutable saved acceptance. Canonical outcome expiry/replay policy and
receipt envelope preparation stay in control::receipt and are reused by both
CLI and display. No observer can replay the operation. The first exact read is
not itself completion of commit notifications or live subscriptions.

General management results reserve common identity/state/scope/coverage/limit/read-
time fields before admitting expansive array fields. Reuse the existing bounded
pointer/preview collector for selected metadata roots and the remaining source;
deduplicate exact pointers under the same128-field/64KiB output budget. Exact
metadata pointers can reserve blocker counts without expanding their large Run
list first; permission/snapshot/source-reference fields remain visible. Do not
clone the raw body to reorder it, raise the field cap, or add per-list geometry
exceptions. Full retained detail remains separately budgeted and addressable.
The common result ordering must keep primary lifecycle/pause and destructive
permission facts before expandable coverage/count/definition sections. Admission
priority alone is insufficient if a later lexical sort pushes those facts below
the initial viewport. Keep exact JSON pointers and the retained full detail;
do not duplicate pause aliases or weaken the real-screen refresh assertion.

Saved attachment inspection reads selection/source/delivery/managed-file facts,
not current external-file contents or filesystem availability. A list copies at
most128 rows across indexed active/excluded prefixes, with a body-free overflow
sentinel and explicit lower-bound/global-order coverage. Each path/reason copy
is at most512 bytes; IDs/kinds are bounded separately. Native file availability
remains an explicit CLI observation, not a periodically sampled saved fact.
Canonical managed-file unavailability takes precedence over selection exclusion
reasons. Share that pure domain precedence with CLI; an unread oversized reason
is explicitly limited, not replaced with an invented missing-reason default.
A followed attachment uses the exact (Thread,attachment) selection primary key,
independent of list membership or the128-row cohort. Its count describes only
that exact selection; a missing saved selection never means the entire Thread
has no attachments. Resolve Thread from the selected record itself, never the
viewer/current Thread. Source-node references continue to the original node.

The saved storage view uses existing category/retention expressions beside GC,
with at most1024 scalar rows per category index,128 execution candidates and1024
output witnesses in total. It reports per-value completeness/lower-bound reasons;
no raw payload or output bytes are copied. SQLite reusable-page facts are saved
DB observations, not filesystem samples. Physical sampling is separately owned
by the existing GC file worker, with finite directory entry budgets and the
configured file-preparation deadline; no new worker/queue/timer is added. An
initial explicit storage read may request one sample. Later reactive metadata
reads must not resample physical storage; visible physical changes are separately
coalesced and sampled no more than once per3s. TUI capacity fields use byte observations such as /logical/bytes plus /logical/
complete, rather than disguising an incomplete prefix as an exact scalar total.
Canonical exhaustive CLI StorageStatus scalar fields, storage status/collection
and mutation eligibility remain unchanged. StorageStatus's shared category input
remains non-filtering, matching the canonical status route; collection owns that
category selector. Core placement
keeps private retention/pin/quota/path/file-worker policy in its existing owner;
frontend owns presentation, visibility and observation lifetime.

The saved deletion view examines at most256 physical descendant candidates plus
a sentinel (and the root), depth8, and1024 combined reference/replacement-owner
witnesses. Deleted descendants/foreign memberships and noncompleted replacements
consume admission before exclusion; they cannot create an unbounded negative
lookup. Scope/reference incompleteness cannot prove negative counts or permission;
unproven values remain unknown, while a witnessed blocker still denies deletion.
Observed lower bounds stay separate. Deletion read scalars are capped
512 bytes before Rust string materialization; invalid/oversized cells fail the
observation rather than creating a partial authorization. Exact CLI preview and
the dedicated destructive confirmation/revision checks remain unchanged. Core
placement preserves private path/replacement/attachment relationships and canonical
scope semantics; frontend owns read lifetime and presentation, never permission.

Saved index inspection stays beside private profile/space/pause/dimension keys
and existing coverage classification. Metadata admission is finite:200 spaces,
1024 work/job/chunk prefixes,128KiB per metadata channel with guarded value cells,
512-byte scalar cells and8MiB aggregate scalar channels. Coverage reads32 source
memberships per window,128KiB per source/512KiB total body copies, then decodes
after the transaction. Cursor preparation binds normalized explicit scope,
profile and selected space; never silently rebind or sum successive windows as
global current totals. Disabled auto_history holds automatic pending jobs; no
read authorizes them, probes a provider, decodes shell raw output or enqueues work.

The TUI keeps its management/spaces, management/coverage and management/definitions
sections. Coverage counts describe the current finite window; adjacent window
metadata explicitly states scope/exhaustion/currentness. Unverified window
counts are null, with observed lower bounds separately retained. Canonical CLI
status/coverage and shared classification/definitions remain unchanged. Metadata
and coverage use one readonly WAL snapshot and existing presentation work; no
schema/index/worker/queue or public Core command is added by this extraction.

Private domain helpers are permitted only where they avoid duplicating canonical
schema/semantics: index saved views know private space/pause/failure keys and must
not call the heavyweight full-status/provider path; GC observations know category
ownership and retention eligibility; deletion observations know descendants,
shared/source references and live-work protections. Keep bounded raw SQL/scalar
facts and canonical validation beside those domain modules. They do not own UI
labels, refresh state, caches or a second execution policy. Document numeric
row/source/channel limits and query-count/index evidence when adopting each
helper; explain why reuse of an existing lighter helper is insufficient.
Physical storage observation uses the existing bounded file work separately from
saved metadata. A preview is information, not deletion permission.

No generic Repository, new Core operation, persistent table, worker, queue or job
engine is needed. If existing committed notifications miss Index/GC writers, use
only a justified bounded in-process hint after commit, recording ownership,
coalescing/loss and shutdown; do not consume a worker's own wake permit. Recheck
actual indexes and then-current schema before an index-only migration. F is17;
a possible next slot is not reserved, and pre-upgrade backup/atomic ledger and
rollback/reopen checks remain required.

### Indexed scope is distinct from constant own-cardinality work

Authoritative count policy6543044236 bounds reread targets, indexed scope,
statements/request, copies/cache and frames; it does not require constant SQLite
VM work as the target's own lifetime grows. Exact indexed child/shell count(*)
and canonical latest-context selection may remain O(matching owned rows), with
native growth observations and no whole-owner/global scan. Do not add counters,
denormalization, unknown results or a migration solely to flatten that cost.
Keep specifically adopted numeric caps (including depth8/256 descendants and
128 owned-ID prefixes) unchanged. Tests must not call bounded returned data or
query counts a proof of constant total SQL VM/memory. An index's ID tie-breaker
extension is optional when the current indexed-scope gate already passes.

Header scalar capture gates each cell before copying: IDs/timestamps512 bytes,
CLI-compatible explicit title2048, profile/model4096, effort64, and cwd/agents
paths128KiB each. A field-specific InputLimit refuses the whole unsafe header;
no truncated path/identity becomes a command default. Recovery64KiB and shared
title witnesses512KiB remain independently bounded. The path allowance is a
conservative UTF-8 display envelope for extended paths, not Windows qualification.
Exact indexed owned-work counts stay factual rather than being capped to zero.

## Cross-surface reactivity and performance admission (comments6520522032/6520544888)

Delivery ownership update2026-11-01: the user moves unfinished saved lists/status
to #107, remaining retained references/pages/waits to #108, and confirmation plus
external/native snapshot labels to #109, outside #41. Their product requirements
below remain adopted. G retains existing inspectors and HistoryGet/List plus its
own reader/SQL qualification. Worker-owned files/functions and count gates are
listed in those issues. Before G lands, workers do not modify active G files.
After G main, reviewer update2026-11-01 in #108 assigns permanent module/target/
dispatcher/dirty/test registration to each task's author on their own branch;
main integration order stays #107 -> #108 -> #109. No unregistered-code success
claim is allowed.


These mandatory additions supersede any blanket rule that reference navigation,
F6 cached metadata or F8 results are fixed snapshots. Reactivity is the default
for every visible mutable saved fact, including changes made through another
CLI/client. A cache is an implementation detail, not a reason to display stale
facts indefinitely. No timer-based DB polling replaces missing notifications.

The inventory below is an adoption ledger, not a claim that the present source
already implements each route. Its owner increment must complete and validate
the route before claiming that surface done. B+C own current conversation,
header/footer, queue interaction, item focus/body caches and mutable F6/F7 facts;
D adds live-work facts, E/F browser/descendant facts, and G existing management,
operation and popup freshness/geometry. Existing gaps remain explicit #41 work;
the general reactivity requirement is not deferred to an unspecified later issue.

### Notification, snapshot and latency inventory

Normal visible saved-fact updates target1s from commit to display on an unloaded
local fixture, using a shared500ms coalescing deadline. This is a measured target,
not a guarantee during DB/file/API failure. Heavy usage totals retain a documented
3s minimum refresh interval (normally up to3.5s plus finite worker/DB latency) to
protect writes. Report delayed/failed reads as stale/unavailable with observed
read time; do not silently extend freshness claims. Local clocks advance only
visible changed second labels. Invisible consumers unsubscribe or remain dormant.

| Surface / fact | Update trigger and finite read | Present gap / owner | Snapshot exception, time and refresh |
| --- | --- | --- | --- |
| Startup stages, failure and cleanup | Existing stage watch/owned completion updates; no DB polling | Existing route, verify unchanged; B+C | Startup error is the recorded failure, with stage/time; restart explicitly |
| Main header: title, connection/model, cwd, saved phase | Thread Changed, including CLI thread set; shared current-view refresh | Verify complete external mutation route; B+C | Loaded config/palette is startup configuration, visibly documented restart scope; saved Thread settings are not snapshots |
| Conversation selected path, new results, saved responses | Thread Changed plus existing selected-Thread Stream; bounded history/queue read and source revisions | #58 retention/fences retained; B+C | Immutable source windows retain node/source/read identity; refresh/hydration must not erase paid text |
| Footer phase, recovery, retries, pending sends | Thread Changed, accepted-send results and typed activity snapshots; shared read | Full recovery+new-send facts and notice lifetime; B+C | Terminal result refers to its saved Run; current state remains reactive |
| Footer elapsed/output silence | Existing visible local animation/receive observations; no tick DB query | Frozen Turn time must not stand for background elapsed; B+C/D | Elapsed is explicitly saved-base or local-observed, never guessed |
| Composer/draft, attachment prep, notices | Local input and existing draft/attachment completion updates | Preserve newer edits and operation notice separation; B+C | Submitted frozen draft is an explicit immutable submission, keyed by revision/operation |
| Pending-message strip and /queue | Thread Changed and paired history/queue read_revision | Verify CLI/foreign sender, cancellation, recovery and stale replies; B+C | Cancellation selection/confirmation is a chosen-ID snapshot; existing transaction revalidates each target |
| Focused/expanded tool output | Saved tool/source changes; bounded exact source and preview reads; no closed-output raw reads | Source batch/read cache and shared body accounting; B+C | Completed raw preview is an identified source snapshot, with read time/retention/decode facts and reopen/refresh action; current lifecycle facts stay reactive |
| Fixed live region: shells, owned children, waits | Relevant Thread Changed in activity transaction; global output Notify only compares at most the configured number of displayed live markers | New D surface | No fixed snapshot for live state; elapsed base includes observation time |
| Background footer work age/output age | Same D snapshot plus local second clock | New D data, no stopped Turn clock | No separate read or hidden timer |
| Thread list titles/states/tree | Per-Thread Changed; bounded ID-only patch batch | Currently open/filter/Ctrl+R/page only; E | Never justify stale list as a snapshot; Ctrl+R remains manual recovery |
| Watch rows and second-line work/recovery/answer | Same changed-ID batch, details for visible rows only | New F surface reusing D/E | Session unread state is local policy, not a saved-fact snapshot; timestamps advance locally |
| Descendant counts/scoped watch | Relevant/new descendant Changed; one bounded recursive SQL on visible parent/scope | New F surface | Truncated count is an explicit lower bound, not silently exact/stale |
| F6 saved node fields and tool/current execution facts | Selected source/Thread Changed; same bounded source/activity refresh, retain selection | Cached snapshot currently lacks general freshness route; B+C | Truly immutable fields may retain source identity; mutable execution/availability fields must update |
| F6 original node page | Immutable node/hash source; retention/deletion/encoding changes invalidate availability | Existing reader plus explicit freshness labels; B+C/G | Valid immutable-source exception: show source/read time and refresh action; raw shell pages can change and must show bounds/age |
| F7 links | Selected source membership/body/decoding change, tied to same ItemKey and source revision | Existing cached links need stale-source/late-reply checks; B+C | Immutable-node URLs may be fixed only with identified source/read time and refresh action |
| F8 operation list and forms | Fixed catalog; local filter/context and selected-Thread Changed | Catalog/context caching and default-origin clarity; G | An edited form intentionally freezes entered IDs/values; show origin/selection time, do not overwrite edits, revalidate at execution |
| F8 mutable inspection results: Run/Turn/shell/status/index/storage/attachments/history | Relevant target Thread Changed or existing state notifications; one visible consumer read, exact target identity | General cached results/reference routes are gaps; G | Following a reference does not itself justify a snapshot; default reactive, with stale/error/read-time indication |
| F8 saved usage totals and usage through references | Relevant Thread Changed; existing3s coalescing/shared read, preserve scope | Reference one-shot exception must be removed; G | Explicit historical/time-scoped totals only: show scope/time and refresh; normal current totals reactive |
| F8 mutation receipt/results and confirmations | Existing acceptance/result updates; downstream mutable state uses its own reactive fields | Separate saved acceptance from current state; G | Immutable operation outcome is a legitimate snapshot with operation/time; confirmations freeze chosen targets, then revalidate |
| Deletion/index cost previews | Relevant target/source Changed marks/reloads bounded read-only preview before confirmation | Preserve existing stronger guards; G | A chosen preview records time/revision; changed targets cannot reuse a silently stale destructive/cost confirmation |
| Model list | Current connection change invalidates target; explicit catalog fetch completion | Remote catalog supplies no Changed; G | Legitimate externally fetched snapshot: show fetched-at and refresh action; no hidden API polling |
| Help/key explanations | Local UI mode/config/capability changes; immutable bundled guide/version | No DB read, cache by width/bindings/mode; G | Bundled text/config-at-startup exception: show version/context and reopen/current-help action |
| F2 terminal-selection fixed display | Explicit entry/refresh/exit only | Retain intentional display freeze; B+C | Label fixed-at time and refresh (Ctrl+L) / exit (F2/Esc); not a silent current-value view |
| Shutdown/progress/force confirmation | Existing owned shutdown progress and completion signals | Preserve input precedence and fact/error delivery; G | Captured pre-cancellation context is identified saved evidence; current shutdown stages remain reactive |

Notification loss, coalescing overflow and refresh failure either trigger bounded
saved-state resynchronization or display a stale marker/read time. Selection,
filter, field edits, scrolling and immutable source identity survive refresh.
Every inventory row requires an external CLI/synthetic mutation test, or an
explicit immutable/external-source snapshot test proving time and refresh labels.
Do not claim an absent remote notification has been solved by a periodic fetch.

B+C F7 links for a selected item whose saved source is outside the current window
use one scoped UI request/reply, fenced by popup request identity, Thread and
generation. Read at most2 already-validated ItemRegion source node IDs in one
existing read-slot DB job/transaction. Check SQL payload length before copying:
at most96KiB per source and192KiB total. Project metadata/links only, without
tools, raw-output or receipt enrichment, and retain at most 128 links in that
popup. Deleted/oversized/unavailable sources report explicit unavailability rather
than an empty successful list. Do not install offpage nodes or change the #58
history window, follow or selection. No new Core command/schema/queue is added.

### One bounded frontend refresh and cached rendering

Use the existing frontend loop, finite read slots and dirty-ID state. Merge
current-view history/queue/header/activity, browser patches, visible watch detail,
live work, descendant invalidation and eligible usage consumers into one deadline
plan. Each visible consumer admits at most one DB-worker job per500ms; coalesce
its data in the same transaction rather than enqueue a job per fact or row.
Consumers sharing target data reuse it. No work is scheduled for invisible
surfaces. Heavy totals honor their existing3s lower cadence without a parallel
refresh engine. Record actual job counts and total DB-worker time per Changed.
Do not hide per-row SQL inside a nominally single job and call it bounded work.

B+C source correlation/missing-result metadata uses one window-bounded source
batch (at most 128 node/call IDs), with at most one in flight and cache by
(node/source identity, accepted read revision). It must not issue a new DB job
per rendered row or redraw. Explicit user expansion can request its bounded raw
preview; its successful display copy belongs to cache.body_bytes, including
String capacities, block/vector metadata and per-view retained entries. The finite #58 visible working-set exception also permits at most64KiB total
explicit-preview working storage for the currently visible/focused active view
when the configured body budget is zero/tiny, so an explicit open remains usable.
This is counted in the shared retained-body accounting, not an additional cache
per item or128 independent allowances. Inactive/offscreen preview copies are
evicted first; the exception never pins an entire Turn. Report this finite
over-budget working set explicitly in the P7 measurement rather than claiming
all bytes fit a zero-byte budget. Evicted previews reload through the existing
source path. Implementation and measurement of that bound remain to be verified.

History focus is a paint overlay on cached row/item geometry and is not a layout
cache key. Opening/closing remeasures only the changed item and adjusts following
heights/source geometry; changing focus never reparses128 nodes/192KiB. Update
click/F6/F7 hit maps only when source/layout geometry changes. Tree rows are cached
by list revision (items, collapse state, filter, mode) and reused by draw/arrow
navigation; no repeated tree rebuilds per frame/key. Popup wrapped originals are
cached by popup identity/page revision/width; wheel deltas coalesce inside the
existing max_fps draw admission. The catalog is const/LazyLock data; filtered
operation rows are recomputed on filter/context changes, never every frame.

D reads owned work in the activity transaction and skips the work query when
owned=false. Global shell-output notifications do nothing unless live work is
visible; compare only its displayed markers and repaint only when those values
change. Clock-only background/watch displays update at most once/sec when their
rendered second changes, with zero DB queries/formatting/layout in clock ticks.
Hidden output must not invalidate visible history or trigger redraw/DB work.

F descendant counting is exactly one bounded recursive SQL statement using the
parent index, depth8/256 discovered rows and lightweight latest-Run/Turn facts.
Do not call activity::project separately per descendant. Count only the visible
parent footer/watch scope, at most one in flight, sharing the500ms deadline.
Ignore Changed IDs outside the cached descendant set unless new-Thread evidence
can affect that scope. Watch details use one visible-ID batch, sanitized/formatted
at fetch time. Explicit truncation/cycle/deletion semantics remain unchanged.

### Performance counts and observations (6543044236)

The user/reviewer decision6543044236 supersedes the former P1–P7 latency/CPU
thresholds and strict one-second-bin conditions. Compare a specified synthetic
fixture and record OS/terminal, source/baseline, command, actual counts, observed
values and unavailable cases. One available CI measure run is sufficient; do
not repeat measurements to characterize variance or analyse strict time bins.

Acceptance gates are countable behavior: no display DB jobs on unchanged clock
ticks/silent work; hidden output causes no visible redraw/read; bounded batch/
source/cache counts; indexed SQL (`EXPLAIN QUERY PLAN`); one statement/root for
descendants; total draws stay within actual changes plus elapsed clock seconds.
Use total elapsed-second opportunities, including a partially observed second,
rather than arbitrary one-second histogram boundaries. Existing unrelated Core
maintenance jobs remain separately classified, not called UI jobs or hidden
from aggregate cost records.

1. B+C:128 nodes/50 tool rows; focus reuses layout. Record UI/draw p95 and baseline
   delta, with no8ms/+2ms acceptance assertion.
2. D/F: one silent background shell over the recorded10min window:0 UI/display
   DB jobs and bounded total clock-driven draws. Record CPU, never assert
   CPU-no-worse or a strict per-bin ceiling.
3. Visible Thread Changed with applicable List/Watch/count consumers: shared
   batch/job/read counts remain gates. Record DB/readonly work and latency only.
4. F:256 descendants/depth8:one SQL statement/root, indexed/bounded traversal.
   Record p95; no50ms performance assertion. The existing50ms slow-diagnostic
   trigger only controls log volume and remains distinct from acceptance.
5. D/E/F:hidden output at the specified load causes0 visible redraws and0 display
   DB reads/jobs; record actual producers/output commits.
6. G:64KiB original/60 wheel events per second:cache/wrap reuse and total frame
   count within configured max_fps/actual event budget. Record p95/baseline delta
   only, with no8ms/+2ms assertion.
7. B+C:128x16KiB expanded previews stay within shared retained-body accounting,
   including metadata and explicit finite working-set exceptions; evict/rehydrate.

Notify the reviewer of an obvious observed regression (for example >2x) rather
than automatically fail time/CPU observations. Functional/configured deadlines,
coalescing/rate policy and60s watchdogs are unchanged. Historical measurements
and prior acceptance decisions retain their sources; this is an explicit policy
revision, not relabelling or removal of observed data.


### B+C freshness follow-up after first-unit review6523314574

The b0e3c29 admission establishes the structural/interaction subset only.
BC-R1–R6 in the delivery plan remain B+C ownership; do not infer freshness
from request-ID tests or structural PTYs. Qualify real external-mutation routes
and their commit-to-visible latency separately before completing those rows.

Snapshot timestamps use an explicit UTC label, captured once when a bounded
read succeeds (or F2 freezes the painted display), never recalculated on draw
or animation tick. Finished explicit tool previews retain that read time in
the existing budgeted notice string and show close/reopen to read again.
Original pages retain a per-accepted-page read time; O refreshes the selected
source and P requests its first page. F2 shows frozen-at time plus F2/Esc to
resume. These local display timestamps are observations, not saved source
creation times or guarantees that mutable lifecycle facts are current.

Changing detail source identity resets unrelated source IDs, cached links and
original pages, invalidates their request tokens, and starts new reads only for
the current selected source. Paging the same source does not invalidate an
independent current links request. Source/availability revisions must invalidate
cached links even when rendered prose is unchanged. Keep mutable lifecycle
fields distinct from immutable saved call/result JSON and preserve selection
identity while updating facts. Failed refreshes retain evidence with an explicit
stale/unavailable notice rather than presenting cached facts as current.


The B+C follow-up reuses the existing item-scoped detail reader/action and read
slots for both fields and links. A selected Thread metadata refresh invalidates
the visible detail consumer; its one dirty flag and one in-flight token coalesce
at500ms. No further reads occur without opening/navigating, an explicit refresh,
or another accepted change notification. F2/covered/closed details do not request
reads. An invalidation during a pending read marks that reply obsolete; retirement
precedes the next read so bounded admission is preserved. Errors remain labelled
stale and await another change or explicit refresh, rather than retrying on ticks.

One detail DB job uses a short transaction for at most2 exact node IDs,96KiB
payload per source, and a bounded current-tool relation lookup per source. No
raw output bytes are read; current state/code/output-complete/retention generation
and saved-output presence come from exact group/call/Turn relationships. Limit
current execution detail to16 calls/source and expose truncation, then cap the
combined fields at128 and links at128 per source within existing presentation
metadata bounds. Decode/Markdown/sanitize on the existing presentation workers;
retain only prepared fields/links, not off-page bodies. Missing/tombstoned/oversized
sources are explicit per-source unavailable results; they invalidate cached facts
for that source without discarding another available source. Every reply carries
source IDs, monotonic read revision and observed UTC time. This is one visible
popup consumer job/500ms; consolidating all main-view jobs remains separately
open in D's shared refresh refactor, and does not gain a false completion claim.


Source availability has a notification gap independent of normal Thread Changed:
raw retention/history GC previously committed without a frontend invalidation.
Use one crate-private `Notify` owned/cloned with the existing GcService, with a
single stored permit (`notify_one`) for the one frontend consumer. Signal only
after a raw/history batch actually commits an availability change, including
committed batches before a later collection error; a rollback/empty pass does
not signal. This adds no durable event, task, table, queue or polling timer.
The frontend forwards a selected-view detail invalidation, not raw bytes or a
full history reload; UI ignores it when no details are retained, and covered/F2
details defer the bounded read until visible. Global GC may affect either of the
at-most2 exact selected sources, so a single coalesced source read is justified;
unrelated shell output does not use this notification. G can reuse it later for
mutable storage/attachment management facts. The notification has no shutdown
task: it drops with GcService/frontend references under existing owner shutdown.


A detail popup opened for a tool retains its exact ItemKey in addition to the
at-most2 source IDs. When the accepted history/tool projection revision changes,
reconcile that key's call/result membership from already-prepared metadata; do
not scan/re-layout on every draw or25ms wake. A newly saved result can therefore
join an already-open F7 without user input. Preserve the chosen original node
and page while updating the surrounding source membership. An unrelated node
navigation clears the item binding. Changed membership invalidates any pending
old-scope read through the same dirty/retirement sequence and500ms coalescing.
No body is pinned and no extra query discovers membership by row during paint.


### Saved sender boundary after #59 (comment6524373262)

Use the adopted [sender inspection](protocol.md#message-sender-inspection-59)
contract when classifying conversation input. New external CLI receipts are
`external_cli` and keep the existing distinct generic-input band; trusted TUI
input is recorded human. Parent/self model and Revisit classifications require
saved identity/allocation evidence, never a self-asserted name. Historical null
retains the existing Human conversation-band convention only; CLI/F6 sender
inspection reports unknown when evidence is missing. This display convention
must not invent a sender identity or authority.

Preserve #59's immutable, credential-free sender kind/IDs and bounded name in
F6/original details even when a newer mutable-source snapshot arrives. Names
remain explicitly self-asserted. This follow-up keeps names in details; adding
them to ordinary bands is optional and is not required to change the existing
band classification. The explicit initial user-source provenance read remains
separate from notification-driven mutable-source refresh, without a new worker
or Core sender inference.


### D0 combined current-view read

The existing refresh task collects receipt states and the visible Thread/page,
paired pending input, sender/tool/recovery/usage facts, received audits and activity
inside one short DB-worker transaction. SQL-only helpers return owned raw data;
JSON/rendering/sanitization and clocks are prepared on the existing presentation
worker after releasing that transaction. The page's paired read revision identifies
the entire transaction snapshot. Failed raw tails reuse validated source/execution
facts from this read and use the existing finite read-only output path, not another
DB-worker correlation lookup. Closed successful output still performs no raw read.

Domain view failures do not erase separately observed receipt states. Generation,
selection/read revisions, paid stream-commit mappings and existing source-window
acceptance remain authoritative. Current-view coalescing and paused-source feedback
must be integrated before the one-job target is claimed or this D0 unit admitted.
There is no new queue, worker, table or protocol operation.


### D1 bounded frontend header/title observation

The shared current-view collector and initial Thread selection use a frontend raw header query, not the CLI
ThreadDetails helper that derives titles while holding the DB worker. Read the
Thread/current Run/open Turn and direct owned counts as raw SQL facts; construct
context and labels on the existing presentation worker. Share the existing pure
user-payload title policy so explicit stored titles remain verbatim and normal
derived titles retain Unicode, text-part ordering and terminal-sanitization rules.
CLI title lookup semantics and wire/schema are unchanged.

For an empty stored title, inspect a prefix of at most 128 selected-path entries
(plus one sentinel), then accepted/held receipt prefixes of at most 128 per state
(plus sentinels), using the existing path/receipt indexes. Inspect prefix membership
before node-kind/path exclusion rather than scan an unlimited history to find128
users. Only user JSON bodies can be copied: at most 128KiB each and512KiB total
across the witness. Compute lengths and cumulative bounded-prefix budgets in SQL
before copying; parse JSON only after releasing the transaction.

An omitted earlier candidate or exhausted path prefix cannot justify choosing a
later user as the first request. Pending prefix exhaustion is conservative because
equal-time receipt and node order can differ: report limited evidence instead of
selecting a possibly later input. Display `名称未設定（元本文の取得範囲外）` when that
uncertainty prevents deriving a title; distinguish this from exhaustive no-user
`未送信` and exhaustive unreadable/empty-user `名称未設定`. An already found earlier
selected-path title wins regardless of later truncation. This label is a frontend
observation limit, never a fabricated sender/Thread state or title mutation.
No new worker, table, index, queue or protocol command is introduced.


### D1 existing retained-source lookup index

Use the existing `tool_result_node(node_id)` index, shipped since migration2,
for exact retained source-relation resolution. This includes current payload-GC
cases; it is not an old-format repair or new authority rule. Keep the existing
uniqueness check and unknown result for genuinely ambiguous relations. Current
conversation schema remains14 and no new index/migration is adopted.


### D2 pending-wait source scope and live DTO

Before the activity DB read, copy at most 128 existing WaitRegistry entries as
(operation ID, source shell execution ID), without credentials or new registry
fields. CLI transport admits at most24 ordinary requests, but direct Control
callers are not globally limited by that semaphore: an over128 snapshot is
explicitly unavailable, never a truncated global prefix. Resolve exact saved
shell ownership and wait.pending operation keys inside the same activity read
transaction, filter by the saved current Run, then sort by saved start/ID and
retain at most the configured `ui.live_work_rows` oldest rows after suppressing verified active wait caller shells. A removed OS handle does not erase a still-pending saved
wait. Missing keys are absent observations, not phantom waits. Unknown wait scope
is separate from zero; wait rows are observations, not extra child/process counts.

Publish existing Changed only after the initial pending commit and registry
installation, and after committed pending-key deletion even when no foreground
Turn transition occurs. Keep existing Turn CAS, cancellation and cleanup order;
add no business event type, lifecycle state, table, queue or background poller.
The registry's existing Guard still owns removal. The bounded snapshot is read
on the frontend task, never by the UI thread or while holding the DB transaction.

LiveWorkSnapshot carries saved Run identity, prepared bounded rows, shell/child
counts, optional exact wait count, overflow and one monotonic observation base.
Rows contain typed Shell(execution/Turn), ChildRun(Run/Thread) or Wait(operation/
execution) identities, prepared label/state, saved timestamps and optional elapsed/
output-age bases. UI timing remains distinct from observed Turn time. Unknown
wait scope is visible and does not claim exact oldest ordering across missing
observations. All normally admitted CLI waits are included in the exact scope.

### D2 child identity, chronological display and unavailable evidence

Only the displayed owned-child rows (at most the configured limit) acquire bounded title witnesses
through the same frontend header title policy: explicit title first, otherwise
at most 128 path/pending membership candidates and512KiB copied evidence per
child. SQL collects owned facts inside the existing read transaction; title JSON
and saved activity classification are prepared after leaving the DB worker.
This fixed display-only title work may issue at most2 extra bounded statements
per displayed child; it never calls the CLI's unbounded derived-title reader.
Child rows reuse the existing saved Run/Turn/attempt/tool activity classifier,
including recovery and background state, rather than presenting every live Run
as undifferentiated running work.

Display chronology uses SQLite julianday timestamps and stable ID ties, with
unknown timestamps last. SQLite's date-function precision can tie starts within
one millisecond; this is display order, not lifecycle ordering or a timestamp
rewrite. Chrono computes elapsed bases from the original saved timestamps.
Malformed or missing required fields in an exact present pending-wait key make
the wait count unavailable; missing keys remain absent, and fully typed but
mismatching source identities remain excluded. No wait row is fabricated from
unverifiable metadata. A terminal selected Run never acquires a live-work
projection even if inconsistent retained rows still say running.

### D2 parent notification for owned child work

An accepted new Run with a proven `owner_run_id` publishes existing Changed for
its owning Thread in the same post-commit effects as child acceptance. This is a
metadata notification, not permission to wake or interrupt the parent model.
Receipt replay, rejected acceptance and rollback publish no repeated effects.
The existing completion/recovery owner nudge also publishes parent Changed after
its saved owner lookup, including a stopping parent; retain the existing quiet
model nudge only for a live parent. Parent display freshness must not wait for a
foreground shell or model request to finish consuming its owner delivery.

Frontend seals the displayed ChildRun Thread IDs (at most the configured limit) to the accepted current
Run/Thread/generation snapshot. Changed for those exact dependencies invalidates
only the selected parent's existing coalesced read; it never launches a hidden
child read. Selection/Run/generation change replaces that dependency set. The
new-owner and terminal/recovery parent notifications cover count changes beyond
the displayed rows; unrelated hidden Thread changes cause no extra reads. Active
Run deletion remains rejected by the existing deletion transaction. No global
owner scan, persistent dependency index, lifecycle state or notification type is
added.


### D2 live output working set

The open shell owns a display-only cursor in the existing frontend task. It
retains separate encoding_rs decoders and at most4KiB total decoded text across
both streams, rebuilding trimmed strings so retained String capacities also
obey that cursor allowance. Its prepared UI copy also retains at most4KiB total, so this
consumer has an explicit8KiB retained text allowance outside cache.body_bytes,
not a4KiB application-wide or RSS bound. Only one such cursor and one UI copy
exist for the selected open live shell; temporary bounded preparation/delivery
copies and small decoder/identity metadata are distinct. Closing, hiding,
completion or selection changes discard both active copies. Inactive views
retain no live output body. Existing history/item cache limits are unchanged.

Each observation uses one read-only transaction, at most 128 chunk metadata rows
and4KiB total new raw bytes. Contiguous ranges retain stream decoder state;
initial observation, retention/encoding change, missing range or excessive
backlog starts a tail and reports skipped/uncertain evidence. UTF8/UTF16 tail
alignment is charged to that same byte allowance. Incomplete units remain in
the decoder until more bytes or a saved terminal output_complete snapshot.
No CLI cursor, read pin, encoding mutation or durable position is created.

### D2 stopping a child without a live Driver

A recovery-wait child can retain a live owned Run after its Driver has exited.
Stopping that child drains its saved ownership synchronously rather than passing
through the Driver's normal owner nudge. Extend Control's existing stop target
lookup with its saved live/stopping parent Thread. After the no-Driver drain
successfully commits, publish existing Changed for that parent without a model
kick; this covers counts even when the child is outside the configured oldest visible
rows. A failed drain publishes the existing child error and no parent terminal
notification. Active Driver stops retain their existing completion owner nudge.
No new query, registry, worker or receipt behavior is introduced.

Live-work captures paired wall/monotonic observation clocks at the end of its
same-transaction DB read, before returning to presentation workers. Preparation
uses that captured wall clock for saved elapsed bases and publishes the captured
monotonic base. Presentation queue delay is then added by normal UI timing; it
cannot make an older snapshot erase a newer committed-output marker.


Live output observations and previews are replaceable display snapshots in the
existing frontend outbox, not critical operation results. Coalesce one marker
snapshot and one preview per Thread/generation through its existing latest-value
slots. Every marker snapshot includes all observed displayed executions (at most the configured limit),
so replacing it cannot lose an earlier different shell's activity. Receipts and
business-operation outcomes keep their ordered critical delivery. This adds no
channel or capacity; raw UI-channel deliveries remain separately bounded.

### E admission: display snapshot ownership

The E-only increment moves the visible current-view plus optional browser-ID
patch, standalone browser-ID patch, and explicit browser page reads off the
persistence worker. They use short-lived query-only WAL connections on the
existing finite presentation pool, admitted by the existing read slots and
shared progress permit. No new worker, queue, schema or F/G consumer is added.
The progress permit is released after raw collection, before projection and
preview decoding. Cancellation is checked again before opening a connection.

Mint one observation sequence at asynchronous read admission, before waiting for
the presentation pool or opening its WAL snapshot. The paired current-view and
browser facts use that same sequence; a late older read cannot mint a newer
sequence after an overtaking canonical history read. Keep independent receipt
facts when one visible surface has a domain error. SQLite busy/locked/interrupted
errors retain their concrete type through per-surface collection and become
retryable display errors, never a persistence-failure latch.

Explicit browser opening/filter/pagination initially retain canonical CLI page,
cursor and title/excerpt semantics in that readonly task. That existing path
still performs per-row title/excerpt queries and may scan filtered candidates;
relocation does not qualify globally bounded SQL or prove a performance win.
Hidden receipt-only delivery remains on its existing authoritative path. E still
requires exact-source functional/native checks and applicable P3/P5 observations;
D2-only evidence does not qualify visible browser work.


E's test fixture forwards the bounded synchronous terminal channel into one
bounded async test channel so it can await real update readiness. It also wakes
for the same pure visible UI action delay used by the native loop (source-link
coalescing or browser filter debounce). This is test-owned forwarding, not a
product worker or a new polling cadence. #77's shared watchdog guards completion
only; saved facts and sequence/count assertions determine success. Paint latency
is recorded without a wall-clock pass/fail threshold. Native terminal polling
retains its existing ceiling and never requests DB work merely for this delay.

A receipt-derived automatic title does not imply selected-path admission. A
coherent readonly snapshot may contain that title, one queued message and no
selected user node. Display the pending queue honestly; only a later saved path
observation establishes admission. Tests must preserve this intermediate state
and wait for the immutable selected node before stopping the Run.


When the browser and selected-view deadlines become ready together, the browser
select arm only wakes the outer admission loop. It must not start an independent
browser read and move the shared deadline forward before the pending selected
read can take that same browser ticket. Keep admission priority in the existing
outer loop, with no new queue/worker or additional timer. A browser-only change
still uses its normal bounded standalone read.

### F shared saved-summary primitive

F extracts native Run/Turn activity decoding and saved summary identities into
control::db::thread_summary because CLI summaries and readonly display consumers
must classify the same stored facts. The lightweight Run/Turn join is reused by
descendant queries without per-descendant attempts/tools/body projections.
Optional stored identities/timestamps remain unknown when absent. This is a
bounded read model on the caller's existing transaction, not a persistent table,
new command, worker, queue or lifecycle authority. Frontend retains membership,
watch/unread/session and presentation ownership. No G inspector model is added.


### F retained descendant membership

Current `control::db::delete_thread` tombstones Thread rows and retains their
parent identity; history GC removes obsolete path/replacement payload ownership
without deleting the Thread row. A retained nondeleted grandchild can therefore
remain reachable through a deleted intermediate. The bounded traversal includes
those intermediates as discovery/invalidation witnesses, excludes them from
counts and public Watch membership, and still spends the discovery/depth budget
on them. A deleted/missing root is unavailable, not an exact empty scope.
Run ownership is deliberately irrelevant to ancestry. These conclusions concern
the inspected current deletion/GC implementation; they do not authorize changing
deletion behavior or inventing missing lineage after future physical removal.


### F visible Watch source admission

Before parsing command JSON or copying any answer/recovery body, the readonly
statement measures source bytes. Command JSON, recovery and answer witnesses
share a16KiB per-row and64KiB aggregate admission budget. Individually oversized
sources are excluded; if remaining eligible sources exceed the row budget, that
row's witnesses are unavailable rather than copied and discarded afterwards.
Malformed JSON stays unavailable, with original/detail navigation retained. A
512-character command label can occupy2048 UTF-8 bytes and is not a512-byte
budget. The complete admitted command source is charged, not only its label.

Stored parent labels have a separate4096-byte SQL-copy bound per visible row;
parent derived-title evidence retains the existing header reader's independent
budget. Native metadata, wrappers, prepared strings and the2000-row session
cache are separate retained allocations:64KiB is a source-witness budget, not
a claim about total process or Watch memory. Reads remain exact-ID batches
(max128); JSON/text projection runs on the existing finite presentation worker
after the caller-owned readonly transaction ends. Shared summaries/titles may
only be reused from that same transaction. No new worker/queue/table is added.


### F Watch discovery-page ownership

Canonical Watch discovery is an explicit paged read on the existing finite
presentation pool and current-view progress admission semaphore. It opens a
short readonly WAL transaction; release it before title/JSON/text projection.
Assign the observation sequence before asynchronous admission, then retain
generation/cursor/root fences so a late page cannot overwrite newer patches.
Check cancellation before admission/open and after collection. Discovery limits
retained candidate metadata to4096 and title witnesses to128, with existing
per-page title-source budgets; these are retained-result bounds, not a claim
that priority sorting touches at most4096 SQLite rows. Ordinary Changed patches
remain exact-ID reads and do not rediscover the whole candidate page.

Lineage query diagnostics add one fixed numeric/boolean aggregate only when
the measured read takes at least50ms, through the existing bounded diagnostic
writer. No titles, query bodies or per-descendant records are logged; no new
queue or worker is added. This event is observational, not a lifecycle event.


### F browser integration boundary

Extend the existing browser observer with explicit List/Watch mode, visible
detail IDs/parent dependencies and bounded cached lineage. Existing List reads
stay readonly and retain E's progress admission, sequenced snapshots and the
same-deadline priority rule. List does not prepare hidden Watch summaries.
In Watch mode, summary/title/detail witnesses from this transaction are reused
before releasing it, then projected on the finite presentation worker. A detail
scope replacement cancels old detail tickets independently of the browser page.
One observer retains one flight/500ms and its start deadline across hiding or
replacement. Output hints only compare already-visible shell markers, not poll
new rows. Runtime Watch exposure waits for actual UI consumption and actions;
backend primitives alone do not claim a usable screen.


### F UI/read/action integration

Browser and selected-footer descendant roots share one500ms admission and one
readonly snapshot with selected history/queue facts when due together. A scope
change/close retires tickets; the UI adopts only matching generation and read
sequence. Watch details are delivered to the visible Watch rows, not silently
discarded. Descendant counts preserve unknown/error/last-observed distinctions.
Local visible age ticks do not request database work.

Stop/retry confirmations retain exact Thread/Run/Turn/generation identities.
Control checks that saved target in the same existing mutation transaction as
the canonical stop/retry reducer and operation receipt, preventing replacement
and generation races. This narrow private guard is Core-owned because frontend
prechecks cannot make the write atomic; it is not a new lifecycle/command/table,
worker/queue or generic mutation engine. Output ownership checks are display
reads and use readonly admission. Accepted mutations retain existing Control
admission/shutdown/durability ownership even if the browser closes.


The explicit Watch output action reuses canonical ShellRead for retained bytes,
decoding/cursors and source availability. Its normal Core metadata/read lifecycle
remains owned by that existing operation rather than duplicated in the UI; only
the frontend ownership check is a readonly presentation snapshot. This explicit
user read is distinct from recurring visible Watch facts/counts, which must stay
on the shared readonly lane. No second output decoder or read engine is added.

The existing popup panel can record its own painted body rectangle. Watch wheel
input is routed only inside that body, and a resize invalidates the old rectangle
and visible-detail IDs before another queued result can be adopted. Footer
descendant labels reserve their width before less urgent facts are fitted, so a
positive count is not silently clipped by a long tool/Revisit status. These are
F view-ownership/layout boundaries, not the transferred #83–#87 display work.

## Conversation row vocabulary (#83)

The display-only `presentation::rows` table owns marks, keyed labels, colors and
item-boundary spacing. Use East Asian Width=N marks, checked at ordinary and CJK
width: `›` human, `↳` parent/other model (with explicit labels), `↺` self, `↻`
Revisit, `▸` answer, `▹` request, `⏵` running, `◌` unavailable, `✓` success,
`✗` failure, `⊘` cancellation, `⁂` notification, `⬦` summary, `↪` spill and `◉`
reserved for the #86 work-region heading. The old ambiguous-width `●`, `◇` and
`◐` are not chosen. Distinct shapes plus labels work without color. Existing
palette roles are reused; no new theme configuration or translation framework.
Issue #91 extends this table for external/unknown input with ⇥, labeled 外から.
Issue #90 registers the conversation spinner frames beside this table
(`SPINNER_CANDIDATES`, default braille `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`; also `◴◷◶◵` and ASCII
`|/-\`), every frame checked at width 1 under ordinary and CJK rules. At most one
row spins and only its mark cell changes: the newest validated
`running_current` foreground shell while the selected Thread's Turn is in
progress, else the current attempt's provisional streaming answer while the
model is awaited/received, else none (`ui/motion.rs::select`, the single rule).
Other running rows keep `⏵`; background shells never spin; an off-screen winner
does not pass the slot to an older visible row. Before the first streamed text
there is no answer row, so only the status bar moves; no placeholder row or
persisted node is created for decoration. The `◉` heading only changes
brightness (never glyph); `NO_COLOR` keeps it static. Final spinner shape is a
pending user choice from real PTY samples.
The user selected candidate B on 2026-10-31 after the real-PTY comparison; this
supersedes provisional A (⤷) and the other candidate (⤳). Main integration still
requires the delegated qualification, not another visual selection.
Shell right-hand metadata uses the same table: ordinary exit/time facts use the
active palette's muted role; failure/nonzero-exit and cancellation retain the
attention role. Live elapsed painting shares this policy. NO_COLOR keeps the
previous plain metadata appearance; marks, labels and layout do not change.

Insert one nonselectable display blank between unlike body/tool/notice/
replacement groups, including within mixed answer/tool nodes. Consecutive tools
stay dense; original Markdown blank lines remain original content. Blank rows
participate in layout/source-anchor calculations but never gain an item key.
No canonical text, item identity, protocol or saved state is changed.

Continuation notifications show saved target identity and available exit/state/
duration facts, then explicit model-delivery wording. Missing names, commands or
results are shown as unavailable rather than inferred. Selecting a notification and pressing Enter follows the loaded tool row
by exact execution identity or opens the saved child Thread. A shell outside
the bounded layout reports unavailable, retaining full F6 facts and sources.
F7 remains HTTP links on this baseline; it is not used for this navigation.
Loaded matching tool sources are correlated without a database read. Richer names absent from saved notification payloads
are tracked in #88 as a separate bounded read-projection task, not a new Core event.

This minimum does not draw the #86 work region or change its height/selection.
History timing uses saved starts in existing bounded tool correlation, painted
through the existing UI animation cadence; it must not introduce a DB poller or
per-second Markdown/layout rebuild. Physical terminal glyphs and P1 performance
must be distinguished from native synthetic PTY/test results.


### E admission with latest Thread observations

Keep #74's canonical ThreadList/Get last_answer/last_input projections. The
shared summary preparer used by exact-ID presentation patches leaves those
optional CLI message projections unobserved/None, matching the bounded header
contract; canonical thread_list explicitly populates them through #74's indexed
helpers. Explicit UI canonical pages retain the shared ThreadList behavior.
No new operation or schema is added by E beyond the inherited main schema16.
The #83 presentation::rows legend and #84 composer remain byte-identical to the
merged main for that E admission snapshot; later row-vocabulary extensions such
as #91 follow their own adopted display contract. Preserve #71 auto_history and
#54 conditional status ticks.


### F closed-consumer read admission

The visible-read plan waits only while at least one captured List/Watch or
descendant-count consumer remains live. Closing one consumer does not cancel a
shared read still needed by another; closing all consumers retires a shared-slot
waiter without acquiring that slot. The existing readonly snapshot helper also
checks the captured plan immediately before opening SQLite on the presentation
worker, so a plan invalidated while queued does not open a database and then
discard its result. Existing per-consumer checks still omit cancelled parts of
a partially live snapshot. No new task, worker, queue, Core state or operation
is introduced. Cancellation is cooperative at admission/worker boundaries; a
snapshot already executing may finish, and stale-result guards still apply.

### F Watch narrow-row field priority

Watch keeps the existing shared popup frame, constant one/two-line row geometry,
selection and visible-ID read scope. Give the activity word its own bounded
leading field so long titles cannot hide recovery/unread/activity meaning,
including NO_COLOR. The title uses the remaining flexible field; a muted parent
field is retained when width permits and omitted in the narrow reduction.
The second line reserves room for available factual clocks before the bounded
command/recovery/answer detail. A long command must not push a known work age
off the edge. This changes presentation only: no Core fields, clock source,
read cadence, worker, queue or dependency changes. Existing original/detail
navigation retains full facts when the row uses an ellipsis.

Watch formats text only near the previous viewport and current selection.
Offscreen rows retain their fixed heights as empty Ratatui rows, so selection
and Home/End scrolling retain native table geometry without formatting every
loaded title on each draw. Both neighborhoods have a one-row margin; no new
cache or persistent state is introduced.
## Manual navigation across a short folded tail (#93)

Upward history gestures remain manual navigation even when the currently loaded
source window fits inside the viewport. At its older edge, request the existing
bounded history window and preserve follow=false while the read, refresh or
repaint is pending. History-focus Up at its first loaded item uses this same
navigation path rather than repeatedly selecting that item and cancelling reads.

A closed, typed tool-result source has known rendered summary rows even if its
hidden canonical output window has an unloaded prefix/suffix. Moving between
those rows must not hydrate hidden output before loading older source nodes.
Ordinary prose, mixed/unknown sources and expanded tool bodies retain their
existing bounded body-window navigation and immutable source anchors.

A positive gesture that moves no rendered row in a partial short window is not
proof of reaching the bottom: it must not enable follow. Actual downward motion
to the true tail, an entirely loaded short history, explicit Ctrl+End, and the
existing send-intent behavior retain their established follow contracts. No
refresh/animation/geometry calculation enables follow by itself. Keep generation,
view/read revisions, pending navigation and coalescing fences; no new lifecycle,
protocol, worker or persisted state is introduced.

The direction of crossing a local projection boundary follows the gesture, not
only the clamped row coordinate. When the rendered neighborhood is shorter than
the viewport, max-row is zero: an upward target can be nonnegative and >= max
without becoming a forward gesture. Never alternate backward/forward projection
focus on that geometry. Drawing also skips closed typed result hydration; manual
navigation supersedes presentation-only reads by advancing its view revision,
while an actual navigation request retains and accumulates pending movement.

The existing 128-source and192KiB projection budgets remain unchanged.


### F browser mode on shortcut reopen

Ctrl+T reopens the last List/Watch mode stored in the existing browser enum for
this UI process. Slash entry remains explicit: /threads opens List, /threads
active opens global Watch, and /threads descendants/Alt+D chooses the current
Thread scope. Shortcut reopening remembers the mode only, not a previous
Thread's descendant root or filter; this avoids silently monitoring an old
root after a conversation switch. A new UI process starts with List. No saved
preference, Core field, worker or queue is added.
## Ordinary tool rows: body and right-side facts (#98)

Restore B's diagnostic-ID boundary: compact tool rows contain only the shared
mark and sanitized command/description body at left. Execution IDs remain exact
ItemRegion/source and detail references, never ordinary row prefixes. #97 owns
the selected-description/command inspection surface; #98 does not add a second
selection or navigation model. Existing expanded/original/F6 references and
Enter notification-to-source navigation are retained, with no new reads.

Use the same mark/body/metadata composition as #91 input bands. Background/reason,
exit code and elapsed are right-side decorated spans excluded from body/source
matching. Background requires an explicit background flag, background_current,
or typed human/additional-input release reason; running/running_current alone
is not background. Drop noncritical metadata before taking body space. A failure
or cancellation fact retains attention styling and may reserve body space only
after other metadata has been dropped. Clock painting uses the cached mark/body
and updates right-side metadata without discarding background reasons, reparsing
Markdown, changing source anchors or reading the database.

### History focus without changing columns (#98)

The user's final correction supersedes the proposed left-edge marker. Focus
changes only the first heading row's style, never indentation, column count,
body text, opened output or read-status rows. Rich dark/light palettes define a
subtle focus background separate from user bands; NO_COLOR/non-256 terminals
use bold on that heading only. Link-specific interactive underlining remains
unchanged. A continuation visible after its heading scrolls away is not styled
as a new heading. No focus marker is added to selectable/copyable text.
Opened-output notices (observed time, refreshing, genuine read error and loading)
use a shared two-space indentation and muted style on every wrapped notice row,
matching the user's example. This is item-internal structure, not focus
indentation. Notice spans are decoration and never canonical output-body spans.

### Pending selected changes during an older read (#95)

A pending Changed for the selected Thread reserves the next joint read even when
an older view is still refreshing (and therefore has no refresh_deadline yet).
Do not admit a standalone browser read in that state. If an older browser ticket
still owns the one-flight slot, defer the selected refresh and its timer until
that ticket completes; then the existing outer loop consumes the pending browser
IDs with the selected snapshot. Otherwise the view can lose the browser ticket
before either deadline tie rule applies. Browser-only/foreign work retains the
same cadence and existing notification wakeups. No epoch, worker, queue, timer
or persistent state is added. The strict paired-sequence test remains unchanged.

### Periodic polls cannot redirty an in-flight selected observation (#95 return)

The existing progress tick requests another snapshot only when no snapshot for
that selected Thread is already in flight. A Changed event can temporarily mark
an idle selection as pollable until its activity facts return; periodic ticks
while that read is pending are not independent saved changes and must not queue
a second observation after its idle result. Real Changed events still advance
the existing epoch and dirty the Thread even during a read; stale activity cannot
clear that new work. Keep the same 500ms cadence, missed-tick behavior, paired
browser ticket and all one-observation/same-sequence/no-reload assertions. This
uses only the existing refreshing set, with no timer/state/worker/queue addition.

### F fitting viewport offset coherence (#100)

When every visible List tree row fits its table body, the actual TableState
offset is zero even after a metadata reorder at unchanged geometry. Keep the
selected identity, filter/collapse state and normal overflow screen anchors;
do not retain an unnecessary blank bottom while hiding a promoted row.

Watch applies the same fitting rule to its actual offset before choosing
formatting neighborhoods and computing its visible-detail scope. A locally
clamped formatting offset is not sufficient if rendering still uses an older
TableState offset. Keep c5785c5's bounded neighborhood formatting and the
2b9514e open-list membership regression. No browser ticket/admission, saved
membership, timestamps, rows, workers, queues or pools change for this fix.

## Preview lifetime and one tool disclosure owner (#99)

A preview read has no wall-clock failure deadline. Byte/row limits and the
existing eight read-worker slots remain its bounds; genuine read/admission
errors remain errors. Each existing ItemState entry owns a cancellation token
for its current request revision and passes a clone with its UI action. Closing,
leaving the visible target, replacing/evicting the entry or changing outcome
cancels it synchronously, even before backend action admission. The frontend
also retains its existing Thread-selection cancellation fence. Late UI replies
remain Thread/generation/request-revision checked. ItemState retains its
128-entry bound and the existing eight read slots remain unchanged; there is no
secondary request registry, new worker, queue, timer or persistent state.

Every projected occurrence of a tool uses the same best correlated outcome and
one ItemKey for its disclosure state. Disclosure uses outcome class, so equivalent
saved/current terminal aliases and exit-metadata refinements do not undo a close.
In particular an older exact-result block
cannot fall back to its own attention default after the header was explicitly
closed. Empty result nodes with an unambiguous exact result-node reference from
an already-rendered tool are not independent placeholder items. Ambiguous
references or unrelated nodes are never coalesced by command text or ID prefix.
These changes require review independently of #98's display-only corrections.

A Changed/history snapshot no longer performs implicit raw-output tail reads.
Default-open failures and explicit opens use the same cancellable ItemState
request path (automatic total2KiB, explicit up to configured16KiB). Metadata
already read in the bounded tool-source batch includes saved_sequence and
output_complete as nonserialized UI facts; no schema/protocol or new query is
added. A disclosure's state and output revision are separate: new output keeps
an explicit open/close choice, retires only an older read and permits one fresh
snapshot. Unchanged terminal+complete observations do not reread or mint a new
"取得" timestamp. Retention generation alone does not invalidate paid display
text. No animation/draw path reads raw output. Legacy raw-preview constructors
remain test-only fixtures for bounds/identity tests, not production refresh work.

## Conversation and header background extents (#91)

Conversation bands end one terminal column inside both window edges. The title
background spans the whole window, but its text uses that same one-column
horizontal inset. Geometry helpers in the UI theme own these extents. Paint each
visible user/input row's full band rectangle, independent of its last styled
span or wrapped text length. Reset the conversation background before content
painting and restore normal rows' trailing cells from their own line/span style;
this keeps differential scrolling from inheriting a previous band's edge color.
Foreground, links, code-surface colors, canonical text and hit positions remain
separate from background extent. NO_COLOR adds no band colors or new columns.
At the minimum eight-column viewport the inset leaves six content cells; the
selection banner uses the compact "固定" label rather than disappearing or
painting over the margin. Wider headers keep the existing full freeze label,
time and resume guidance according to their available content width.

## Contextual keys and canonical copy (#87)

The #87 UI work may precede F/G (the user's 2026-10-31 decision). G builds on
this work; no Core state, event/protocol or persistent format changes are needed.
A small UI operation/key vocabulary owns key labels and descriptions shared by
help, readonly detail controls and contextual bottom-row hints. Readonly shortcut
letters accept either case; editable form/query text retains its actual case.
Hints contain 2-4 available actions for the actual owner/focus, use configured
send/newline bindings, and drop low-priority/rightmost actions with F1 as the
narrow fallback. Notices/recovery retain priority. Hint text is cached by owner,
focus/state, width and bindings, not rebuilt on elapsed-time repaint. Popup hints
must describe their real modes, including available original-page navigation.
Activity/elapsed/recovery facts belong to #85; only the key-hint surface changes.

Focused history c/C copies the canonical whole answer; o/O opens its original.
Code choices use b/B, with numbered copy labels and a bounded transient chooser;
choosing a block uses canonical Markdown parser extents, never displayed/folded
text or language labels. Bare /copy prefers the single code block, opens choices
for several blocks, and copies the body if there is no block. /copy all explicitly
copies the full body; explicit /copy NODE [zero-based-index] remains available.
Display block numbers are one-based. A chosen answer precedes the viewport anchor,
then the last loaded answer when no focus exists. Source identity is exact and
late choice responses must not replace another thread/focus's active operation.

Copy summaries include Unicode character counts and block number or original
page/continuation facts. OSC52 reports only that a terminal request was sent, not
that the host clipboard accepted it. The existing body/page copy limit remains
64KiB; larger bodies offer original-page navigation rather than partial success.
Explicit complete-code retrieval may paginate within a finite UI copy budget;
unavailable/tombstoned/incomplete or over-budget source is an actionable error,
never a successful partial block. All source reads/Markdown work remain off the
main UI OS thread in existing workers; no new worker/queue/poller is introduced.
Code-copy decoration/hit targets are display-only and cannot enter canonical text,
source-based copies, source offsets or model input. F2 terminal-native selection
still copies visible presentation, not a canonical whole block; b or /copy uses
canonical source instead.

Image guidance advertises /image and /attach without probing the host clipboard.
An empty paste is not proof of an image; guidance may state available operations,
not infer clipboard contents. Actual image acquisition stays explicit. Help and
slash descriptions teach copy/attachment paths near the start; failures suggest
/attach. Synthetic PTYs use fixed OSC52 output captured by pipes and synthetic
clipboard adapters/files only, never real OS clipboard reads or writes. Native
80/wide, dark/light/NO_COLOR/vt100 and changed send-key scenes must demonstrate
bottom-row hints; P1 interaction performance is compared separately from replay.

### Contextual hint implementation boundaries (#87, stage 1)

The second row is right-aligned only when no notice is present. Existing notice
priority is input rejection, operation outcome, active recovery, then hints;
history/live-work focus cannot replace a selected notice. Activity facts and the
resolved-recovery filter remain #85's responsibility. Hints drop whole actions
from the right, retaining F1 at two columns and hiding below two columns. Safety
confirmation/shutdown keep their own panel instructions rather than advertising
an unavailable F1. The one-entry UI cache uses owner/mode, width and actual
send/newline bindings; elapsed ticks cannot invalidate it.

F1 explicitly raised over a popup temporarily covers that popup without deleting
its source, form or selection. A UI-only help-layer flag distinguishes that case
from ordinary readouts in the existing stack. Esc/F1 restores the prior owner;
confirmation and shutdown retain precedence. Editable model/filter/form text is
not case-normalized. Readonly detail, management and queue letters accept either
case. Form Tab means next field, not completion, and Enter retains the existing
send/validation/confirmation path. Empty composer image guidance is static and
width-conditional; it never infers clipboard contents or acquires an image.
The Thread browser retains its existing F1 selected-Thread information action;
its F1 fallback is contextual information, not an unrelated global help page.

### Canonical code selection bounds (#87, stage 2)

The existing Details popup hosts a separate transient code-selection view; it
never shares the field/link cursor. Preparation uses the existing bounded read
slots and presentation worker. At most 1MiB, 64 canonical history pages and 128
code blocks are inspected; clipboard payloads still have the 64KiB bound. An
incomplete source, repeated cursor, unavailable body or exceeded bound fails
without copying a prefix. Only numbered, sanitized 80-character previews and
Unicode counts reach the UI. Canonical text is read again for an explicit copy.
The request is bound to draft, Thread generation, selected item and node/request
identity; a late response cannot open a chooser or copy for another focus.
Bare /copy auto-selects a sole block or the whole body when no block exists;
b/B always opens numbered choices. Enter/c or digits 1-9 select a block, 0 copies
the whole body, Esc closes. /copy all selects the whole body explicitly. Source
reads and Markdown parsing are never done by UI drawing or key handlers.
Code offers carry a digest of the complete canonical source. A chosen block is
reread and checked against that digest before clipboard work, so a changed body
cannot silently change the numbered target. Closing the chooser cancels pending
preparation even before admission; no additional read pool is introduced. Copy
feedback reports the exact Unicode scalar count handed to the adapter and a
one-based block number. Image acquisition/preparation errors retain the original
code/details and suggest an existing image file via /attach, without probing on
ordinary paste or claiming that every environment supports clipboard images.
Complete assistant Markdown projections add numbered copy captions beside code
surfaces, using the same one-based parser order as the canonical chooser. These
captions are rendered decoration only; partial/evicted projections do not claim
global block numbers. The b shortcut opens that answer's numbered chooser, then
one digit copies the selected full block without typing a node identity.

## Tool description labels

When shell.describe_calls is enabled, compact tool identity uses its nonblank,
sanitized model-authored description instead of command text. The existing
history item focus/open state reveals the command; details and raw provenance
continue to expose the real command. Losing focus restores the compact label.
No independent selection, lifecycle or disclosure state is introduced. Description
text is not evidence that the actual command is safe or matches its explanation.

Command metadata keeps its existing 512-character bound. Focus/disclosure labels
that text as a preview and supplies an exact-call command-original action (`o`).
F6/original reuse the existing read-only, cursor-bounded source pages to reach
all command text, including adjacent calls and long-command tails. Do not eagerly
materialize all calls or increase aggregate preview retention for this label.

Original pages are the verified shared assistant/source node and can include
adjacent calls. The selected call ID and actual command preview identify the
chosen item; the view does not claim an exclusive per-call raw stream. All of
the selected command remains reachable through those bounded source pages.

## Bounded notification references (#88)

Extend #83's minimum without changing Core events, protocol, schema or canonical
history. The existing admitted frontend reader resolves at most 128 event-node
IDs in one indexed metadata statement. It follows node.event_id to the saved
event, execution to the unique tool_call.operation_id and matching shell/group
Turn, and child Run to its saved Thread. Duplicate call IDs or similar commands
are never identity evidence. Select bounded command/title/identity fields only;
never return provider bodies or shell output for display. Missing, deleted and
expired sources remain explicit. Current Thread titles say "current name";
a recorded title is not silently replaced with a claimed historical title.

Reuse the finite presentation worker, source metadata allowance, existing
read-revision/generation fences and paused-source observation scope. Decoration
lives in bounded /ui_notice fields, not original text or source offsets. Existing
Changed notifications invalidate only the selected view's bounded referenced
Threads; no poller, owner scan, per-notification query, new worker or queue.
Late observations cannot revive a replaced scope, renamed view or deleted source.

Enter first follows an exact loaded execution row. Otherwise an exact saved
(group,call,node) reference opens that original source through the existing
Details reader; it must not fall back to a nearby node or search by name/call ID.
Child navigation uses the saved/resolved Thread identity and never a short name.
Original-source reads occur only on explicit action. Unknown/deleted targets
retain factual identifiers and a clear unavailable message instead of guessing.

Synthetic validation covers repeated call IDs, equal short names, rename/delete,
run-only child references, missing/expired source, out-of-window original access,
stale scope/generation replies and query plans/counts. Functional PTYs cover
80/wide, dark/light/NO_COLOR/vt100 without a host clipboard or user data. Record
P1 observations separately; latency/CPU observations are not acceptance gates.
The existing finite background-result channel additionally carries one bounded
reference set (at most 128 IDs for the 128-node current page). The independent
paused scope remains bounded to 128 nodes. Selection owns these references and
adopts the current-page set only for the current Thread/generation/
progress epoch; switching views clears it. This is metadata on an existing
reader completion, not a durable event, queue or subscription. Unrelated Changed
IDs do not refresh it. The existing paused scope supplies older visible sources.
