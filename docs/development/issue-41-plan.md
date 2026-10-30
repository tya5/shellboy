# Conversation redesign delivery plan (#41)

Baseline: main e8c1ff2 after independently completed #58. Follow the issue body
and comments through6523314574 (the first B+C review).
The latest user instruction permits independent usable main increments, each
with complete local standard checks and main three-OS CI. Do not close #41
until every adopted requirement is delivered. Keep #58 separate and first;
English migration (#35) follows #41. Each implementation increment and meaningful
subpart gets a reviewable commit; avoid a combined redesign commit.

## Ordered increments

| Increment | Scope | State |
| --- | --- | --- |
| Prerequisite #58 | Follow ownership, paid-text retention, body hydration, stale replies and private-safe diagnostics | Complete: e8c1ff2, native three-OS CI41034892332 green |
| A: adopted design | Stable node/tool/work identities, bounded projections, read limits, input/focus and validation decisions | Adopted; structural/interaction subset shipped; B+C freshness follow-up open |
| B: conversation structure | User/sender bands, distinct fixed header, one collapsed tool row, failures with true tail, concrete notifications and quiet chrome | Structural subset shipped in b0e3c29; B+C freshness follow-up open |
| C: history interaction | Inline expansion/focus/click, stable F6/F7/original/copy targets, retained composer recall | Interaction subset shipped in b0e3c29; B+C freshness follow-up open |
| D: live work | Foreground/background shells, owned children/waits after Turn completion, bounded expansion/output reads, real saved elapsed time | Planned |
| E: browser reactivity | Shared ID-restricted summary query, bounded coalescing and stale guards, stable filtered tree patches, state colors | Planned |
| F: watch/descendants | Existing-browser watch mode, unread session policy, confirmed actions, bounded lineage counts and scoped entry | Planned |
| G: operations/forms | Shared operation catalogue/effects/terms/fields, complete CLI mapping, clear forms, nonselectable headings and popup wheel ownership | Planned |

Each increment must be usable independently. A requirement that crosses these
boundaries must land with its complete action/read/render path, not a stub.
Report shipped and remaining scope on the issue after each main admission.

The first B+C structural/interaction subset shipped as main b0e3c29. Source342cb6e
passes the native standard check and six actual PTY conditions; main three-OS
CI41057195099 passes at b0e3c29. P1/P3/P7 results and limits are in
[the qualification record](../validation/conversation-redesign-bc.md). This does
not complete all B+C inventory rows: the freshness follow-up below remains open
alongside D–G. Unchecked cross-cutting requirements must not be inferred complete
from shipment of their structural subset. Existing background management and F8 controls remain available
until the new live/watch/form increments are delivered.

## B+C freshness follow-up (review6523314574)

This remains **B+C work**, not an implicit transfer to D–G. Complete it before
claiming B+C done; the next main admission is this follow-up. The shipped
structure/interaction remains usable, but current cached facts may become stale.
No new worker/table/queue or timer-driven DB polling is authorized.

- [ ] BC-R1: change title/model/connection/cwd through a separate CLI client;
  verify the selected header updates without input, measuring commit-to-paint
  against the1s unloaded-local target. Existing refresh code is not test evidence.
- [ ] BC-R2: keep F6 open while its execution state or source availability changes;
  update mutable facts on relevant Changed without moving the selected identity.
  Coalesce reads and mark failed/lost refreshes stale/unavailable.
- [ ] BC-R3: invalidate F7 cached links when exact source membership/revision or
  availability changes; reject delayed old replies. Verify actual source-change
  routes, not just mismatched request IDs in isolation.
- [ ] BC-R4: identify valid immutable original/finished-preview snapshots with
  read/source time and an explicit refresh/reopen action. Current preview text
  only explains reopening and lacks the read time. Mutable lifecycle facts must
  remain reactive even when associated raw content is an identified snapshot.
- [ ] BC-R5: F2 intentionally freezes display; show the time it was frozen and
  how to resume. Current frozen banner has only the resume instruction.
- [ ] BC-R6: cover the remaining current-view inventory routes (footer/recovery,
  foreign pending sends and cancellation) with external-mutation latency tests,
  and document every untested route separately. Keep stale/late/read-budget and
  paid-text preservation regressions green.

Local follow-up source4bf14a9 now implements BC-R1–R6: standard1,671/0/32,
four actual separate-CLI/current-view tests, six final native PTYs and P1
p95 3.324ms. See the qualification record for precise latency starts, snapshot
semantics, code limits and untested cases. These boxes await main admission and
its exact-SHA three-OS CI; local success alone does not mark shipment complete.

Review choice at the first report: record the missing routes explicitly rather
than claim a rushed unvalidated fix. The follow-up above delivers code and local state/CLI/PTY evidence together;
its main gate is still pending. G still owns general F8 management
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

- [ ] A/B+C: inventory-driven current conversation/header/footer/queue/F6/F7
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

- [ ] P1 B+C:128 nodes/50 tool rows, focus/open-close UI+draw p95<=8ms and<=baseline+2ms.
- [ ] P2 D/F: silent background shell10min, <=1 redraw/sec,0 DB jobs/tick, CPU no worse.
- [ ] P3 Each applicable increment: visible Changed jobs and DB-worker time with
  list/watch/descendant consumers enabled, showing combined/coalesced work.
- [ ] P4 F:256 descendants/depth8 one-query p95<=50ms plus bounded slow-only logging.
- [ ] P5 D/E/F: hidden output40 batches/sec ->0 visible redraws/0 output-driven DB jobs.
- [ ] P6 G:64KiB original,60 wheel events/sec ->frames<=max_fps and p95<=8ms/+2ms.
- [ ] P7 B+C:128x16KiB expanded previews counted within cache.body_bytes, including
  retained metadata; discarded previews rehydrate correctly under #58 rules.
  Report the explicit finite active working-set exception (<=64KiB total for an
  active visible/focused preview at zero/tiny budget), never as an uncounted cache
  or a per-item allowance; inactive/offscreen preview copies remain reclaimable.

Measure the same synthetic fixture before/after; distinguish measurement from
functional tests and native PTY observation. Record justified target changes
before admission. No new worker/table/queue is authorized by these requirements.
