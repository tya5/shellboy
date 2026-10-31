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

Ui history focus is Option<ItemKey>; ordinary conversation bodies default open, while successful tool output defaults closed. Enter/Space operates on the selected item's available fold/preview; original/copy access remains independent.  expansion state is a bounded LRU of at most128 identities per selected conversation, retained across body/layout eviction and cleared when its Thread view is discarded. UI source-window/cache budgets remain those of #58. No entire Turn is pinned. Geometry maps click regions and F6/F7 to the same ItemKey; the mapping is regenerated on layout and width changes. Late replies must match Thread/generation, ItemKey and per-item request revision.

## B+C: bands, rows, interaction and quiet chrome

Add the frontend configuration enum ui.palette = "dark" | "light", default dark, using the existing strict config Ui section and validation/sample/docs path. No runtime model setting or database migration. Palette resolution is immutable for one UI process. A small UiPalette value is passed to render functions rather than mutable global colors. Retain existing OSS Markdown parsing/rendering and paired code surfaces.

Capability resolution happens before the UI loop: NO_COLOR wins and uses plain text plus spacing; known 256/truecolor terminals use the chosen palette; other terminals use reverse/separator fallback. Do not probe terminal background using blocking terminal I/O. NO_COLOR may also need nonchromatic selection cues, so retain markers, labels and borders rather than relying on color.

Human messages are full-width bands covering every wrapped content line, with the saved time muted at the right where space permits. Parent/self/Revisit inputs have a source marker independent of color. Do not classify every role=user as a human: use receipt sender/origin and the saved Revisit event facts. Width reduction sacrifices the timestamp before the message content. Plain mode uses the human marker and blank separators.

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

## Cross-surface reactivity and performance admission (comments6520522032/6520544888)

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
| Fixed live region: shells, owned children, waits | Relevant Thread Changed in activity transaction; global output Notify only compares at most3 displayed live markers | New D surface | No fixed snapshot for live state; elapsed base includes observation time |
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
tools, raw-output or receipt enrichment, and retain at most128 links in that
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
batch (at most128 node/call IDs), with at most one in flight and cache by
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

### Mandatory performance evidence per applicable increment

Compare the same synthetic fixtures against baseline main1fec7f0; report native
OS/terminal, fixture counts, command, samples and unmeasured cases. These are
admission measurements, not claims inferred from passing functional tests:

1. B+C:128-node window/50 tool rows, repeated focus and open/close. UI handling +
   draw p95 target<=8ms and no more than2ms above baseline; focus must reuse layout.
2. D/F: one silent background shell for10min: at most1 redraw/sec, zero DB jobs per
   clock tick, and CPU no worse than baseline; report CPU/sample uncertainty.
3. B+C/D/E/F/G: visible active Thread Changed with applicable list/watch/count
   consumers: DB-worker jobs and aggregate time before/after, with shared batching.
4. F:256 descendants/depth8: one-query p95 target<=50ms, and slow-only diagnostics
   through the existing bounded writer; no per-descendant diagnostic flood.
5. D/E/F: hidden Thread output at40 batches/sec causes zero visible redraws and
   zero output-induced DB jobs for visible consumers.
6. G:64KiB original popup,60 wheel events/sec: frames<=max_fps and UI/draw p95 within
   the same<=8ms/+2ms target, using cached wrapped rows.
7. B+C:128 expanded previews at16KiB each: retained bytes conform to the shared
   body-cache accounting (including metadata), with correct eviction/rehydration.

If a target requires adjustment based on native baseline evidence, record that
reason and measured impact before admission; do not silently drop the requirement.


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

For an empty stored title, inspect a prefix of at most128 selected-path entries
(plus one sentinel), then accepted/held receipt prefixes of at most128 per state
(plus sentinels), using the existing path/receipt indexes. Inspect prefix membership
before node-kind/path exclusion rather than scan an unlimited history to find128
users. Only user JSON bodies can be copied: at most128KiB each and512KiB total
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

Before the activity DB read, copy at most128 existing WaitRegistry entries as
(operation ID, source shell execution ID), without credentials or new registry
fields. CLI transport admits at most24 ordinary requests, but direct Control
callers are not globally limited by that semaphore: an over128 snapshot is
explicitly unavailable, never a truncated global prefix. Resolve exact saved
shell ownership and wait.pending operation keys inside the same activity read
transaction, filter by the saved current Run, then sort by saved start/ID and
retain the oldest3 rows. A removed OS handle does not erase a still-pending saved
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

Only the at most3 displayed owned-child rows acquire bounded title witnesses
through the same frontend header title policy: explicit title first, otherwise
at most128 path/pending membership candidates and512KiB copied evidence per
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

Frontend seals the at most3 displayed ChildRun Thread IDs to the accepted current
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

Each observation uses one read-only transaction, at most128 chunk metadata rows
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
kick; this covers counts even when the child is outside the oldest3 visible
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
slots. Every marker snapshot includes all observed displayed executions (at most3),
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
merged main; preserve #71 auto_history and #54 conditional status ticks.


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
