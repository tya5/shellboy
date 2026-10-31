# Measured native test performance (#46)

Optimize development/CI execution without changing production durability,
platform coverage, test assertions, or ignored-test policy. Record source,
runner, build/link time, per-binary/test observations and failures separately.
Earlier logs are a baseline, not evidence that a later source passes.

## Layout and process boundaries

Group ordinary integration suites as sorted modules in one explicit integration
harness. Keep native termination and crash-owner subprocess suites as explicit
standalone targets so their self-executable child filters retain their contract.
Disable automatic discovery only with a completeness check: every direct
`tests/*.rs` suite must be registered exactly once, as a module or standalone
target. Preserve all existing test functions and platform cfgs. Verify inventory
identity and ignored sets before/after, normalizing only the added module prefix.
Do not copy test bodies or merge mutable fixture databases.

Initial measurement retains two Windows test threads. Grouping reduces repeated
linking and lets the existing harness schedule across modules; raising concurrency
at the same time would obscure attribution. Tests with native child ownership,
process-global configuration or internal serialization require explicit auditing.
No automatic retry may convert a failed test into success.

## Measurement and fixture work

Use stable Cargo/libtest and standard-library tooling; no nextest dependency or
nightly feature is required. Capture Cargo JSON artifacts from the complete
all-target build, then record native harness wall time, exit status, test counts
and names. Optional per-test profiling runs selected tests individually and must
be labeled isolated-process timing, not reconstructed concurrent duration.
Keep failure diagnostics and publish measurement artifacts even when a harness
fails. Compare Windows job/Tests/build/run timings and the three-OS test inventory.

Historical-schema fixture construction may batch raw setup DDL into a transaction;
production DbWorker startup/migrations, WAL/FULL settings, reopen checks and tested
receipt transactions remain unchanged. Large index source seeds may batch inserts
in one DB call/transaction while preserving every identity and assertion. Measure
these fixture-only I/O savings; never disable fsync in the implementation under test.

## Time and failure evidence

Distinguish product deadline selection, failure classification and retry policy
from native HTTP/process wiring. Pure policy tests should construct the relevant
facts or use an isolated virtual clock. Preserve meaningful native wiring tests;
a one-millisecond timeout can fire before a large upload starts and cannot prove
partial body-source consumption. Do not mechanically replace every timeout with
1ms or pause time around real SQLite/socket workers.

Deadlock watchdogs are generous upper bounds, not latency assertions. Success
returns immediately; increasing their bound does not add a sleep. Keep explicit
responsiveness/deadline checks separately named. Prefer existing change
notifications to polling when the observation is available, with subscription
before the state check and lag handled by reinspection. Do not introduce a new
Core clock, worker, event, queue, persistent table or test-only product mode.

Repeated native diagnostics remain available as an explicit workflow option.
The ordinary all-target suite still executes the same substantive cases on all
three OSes. Moving repetitions out of normal push does not permit omitting their
underlying tests. Defender exclusions and new concurrency limits require measured
causal evidence rather than assumptions.

The IPC saturation regression must demonstrate that status succeeds while all
24 wait receipts are still pending, before stopping their Run. Its client wait
expiry is longer than the deadlock watchdog, so waiter expiry cannot silently
free admission and make a blocked dispatcher appear responsive. Attachment
mock-arrival watchdogs retain saved-phase diagnostics. These guards are not
product response-time SLAs.

The first native four-thread experiment failed eight runtime_features cases in
shared five-second mock-arrival/settled guards, while the ordinary two-thread
suite passed. Fix that fixture boundary with a generous deadlock watchdog and
bounded saved-state diagnostics. Its settled helper subscribes before reading
state, waits on existing Changed notifications and reinspects on lag. Preserve
the exact no-live-Turn predicate; do not treat a timer expiry as successful
settlement or claim an unproven database/port collision. The first follow-up kept default Windows concurrency at two while checking
the repaired four-thread fixture.

The isolated native Windows profiles support historical-DDL batching
(storage28.687→4.719s), but do not support the405-node source-seed candidate
(index20.298→24.953s overall, that case7.812→8.937s). Discard that source-seed
change rather than retain duplicated setup SQL without demonstrated benefit.
Keep the original per-node helper and all405-source/embedding assertions.


The repaired native source `8fa0429` passes both two- and four-thread Windows runs,
with identical 1,361 identities/ 20 ignored. Recorded harness sums are 350.876s
(two) and 237.908s (four), despite a noisy standalone child-process sample.
Adopt a bounded four-thread Windows CI default, followed by a normal three-OS
verification. Keep explicit two-thread diagnosis and a Windows-only four-thread
experiment available through the manual input; its `default` choice always
preserves the ordinary three-OS matrix. Do not generalize these samples to
arbitrary higher concurrency or promise deterministic runner latency.

The normal four-thread run at `51d2702` exposed three more shared fixture guard
failures in response_retries: two five-second HTTP-arrival guards and one
five-second Control-shutdown guard. Retain that failed run. These helper guards
are not product deadline or retry-delay assertions: give them a 60s deadlock
bound and bounded persisted-phase diagnostics. Use existing notifications for
settled-state observation where guaranteed, without changing exact predicates,
retry counts, absence checks, native upload phases or configured product timers.
This addresses fixture readiness/cleanup rather than adding an automatic retry
or turning an observed failure into an old-SHA success.

## Unicode failure diagnostics (#51/#48 Windows follow-up)

Timing artifacts already use explicit UTF-8. The c0f Windows failure was a
console-encoding error while printing the failed harness tail, not loss of the
raw artifact. Configure stdout/stderr as UTF-8 with replacement error handling
at runner startup, and decode native libtest listings explicitly as UTF-8.
Persist each original harness result and failure identity before console
reporting. Emit every recorded failed test name as one ASCII-safe JSON string
line before the unchanged diagnostic tail. Keep raw UTF-8 logs complete and
retain the native harness exit code; a failed harness still makes the runner
return 1, never success. Keep inventories, ignored policy, concurrency and timers
unchanged. A child-runner self-check starts with ascii:strict output, exercises
Unicode logs and test names with native exit 101, and verifies complete logs,
visible failure names and no secondary encoding failure.

## CI execution and publication policy (#57)

Automatic Rust checks run only for pushes to `main`. Branch pushes and pull
requests do not start this workflow. Keep `workflow_dispatch`, its existing
inputs and native OS matrices available unchanged for explicit branch checks,
platform diagnosis and performance comparisons. This changes when checks run,
not test coverage, assertions, failure gates or the Windows thread setting.

Run the complete local standard check (`sh scripts/check.sh` or
`./scripts/check.ps1`) successfully on the exact proposed tree before pushing.
A focused test or an earlier SHA's success is insufficient. When main CI is
red, investigate and restore it before landing other main work. Use an explicit
manual branch run before main integration when native-OS concerns warrant it;
local success does not establish another platform's result.

Create release tags and publish releases only from an exact main SHA whose
ordinary three-OS Rust checks have passed. A green older commit, a focused
Windows-only experiment or a successful distribution build does not establish
that release candidate's full CI result. Push/release authorization remains
separate from this validation policy.

An unrelated intermittent Windows failure may be investigated by explicitly
rerunning its failed job at the same SHA. Retain the original failed run/job,
failed test, diagnostics and the rerun outcome as separate evidence. A rerun
does not erase a failure or establish its cause; fix reproducible product or
fixture failures rather than adding automatic retries or weakening assertions.

GitHub's [workflow syntax](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#onpushbranchestagsbranches-ignoretags-ignore)
confirms that a branches-only push filter excludes tag events.
[Manual runs](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/manually-run-a-workflow?tool=cli)
can select a branch with `--ref`; the workflow must remain available on the
default branch. Preserve that existing entry point.

## Shutdown fixture registration (#52 Windows follow-up)

Main run41016702414 failed before shutdown: the recovery fixture exhausted its
three-second guard waiting for durable wait registration. Separate explicit
Control startup from that guard, inspect the same saved app_meta predicate under
a60-second deadlock watchdog, and surface an early wait response as a failure.
Initial human wait registration emits no Changed event, so a local5ms fixture
poll is permitted; it adds no product observer. The fixture's wait expiry must
exceed the registration watchdog. Retain every capture-failure caching and
wait/Run-drain assertion. This does not establish runner load as the cause;
keep the failed native evidence and qualify the correction on Windows again.

The follow-up manual Windows run41018286041 passes both shutdown fixtures but
fails the existing large-upload prefix observation at15seconds. That observation
includes16MiB preparation/reduction between native requests, not just the
configured3-second header deadline. Preserve the real1024-byte socket prefix,
backpressure, product timeout and exact two-attempt/phase assertions. Give only
fixture arrival/settlement watchdogs60seconds and include prefix ordinal plus
saved attempt/Turn facts if they expire. The original log does not identify
which prefix was delayed; do not claim a specific product/network cause.


## Retry fixture watchdog follow-up (2026-10-30)

Apply one 60-second deadlock watchdog in response_retries and
response_upload_recovery to fixture requests, progress diagnostics, persisted
state observations, and cleanup. Subscribe before reading committed state and
use existing Changed events, rereading on lag, for upload recovery, refused-proxy
attempts/completion and manual compaction completion. Preserve every predicate,
retry count, real upload prefix/backpressure observation, configured product
HTTP/retry timer and absence assertion. The 50ms no-extra-request observation,
900ms Retry-After lower bound and intentional 61s outage/70s receive allowance
are test semantics, not short fixture-readiness watchdogs, and remain unchanged.

Retain b16f414 main run41063015359 attempts1 and2 separately: respectively the
long-Turn tool-followup arrival and partial-upload Control shutdown expired.
The former arrival watchdog was already60s; this cleanup does not establish
that runner load caused either failure or prove that all arrival stalls are
fixed. Qualify the exact revised branch with at least two native Windows runs
before main admission; retain any additional failure without automatic retry.

## State-based test guards (#77)

The earlier short negative observation windows and wall-clock latency assertions
are superseded: test success depends on saved state, notifications, counts and
recovery, never runner speed. All real-time deadlock protection uses the single
60-second constant in tests/support/watchdog.rs. The helper identifies the
awaited operation and call site, returns the original timeout result, and permits
existing failure branches to retrieve saved progress diagnostics. No product
deadline, grace, busy timeout, lifecycle or process policy changes.

Virtual-clock tests may directly exercise timers and deadlines; each exception
must have an explicit reason in the checker allowance inventory. Native socket,
filesystem and database waits must not use a paused clock. Negative assertions
inspect completed-operation counts or explicitly poll a blocked future, rather
than infer absence from elapsed time. Subscribe before a saved-state inspection
and reinspect on notifications/lag where an existing notification is available;
otherwise retain the observation polling interval inside the shared watchdog.

Shutdown duration is not a guarantee. A guard only detects stuck test execution.
Crash fixtures vary persisted shutdown boundaries and force the owned process
to die, then verify durable facts, interrupted-work recovery and no duplicate
side effects. Preserve the Unix SIGKILL and Windows Job-object boundaries.

Files currently changed by issue-41/shared-read are deferred, with exact existing
occurrences recorded as temporary allowances, not blanket file exemptions. #41
integration must migrate those tests to this helper and remove their allowances.
Minimal cfg(test) module wiring is shared infrastructure, not a rewrite of #41.
A source scanner in the standard checks rejects new raw test timeouts and
real-time deadline assertions, including newly added occurrences in deferred
files. Its allowlist gives a reason for each intentionally retained occurrence.

## Exact-SHA reuse and integration batches (#92)

Keep both main push and manual Rust checks triggers. On main push only, a
read-only Actions API lookup may reuse a completed successful first-attempt run
of this repository's `ci.yml` on exactly the same SHA. Require actual successful
Linux, macOS, both Windows shard jobs and the shard-inventory audit; a skipped
job, Windows-only experiment, rerun or reuse-only push is not qualifying evidence.
Log the source run URL in the job log and summary. API errors, absent evidence
and unexpected responses fail open to the full checks, never to reuse.
Manual dispatch always executes checks. Measurement/release workflows are unchanged.

The integration owner accumulates approved commits on current main, returns
conflicting branches to their authors without resolving their code, dispatches
one ordinary three-OS batch and fast-forwards main to that exact passing SHA.
If main moved, rebuild/recheck the batch. Bisect failing batches by constituent
branch, returning failures with test names and run URLs; land passing subsets.
Do not treat a failed-job rerun as batch qualification. Report intermittent
failures with their original evidence rather than retrying into success.

Windows retains four test threads per process and two independently scheduled
jobs. Measure modules before choosing their assignment. Each job inventories
all Cargo harnesses; the audit requires identical full inventories/ignored sets
and an exact disjoint union of executed identities, including ignored tests.
The source-layout check rejects unassigned or duplicate integration modules;
new Cargo harnesses must also have explicit ownership. Linux/macOS still execute
all targets. Exact timing and assignment evidence belongs in acceptance.
