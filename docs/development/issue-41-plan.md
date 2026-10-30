# Conversation redesign delivery plan (#41)

Baseline: main1fec7f0. Follow the issue body and comments through6518940167.
The latest user instruction permits independent usable main increments, each
with complete local standard checks and main three-OS CI. Do not close #41
until every adopted requirement is delivered. Keep #58 separate and first;
English migration (#35) follows #41. Each implementation increment and meaningful
subpart gets a reviewable commit; avoid a combined redesign commit.

## Ordered increments

| Increment | Scope | State |
| --- | --- | --- |
| Prerequisite #58 | Follow ownership, paid-text retention, body hydration, stale replies and private-safe diagnostics | Implemented/locally verified; standalone main CI gate next |
| A: adopted design | Stable node/tool/work identities, bounded projections, read limits, input/focus and validation decisions | Planned |
| B: conversation structure | User/sender bands, distinct fixed header, one collapsed tool row, failures with true tail, concrete notifications and quiet chrome | Planned |
| C: history interaction | Inline expansion/focus/click, stable F6/F7/original/copy targets, retained composer recall | Planned |
| D: live work | Foreground/background shells, owned children/waits after Turn completion, bounded expansion/output reads, real saved elapsed time | Planned |
| E: browser reactivity | Shared ID-restricted summary query, bounded coalescing and stale guards, stable filtered tree patches, state colors | Planned |
| F: watch/descendants | Existing-browser watch mode, unread session policy, confirmed actions, bounded lineage counts and scoped entry | Planned |

Each increment must be usable independently. A requirement that crosses these
boundaries must land with its complete action/read/render path, not a stub.
Report shipped and remaining scope on the issue after each main admission.

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

## Decisions to settle in adopted design

The global shell-output Notify cannot identify which shell emitted data. Use
saved last_output_at or an explicit bounded keyed in-process observation; never
invent per-shell times from the global signal. Stateful encoding tail reads must
not secretly scan the entire log. Watch state age distinguishes saved facts from
local observation. Patches retain finite pagination/watermarks and deduplicate
IDs without fetching every page. None justifies a new persistent job engine,
table, worker, business event or protocol operation.
