# Conversation redesign delivery plan (#41)

Baseline: main e8c1ff2 after independently completed #58. Follow the issue body
and comments through6520544888.
The latest user instruction permits independent usable main increments, each
with complete local standard checks and main three-OS CI. Do not close #41
until every adopted requirement is delivered. Keep #58 separate and first;
English migration (#35) follows #41. Each implementation increment and meaningful
subpart gets a reviewable commit; avoid a combined redesign commit.

## Ordered increments

| Increment | Scope | State |
| --- | --- | --- |
| Prerequisite #58 | Follow ownership, paid-text retention, body hydration, stale replies and private-safe diagnostics | Complete: e8c1ff2, native three-OS CI41034892332 green |
| A: adopted design | Stable node/tool/work identities, bounded projections, read limits, input/focus and validation decisions | Adopted; B+C locally qualified, main CI pending |
| B: conversation structure | User/sender bands, distinct fixed header, one collapsed tool row, failures with true tail, concrete notifications and quiet chrome | Local standard/PTY/P1/P3/P7 qualified; main admission/CI pending |
| C: history interaction | Inline expansion/focus/click, stable F6/F7/original/copy targets, retained composer recall | Local qualification complete with B; main admission/CI pending |
| D: live work | Foreground/background shells, owned children/waits after Turn completion, bounded expansion/output reads, real saved elapsed time | Planned |
| E: browser reactivity | Shared ID-restricted summary query, bounded coalescing and stale guards, stable filtered tree patches, state colors | Planned |
| F: watch/descendants | Existing-browser watch mode, unread session policy, confirmed actions, bounded lineage counts and scoped entry | Planned |
| G: operations/forms | Shared operation catalogue/effects/terms/fields, complete CLI mapping, clear forms, nonselectable headings and popup wheel ownership | Planned |

Each increment must be usable independently. A requirement that crosses these
boundaries must land with its complete action/read/render path, not a stub.
Report shipped and remaining scope on the issue after each main admission.

B+C are one usable implementation unit. Source342cb6e passes the native standard
check and six actual PTY conditions; P1/P3/P7 results and limits are in
[the qualification record](../validation/conversation-redesign-bc.md). Main
admission/three-OS CI remain pending, so shipment checkboxes are not yet marked.
D–G remain pending. Existing background management and F8 controls remain available
until the new live/watch/form increments are delivered.

## Complete requirement ledger

- [ ] Full-width multiline user bands with right-side muted saved time; distinguish
  human, parent, self and Revisit input; dark/light selection and non256/NO_COLOR.
- [ ] Fixed header distinct from user bands; title prominent, state symbol/color,
  connection/model/cwd muted/right-aligned with leading cwd truncation.
- [ ] One tool row per saved call identity, no ordinary IDs/repeated command/#N;
  right-side saved outcome/time/count only when known.
- [ ] Successful tool output closed without raw reads on page load; failures open
  a small real-tail preview; on-demand expansion uses bounded existing limits.
- [ ] True output tail beyond8KiB; honest encoding boundaries, controls stripped,
  carriage-return progress collapsed without unbounded reconstruction.
- [ ] Concrete muted Turn-end notification facts; actionable failures remain visible.
- [ ] History focus/keys/click and F6/F7/original/copy share stable selected identity
  across refresh, scroll, follow, expansion and eviction; preserve composer Up.
- [ ] Quiet status/input chrome, conditional new count, help hint; no generic
  operation-prepared message; transient successes clear on input, failures persist
  until the next deliberate user action; activity and result messages are separate.
- [ ] At most3 oldest live work rows plus overflow; own shells/children/waits remain
  after the answering Turn. Sanitize all external strings and reuse detail targets.
- [ ] Background elapsed uses saved shell/Run start, not frozen observed Turn time;
  last-output age uses actual evidence, survives view switching, disappears at end.
- [ ] Closed live output causes no output DB reads. Open output at most2reads/sec,
  4KiB/read, per-execution cursor/tail skipping, stopped on close/end/view switch.
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
