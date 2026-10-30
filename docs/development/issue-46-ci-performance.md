# Native test performance investigation (#46)

## Baseline evidence

Source `0469ca0` was pulled before work. The historical successful native run
[40954267575](https://github.com/tya5/shellboy/actions/runs/40954267575)
uses `a45f54e`; it is retained as historical timing evidence rather than an
exact-current-source pass. Its Windows Tests step took 653 seconds: about 290
seconds in build/link and 359.83 seconds summed harness execution. The complete
job took about 855 seconds. Windows used two test threads. The repository has
82 integration source suites;86 harnesses includes library, main and examples.

| Windows harness | Seconds | Passed |
| --- | ---: | ---: |
| Library |79.61|576|
| storage |65.58|23|
| index |29.87|50|
| control_db |23.64|23|
| response_retries |20.09|22|
| response_upload_recovery |18.04|3|

The same-SHA main run 40954266583 failed different watchdog tests on its first
two attempts before succeeding: IPC waiter completion, then two attachment
mock-arrival waits. These are not treated as evidence of shared database/port
collisions or fixed by automatic retries. Preserve their names and diagnostics.

## Chosen changes and attribution

- Consolidate 80 ordinary suites into one harness; retain two self-spawning
  native suites. Integration links fall from 82 to 3. A registration checker and
  same-OS normalized inventories prevent silent coverage loss.
- Batch only historical raw fixture seed DDL and the 405 excluded-source inserts.
  Real DbWorker migrations, production FULL/WAL, tested Store transaction
  boundaries,405 actual embeddings and all assertions remain unchanged.
- Keep Windows concurrency at 2 for the first comparison. A separate four-thread
  experiment records failures rather than retrying until success. No conflicting
  shared mutable database/port/env fixture was identified by the source audit.
- Give deadlock watchdogs generous bounds while retaining the observed condition.
  IPC status must answer with all 24 waiters still pending, before their much
  longer expiry. This strengthens the causal admission check.
- Keep underlying native tests on every OS; make repeated diagnostics opt-in.
  Ordinary CI records build/harness times, failed names and inventories; isolated
  profiles are explicitly labeled and add process-start cost.
- Do not add nextest/nightly or Defender exclusions: stable Cargo/libtest plus
  Python standard-library measurement suffices, and Defender causality is unproven.
- Do not mechanically use 1ms or pause virtual time around real DB/socket work.
  Partial 16MiB upload source consumption is a native transport observation;
  a pre-upload 1ms expiry would test a different phase. Existing pure deadline
  and retry matrices already test policy without waiting. Further elimination
  of duplicate waits must preserve that boundary, not introduce a Core test mode.

## Current verification boundary

macOS ARM64 isolated before/after measurements use fresh process execution of
all 23 storage, 50 index and 23 control_db tests. Storage samples total 1.014→0.662s;
public-schema convergence 0.232→0.089s; v6/v7 matrix 0.189→0.072s. The405-node
index case is 0.913→0.902s and index's aggregate isolated time did not improve
(5.171→5.320s); do not claim a native Windows saving from these local samples.
control_db is unchanged and 1.417→1.510s. Cache/process overhead and runner noise
remain part of these isolated measurements.

The initial aggregate local run passes 1,452 tests, 0 failed, 28 ignored with
all 1,480 identities and the ignored set equal to the original 82-suite inventory.
This includes real Unix termination/crash tests; it is not Windows success.
Final standard-check and native before/after results belong in
[acceptance](../validation/acceptance.md). Native evidence below supersedes this initial local-only boundary.

## Native comparison started

The user authorized pushing only the measurement branch and running CI; main
remains unchanged. Source `70259d3` is pushed to `issue-46/test-performance`.
[Run 40975268982](https://github.com/tya5/shellboy/actions/runs/40975268982)
combines the ordinary three-OS check with Windows baseline/current two-thread
and current four-thread samples. The duplicate push-triggered run 40975197111
was cancelled before completion; it is not passing evidence.

The two-thread cap originated at `60847e3`, after Windows fixture cleanup slowed
under simultaneous DB/IPC/file workers; the historical record did not establish
a shared mutable database/port collision. The source audit found per-test stores,
dynamic active ports and child-only environment changes. Keep causal uncertainty
separate from the native contention experiment.

An additional current-source macOS four-thread run passes 1,452 tests with the
same 1,480 identities and 28 ignored identities, no failures. Summed native harness
wall time is 52.007s, versus 90.997s in the earlier two-thread aggregate sample.
These are single local samples; they do not establish Windows safety or speed.
The stable 1.99.1 test harness explicitly rejects `--report-time` without nightly
`-Z unstable-options`; no bootstrap/nightly workaround was enabled. This supports
using the small stable artifact/timing wrapper rather than relying on that flag.

## First native results: source `70259d3`

[Run 40975268982](https://github.com/tya5/shellboy/actions/runs/40975268982)
passes all three ordinary check jobs: Linux 1,453 / macOS 1,452 / Windows 1,341
passed, zero failed; ignored 27 / 28 / 20 respectively. The overall comparison run
is **failed**, because its separate four-thread experiment failed eight cases;
it is not described as a fully green workflow. The two-thread candidate and
baseline profiling jobs both pass. Windows inventories match exactly: 1,361
identities including 20 ignored, no missing/added names.

The direct pre-change main run
[40973071843](https://github.com/tya5/shellboy/actions/runs/40973071843)
also succeeds on all three OSes at source `0469ca0`. Compare ordinary jobs/steps:

| OS | Before job / Tests (s) | Candidate job / Tests (s) |
| --- | ---: | ---: |
| Windows |888 /640|554 /384|
| macOS |409 /332|384 /239|
| Linux |328 /278|208 /149|

Thus the initial Windows ordinary Tests step falls 40%, and its job falls 37.6%.
The macOS job benefits less than its Tests step because other/cache steps still
contribute. These are single native runs, not a statistical latency guarantee.

The separate Windows two-thread before/after profiles use four CPUs, Python
3.12.10 and image win25-vs2026 / 20261021.278.1. Build/link is 340.250→189.047s;
summed harness execution 254.567→219.218s. These independent runners have the
same recorded image/hardware class, but cache/runner variability is not removed.
Isolated profiles include process startup and are not concurrent-test durations:

| Suite / case | Before (s) | Candidate (s) |
| --- | ---: | ---: |
| storage, all 23 isolated cases |28.687|4.719|
| storage public-schema convergence |8.859|0.703|
| storage v6/v7 matrix |7.328|0.656|
| index, all 50 isolated cases |20.298|24.953|
| index 405-source case |7.812|8.937|
| control_db, all 23 isolated cases |3.234|4.345|

Keep the measured storage optimization. Discard the index source-seed rewrite
(`aa957e2`), which adds duplicate fixture SQL without observed benefit. control_db
stays unchanged; production transaction/durability checks are not bypassed.

## Four-thread findings and fixture repair

The initial four-thread library passes 577 tests. Integration passes 754 and fails 8;
all failures point to shared runtime_features mock-arrival/settled 5s guards:

- edited_request_requires_exact_run_consent_and_waits_for_old_run_to_stop
- editing_first_request_forks_an_empty_prefix_and_accepts_once_with_new_settings
- invalid_second_tool_prevents_first_side_effect_and_every_call_receives_a_result
- manual_compact_accepts_during_http_wait_and_adopts_without_starting_an_extra_turn
- model_a_b_a_never_revives_prior_reasoning_but_preserves_canonical_dag
- preparation_failure_preserves_accepted_message_and_retry_reuses_its_turn
- quota_recovery_wait_collects_via_control_and_requires_explicit_same_message_retry
- responses_session_header_uses_stable_thread_uuid_and_distinct_child_and_fork_ids

Do not rerun that failed sample into success. `e529d91` replaces busy DB polling
with subscribe-before-query target Changed notifications and lag reinspection,
keeps the exact settled predicate, and adds 60s deadlock guards with 5s-bounded
saved-state diagnostics (up to 8 Runs / 8 Turns, no payload/error text). Local
integration 808 passed / 0 failed / 16 ignored at four threads after that repair
and removal of index batching; strict all-target Clippy passes. No product timer,
new Core notification, transaction or native lifecycle behavior changes.

The follow-up source `8fa0429` is pushed only to the measurement branch. Its ordinary
[three-OS run 40977425641](https://github.com/tya5/shellboy/actions/runs/40977425641)
and separate
[Windows-only four-thread run 40977438259](https://github.com/tya5/shellboy/actions/runs/40977438259)
both pass. The normal run has Linux 1,453/ macOS 1,452/ Windows 1,341 successes;
Windows-only four-thread has 1,341 successes, 0 failures, 20 ignored, including all
eight earlier failing cases. Inventories and ignored sets remain equal.

The repaired source's two-thread ordinary Windows job/Tests is 594 / 460s, with
build 108.782s and harness 350.876s; the four-thread sample build is 136.281s and
harness 237.908s. A child-process harness alone varies 2.016→22.454s without any
code/concurrency change to its two active cases, so these are noisy individual
runs, not deterministic speed guarantees. Overall evidence favors a bounded
four-thread CI default. Preserve explicit two-thread diagnosis and require the
normal three-OS workflow for the adopted configuration. No main merge or release
is part of this authorization.

## Second four-thread findings: source `51d2702`

[Normal run 40979048726](https://github.com/tya5/shellboy/actions/runs/40979048726)
passes Linux/macOS but fails Windows: 577 library and 759 integration successes,
three integration failures. The failing cases are:

- response_retries::first_large_failure_maximally_trims_before_using_retry_budget
- response_retries::metadata_and_partial_output_transport_loss_retry_by_default_without_executing_tools
- response_retries::no_unprotected_groups_follow_ordinary_retries_without_replacement

The first two hit the shared 5s mock-arrival guard; the third hits the shared 5s
Control-shutdown guard. Preserve this failure and do not use its elapsed time as
a successful benchmark. The subsequent fixture-only change makes generic arrival,
request, settlement, retry-persistence and cleanup guards 60s, adds 5s-bounded
lifecycle diagnostics and actual caller locations, and replaces polling with
target Changed notifications. Driver retry code publishes Changed after saving
automatic_retry, so subscribe-before-read observes that boundary without a new
Core hook. Closed/lagged subscriptions fail/reinspect respectively.

Keep the native HTTP partial-source tests, configured product deadlines,
Retry-After/retry counts, 50ms absence checks, explicit 2s responsiveness assertions
and all state predicates unchanged. These fixture guards do not define product
latency SLAs; correctness remains in the same persisted facts and request counts.

The response fixture repair passes local macOS strict all-target Clippy and
all 808 integration cases at four threads (zero failed, 16 ignored). The five
Python failure-gate checks also pass. Its native source-specific verification
is the next ordinary three-OS run on the measurement branch; no product source
or test membership changed, and no old failed run is rerun into success.

## Completed native benchmark

[Run 40980978976](https://github.com/tya5/shellboy/actions/runs/40980978976) at
`03ceac1` succeeds on all three OSes without an automatic rerun. Linux/macOS/
Windows passed counts are 1,453 / 1,452 / 1,341, with zero failures and 27 / 28 / 20 ignored.
All normalized test and ignored identities match their same-OS baseline records.

Final ordinary Windows four-thread job/Tests is 379 / 285s, versus 888 / 640s at the
pulled baseline `0469ca0`: 57.3% / 55.5% shorter. Build 102.390s plus harness 181.937s
accounts for the Tests step within wrapper overhead. Linux job/Tests 234 / 178s;
macOS 319 / 227s. This is measured native evidence, not a guarantee about future
runner duration. The two earlier failed four-thread samples remain above;
no failed old SHA was rerun until green.

Retain four Windows threads, combined integration linking and historical-DDL
batching. Keep both native subprocess suites standalone, use only existing
notifications for state observation, and do not weaken actual product timing
assertions. No index seed rewrite, Defender exclusion, nightly toolchain,
nextest dependency, product test clock or new Core resource was adopted. The
final evidence update itself is documentation-only; main remains unchanged.
