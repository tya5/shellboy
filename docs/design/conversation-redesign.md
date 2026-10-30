# Conversation presentation and reactive Thread browser (#41)

Adopted increment plan after #58 main e8c1ff2 and successful three-OS CI41034892332.
This design covers the issue body and comments through6519745314; the complete
implementation ledger is [the delivery plan](../development/issue-41-plan.md).
Only the next complete B+C increment is starting; D/E/F/G remain planned.

## Delivery and ownership

#58 must ship alone first. Each later main increment is usable independently, passes the local standard, and has green main CI on all three OSes before another main increment proceeds. Report delivered and remaining requirements after each admission; close #41 only when the complete ledger is satisfied. #35 follows #41. No new public protocol command, lifecycle state, schema, persisted UI state, business event, worker or queue is required. Existing Core facts may gain a narrow bounded in-process observation only where D explicitly requires it.

A is the adopted design commit: this text, effective-spec link, complete requirement ledger and decisions. A changes no product behavior and does not advertise features. B+C is one main-admission unit: conversation structure and working inline interaction must ship together. Internal commits may separate projection/read primitives, render/focus, and tests/guides, but a collapsed row may not ship without a working way to open it. D adds live work; E adds reactive browser rows; F adds watch and descendants. E/F may be admitted together if their query sharing makes that smaller. No unfinished screen/key or successful placeholder implementation enters main.

B+C is intentionally useful without D: existing activity and background controls remain functional while the new fixed live-work region is not yet present. Remove no existing background access until D supplies its replacement. Report the absent live region, background elapsed correction, browser patches/watch and descendant counts as remaining #41 work, not hidden completion claims.

## Common presentation model

Retain the canonical DAG, source offsets, saved tool results, immutable receipt assignment and #58 body/follow/read fences. The UI model is a projection, never a rewritten history.

Use internal identities:

- Message: Node(node_id).
- Tool: Tool(group_id, call_id), not call_id alone, ordinal, display label or execution ID. Pending/cancelled calls may have no execution.
- Live shell: Execution(execution_id); live child: Run(run_id); wait: saved wait operation ID.

Add group_id: Option<String> to ToolPresentation as display-only enrichment, populated by frontend/tools.rs from its already validated SOURCE_SQL correlation. No wire type is changed. For uncorrelated source data, retain a node-local tool key and the existing original/details route; never merge two uncertain calls by repeated #1/call_id labels. Extend the ordinary module rather than creating a general identity service.

A conversation item carries its stable identity, source node IDs, kind, canonical ordinal anchor, saved timestamp, relevant tool facts and expansion availability. A projected tool row merges call/result by the saved key even across source-window boundaries. Nonadjacent source members are not a contiguous ordinal span: suppress only the duplicate tool block, preserve intermediate prose, and keep each emitted row's actual node/source anchor. Saved result facts can enrich call-only rows through bounded exact-source metadata; do not fetch hidden raw output merely to correlate an item. When a result is outside the loaded source window, perform a bounded exact-source lookup; until then the row shows the known call state and an explicit unavailable/loading affordance, never an invented successful result. A call embedded with prose does not hide the surrounding prose.

Ui history focus is Option<ItemKey>; ordinary conversation bodies default open, while successful tool output defaults closed. Enter/Space operates on the selected item's available fold/preview; original/copy access remains independent.  expansion state is a bounded LRU of at most128 identities per selected conversation, retained across body/layout eviction and cleared when its Thread view is discarded. UI source-window/cache budgets remain those of #58. No entire Turn is pinned. Geometry maps click regions and F6/F7 to the same ItemKey; the mapping is regenerated on layout and width changes. Late replies must match Thread/generation, ItemKey and per-item request revision.

## B+C: bands, rows, interaction and quiet chrome

Add the frontend configuration enum ui.palette = "dark" | "light", default dark, using the existing strict config Ui section and validation/sample/docs path. No runtime model setting or database migration. Palette resolution is immutable for one UI process. A small UiPalette value is passed to render functions rather than mutable global colors. Retain existing OSS Markdown parsing/rendering and paired code surfaces.

Capability resolution happens before the UI loop: NO_COLOR wins and uses plain text plus spacing; known 256/truecolor terminals use the chosen palette; other terminals use reverse/separator fallback. Do not probe terminal background using blocking terminal I/O. NO_COLOR may also need nonchromatic selection cues, so retain markers, labels and borders rather than relying on color.

Human messages are full-width bands covering every wrapped content line, with the saved time muted at the right where space permits. Parent/self/Revisit inputs have a source marker independent of color. Do not classify every role=user as a human: use receipt sender/origin and the saved Revisit event facts. Width reduction sacrifices the timestamp before the message content. Plain mode uses the human marker and blank separators.

The fixed header uses a different band or bottom rule from user bands. Title is prominent, activity has symbol/color, and connection/model/cwd are muted right-side fields. Cwd truncates from the beginning. Footer and composer have their own separators. Narrow width drops optional metadata instead of wrapping the fixed chrome unpredictably.

Render a tool as one row: symbol + sanitized command at left; saved exit/duration/known line count at right. Hide normal IDs and #N. Unknown line count is absent. Successful output defaults closed. Failure opens a bounded true-tail preview automatically; cancellation/error without an execution uses the saved structured reason and performs no raw read. An explicit user close overrides automatic failure expansion until that tool identity changes outcome.

Each stream remains visually distinct through existing block styling; no stdout/stderr title is needed. Omission captions use known numerical counts only; do not infer omitted line counts from bytes or a truncated prefix. Preserve exact original/copy/link targets even though the screen is abbreviated.

Group concrete quiet observations at the end of their user-message section. Do not label them state changes/ordinary/other. Preserve important interruption/recovery/error instructions as visible actionable rows. A rendered section can begin before its user node is loaded: never fabricate a user band or move unrelated notices across canonical order.

Keyboard contract: Tab in composer with no completion enters history; arrows choose items; Enter/Space toggles selected inline content; o/c open/copy originals; Esc/Tab returns to composer. Empty-composer Up retains sent-message recall. F6 details and F7 links target the same selection, with a deterministic current/nearest visible fallback if focus has not been established. Clicking a row toggles it only with mouse capture. Item hints appear only in history focus. Explicit detail access remains available for every item, including nodes with no tool output.

Replace overloaded view.status with separate ephemeral OperationNotice {kind:Success|Failure, text} and existing factual activity state. No operation-prepared placeholder. Opening an overlay produces no artificial notice. A concrete success clears on the next input or completion of its command flow; a failure remains until a deliberate subsequent user action. Do not clear a failed send before its restored input can be understood. Footer uses short symbols/counts, ? help and conditional down/new count; remove composer key legends and generic follow/accepted prose. Preserve #52 sending/queued/error visibility with symbols and concrete error text.

## Output-read API, bounds and decoding

Successful closed tools on page load have zero raw-output DB reads. frontend/streams.rs continues metadata/recovery/usage enrichment but raw enrichment is invoked only for failed default previews or explicit expansion. Do not route display reads through ShellOutputReader::read_stream: it writes read pins and reconstructs decoders from byte zero for a cursor, so it cannot establish the required bounded independent read path.

Add narrow connection-based preview helpers in control/output.rs, shared decoder/sanitizer primitives where useful:

    preview_tail(db: &Connection, execution: &str, limits: PreviewLimits) -> PreviewPage
    // D adds incremental reading only when its live-output consumer exists.

These are crate-private in-process APIs, not new CLI/protocol semantics. PreviewCursor contains execution ID, saved sequence/stream offsets, retention_generation and canonical encoding; PreviewPage contains bounded stdout/stderr decoded segments, next cursor, skipped/decode-uncertain facts, terminal/output-complete and saved timing metadata. PreviewLimits is fixed call-site data, not another public config family. Query one SQLite read transaction so metadata/raw retention are consistent. Preview reads never override output_encoding or invalidate vectors.

Run the synchronous query/decoding on the existing finite frontend read/presentation blocking path, opening the existing DbWorker path with SQLITE_OPEN_READ_ONLY and query_only. Drop the connection/transaction when the bounded task ends; do not retain snapshots across sleeps. Existing read semaphore owns admission and existing cancellation/generation owns acceptance. No new DB thread or connection pool is introduced.

Query/copy limits bound returned rows and application bytes, not SQLite's internal filesystem I/O for giant BLOB pages. Use existing output_stream(execution_id,stream,stream_offset) to seek each stream's actual tail; use the primary key for sequence continuation. Use substr to copy only the bounded suffix from a large chunk. Bound both raw bytes and inspected/copied chunk rows (at most128 per preview); short fragmented output may return fewer bytes. Head/tail presentation reads share the existing per-tool byte allowance rather than paying it twice. Failure automatic preview budget is2KiB TOTAL, with a bounded share reserved for stderr when present. Explicit completed expansion uses existing limits.tool_excerpt_bytes (currently16KiB) TOTAL and maintains the true tail when abbreviated.

Use encoding_rs and the existing encoding resolver. Keep separate stream decoder state for contiguous incremental reads in frontend memory. A tail jump resets decoder state; UTF8/UTF16 boundary alignment can be repaired with a small lookbehind charged to the same budget. Stateful encodings cannot promise exact arbitrary-tail decoding: display a fixed decode-uncertain fact and retain original access rather than scan from the start or claim exact output. Trim ANSI/OSC/cursor/control/invisible-format characters with existing sanitization. A preview-only bounded current-line normalizer handles carriage-return overwrite before sanitization (the global sanitizer only strips CR); do not change CLI raw semantics. Cap decoded preview text by the existing excerpt/body budget.

## D: owned live work and accurate output activity

Extend frontend/activity.rs with LiveWorkSnapshot captured in the same existing refresh as activity: current Run ID, observed wall/monotonic time, total counts and at most3 oldest rows. Row enum Shell/ChildRun/Wait carries IDs, sanitized display text, saved start time, state, elapsed base and last_output_at where known. Shell commands come from saved shell_json, child titles/classification from the existing Thread projection. Wait rows reflect saved pending waits and do not count as extra child work. The query orders by saved start and ID, uses existing owner indexes and caps returned rows; counts are scalar queries. Completed work disappears on the next Changed refresh. The fixed region sits immediately above composer and remains after the Turn answer when the Run still owns work. Small screens collapse overflow before suppressing composer/history usability.

Background footer derives oldest-work elapsed and last real output from this same snapshot, never a stopped Turn observation clock. Local Instant advances saved elapsed without DB reads; switching away/back reloads saved timestamps. Unknown saved timing remains unknown. No animation tick performs DB work.

Precise per-shell output activity needs a narrow ephemeral extension to the existing LiveRegistry shell entry: Option<Instant> last_committed_output (or equivalent scalar). collect_shell updates that entry only after an output-batch commit, before the existing global shell_output_changed notification. The entry is installed before collection and removed with its existing handle, so storage is bounded by already-live shells and has the same owner/cleanup. A crate-private getter accepts at most3 displayed execution IDs and copies those scalars under the existing async registry lock. It performs no I/O/DB read and creates no event/queue. Frontend listens to the existing Notify, looks up only displayed live executions, and coalesces repaint in the existing cadence. Never label another shell's output as activity of the selected shell. Initial/past output age is supplied by #45 saved last_output_at in the normal activity snapshot.

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

Prove a complete mapping between catalogue operations and executable CLI leaves,
including standalone docs/RAG and attach shorthand; parser help/version and UI
launch are not operation leaves. Do not hide missing commands by testing only the
old OpKind.ALL inventory. Introduce no successful stub for an unsupported form.

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
