# Conversation redesign delivery plan (#41)

Baseline: main e8c1ff2 after independently completed #58. Follow the issue body
and comments through6524373262 (including the #59 sender boundary).
The latest user instruction permits independent usable main increments, each
with complete local standard checks and main three-OS CI. Do not close #41
until every adopted requirement is delivered. Keep #58 separate and first;
English migration (#35) follows #41. Each implementation increment and meaningful
subpart gets a reviewable commit; avoid a combined redesign commit.

## Ordered increments

| Increment | Scope | State |
| --- | --- | --- |
| Prerequisite #58 | Follow ownership, paid-text retention, body hydration, stale replies and private-safe diagnostics | Complete: e8c1ff2, native three-OS CI41034892332 green |
| A: adopted design | Stable node/tool/work identities, bounded projections, read limits, input/focus and validation decisions | Adopted; B+C shipped and freshness gate green; D–G open |
| B: conversation structure | User/sender bands, distinct fixed header, one collapsed tool row, failures with true tail, concrete notifications and quiet chrome | Shipped including freshness follow-up17c4271; native three-OS CI green |
| C: history interaction | Inline expansion/focus/click, stable F6/F7/original/copy targets, retained composer recall | Shipped including freshness follow-up17c4271; native three-OS CI green |
| D: live work | Foreground/background shells, owned children/waits after Turn completion, bounded expansion/output reads, real saved elapsed time | Local live UI/ownership/wait/output and delivery source807ac97 qualifies1777/0/32 plus6 actual PTYs; P2/P5/global planner and applicable native gates pending |
| E: browser reactivity | Shared ID-restricted summary query, bounded coalescing and stale guards, stable filtered tree patches, state colors | Implemented locally; initial three-OS CI exposed repaired same-deadline admission race. Rebased #77/#79 check, repaired-source CI and P3/P5 qualification remain gates; main admission awaits Claude |
| F: watch/descendants | Existing-browser watch mode, unread session policy, confirmed actions, bounded lineage counts and scoped entry | Planned |
| G: operations/forms | Shared operation catalogue/effects/terms/fields, complete CLI mapping, clear forms, nonselectable headings and popup wheel ownership | Planned |

Each increment must be usable independently. A requirement that crosses these
boundaries must land with its complete action/read/render path, not a stub.
Report shipped and remaining scope on the issue after each main admission.

The first B+C structural/interaction subset shipped as main b0e3c29. Source342cb6e
passes the native standard check and six actual PTY conditions; main three-OS
CI41057195099 passes at b0e3c29. P1/P3/P7 results and limits are in
[the qualification record](../validation/conversation-redesign-bc.md). This does
not itself complete every B+C inventory row. The later freshness follow-up below
shipped on17c4271 and passed its native main gate. D–G remain open; unchecked
cross-cutting requirements must not be inferred complete from the B+C shipment. Existing background management and F8 controls remain available
until the new live/watch/form increments are delivered.

## B+C freshness follow-up (review6523314574)

This is **B+C work**, not an implicit transfer to D–G. It shipped on17c4271
and passed its native main gate; the checklist below records its completed scope. The shipped
structure/interaction remains usable, but current cached facts may become stale.
No new worker/table/queue or timer-driven DB polling is authorized.

- [x] BC-R1: change title/model/connection/cwd through a separate CLI client;
  verify the selected header updates without input, measuring commit-to-paint
  against the1s unloaded-local target. Existing refresh code is not test evidence.
- [x] BC-R2: keep F6 open while its execution state or source availability changes;
  update mutable facts on relevant Changed without moving the selected identity.
  Coalesce reads and mark failed/lost refreshes stale/unavailable.
- [x] BC-R3: invalidate F7 cached links when exact source membership/revision or
  availability changes; reject delayed old replies. Verify actual source-change
  routes, not just mismatched request IDs in isolation.
- [x] BC-R4: identify valid immutable original/finished-preview snapshots with
  read/source time and an explicit refresh/reopen action. Current preview text
  only explains reopening and lacks the read time. Mutable lifecycle facts must
  remain reactive even when associated raw content is an identified snapshot.
- [x] BC-R5: F2 intentionally freezes display; show the time it was frozen and
  how to resume. Current frozen banner has only the resume instruction.
- [x] BC-R6: cover the remaining current-view inventory routes (footer/recovery,
  foreign pending sends and cancellation) with external-mutation latency tests,
  and document every untested route separately. Keep stale/late/read-budget and
  paid-text preservation regressions green.

Local follow-up sourcefd968e5 (including main ed40f33/#59) implements BC-R1–R6:
standard1,679/0/32,
four actual separate-CLI/current-view tests, six final native PTYs and P1
p95 3.974ms. See the qualification record for precise latency starts, snapshot
semantics, code limits and untested cases. Main admission17c4271 and exact-SHA three-OS CI41068514157 now succeed.
Downloaded timing artifacts confirm Linux1,680/0/31, macOS1,679/0/32 and
Windows1,563/0/24 (passed/failed/ignored; Windows threads4). This closes BC-R1–R6;
D–G and combined current-view read consolidation remain open.

Review choice at the first report: record the missing routes explicitly rather
than claim a rushed unvalidated fix. The follow-up above delivers code and local state/CLI/PTY evidence together;
its main gate is now green at17c4271. G still owns general F8 management
freshness and popup wheel/layout work. Shared one-job/500ms consolidation remains
open for the subsequent shared refresh refactor; measured seven jobs is not a
claim that the final one-job target has been met.

## Complete requirement ledger

- [x] Full-width multiline user bands with right-side muted saved time; distinguish
  human, parent, self and Revisit input; dark/light selection and non256/NO_COLOR.
- [x] Fixed header distinct from user bands; title prominent, state symbol/color,
  connection/model/cwd muted/right-aligned with leading cwd truncation.
- [x] One tool row per saved call identity, no ordinary IDs/repeated command/#N;
  right-side saved outcome/time/count only when known.
- [x] Successful tool output closed without raw reads on page load; failures open
  a small real-tail preview; on-demand expansion uses bounded existing limits.
- [x] True output tail beyond8KiB; honest encoding boundaries, controls stripped,
  carriage-return progress collapsed without unbounded reconstruction.
- [x] Concrete muted Turn-end notification facts; actionable failures remain visible.
- [x] History focus/keys/click and F6/F7/original/copy share stable selected identity
  across refresh, scroll, follow, expansion and eviction; preserve composer Up.
- [x] Quiet status/input chrome, conditional new count, help hint; no generic
  operation-prepared message; transient successes clear on input, failures persist
  until the next deliberate user action; activity and result messages are separate.
- [ ] At most3 oldest live work rows plus overflow; own shells/children/waits remain
  after the answering Turn. Sanitize all external strings and reuse detail targets.
- [ ] Background elapsed uses saved shell/Run start, not frozen observed Turn time;
  last-output age uses actual evidence, survives view switching, disappears at end.
- [ ] Closed live output causes no output DB reads. Open output at most2reads/sec,
  4KiB/read, per-execution cursor/tail skipping, stopped on close/end/view switch.
- [ ] E producer gap found during B+C audit: external Thread deletion currently
  returns no changed_threads. Publish exact affected IDs after commit (including
  cascade targets) before claiming external-delete list reactivity; test rollback
  and late patches cannot resurrect them. This is not fixed by a manual reload.
- [ ] Reactive list and watch refresh changed IDs only, bounded batches at most
  once/500ms, preserving selection/tree/filter/viewport; additions/removals/order
  use shared classification. Full reload only open/filter/Ctrl+R/pagination.
- [ ] Watch in existing browser: direct entry, one-key switch, session mode memory,
  priority order, parent context, two lines/narrow fallback, queue/Revisit counts.
- [ ] Session-only unread completions; read rows remain until watch closes;
  category-only reorder, local clocks never reorder/query; no timed disappearance.
- [ ] List-only deletion; watch-only confirmed Run stop/retry/output; reuse commands.
- [ ] Bounded descendant lineage counts, active versus recovery, excluding deleted
  Threads and independent of Run ownership; lower-bound truncation, zero hidden,
  changed-notification coalescing, one-key descendant-scoped watch entry.
- [ ] Shared state styling: active bright cyan/bold, completion subdued, waiting/
  background/retry yellow, recovery/failure red; noncolor labels remain present.
- [ ] Actual PTY captures80/wide and palette variants, synthetic large history,
  sustained output responsiveness, raw-query/batch/read counters and timing
  comparison. Never claim mock/auto tests establish physical Windows terminal UX.

- [ ] G: Wheel scrolls the top popup only under its painted bounds; outside/edge/no
  content does not redraw or move history; F2 ignores mouse.
- [ ] G: F8 section headings are nonselectable separate rows, skipped by arrows/pages;
  counts remain actions only, residual category is Other, filtering hides headings.
- [ ] G: One typed catalogue supplies operation meaning/effect, CLI identity, terms,
  field help/default source/discovery and confirmation policy; complete CLI/TUI
  coverage includes docs/RAG and attach shorthand. No generic form DSL.
- [ ] G: Forms explain the exact operation and effect; Enter is display/execute/send
  as appropriate, read-only RunGet is clear, empty IDs have discovery help; full
  operation-to-CLI and effect/label tests plus actual PTY form captures.

## Decisions to settle in adopted design

The global shell-output Notify cannot identify which shell emitted data. Use
saved last_output_at or an explicit bounded keyed in-process observation; never
invent per-shell times from the global signal. Stateful encoding tail reads must
not secretly scan the entire log. Watch state age distinguishes saved facts from
local observation. Patches retain finite pagination/watermarks and deduplicate
IDs without fetching every page. None justifies a new persistent job engine,
table, worker, business event or protocol operation.

## Mandatory cross-surface freshness and performance additions

Comments[6520522032](https://github.com/tya5/shellboy/issues/41#issuecomment-6520522032)
and[6520544888](https://github.com/tya5/shellboy/issues/41#issuecomment-6520544888)
are adopted in the [notification/snapshot inventory](../design/conversation-redesign.md#notification-snapshot-and-latency-inventory).
Every screen/popup fact has a notification/read owner, known gap, latency target
and explicit snapshot exception there. This includes existing F6/F7/F8 and #47
reference results, not only new E/F surfaces. All unchecked items remain pending;
this design update proves neither implementation nor measurement.

- [x] A/B+C: inventory-driven current conversation/header/footer/queue/F6/F7
  reactivity for CLI mutations; target1s visible updates; stale/lost/error labels.
- [ ] D/E/F/G: finish every remaining mutable-fact inventory row as its owning
  surface lands; no blanket fixed reference/result snapshot exception.
- [ ] All legitimate immutable/intentionally time-scoped/external-source snapshots
  show their observed/source time and explicit refresh/reopen action.
- [ ] B+C/D/E/F/G: one combined frontend refresh plan/deadline; per visible consumer
  <=1 DB-worker job/500ms, shared transaction/data, no hidden consumer work; usage
  totals retain the documented3s interval. Record actual jobs and DB time.
- [ ] B+C: bounded128-ID source/result batch, one in flight, source/read-revision
  cache; no query per rendered row/frame. Focus remains a paint overlay; only a
  changed expanded item is remeasured and its following geometry adjusted.
- [ ] D: owned-work projection shares activity transaction, skipped when not owned;
  output Notify compares visible markers only; background second clocks do not
  format/query/layout or repaint unchanged text.
- [ ] E: cache tree rows by item/collapse/filter/mode revision; arrows/draw reuse it.
- [ ] F: descendant counts use one depth8/256-row recursive SQL with lightweight
  latest-Run facts, visible scopes only, one in flight; no per-descendant activity
  projection and no recount for unrelated non-new IDs. Watch details batched by
  visible IDs and preformatted off UI draw.
- [ ] G: popup wrapped rows cached by popup/page-revision/width, wheel coalesced at
  max_fps; operation catalogue const/LazyLock and filtered rows cached on changes.

### Required comparison ledger (baseline main1fec7f0)

Historical P1/P7 use the baseline above. The first dispatched E P2/P5 pair uses
the dispatch-time main912b2e2 and candidatecbc9960, as explicitly reported in
[comment6538392801](https://github.com/tya5/shellboy/issues/41#issuecomment-6538392801).
Do not relabel those values as a comparison against1fec7f0 or the later repaired
and rebased candidate. Qualification gaps remain unchecked below.

- [x] P1 B+C:128 nodes/50 tool rows, focus/open-close UI+draw p95<=8ms and<=baseline+2ms.
- [ ] P2 D/F: silent background shell10min, <=1 redraw/sec,0 DB jobs/tick, CPU no worse.
- [ ] P3 Each applicable increment: visible Changed jobs and DB-worker time with
  list/watch/descendant consumers enabled, showing combined/coalesced work.
- [ ] P4 F:256 descendants/depth8 one-query p95<=50ms plus bounded slow-only logging.
- [ ] P5 D/E/F: hidden output40 batches/sec ->0 visible redraws/0 output-driven DB jobs.
- [ ] P6 G:64KiB original,60 wheel events/sec ->frames<=max_fps and p95<=8ms/+2ms.
- [x] P7 B+C:128x16KiB expanded previews counted within cache.body_bytes, including
  retained metadata; discarded previews rehydrate correctly under #58 rules.
  Report the explicit finite active working-set exception (<=64KiB total for an
  active visible/focused preview at zero/tiny budget), never as an uncounted cache
  or a per-item allowance; inactive/offscreen preview copies remain reclaimable.

Measure the same synthetic fixture before/after; distinguish measurement from
functional tests and native PTY observation. Record justified target changes
before admission. No new worker/table/queue is authorized by these requirements.


## #59 integration before the next main admission

Comment6524373262 requires the newly saved sender boundary. Merge main ed40f33,
preserve external_cli versus trusted-human classification and original/F6
self-asserted names, and verify that source snapshots do not erase sender facts.
Pre-merge source4bf14a9 standard/PTY/P1 evidence is retained as such; repeat
qualification on the merged runtime/embedded guides before push. This is now
locally complete at fd968e5 (standard1,679/0/32, actual PTYs6/6, P1 gates pass);
main17c4271/CI41068514157 is green on all three OSes. No historical
null is upgraded into a verified sender or Core compatibility path.


G's later CLI/form catalogue must include the #59 `message send --sender-label`
argument and sender-inspection fields. Its name is self-asserted, never a source
of authority; preserve the sender contract rather than inventing a TUI-only
meaning or silently treating external CLI as a verified human.


## D0 shared-read local qualification

Commitsf43f7dd/55cdc67 separate the coherent selected-view snapshot from tree
caching. Merge2fd4197 integrates #59/#69 mainbb7e29b;67f6f09 adapts exact header,
path and stream-mapping integration observers. Native local standard1711/0/32
passes, including actual CLI mutations/GC and paired diagnostics. See
[qualification](../validation/conversation-redesign-shared-read.md) for observed
failures and boundaries. This does not check off global refresh consolidation,
D live work, E patches, F, G or their performance/native gates.

G's typed catalogue must reflect #69's `config init`/`config check` leaves and
all their current options in addition to #59 sender-label. Completeness follows
the current CLI, not the historical57-leaf count.

D0 selected-view measurement:7aea088→2fd4197 has7→1 actual DB jobs on every
128-node/50-tool sample. Worker p95 is3.675→3.940ms; do not report a worker-time
improvement. Open-popup/list/watch/descendant combined measurements remain future
P3 gates. A stale shared-target artifact was rejected, with source-witness and
per-sample job assertions required on the final after-run.

E producer prerequisite161b4c9 fixes the previously tracked external ThreadDelete
notification gap using existing postcommit Changed effects. Focused Store/Control
checks cover exact singleton/cascade IDs and effect-free replay/rejection/rollback;
full follow-up standard at161b4c9 passes1713/0/32. The notification producer fix is
not an implemented list patch consumer. Existing per-node SQL in D0 raw helpers
also remains to be converted to real batched queries before global P3 completion.

D0/D1 raw-helper audit: the reused control::db::thread_details still calls
thread_display_title for an empty stored title, which scans/parses saved user
payloads on the DB worker. Separate a bounded frontend title observation from
off-worker preparation before claiming SQL-only/global batch completion, without
silently changing CLI title fallback or adding Core compatibility policy.


## D1 local batching/header qualification

Source558b870 (including currentmain1b559b9) passes1745/0/32 standard checks.
Tools modern/projected scopes use actual2SQL (whole groups3), sender scopes2/4,
recovery1 and header1/3; independent query oracles and native unrelated-record
scaling tests preserve source/identity/time/byte semantics. No new schema/index
is needed: retained sources already have migration2's node index. Header/title
and initial draft JSON no longer parse in the frontend DB closure.

Same-main P3 selected workload:1workerjob retained, SQL176→26, worker p50/p95
4.813/9.826→3.334/4.359ms, VM16,602→22,632. Intermediate61,255VM revealed a reversed
selector plan and was fixed; higher remaining VM and measurement limits stay
explicit in [qualification](../validation/conversation-redesign-shared-read.md#d1-actual-sql-batches-and-bounded-raw-headers).
This closes the recorded D0 per-node SQL/header debt for those paths, not D live
work, E patches/F/G, global multi-consumer P3, or native/visual admission gates.

## E implementation seams after the D2 audit

Keep E's next complete unit separate from live-work display. Standalone
ThreadCreate/ThreadFork currently return no changed_threads; notify the exact
created ID after commit before claiming additions work. UIEvents already
forwards all existing Changed facts, so no broader subscription is necessary.

Extract saved summary facts from THREAD_LIST_SQL into an ordinary connection
helper for at most128 IDs and reuse the existing activity classifier. Do not
call ThreadGet or the CLI's unbounded title/excerpt derivation per changed row.
D2's at-most-three child title witnesses are an explicit bounded exception, not
an acceptable256-query/64MiB128-Thread browser batch. Design a set-oriented title
witness with a shared byte allowance before enabling that batch.

The existing frontend/read slots should own one visible-browser dirty set and
500ms deadline; a changed ID outside the loaded page still needs one bounded
summary lookup to discover matching additions. Closing clears that work. Patches
must preserve selected ID, screen row, expansion and filters, update ordering
from saved facts, remove deleted/nonmatching IDs, and reject old page/patch
snapshots so pagination cannot resurrect a deletion. Keep preview-body hydration
separate from summary patches: current list rows do not need excerpt bodies,
but clearing a selected row's known excerpt silently would be a regression.
These are implementation constraints, not completed E validation.


## D2 local live-work functional qualification

Reviewable commits552e325/39b81cc/9bf7369/51a1eda/761c232/45b8b15 separate
pending waits, child-owner facts, output markers, incremental read cursors, UI and
frontend integration.78511d4 coalesces display snapshots in existing latest-value
slots;807ac97 adds bundled help/User Guides. Standard **1777/0/32** and actual
80/140-column ×dark/light/NO_COLOR PTYs6/6 pass. See
[live-work qualification](../validation/conversation-redesign-live-work.md).
This does not check off P2/P5/native GUI/OS or full multi-consumer P3, and does not
complete E/F/G. Preserve that distinction before admission.

The existing D1 title policy deliberately skips NULL/unparseable bodies and may
choose a later readable input (covered by its original tests/CLI-parity policy).
Changing that to limited/unknown evidence is a possible E policy refinement,
not a silent correction bundled into D2. Decide and document it before the
set-oriented title reader changes those cases.

## D2 isolated admission after review6535884761

37e0929 is a latest-main7393ccb candidate containing only reviewed D2 ancestry
through4ce6913. Its exact-source full standard recheck passes1827/0/32; retain
the initial native PTY diagnostic failure and unproven intermittent cause in
acceptance. The D2 cutoff contains no E/F/G source or schema15/16. Do not merge
`issue-41/shared-read` into main. E remains the next separately admitted unit
after D2's native main gate; F and G follow their own complete ledgers.

Review6536376198 adopts manual-CI measurement for long P2/P5 windows. Existing
`measure` currently measures test timings on Windows only; it does not yet
measure TUI CPU/draw/DB work. Extend that measurement path with exact-source
baseline/candidate, OS/terminal capability, synthetic fixture and counter/harness
witnesses, and upload structured results/logs. Measurements record values, with
no elapsed-performance assertions in CI; fixture failures remain failures.
Native Windows measurement support must be implemented/observed or explicitly
unavailable, never inferred from a Unix PTY. Publication/dispatch authorization
remains distinct from local preparation. No new local600s P2 window is started.

D2-only2e6973d now passes exact-source local standard1835/0/32 against latest
mainb581283; reviewed wait watchdogs are included and no E/F/G source/migration
is admitted. Retain the initial native diagnostic failure and no causal claim.
Do not admit E to main before D2's independent native main gate. Long CI
measurement implementation/dispatch remains open, not hidden behind old samples.


## E standalone admission ledger (2026-10-31)

This ledger covers List mode only, currently rebased onto main0d4f63a
(#75/#77/#79, schema15) in local merge79d8be2. F Watch,
descendant counts/migrations and G catalogue/forms/management observation are
excluded. A row identifies implementation and proof, not an automatic release
or push approval. Reviewer Claude owns the E push decision after the ledger is
reviewable. Exact-source results are appended to acceptance; pending gates below
must not be inferred from D2 or the combined development branch.

| ID | Requirement and implementation | Validation / admission state |
| --- | --- | --- |
| E1 | Shared canonical Thread summary/activity decoding and bounded128-ID saved-title batch. Core owns saved meanings reused by CLI and TUI; browser dirty state, paging and rendering stay in Frontend/UI. No new command/event/table/worker/index. | `control::db::thread_summary` parity, point-query plan and filter tests; `browser_read` two-statement metadata batch, body allowance and selected-only excerpt tests. Merged sourcea0d8865 standard1891/0/32 passes. |
| E2 | Exact Changed IDs after Thread create/fork/delete commit; rollback publishes nothing. Unloaded new rows are observed; missing/nonmatching rows disappear without normal full reload. | Core event tests and real separate-CLI frontend create/delete tests. Merged sourcea0d8865 standard1891/0/32 passes. |
| E3 | One bounded dirty set, one in-flight read,500ms admission; covered/closed browser has no scheduled reads. Overflow/lag alone requests explicit resync. | Browser ownership/coalescing/overflow tests use supplied clock instants, not real-time speed assertions. |
| E4 | One readonly WAL snapshot for visible conversation, receipts and optional browser patch. Standalone patches/pages use the same existing finite pool and progress permit. Admission-order sequence rejects late old snapshots; transient contention never becomes a persistence-failure latch. | Real writer-queue-held page/patch and combined-view tests, concurrent WAL/canonical-page ordering test, cancellation/error-mapping tests. No new polling or connection pool. |
| E5 | Stable selected identity/screen anchor, collapse/filter/tree cache, bounded overlays, canonical cursor/watermark and late page/patch fences. Off-page selection remains explicitly unconfirmed until exact observation. | `ui::threads` reactive/coverage/off-page suites and `thread_browser` integration; no Watch behavior included. |
| E6 | Shared saved activity classification, color and noncolor labels; current selected excerpt only; sanitized bounded titles and no blank fallback labels. | Existing classification/render tests plus Merged E+#75 native80/140 dark/light/NO_COLOR captures pass6/6. Native Windows terminal UX remains untested. |
| E7 | Preserve #71 auto-history admission and #73 summary behavior; preserve #54 conditional tick and dirty status retry. | E changes no `src/index` or Program Status implementation; tick guard remains `progress_running || usage_waiting || (status_enabled && status_dirty)`. Merged sourcea0d8865 standard1891/0/32 passes. |
| E8 | #77 single60s helper, notification-driven actual UI updates and scheduled visible-action wakeups; report latency without asserting machine speed. | Main #77 is merged. Standard layout/policy gate passes at0b7bd73; obsolete allowances for9 converted guards removed. Six exact occurrences in3 pure-clock UI tests remain reasoned/fingerprinted samples, not runtime watchdogs. No broad exception. |
| E9 | P3 visible List plus selected conversation coalescing/DB cost; P5 hidden40-batch/s output workload. | InitialCBC Linux P5 meets40commits/s with0 UI reads/draws; Mac16.62–17.77 does not. Full12pair report6538392801 records values and source limits. P3 and repaired-source load qualification remain open; strict shared-sequence tests are functional evidence only. |
| E10 | Exact-source standard check, secret scan, reviewer decision and ordinary3-OS main CI, then #41 scope report. | Approved measurement7c6f4d6 passes ordinary3OS branch CI41166258538: Linux1905/0/31, macOS1904/0/32, Windows1780/0/24. Actual paired-snapshot test passes in all3 artifacts. CBC failures remain recorded. Formal P2/P5 are observing; no E main admission or main CI, and later Claude approval remains required. |

Canonical open/filter/pagination still uses the existing CLI title/excerpt path
inside the readonly task. That path can issue per-row queries and scan filtered
candidates; bounded patch SQL and writer independence must not be presented as a
global constant-query or lower-CPU claim. Long P2/P5 measurements belong in the
manual CI measurement job, distinct from functional/native UI tests.


## Subsequent G mandatory copy/paste and attachment additions

These requirements are tracked now and implemented with G after separate E/F
admission. They do not change E's current key or CLI contracts.

| ID | Required outcome | Evidence required before G admission |
| --- | --- | --- |
| G-C1 | A manually typed leading slash command still executes when only its arguments contain pasted IDs. A wholly pasted leading slash remains literal, visibly explained rather than silently submitted as a command. | Typed `/copy ` plus pasted ID, wholly pasted command and input-hint tests plus actual PTY records. |
| G-C2 | One-key copy from focused answer (fallback reading position), clear composer/help/F2 escape route for long answers, and factual page/remaining instructions beyond64KiB. | Actual saved-original identity, Unicode/body/page boundaries and discoverability PTY records; host clipboard changes require explicit fixture isolation. |
| G-C3 | Popup letter keys accept upper/lowercase consistently; hints and operation catalogue agree. | Table-driven popup key parity and native records. |
| G-C4 | Per-code-block number/copy affordance; focus+one key or click copies the whole block from saved raw Markdown, never folded/wrapped paint text. | Long prompt/code examples, multiple blocks, exact original bytes/text and1–2-action native captures without entering node IDs. |
| G-C5 | Argumentless `/copy` offers the single code block directly and a choice when multiple blocks exist; whole-answer copy remains explicit and discoverable. Final syntax/default is decided in G's adopted design, with factual block number/character-count success notice. | Catalogue/help/command parser and original-block extraction agree; existing Markdown parser/extraction is reused. No independent rendering parser. |
| G-C6 | Empty-composer/help/slash discovery for `/image` and `/attach`; image-only paste guidance only when observable, and failure explains `/attach <path>` fallback. | Facts-only clipboard adapter cases and native discoverability capture; no guessed clipboard contents. |

Sources: review comments6536761757,6536813721,6536844049,6536999852 and
[6537521409](https://github.com/tya5/shellboy/issues/41#issuecomment-6537521409).
Copying a code block is UI/Frontend composition of the existing saved-original
read and Markdown extraction path, not justification for a new Core operation,
worker, table, parser or storage format.

## Additional conversation spacing review

[Comment6538457249](https://github.com/tya5/shellboy/issues/41#issuecomment-6538457249)
adds a small optional presentation improvement, tracked separately from E's
browser admission. It is not implemented or verified by E's existing tests.

| ID | Requested outcome | Required evidence / placement |
| --- | --- | --- |
| B-S1 | One presentation rule controls blank spacing at item-kind transitions, including answer/tool and notification boundaries; adjacent tool rows remain compact. | Adopt the concrete pair rules in conversation design before implementation; one shared layout policy. No saved nodes or extra selectable rows. Separate reviewable B presentation follow-up, after E's independent admission. |
| B-S2 | Padding participates in the same measured layout and viewport/focus mapping as drawing; F6/F7 still resolve actual saved items and blank lines cannot receive focus. | Boundary, wrapped text, scroll anchor and focus tests, plus actual80-column PTY captures. |
| B-S3 | Long-conversation scrolling retains P1 performance. | Repeat the applicable128-node/50-tool-row workload against the same baseline; do not infer speed from screenshots or a single layout test. |


### E merge of concurrent main #75

Main15037c1 adds readback schema15 independently of E. Preserve its shared finite
presentation pool, readonly CLI paths and readback fields/fixtures. E adds no
migration. F's previously proposed15/16 parent/view-lookup numbers now conflict:
recheck at F admission (currently next free16/17), update its isolated migration
files/ledger and retain the pre-upgrade backup warning. Neither old F migration
is imported here. #71 admission and #54 tick conditions remain unchanged.


### E CI observation gate after the initial run

The source-qualified [result report](https://github.com/tya5/shellboy/issues/41#issuecomment-6538392801)
keeps E9/E10 open: CBC ordinary Linux/Windows red; P5 Linux50Hz/0draws/0UIreads,
Mac below40commits/s; Mac80 P2 max2draws/1s-bin despite600/600s totals. P3 remains
unmeasured. Local E-only68304cc repairs the deadline priority and test guards,
includes latestmainc950 and qualifies1891/0/32 plusPython11; no follow-up push or
new three-OS CI is claimed. Keep this fixed cut separate from published CBC and
from its historical main912 comparison. Do not merge F/G or close #41 on this data.
