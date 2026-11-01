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
