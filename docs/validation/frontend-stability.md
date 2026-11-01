# Frontend State-Driven Stability (#124)

Scope: the five cases in issue124, not production timing, admission, queue limits,
retention or shutdown grace. No test deadline value is increased, no assertion
is weakened, and no test is ignored or serialized to obtain a pass.

| Case | State/fact that permits progress | Existing diagnostic bound |
| --- | --- | --- |
| edit_adapter | Original/fork/management observations; then frontend exits while its bounded consumer stays live | shared update60s; shutdown5s |
| structured_history | Exact page/detail/wait-cancellation facts; then consumer drains until task termination | shared update60s; shutdown8s |
| persisted_output_identity | Consumer observes the selected Thread's current view before the proxy sends the first delta (the frontend ignores deltas for an unselected Thread), then exact stream/current-view identity before releasing each proxy phase | shared update60s; shutdown10s |
| unsaved_agents_round_trip | Initialize, exact origin draft AgentsSelected, then the durable same-Thread receipt in the database (the UI may hold or coalesce its receipt update), rechecked after each delivered update | Initialize/AgentsSelected 5s; receipt 60s watchdog |
| full_reserve_cancel_quit_force | Committed stopped Run; draft revision99 and40 exact held receipts; force-ready level; one explicit force result | original5/10/15/2s |

The three integration shutdown checks used to await the producer while no longer
consuming its bounded update channel. They now drain that consumer until the
JoinHandle completes, under the identical original guard. The state receiver
blocks via block_in_place on concrete channel delivery instead of scheduling a
5ms polling sleep; SavedMessage still exposes the same receipt/history facts.
It uses a fixed deadline once per phase, never a renewed timeout per update.

The stream proxy's two independent10s timers measured unrelated UI processing
latency while the producer waited for consumer commands. Replace them with the
existing oneshot phase states: a consumer failure drops its gate, while its
unchanged shared60s update watchdog diagnoses missing progress. No longer timer
is substituted on the proxy and no model IO timeout is changed.

The saturated-outbox fixture does not start consuming its deliberately stalled
raw UI channel. A cfg(test)-only broadcast-style notification records delivery
attempt completion after the outbox lock is dropped; it is separate from the
production single-consumer ready Notify, so observation cannot steal a wake.
The fixture registers before inspecting committed/retained state, and also uses
the existing committed Control broadcast. Counts/levels, not time or a number
of yields, decide success. Production code/layout has no added observer field.

## Evidence And Acceptance

Historical Windows sourcecd86d259, run41272742726 attempt1, fails at integration
frontend shutdown5s/8s and the producer's10s next-delta gate. Its same-SHA rerun
success does not erase those failures. The issue also records independent107/109
failures; do not infer a product regression or exact shared-pool root cause from
the timing symptoms alone.

Acceptance requires five consecutive actual Windows CI executions of the five
cases at one frozen SHA. Use ordinary ci.yml manual dispatches: the first native
three-OS run, followed by four Windows-only4-thread runs only after the preceding
run succeeds. Manual dispatch always executes and cannot reuse an earlier native
pass. Keep each run URL, SHA, attempt, Windows conclusion and per-case counts;
any failure stops the consecutive sequence and remains separate evidence.
No timing observation is a pass threshold and no local full standard run is used.

## macOS paired-observation baseline (#138, CI half)

- Original evidence: run `41301719896`, attempt 1, macOS job `124833906560`.
  `frontend_reactivity::selected_header_and_open_browser_share_one_changed_snapshot_without_reload`
  panicked at `tests/frontend_reactivity.rs:989`: `one selected observation`,
  actual `2`, expected `1`. The first Changed-to-paint observation was 498987 us;
  this was a count assertion, not a timeout or the informational 1 s target.
- The fixture's initial header/history readiness can be satisfied by the
  selection reader's separate `Thread` and `History` updates. It does not prove
  the initial `CurrentView` refresh has reached the test receiver. Likewise,
  a browser excerpt only proves the browser consumer is ready. Clearing the
  observation vectors at that boundary can count a pending setup `CurrentView`
  together with the first Changed refresh.
- Original correction (superseded by the Windows follow-up below): wait for
  both an initial `CurrentView` and the selected browser excerpt before
  clearing the vectors or issuing the first measured mutation. The readiness
  regression covers both delivery orders and specifically rejects browser-only
  readiness. A real-fixture regression also holds initial `CurrentView` updates
  while allowing `Thread`, `History` and the browser excerpt through, proves
  that the old readiness boundary can be reached, and releases the held updates
  explicitly. The baseline consumers need not have the same read sequence: a
  standalone initial browser observation is valid. Each of the 16 measured
  mutations still requires exactly one selected observation, a shared browser
  sequence, unchanged browser generation and unchanged page count.
- This is the same class of asynchronous fixture-readiness assumption as #124's
  Windows fixes (particularly awaiting current-view adoption before the first
  persisted-output delta), not evidence of the same Core or OS-specific defect.
  No sleeps, retries, timeout increases, assertion relaxation, or product
  behavior changes are introduced. The original failure is confirmed by CI;
  local diagnostic repetition did not reproduce it (11 focused passes and one
  passing related-module run before the fix).

### Windows committed-baseline follow-up (#138)

- Original failure: run `41342038037`, attempt 1, Windows job `124953965722`;
  `initial_browser_excerpt_does_not_complete_a_delayed_selected_view` timed out
  at `frontend_reactivity.rs:342`, with `bridge_finished=false` and no delivered
  CurrentView snapshots. Retrying succeeded; that does not erase the failure.
- Source tracing: SelectThread emits separate Thread/History, then activity.
  The initial fixture data predates subscription and emits no Changed. A
  periodic tick can admit a paired read before idle activity, but idle activity
  disables `Selection::progress_running`. Browser observation has independent
  admission and can finish without a selected CurrentView ever being produced.
  Waiting for a nonempty held vector therefore depended on a startup tick, not
  a guaranteed final state. Extending the watchdog cannot produce that read.
- Both affected fixtures now commit distinct baseline titles through the real
  separate CLI/owner Changed route. The delayed case requires a held CurrentView
  with the exact committed title plus browser adoption, verifies no selected
  adoption before release, then awaits selected adoption after release. The
  measured shared-header/browser case requires the matching snapshot identity
  and both adopted titles before clearing its vectors. Old startup snapshots
  and browser-only delivery cannot satisfy the committed boundary.
- The 16 measured changes retain exactly-one-selected-read, shared browser/read
  sequence, fixed browser generation and fixed page count. No production code,
  periodic rates, stale-read guards, timeout budgets, retries or assertions are
  relaxed. Watchdog diagnostics additionally retain bounded browser traces and
  the held-view count.
- Validation uses disposable databases, loopback-only/unreachable provider URLs
  and the real separate CLI. Local focused results and three consecutive
  same-SHA all-OS CI run IDs are supplied at handoff; they are not claimed by
  source inspection alone. Physical terminals/live providers and Windows-local
  execution are not tested by this fixture change.

Local macOS candidate preparation: `cargo fmt --all`,
`cargo check --locked --tests`, and
`cargo test --locked --test integration frontend_reactivity:: -- --nocapture`
passed (17 tests, 1032 filtered). This covers both baseline regressions and all
16 measured changes; it is not a Windows result or a local full standard run.
