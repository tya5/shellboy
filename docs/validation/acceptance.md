# Validation record

## Issue #46 completed native benchmark: source `03ceac1`

[Run 40980978976](https://github.com/tya5/shellboy/actions/runs/40980978976)
succeeds on all three OSes at exact source
`03ceac1e8a62b576fc24cc3b0438f7493830504c`. Format, strict all-target Clippy,
layout completeness and five Python failure-gate checks pass. The substantive
Rust results and native timing artifacts are:

| OS | Passed / failed / ignored | Job / Tests (s) | Build / harness sum (s) |
| --- | --- | ---: | ---: |
| Linux |1,453 /0 /27|234 /178|77.708 /100.217|
| macOS |1,452 /0 /28|319 /227|140.084 /86.205|
| Windows |1,341 /0 /20|379 /285|102.390 /181.937|

Windows actually uses four test threads. Against the pulled successful baseline
`0469ca0` in run 40973071843, Windows Tests 640→285s (55.5% shorter), job 888→379s
(57.3% shorter). These are first successful runs of different source revisions,
not repeated attempts selected for a favorable time. Earlier failed concurrency
candidates remain recorded below. Independent runner/cache variation limits
latency claims; this is not a future duration guarantee.

The downloaded final inventories match the baseline/source-specific records
exactly, including ignored identities: Windows 1,361 / 20, macOS 1,480 / 28,
Linux 1,480 / 27. No test or assertion was dropped. Both self-spawning suites and
all ordinary native cases remain included. The final code keeps product timers,
retry counts, negative and explicit responsiveness checks. Existing real native
HTTP deadline/upload checks remain; removing every wall-time test was not part
of this optimization. No paid API or default datastore is used.

This final evidence update changes documentation only; executable/configuration
source remains identical to `03ceac1`. Later CI results must still be attributed to
their own SHA, rather than silently relabeling this benchmark.

## Issue #46 repeated four-thread guard findings

The adopted four-thread configuration's first ordinary verification at
`51d2702`, [run 40979048726](https://github.com/tya5/shellboy/actions/runs/40979048726),
passes Linux/macOS but fails Windows in three response_retries fixture guards:
two HTTP-arrival 5s watchdogs and a Control-shutdown 5s watchdog. It is failed
evidence and is not retried into a green old-SHA result. The names and exact
locations remain in the [investigation](../development/issue-46-ci-performance.md).

The fixture repair keeps product timers, retry counts, terminal predicates,
negative checks and explicit responsiveness limits unchanged. Common deadlock
guards allow 60s and emit bounded saved phase facts on failure. Subscribe-before-
query Changed/lag handling replaces repeated DB reads for settlement and saved
retry metadata; the existing retry path publishes after the metadata commit.
There is no Core, schema or dependency change. Source-specific CI results remain
available from [issue 46](https://github.com/tya5/shellboy/issues/46).

## Issue #46 native measurements and repaired concurrency

The pulled baseline `0469ca0` passes all three native check jobs in
[run 40973071843](https://github.com/tya5/shellboy/actions/runs/40973071843).
Windows job/Tests: 888 / 640s. The first grouped candidate `70259d3` ordinary
checks pass on three OSes in
[run 40975268982](https://github.com/tya5/shellboy/actions/runs/40975268982):
Windows job/Tests 554 / 384s. That workflow overall is failed because its separate
four-thread experiment failed eight runtime_features watchdogs; do not label the
whole run green. No automatic retry is used to hide those failures.

Its successful baseline/current Windows two-thread profiling jobs retain
exactly 1,361 test identities including 20 ignored. Historical-DDL fixture batching
has strong isolated evidence: 23 storage samples 28.687→4.719s, public-schema
convergence 8.859→0.703s, v6/v7 matrix 7.328→0.656s. Index seed batching has no
observed benefit and is discarded; control_db is unchanged. Native source `8fa0429`
then passes both:

- Normal [three-OS run 40977425641](https://github.com/tya5/shellboy/actions/runs/40977425641):
  Linux 1,453 / macOS 1,452 / Windows 1,341 passed, zero failed; ignored 27 / 28 / 20.
- [Windows-only four-thread run 40977438259](https://github.com/tya5/shellboy/actions/runs/40977438259):
 1,341 passed / 0 failed / 20 ignored, with every earlier failing runtime_features
  case successful. This is the repaired fixture, not a rerun of the old failure.

The repaired source's two/four-thread Windows harness sums 350.876/237.908s
support adopting a bounded four-thread default. Build times 108.782/136.281s and
a standalone child harness 2.016/22.454s demonstrate runner/process variability;
do not attribute every timing delta to concurrency. Both use four CPUs, Python
3.12.10 and win25-vs2026 image 20261021.278.1. Ordinary pushes still check all three
OSes and every original test; a manual four-thread stress run selects Windows
only. No production timer, test assertion, durability policy or dependency was
weakened to obtain a pass.

After the notification/guard repair and index reversion, local macOS strict
all-target Clippy and all 808 integration cases at four threads pass (16 ignored).
Library/native subprocess test sources are unchanged from the earlier full
standard check and separately verified by the native runs above. Python 5 timing
self-checks pass. Final source-specific workflow results are linked from
[issue 46](https://github.com/tya5/shellboy/issues/46); this record does not apply an
earlier SHA's result to later configuration/docs commits. Native hosted Windows
execution is real; physical console-close and arbitrary higher concurrency are
not established. No user datastore or paid API is used.

## Issue #46 local performance candidate

Baseline source `0469ca0` was pulled first. Historical Windows run 40954267575
(`a45f54e`) provides an independent measured baseline: 653s Tests, 290s build/link,
359.83s summed harness execution and about 855s job. This earlier source is not
current-source verification. See the [investigation](../development/issue-46-ci-performance.md).

macOS ARM64 / pinned Rust 1.99.1:

- Baseline all-target no-run build took 122s; aggregate candidate 28.11s in this
  local incremental/cache state. These are local observations, not a Windows
  speedup claim or clean-build benchmark.
- The timing runner executed the aggregate candidate and retained its complete
  inventory: all 1,480 test identities and 28 ignored identities equal the original
  layout after normalizing only the module prefix. 7 harnesses replace 86; ordinary
  integration suites link once, and the two self-spawning suites stay separate.
- `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 RUST_TEST_THREADS=2 sh scripts/check.sh`
  passes format, strict all-target Clippy and **1,452 passed / 0 failed / 28 ignored**.
  This includes the strengthened IPC pending-waiter assertion and attachment
  watchdog change. The subsequent five Python runner self-checks pass separately;
  no Rust source changed after the standard check.
- Isolated-process profiles cover 23 storage, 50 index and 23 control_db tests
  before/after. Storage sample totals 1.014→0.662s; index 5.171→5.320s and
  control_db 1.417→1.510s. Do not infer a Windows benefit for index from this result.
- Failure-gate self-checks reject lost counts, zero-test exact filters and unknown
  profiling suites; actual failures retain names and do not skip later harnesses.
  The timing runner uses no automatic retries.

At this initial local stage, native measurements were pending; the completed
native measurements and repaired concurrency results are recorded above. The test set,
ignore policy, production durability and native assertions are preserved;
repeated diagnostics become manual options while their substantive cases remain
in every ordinary native suite. No paid API or default user datastore was used.

## Issue #44 review follow-up: compact column labels

macOS arm64 / Rust 1.99.1, isolated worktree based on a45f54e; self-review,
without child threads. Fix 99982d7 addresses the public review's Medium label
truncation; the Low wall-clock latest-Turn observation remains a limitation.

- `cargo test --lib every_activity_fits_the_actual_state_column -- --nocapture`:
  1 passed. All 17 phases are rendered at 20 widths (28 through 120 columns,
  including responsive breakpoints) and heights 10/20. The assertion reads
  exactly eight cells directly below the state header, not the details. Labels
  must equal the compact label and contain no ellipsis. Full labels remain
  unchanged. Outer widths too small to expose the column were removed from the
  initial test matrix after a fixture failure; product column visibility is
  unchanged below 20 inner cells.
- `sh scripts/check.sh`: exit 0; format/strict all-target Clippy, 86 test groups /
  1,452 passed / 0 failed / 28 ignored. No warnings. Background shell completion
  was observed with shellboy wait. Synthetic TestBackend rendering is not
  physical-terminal verification.
- Exact-SHA native CI is observed after push and reported in the completion
  message; it is not inferred from the prior #44/#45 CI. No provider, real data,
  installed owner or binary was changed for this follow-up.

## Issue #45: read-time shell timing

Same isolated macOS arm64/Rust 1.99.1 environment as #44.
The focused work_status suite passes 13 tests before the 1,001-shell bounded
page fixture was added. Added checks compare get/status start and output facts,
observe live duration increasing, preserve terminal duration, return null after
output reclamation, and keep status fingerprint/order unchanged by output and
time. Library timing tests verify future-start clamping, sequence rather than
wall-clock ordering, unknown terminal duration and primary-key output lookup
without a sort. The final focused work_status suite passes 14 tests, including the
1,001-shell bounded-page case (first 50-row page 62 ms, debug fixture).
`sh scripts/check.sh` succeeds: 86 groups / 1,451 passed / 0 failed /
28 ignored, 247,940 ms. Format/strict all-target Clippy and focused checks
were repeated successfully at 68f4f5d. CI 40951871499 at exact head
68f4f5dfaec20b120ea2eaa70f97002aa03be091 succeeds on all three OS jobs:
Linux 86 groups / 1,452 passed / 0 failed / 27 ignored, macOS 86 /
1,451 / 0 / 28, Windows 86 / 1,340 / 0 / 20. These are Tests-step totals;
extra observation steps are not double-counted. Later documentation changes
are not covered by this earlier exact-SHA CI.
Physical terminals and real providers have not been exercised.

## Issue #44: saved activity projection

macOS arm64, pinned Rust 1.99.1, isolated worktree based on e194469.
`cargo test --lib activity_list_tests -- --nocapture`: 2 passed, including
2,000 synthetic active-shell Threads, 40 paged reads in 279 ms.
`cargo test --test thread_browser`: 6 passed. Query-plan regression checks
require run_thread_created, shell_active_owner and runs_by_owner access.
The list/footer fixture verifies live shell and child work, stopping children,
finalization without owned work, terminal Runs, idle Threads and current versus
stale retry generations. Pure classifier and UI label/color tests also pass
in the library run (632 passed / 0 failed / 10 ignored before the bulk fixture
was added). Existing exit-marker and Revisit regressions remain covered.
Combined-task standard checks and native CI pass at 68f4f5d as recorded
above. The final list rerun observed 267 ms for the same 2,000-Thread fixture.
Physical terminal use is not claimed here. No installed owner, real provider or user data was modified.

## Issue #40: focused validation

Environment: macOS arm64, pinned Rust/Cargo 1.99.1, isolated issue worktree based
on `91f92c2`. Synthetic temporary SQLite stores and loopback mocks only; no
installed owner replacement, real provider traffic or user-data changes.

- `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test --locked --test response_retries`:
  22 passed, 0 failed, 1 intentionally ignored (the existing 61-second outage).
  No-candidate coverage includes both an initially all-protected large request
  and a path shortened after POST: three retries followed by success or budget
  exhaustion, plus zero retries. It verifies no replacements, unchanged failed
  paths, recovery notification and saved no-safe-reduction diagnostics. Repeated
  no-candidate observations on still-large requests also show the reduction
  allowance remains available. Existing partial-output, Retry-After, stop,
  adopted-reduction and overflow checks pass in the same suite.
- `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 RUST_TEST_THREADS=2 cargo test --locked --lib`:
  627 passed, 0 failed, 10 intentionally ignored, including the injected
  unsaved index-pause test.
  Both processing and pause codes in readable status match search wire details;
  the durable pause prefix survives and stopped workers do not silently restart.
- The same library run covers draft backlog rejection at 64/65 Threads with a
  live receiver and a stopped worker before/after pump detection. Existing-Thread
  revisions, closing admission and one warning per retained Thread are preserved.
  Exact Japanese failure messages are asserted; the frontend projects the tested
  message directly. This is not a claim of physical-terminal evaluation.
- The initial command could not find `cargo` on PATH and did not run tests;
  exporting `$HOME/.cargo/bin` corrected the environment without changing the
  toolchain or dependencies.

The coder's standard check at `e01d397` also passes:
`CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 RUST_TEST_THREADS=2 sh scripts/check.sh`
completes format, strict all-target Clippy and all-target tests with exit 0:
86 groups / 1,443 passed / 0 failed / 28 ignored. Its background shell completed
in 324,536 ms (about 5 minutes 25 seconds) and `shellboy wait` observed completion;
this is not a ten-minute run. Ignored real-provider tests were not enabled.

Independent review approved `73c25c2`, `03d969b` and `e01d397` without findings.
The reviewer independently ran Responses retries (22 passed, 1 ignored) and the
final library suite (627 passed, 10 ignored), plus format/diff checks, in a
separate macOS arm64 worktree. Post-integration full checks remain separate
from the coder's standard check. Native Linux/Windows execution, physical
terminal evaluation and three-OS CI are not established by these local results.
See the [Shellboy development trial](../development/issue-40-shellboy-trial.md)
for execution observations and aggregate measurements.

### Post-integration and native CI evidence

The integrator's separate macOS arm64 / Rust 1.99.1 worktree at `e01d397`
passes the same standard command with exit 0: format, strict all-target Clippy,
86 groups / 1,443 passed / 0 failed / 28 ignored. The background execution took
433,698 ms (about 7 minutes 14 seconds). Its initial shell failed before testing
because `rustc` was outside PATH; adding `$HOME/.cargo/bin` locally to the command
fixed that without changing source or installed tools.

The reviewed documentation through `8ff6c46` was then fast-forwarded and pushed
normally to `issue-40/integration`.
[Native CI run 40916772603](https://github.com/tya5/shellboy/actions/runs/40916772603)
completed successfully at exact source `8ff6c46bad29ce9e88716a27c28c42e0d06ba588`:

| Native runner | Test groups | Passed | Failed | Ignored |
| --- | ---: | ---: | ---: | ---: |
| Linux (`ubuntu-latest`) | 86 | 1,444 | 0 | 27 |
| macOS (`macos-latest`) | 86 | 1,443 | 0 | 28 |
| Windows (`windows-latest`) | 86 | 1,332 | 0 | 20 |

These totals were independently summed from each job's Tests step. Each job
also passes format, strict Clippy, the separate closed-proxy diagnostic and two
fresh attachment runs; Windows passes its additional overflow diagnostic.
Repeated focused runs are not counted as additional distinct full-suite tests.
This native CI evidence supersedes the local-only platform boundary above,
but does not establish physical-terminal or real-provider behavior. Ignored
tests remain unexecuted. A later documentation commit is not covered by this
run's exact source SHA.


## Review follow-up #36–#39 verification (2026-10-30)

Source `9e4f8ea` includes separate #36/#37/#39/#38 commits listed in the
[follow-up record](../development/issue-36-39-follow-up.md). Environment:
macOS ARM64 / pinned Rust 1.99.1. All new fixtures use temporary synthetic stores
and loopback HTTP, not paid APIs or the default datastore.

`CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 RUST_TEST_THREADS=2 sh scripts/check.sh`
passes format, strict all-target Clippy and all tests: **86 groups / 1,442 passed /
0 failed / 28 ignored**.

Current focused checks also passed: 625 library tests, 11 Revisit integration
tests, three real-owner isolation tests, the Thread exit-marker integration test,
and the corrected interactive bash/zsh PTY-close regression. The first focused
run exposed a fixture's assumption about CHECK-violating JSON and interactive
shell startup; those fixtures were corrected before the final standard run.
The first standard attempt rejected nested formatting under strict Clippy;
`0288bc1` fixes the formatting without changing display behavior. The next run
failed before terminal closure because the fixture treated endpoint.json
publication as IPC readiness. `9e4f8ea` waits for a successful read-only request;
creation is never retried, and all shutdown assertions remain intact. The
separate native signal suite then passed seven tests with one subprocess entry
ignored (invoked by its parent).

Specific evidence:

- Revisit ordinary completion survives a closed start gate when no reservation
  is pending, including child Run completion. Malformed current-format Revisit
  values do not block stop/list/startup; explicit reads still report errors.
- Real DataOwner/CLI tests cover malformed typed Revisit capture, bad cursor,
  healthy HTTP conversation, blocked bad history before HTTP, fork before an
  incomplete group, and deletion. A separate CHECK-violating recovery JSON case
  correctly fails startup quick_check; integrity corruption is not relabeled as
  a recoverable Thread-local condition.
- Snapshot tests preserve explicit_stop during both startup and shutdown.
  Detail keeps ordinary state next to the exit marker. Actual correlated list
  query plan resolves the latest Run once using existing indexes; activity and
  list agree for a backdated last-inserted Run.
- Native Unix process tests cover direct TERM/HUP, actual PTY loss through
  interactive bash/zsh with TERM-ignoring shell, EOF without controlling HUP,
  mixed TERM/duplicate HUP with a 5.5-second configured TERM grace, repeated TERM
  while DB cleanup is blocked, and independent deadline enforcement with a
  deliberately stalled main thread. The ignored subprocess entry is invoked by
  its substantive non-ignored parent test.
- The exact reviewer-provided `winclose2.py` was additionally run against a
  separately built baseline `546102d` and current debug binary with fresh data.
  Baseline zsh: about 0.1 seconds, signal_force, Run stopping, shell running.
  Current zsh and bash: about 2.1 seconds, no signal_force, Run stopped with
  app_shutdown, shell stopped, no orphan matches. Duplicate HUP is still observed
  with current zsh. The reviewer's M2 normal-close orphan report was withdrawn
  after correcting pgrep option parsing; baseline bash already cleaned up.

Documentation audit passes: 645 relative links, 38 heading fragments and four
TOML blocks, with 13 English guides and one Japanese User Guide. Gitleaks finds
no leaks in the six review-fix commits or the staged verification record.

Native Windows/Linux execution, Windows console close/logoff/shutdown, physical
terminal force restoration and three-OS CI are untested in this round. Windows
callback limits, blocked terminal restoration, macOS select descriptor ceiling,
and best-effort process-group cleanup remain documented limits. No push,
release or installed-binary replacement was performed.

## Integrated #36–#39 lifecycle verification (2026-10-30)

Source based on local commit `1d56487`, macOS ARM64 / pinned Rust 1.99.1.
`CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 RUST_TEST_THREADS=2 sh scripts/check.sh`
passes against the integrated source: format, strict all-target Clippy and
**86 groups / 1,430 passed / 0 failed / 28 ignored**. All fixtures use temporary data, loopback
HTTP and synthetic text; no paid API or default datastore is used.

New mechanical coverage includes:

- Revisit: real Control and HTTP, sixteen unlimited cycles and configured zero/two
  caps, one-shot ordinary developer-event delivery, recovery and held-request stop,
  native owned-shell completion before firing, actual model shell→CLI→LocalServer
  authenticated self registration, direct-parent authority/replacement/receipt
  replay, and rejection of unrelated/non-direct or forged callers. The child
  scenario routes real session headers rather than assuming cross-Thread HTTP
  arrival order, and requires child Revisit completion before child_run_ended and
  parent Revisit. Controlled completion/set races accept only their valid serial
  outcomes; queued input and cancellation keep the same Run without premature
  firing. Restart capture/drop, next actual-Turn observation and deletion are
  separate durable checks.
- Revisit source protection and capacity: current event source is protected by
  summary/trim, real revisit.* rows are included in derived byte totals, migration
  12 replaces the existing v11 covering index without rewriting metadata, and
  supported migration paths converge with a fresh current schema.
- OS exit: four substantive real Unix-process tests pass in a separate parallel
  invocation (two test threads). Actual TUI/PTY TERM and HUP preserve tool results and terminal
  Run/shell state; real PTY master loss still drains; a second signal forces
  exit while DB cleanup is held. The deadline test uses the production observer
  in an isolated subprocess with a stalled main thread; it is not a full TUI
  owner. Its ignored child entry is invoked by the non-ignored parent test.
- Exit visibility: latest app_shutdown/previous_instance_interrupted reasons
  mark the Thread list; explicit_stop and completed Runs do not. A new Run clears
  the marker. Query plans retain indexed latest-Run lookups. UI rows preserve
  narrow-width markers and display pending/count Revisit facts without treating
  them as proof of progress. Run/status, activity and event presentations expose
  the same recorded facts and retain original detail/provenance.
- #37 review: structural group repair rolls back within its savepoint, preserves
  uncertain original data/attachments and continues good groups. SQL failures and
  missing global metadata remain fatal with bounded context. The startup capture
  guard handles bad selected cursors before append; an actual reopened DataOwner,
  real CLI list/delete and a good-Thread loopback response verify isolation. Saved
  execution facts after actual SIGKILL are stopped, matching the committed DB.
  Diagnostics contain bounded IDs and fixed reasons only, with at most 32 previews.

Earlier integration attempts exposed fixture/schema issues rather than a reason
to weaken admission: platform-specific PTY FFI types, obsolete downgrade-ledger
fixtures and exact migration SQL formatting were corrected. The owned-shell and
child tests were changed to process legitimate completion/progress events before
expecting Revisit; their ordering assertions were retained. The full check also exposed a real regression: using the closed tool-start gate
to stop all request preparation prevented existing cancelled-tool result followup.
The guard now applies at idle Revisit firing; ordinary result persistence keeps
its previous path. Both existing Quit regressions pass, with added assertions
that the pending reservation is not consumed before shutdown capture. Final
A parallel PTY-loss run also exposed a real pinned-Crossterm EOF/EIO busy loop:
only HUP and deadline diagnostics appeared while persisted work stayed live.
Switching to that same version's existing use-dev-tty backend fixes the unbounded
inner read loop without changing input routing/parser or the cleanup deadline.
filedescriptor 0.8.3 was already locked; only its dependency edge is added. The
four process checks pass with two test threads after this change. The final passing standard check above supersedes these intermediate failures.

Not performed: native Windows/Linux execution, three-OS CI (source remains local),
physical Windows console close/logoff/shutdown, actual power loss, or the separate
real-model longitudinal task-quality evaluation. Windows console notifications
are best effort where the OS delivers them; GUI-library suppression is documented
in the adopted design. Mechanical delivery tests do not establish that a model
remembers every requirement, avoids question false triggers, or judges completion
correctly. The existing live non-storage tool-conflict path remains unreproduced;
no possibly live collector is synthetically completed. No release or installed
binary replacement is included in this work.


## Issue #37: interrupted tool groups (2026-10-29)

macOS ARM64 / pinned Rust 1.99.1, working tree based on `02d94e3`:
`CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 RUST_TEST_THREADS=2 sh scripts/check.sh`
passes format, strict all-target Clippy and **82 groups / 1395 passed / 0 failed /
27 ignored**. The ignored subprocess entry for the crash regression is invoked
by its non-ignored parent test; opt-in live API tests remain ignored.

New checks cover:

- Actual Unix owner process-group SIGKILL while a native shell is running;
  restart and the first loopback HTTP request contain complete adjacent pairs,
  and no second execution is created. The test explicitly cleans up its own
  native process group; production startup does not inspect/kill old PIDs.
- Actual tool-result INSERT failure, storage-failure shutdown, restored storage
  and successful same-Thread continuation after restart.
- Durable pending/no admission, partially saved multi-call, already-terminal
  Turn and child-Thread fixtures; immutable existing result IDs/payloads/edges,
  second-startup idempotence, acyclic DAG and transaction rollback on write error.
- Real DataOwner restart with accepted/held same-/cross-Thread shell attachments:
  a trigger checks cancellation before group completion; no pending source is
  adopted, while applied sources, refs and external synthetic files remain.
- Original interruption facts distinguish tool calls from actual active shells,
  including an admitted call whose execution record is missing. Uncertainty and
  unstarted results have explicit presentation labels and no fabricated exit.
- Replay cache hits/appends, repeated provider call IDs across groups, source
  identity, no pin acquisition for malformed pairs, and current spill projection
  with interleaved context while persisted JSON and source links remain intact.
- Deleted/unreachable exact GC tombstones do not block unrelated startup repair;
  retained/live/fork/summary inconsistencies are not silently skipped.

An initial focused run exposed missing required `started_at` values in three new
unit fixtures, plus the existing spill projection's interleaved input. Fixtures
were corrected to the current schema and spill output order is normalized before
caching; pair validation was not weakened. The final standard check above passes.

All data is synthetic and temporary; HTTP is loopback only. No paid API, default
store mutation, or installed binary update is performed. Native Windows/Linux,
physical TUI F→Y, SIGTERM/SIGHUP, power loss and OS restart are unexecuted here;
their interrupted durable shapes are covered without claiming native process or
terminal verification. Signal handling itself belongs to #38. A normal live
admission-conflict exit is not reproduced; preserve its existing transition
guard and fail incomplete replay locally rather than inventing a tool result.


## Final RAG lease correction and local verification (2026-10-29)

Source `f659197`, macOS ARM64 / Rust 1.99.1: format, strict all-target Clippy
and `cargo test --locked --all-targets --no-fail-fast` pass **80 groups / 1,366
passed / 0 failed / 25 ignored** with RUST_TEST_THREADS=2. Original HTTP+PTY
10-conversation checks, offline docs33 and actual RAG CLI13 also pass on the
new executable (SHA-256 `0e1707a325df96483c4267c9fa7bda6b5c38dd92a6debf21cd55b0e3613acfca`).

Native run 40798359957 on `b938c68` passes Linux and Windows, including all
new #34 integrations and the corrected FIFO fixtures, but macOS reports two
RAG-selection failures. Local 14-way parallel tests reproduce real flock
WouldBlock/errno35 after prior awaited commands. File destructors had returned;
FD/lsof probes found no other visible owner at inspection. The exact kernel
mechanism is not proven. This differs from the old local loopback incident and
from the isolated initial attachment watchdogs.

The writer guard now explicitly unlocks after DB close and before File close,
without retries or deadline changes. Its deterministic duplicate-handle/live-
writer exclusion regression passes; 20 x14 parallel RAG cases pass versus three
failed rounds among the first six before the fix. New tests assert prerequisite
registration/index success so unexpected busy cannot hide behind a null unwrap.
All temporary probes/loggers are removed. A fresh native CI is pending the
next authorized push; the previous two-OS success is not claimed for this source.

## Issue #34 and additional #32/#33: verified locally (2026-10-29)

Verified final implementation/test source: `d84a06f`, including `96669a5`
(reduction and HTTP), `cf1e7f2` (shared search scope, store upgrade and index-stop
diagnostics), `4c1d200` (autosave/receipt follow-up), and `2f8a198` (remove the
unnecessary packet-timing workaround). macOS ARM64 / Rust 1.99.1.

All standard gates passed with RUST_TEST_THREADS=2: `cargo fmt --all -- --check`,
`cargo clippy --locked --all-targets -- -D warnings`, and
`cargo test --locked --all-targets --no-fail-fast`: **80 groups / 1,365 passed /
0 failed / 25 ignored**. The last command uses the same targets as the standard
script, adding no-fail-fast to collect every target. Earlier script attempts
found a renamed guide anchor, misleading transport text on complete provider
errors, fixture assumptions about preparation nodes, and a pre-completion
manifest compared with a completed manifest. These were corrected without
relaxing production safety, deadlines or the immutable request evidence.

Verified boundaries:

- First large-input header timeout, 500/501/503 and disconnect reduce immediately;
  explicit hint alone enables pre-POST reduction. Protected request/tool/unsafe
  groups remain, partial safe omission applies without a numeric goal, and the
  next failure after an adopted reduction recovers without another retry loop.
- Thread-scoped keyword search retrieves omitted originals without embedding or
  knowledge DB configuration; no foreign Thread or pending replacement leaks.
  Actual semantic and keyword scope node sets match through one recursive CTE.
- Header deadline and post-header body-idle are independent. HTTP observations
  distinguish unconsumed source, headers with unknown delivery and body receive.
  Complete provider error messages remain unchanged. Retired pool generation
  and a fresh retry are verified with a real HTTP/1.1 keepalive fixture.
- Real Driver + 16MiB old history + stopped request reader observes two actual
  POST prefixes, then recovery, no third POST, no reduction, and intact originals.
- Closed autosave projects one notice across 1,200 ticks while retaining the
  newest snapshot; fresh receipt scans avoid redundant passes, partial scans
  retain fairness. Index storage-stop and normal owner stop have distinct errors.
  rag init reports actual v1/v2 upgrade and null for fresh/current stores.

Manual/offline checks on the current executable also passed: bundled docs 33
checks; RAG CLI 13 scenarios; original ten-conversation HTTP+PTY script with
early delta, paused-terminal recovery, exact outputs and visible ordering. All
ten completed attempts durably contain header latency/phase/source counts.
There were ten local mock requests and zero external model requests.
Opt-in header measurement: eight 1,024-character requests p50 0ms / max 1ms,
eight 3,480,000-character requests p50 2ms / max 3ms. These are loopback samples,
not production provider performance. The provisional 120-second header default,
retained retry count of eight and unmeasured operational reassessment are
explained in decisions; exact socket reuse/peer receipt remain unknown.

The user subsequently authorized push; main is pushed through 6f19b89.
Run 40796272377 passes Linux (80 groups / 1,366 passed / 0 failed / 24 ignored)
and macOS (80 groups / 1,365 passed / 0 failed / 25 ignored). Windows passes
all integration targets but fails three library FIFO fixture watchdogs. Their
test-only portable correction keeps all deadlines/counts/FIFO/permit checks and
is verified locally by 14 delivery tests; its new native CI remains pending;
no release, installed-binary update, paid API or default-data modification is
performed. Raw synthetic evidence remains ignored locally.

## Post-restart baseline verification (2026-10-29)

Exact source `6c577c8`, isolated source snapshot, macOS ARM64 / Rust 1.99.1.
`RUST_TEST_THREADS=2 sh scripts/check.sh` passed format, locked strict all-target
Clippy, compilation and **76 groups / 1,343 passed / 0 failed / 24 ignored**.
The original ten-conversation HTTP+PTY script also passed: first delta before
completion, ten exact canonical outputs, ordered unique visible text, hidden
Threads in the tree, all ten Turns completed, ten local requests and zero
external requests. This verifies the old source after restart; it does not
validate the in-progress #34 or additional #32/#33 changes.

Review #33/6484081072 supplied independent Python TCP evidence from the failure
window: payloads over about 16KB stopped at 16,332 bytes on lo0; 1KB/4KB/16KB
worked, while 100KB/1MB stalled. After Mac restart the same probe passed up to
10MB. Root independently reproduced complete transfer at 1KB, 4KB, 16KB, 100KB,
1MB and 10MB in the new environment (10MB about 4.53ms). Together with failures
on unchanged `e977dfe`/public 0.1.6 and the successful unchanged-source rerun,
this identifies the earlier loopback fault as the source of the previous large
HTTP-fixture stalls. The original probe is reviewer-supplied evidence, not a
measurement performed by this agent; high load alone is not the causal claim.

The isolated variant without the three `8368874` mock changes also passed all
library tests: 582 passed / 0 failed / 10 ignored. Commit `2f8a198` removes that
unnecessary workaround from current source; it was not established as a fix for
the loopback fault. Current #34/follow-up checks remain pending. Native Windows/Linux CI, push and release are not performed.

## Review round 4: local fixes and verification (2026-10-29)

Product fixes: `7fb41d9` and `c6f2b39`; test-only mock stabilization: `8368874`.
Conversation schema 11, wire protocol 2, RAG schema 3. Complete finding-by-finding
tracking is in [review round 4](../development/review-round-4.md); judgment and
migration boundaries are in [decisions](../development/decisions.md).

Focused checks on this source:

| Check | Result |
| --- | --- |
| Frontend unit/integration boundaries | 104 passed, 1 opt-in ignored |
| Stopped UI, full 512 reserve, closing sends | Exactly 40 receipt/user-node pairs and draft revision 99 persisted before ForceQuit |
| IPC diagnostics and dispatch boundaries | 44 passed |
| Storage / hot-query regressions | 22 / 3 passed |
| RAG selection and old-store writer upgrades | 14 passed |
| Complete index integration suite | 50 passed, including both new real-worker counter/failure regressions; explicit retry succeeds |
| Fresh schema versus all startup migrations | 97 schema objects identical; current version 11 |
| Offline bundled docs | 33 checks, 13 English guides, 3 categories, 127 links |
| Actual offline RAG CLI | 13 scenarios, no external model requests |
| macOS deletion PTY | 5 scenarios passed |
| Isolated ten-Thread / 250ms stopped-UI delivery pressure | p50 7.29ms, p95 12.16ms, lagged 0; synthetic local observation |

Final standard gate on `8368874`: format, locked all-target strict Clippy and
all-target test compilation passed. The standard script did **not** complete
successfully: library cases finished with 579 passed / 2 failed / 10 ignored,
while one summary case remained stuck after all other 591 cases had finished.
The owned lib-test process was interrupted (script exit 101); integration targets
were not reached by the script. Separately executed index/storage/hot-query/RAG
integration suites passed as recorded above.

Remaining failed cases:

- `compaction::edit::summary_tests::explicit_tool_span_summary_reuses_whole_group_sixty_four_thousand_character_partitioning`:
  HTTP-header inactivity deadline. Same failure reproduced on baseline `e977dfe`.
- `control::progress_tests::completed_shell_cannot_be_backgrounded_while_excerpt_is_loading`:
  fixture deadline; unchanged by this batch, but baseline equivalence is not yet
  established for this specific failure.
- Unfinished `compaction::tests::nested_summary_and_spills_preserve_current_turn_boundary`:
  still awaiting the mock; separate 30-second bounded run also failed to finish.

TCP_NODELAY alone did not resolve the summary fixture stalls. Index mock
stabilization was not proof of a production fix; later independent raw TCP
evidence and the post-restart exact-source rerun identify the loopback fault.
The successful focused checks do not waive this whole-suite gate.

Final document audit: 612 relative links / 38 heading fragments, four TOML blocks
parsed, English entry points and 13 English + one Japanese guide preserved;
zero errors. Gitleaks found no secrets in all three implementation/test commits
and their staged diffs. The records commit is also scanned before committing.

Validation attempts matter: before mock stabilization, a whole-library run
completed with 571 passed / 10 failed / 10 ignored, excluding one summary test
that had hung in the earlier standard run. Failures were HTTP/native-shell
fixture deadlines, not evidence of a passing gate. The unchanged baseline
`e977dfe` also failed the large-summary mock's HTTP-header deadline. Index seed
HTTP stalled before the new migration/worker assertions; setting TCP_NODELAY and
writing its headers/body together made both index regressions pass. Deadlines and
assertions were not relaxed. Two RAG test expectations were corrected: the new
hint names rag init, and built-in sources count as existing sources.

The ten-conversation HTTP+PTY manual script did **not** pass this round: it timed
out observing the first delta, before launching the other nine requests. The
installed public 0.1.6 binary also failed this precondition. TCP_NODELAY and a
fuller initial Responses lifecycle did not establish success. This does not
validate concurrent HTTP streaming; cause and a controlled retest remain open.
A prior run's successful manual result does not apply to this source. The later independent loopback/restart evidence and exact-source successful
rerun above explain this failure window; high host load alone was not the cause
established by those measurements.

Raw synthetic captures/logs remain ignored locally. No default data, installed
binary, external model API, push, release or native three-OS CI was changed/run.
Issues remain open until their push and three-OS CI gate is met.


## Latest comments and #32 / #33: final local validation (2026-10-29)

Verified source: `0a72f9c`, including `02b994a` for saturated frontend delivery.
On macOS ARM64 / Rust 1.99.1, the final `sh scripts/check.sh` completed with exit
0: format, locked all-target strict Clippy and **76 groups / 1,334 passed / 0
failed / 24 ignored**. Earlier attempts stopped on obsolete test fixtures: absent
attachment positions and duplicated degraded scope_errors warnings. They were
updated to current inputs/contracts; the final whole-suite run is authoritative.
The UI fixtures now use real clipboard updates and verify retained marker
positions and the full byte limit rather than accepting old append-only input.

New/extended regressions establish:

- Real frontend cancellation with raw channel full, critical outbox 512 full,
  retained receipt batch 128, and 40 queued closing sends. Durable Cancel, Quit
  and one explicit ForceQuit work without consuming UI updates. Separate tests
  cover terminal shutdown success/error saturation, FIFO after resumption,
  worker-permit retention/release, finite receipt scans and disconnect cleanup.
- Owned-shutdown Notify delivery from an initially empty outbox while the normal
  conditional flush timer is inactive; the test reaches one explicit force.
- Uncertain deletion outcomes never acquire a contradictory definitive-failure
  prefix in the dialog/status, and deletion admission can be reused.
- Actual Unix FIFO/socket and symlink source-root replacements preserve previous
  documents/revisions in dry-run and live sync. Directory/regular-file transitions
  follow the current path; no historical-root-type column is added.
- Actual readonly SQLite busy, missing-table and NOTADB failures retain typed
  cause-specific guidance and do not stop the writer/owner. The exclusive-lock
  fixture uses rollback journal after closing the WAL writer; it does not prove
  production WAL contention behavior.
- Public conversation schema v8/v9/v10 upgrades preserve data and match the fresh
  current schema across repeated opens. Isolated current bootstrap/validation
  does not interpret old formats. Older readonly RAG/conversation schemas reject
  without migration, while writer upgrades remain startup-only. Independent
  SQLite comparison confirms all 98 schema objects, metadata, quota counters and
  ledger names match the immutable migrations (application timestamps excluded).
- Current required fields reject absence instead of accepting old responses;
  genuine tombstone/unknown/inapplicable values remain nullable or omitted.
  Missing query markers reject before HTTP without repair. The actual CLI rejects
  `status --operation` with clap UnknownArgument without creating a data directory;
  offline envelopes report protocol 2. IPC regression rejects an older welcome
  before sending a command.
- Full bundled-guide navigation captions match actual labels, with invalid old
  captions rejected even when valid captions also appear. Catalogue scope is 13
  English guides / 3 categories; only the Japanese User Guide remains.

Actual executable SHA-256:
`4f7f4ffe575beb22317f91b5141b3739121d18f92f18047e2c1d38cd7a36c63c`.
Offline docs: **33 checks / 13 guides / 3 categories / 127 bundled links**, empty
environment/cwd and absent config/owner/API. RAG CLI: **13 scenarios**, including
partial failure, stale retention/removal and warnings without duplicated errors;
no model API requests. Relative-link/fragment and TOML example audit passed.

Actual macOS PTY deletion passed 5 scenarios. Local HTTP mock + PTY passed 10
conversations, a 1,171ms reading pause, exact canonical outputs without missing or
duplicated content, 50 ordered unique visible tokens, and completed background
threads in the tree. These functional checks ran alongside the standard suite;
their timings are not isolated performance evidence. No external API was used.

The separate ignored ten-thread pressure test ran serially after all other checks:
7 samples, 1,000 deltas/trial, 250ms UI stopped/trial, action p50 **6.499833ms**,
p95 **6.842125ms**, lagged 0, reloads 7. Regression watchdogs and debug synthetic
measurements do not guarantee latency on arbitrary terminals or OSes.

Source audit found no legacy index preparation/key, previous pending-snapshot
key, unused history budget, status operation argument or old-owner IPC guidance.
Historical SQL imports are startup migration code or cfg(test) fixtures. Remaining
legacy-named tests exercise current safety policies or performance baselines.
Secret scans of staged changes and added local source history found no leaks.
Raw evidence is retained under ignored `.local/review-32-33/`.

No native Windows/Linux, corporate proxy, real IME/clipboard or production
WAL-contention validation is claimed. No push, release, installed-binary update,
paid API request or modification of the user's default data/settings occurred.
The issues remain open for their requested push and three-OS CI gate. The records
below describe earlier verified source and may contain superseded scope choices.

## Latest review follow-ups #19/#20/#27/#30/#31 (2026-10-29)

Source implementation commit: `8f7b036` (starting at `69be5ad`). Implement the new English-guide review and subsequent
re-reviews recorded in [Issue #31 tracking](../development/issue-31-documentation-review.md)
and the [reopened-issue table](../development/reopened-19-20-27-30.md).
The final `sh scripts/check.sh` passed on macOS ARM64 / Rust 1.99.1:
format, locked all-target strict Clippy, **76 groups / 1,322 passed / 0 failed /
24 ignored**. Only this final completed run establishes the whole-suite result;
the initial preflight was stopped to correct the recovery-hint fixture and add
the reviews received during work.

New or extended checks cover:

- Production RPC `Response::failure` recovery hint followed through the actual
  executable's offline search, requiring executable shell examples. Standalone
  docs errors use a separate envelope and are not asserted to contain that hint.
- Fourteen guide navigation references across six English/Japanese guides checked
  against actual OpKind labels, without changing runtime UI labels. All twelve
  reciprocal guide-language links are present. English/Japanese semantics were
  reviewed against source for cache zero budgets, expiry, alias behavior, search
  busy/no-charge, and current displayed captions.
- Actual frontend + UiState cleanup SaveFailed: warning, explicit stopped-admission
  message, a new editable draft, rejection of a later mutation and SaveFailed
  shutdown. Deletion unwind becomes an uncertain terminal result; UI releases its
  global deletion inflight state. Abort/stack overflow/process termination are
  not caught or represented as recoverable.
- Actual Unix root-directory replacement with symlink: incomplete source,
  unchanged prior revisions and no removals in dry-run or write; restored root
  resumes ordinary synchronization. Explicit-file glob exclusion now verifies
  real sync removal while preserving the original file. Windows real symlinks
  remain untested.
- Actual readonly rank/check_space/db_error failure with inherited writer hint:
  returned guidance is readonly-specific and warns about repeat embedding charges;
  writer classification/hint remain unchanged. Corrupt/missing-table guidance is
  explicit. The existing exclusive-lock fixture uses **rollback journal after
  closing the WAL writer**, not production WAL-contention evidence.
- Worker enqueue into an initially empty outbox wakes a select whose conditional
  flush timer was disabled, and delivers the queued result. One finite Notify
  permit is shared by existing worker clones; no new task/channel/Core state.

The debug executable SHA-256 is
`b1e68246eccb61e5537c09f3f4c2361bf0627d9bbd92840f0e604f8b5070cb04`.
Actual offline docs passed **33 checks / 13 guides / 3 categories / 127 bundled
links**. Actual RAG CLI passed **13 scenarios**, including partial-failure
nonzero exit with unrelated removals applied and stale failing-file content
retained. There were zero external model requests.

The explicitly invoked ignored slow-UI fixture passed: 10 Threads, seven 250-ms
UI stops, 1,000 deltas/trial, action p50 **6.26 ms**, p95 **7.37 ms**, lagged **0**,
reloads **7**. This synthetic observation is not a performance guarantee.

macOS PTY deletion passed **5 scenarios**. A separate real HTTP-mock/PTY run
completed **10 conversations**, with **1,126 ms** of stopped terminal reading,
exact canonical bodies without missing/duplicate text, ordered unique selected
stream tokens and completed hidden threads in the tree. All ten requests were
loopback mocks. Repository audit passed **734 relative links / 50 heading
fragments**, plus twelve reciprocal guide-language pairs. Staged secret scanning
passed without findings; private local logs/fixtures remain ignored.

Explicit ForceShutdown is a best-effort UI marker under stalled/full delivery;
the forced result and nonzero exit are authoritative. The exceptional 512-critical
frontend priority-delay case is tracked in
[Issue #32](https://github.com/tya5/shellboy/issues/32) and is not solved by Notify.
No new Windows/Linux CI, corporate proxy, physical Windows terminal/clipboard/IME,
paid provider, default data, installed-binary replacement or release was tested
or performed. Keep all five reviewed issues open pending push/three-OS CI.
Earlier records below describe their own source versions.


## Issue #20 no-compatibility follow-up (2026-10-29)

Issue comment `6480908316` overrides the previous Windows-key compatibility
choice for the planned 0.2.0 release. Removed the Windows legacy-key remapping,
lookup and two dedicated retention tests. Current source uses the scanner's
slash-separated keys directly. Adopted specification, RAG design and both guide
languages require recreating older indexes rather than promising key/ID/revision
preservation. This change does not open or modify existing user data.

On macOS ARM64 / Rust 1.99.1, format and locked all-target strict Clippy passed.
`cargo test --locked --test rag_selection_status --test rag_store --test rag_cli
--test docs_cli` passed **4 groups / 40 passed / 0 failed / 0 ignored**. Existing
failure/removal, incomplete-walk, root-glob, case-sensitivity, provider-breaker and
bundled-doc contracts still pass. Actual-executable offline docs passed **33
checks / 13 guides / 3 categories / 127 bundled links**. Repository audit passed
**722 relative links / 50 heading fragments**. Removed helper and dedicated-test
names have no remaining source/test references.

Debug executable SHA-256:
`c88f5978b1afd8f32794dbbd1e1fc7a59b3ec1d596d611d82908303563d38a88`.
No new full-suite, Windows/Linux CI, paid API, hardware terminal or installed
binary check was performed for this focused removal. The 1,315-pass result below
is evidence for the earlier `deee3e1` worktree, not a rerun of this follow-up.
Push/release/version bump remain outside this change. Staged secret scanning
passed without findings; local logs and fixtures remain ignored.


## Reopened #19/#20/#27/#30 (2026-10-29; local source)

Baseline is local `8f00000` (including the English documentation changes), not
only the public 0.1.6 source. The [tracking plan](../development/reopened-19-20-27-30.md)
accounts for all Medium/Low findings. On macOS ARM64 with Rust 1.99.1, the final
`sh scripts/check.sh` passed format, locked all-target strict Clippy and
**76 test groups / 1,315 passed / 0 failed / 24 ignored**. An initial compile
caught missing arguments in existing browser draw fixtures; those were fixed.
The intermediate full check was stopped after Clippy to incorporate an additional
force-shutdown review finding; only the final completed run establishes the
whole-suite result.

Focused delivery tests passed **6/6**: root/worker ordering under saturated UI,
512-result overflow FIFO, latest-state coalescing, disconnect cleanup, final-drain
force and one accepted force with both raw queue and critical reserve full. The
last case also proves a consumed force does not enter a second UI drain. The
normal owned-work shutdown grace regression remains unchanged and passes.

Readonly search regressions exercise missing-path open, exclusive-lock timeout in rollback-journal mode (not WAL),
query failure, preserved writer error classification, cancellation and a real
Control search using a loopback embedding mock followed by a successful mutation.
Deletion tests cover storage-failure shutdown/rejected admission, a benign bounds
error, serialized targets, rejected-action release and unseen/covered/tiny notice
lifecycle. Warning-plus-new-draft UI behavior and cleanup-failure stopping are
component tests; the frontend late-stream integration uses successful deletion
without an injected cleanup write failure. Do not describe that combination as a
single end-to-end warning-path test.

RAG regression tests cover supported-file failure with unrelated removal,
repeat/dry-run, incomplete walks, provider-deferred removal protection, root and
descendant globs, case sensitivity and legacy Windows key lookup. Synthetic
native-key lookup runs on macOS; the Windows-only full-sync case is unexecuted
locally and still requires Windows CI.

Additional actual-executable checks used debug binary SHA-256
`8136e7461293423e896da0752e5b9b9a3accb146ae35e134dfff3b110d84a9e4`:

- **13 RAG CLI scenarios** passed, including an invalid-UTF-8 supported file
  alongside a removed file. Dry-run reports removal without applying it; actual
  sync removes only the absent file, preserves the failed file's searchable stale
  revision, and repeat sync reports no duplicate removal. These operations return
  the expected nonzero `partial_failure` envelope while preserving successful
  changes; the smoke helper explicitly checks that envelope rather than treating
  it as ordinary success. No external model request was made.
- **33 offline docs checks**, 13 guides / 3 categories / 127 bundled links passed
  from empty cwd/environment and nonexistent config/data paths. Repository audit
  checked **722 relative links / 50 heading fragments**, with no errors.
- **5 macOS deletion PTY scenarios** passed: abort, background delete, parent-only,
  explicit descendants and editable draft after current-thread deletion.
- **10 local HTTP-mock + PTY conversations** completed. With terminal reading
  paused for **1,122 ms**, all canonical outputs matched exactly without missing
  or duplicate text; visible selected-thread tokens stayed ordered and unique,
  the early first delta appeared, and completed hidden threads remained in the
  tree. This made 10 loopback requests and zero external requests.
- The explicitly run ignored slow-UI bridge fixture passed: 10 Threads, 1,000
  deltas/trial, seven 250-ms UI stops, action p50 **5.94 ms**, p95 **7.12 ms**,
  lagged **0**, reloads **7**. These are synthetic observations on this machine,
  not timing guarantees or proof for the exceptional full-reserve case.

No default data/configuration, installed binary or release was changed. No paid
provider, corporate Windows proxy, hardware clipboard/IME, or new Windows/Linux
CI was exercised. Push and three-OS CI remain required before closing the four
issues. The live selection-prefix gap and exceptional full-reserve frontend
priority delay are explicit limitations in the
[delivery design](../design/tui-render-delivery-performance.md) and
[known limitations](../development/known-limitations.md). Earlier release CI does
not validate this source. Local private logs/fixtures are ignored by Git. Implementation commits are
`cdc04d6`, `4c8dfe3` and `9c83898`; later edits are tracking records only.
Staged gitleaks scans passed with zero findings.


## English documentation defaults (2026-10-29; local source only)

Based on `9f4a9b6`, all 13 unsuffixed product guides and bundled docs metadata/categories are English. Twelve Japanese user-facing originals are retained as `.ja.md`; there is no Japanese Agent Guide. README/AGENTS and documentation entry points are English. Independent translation reviews compared CLI flags, examples, limits, ownership, retry, receipt, retention, and recovery caveats with the originals. Current Japanese UI captions remain literal quoted data with English explanations.

The [UI comparison](../design/documentation-language.md) is complete: recommend English default with an explicit Japanese startup selection in a future task, sharing one UI and keeping locale out of Core/persistence. No UI/frontend/configuration/system-prompt/dependency files changed. No unsupported locale field was added. Existing design and historical records are not represented as fully translated.

On macOS ARM64 / Rust 1.99.1, `cargo fmt --all -- --check` and `cargo clippy --locked --all-targets -- -D warnings` passed. Targeted `cargo test --locked --lib docs::tests` plus `cargo test --locked --test docs_cli --test rag_cli --test rag_store --test rag_search_readiness --test rag_selection_status --test rag_extract` passed: **7 groups / 52 passed / 0 failed / 0 ignored**. Tests cover actual keyword results, UTF-8 pagination of the translated TUI guide with Japanese captions, cursor/section/link behavior, docs RAG readiness/version changes, explicit indexing, and stored-source behavior with loopback embedding mocks. Japanese user-document/history fixtures remain Japanese; only bundled-doc test search terms changed to English.

The first scoped-search run used plural `child agents` while the translated body used singular `child agent`, so the literal search correctly returned no delegation hit. Use the documented discovery term `subagent` and define that alias in the guide. Keep the expected guide and nested-scope assertions; do not add stemming, automatic query translation, or relax the assertion. The final test run above passed after that correction. The shell example's pre-existing 15-byte claim was separately corrected to 19 bytes in both languages, confirmed with an actual isolated Bash command whose stdout exactly matched the token. The contradictory CLI-preview sentence, stale operation count, and broken/translated heading links were corrected as recorded in [decisions](../development/decisions.md).

An audit of README/AGENTS and all documentation checked **713 relative links / 50 heading fragments**, with no missing files/anchors. Original CLI `--flag` sets and fenced-example counts were retained in every English guide; Japanese counterparts retain the originals with navigation, discovery guidance, and the recorded small corrections. Four English TOML example blocks parsed successfully. These mechanical checks supplement the translation reviews, not a claim of semantic equivalence from token counts alone.

The final debug binary SHA-256 is `f2e191dee538892b7770d0532d8734d3392ea563c4890b159cb539c7b9863434`. `scripts/check-docs.py` passed **33 offline CLI checks / 13 guides / 3 categories / 127 bundled links** from an empty cwd/environment with nonexistent config/data paths. An additional actual-executable probe verified all 16 catalogue titles/summaries are English, default User Guide content, an English-only Agent Guide, scoped `subagent` discovery, Japanese literal matching in quoted TUI text, and `.ja.md` references classified as repository-only rather than false bundled targets. It created no local state. The English catalogue version is `0.1.6:c1218302f7417409be2ba68ecdf8da055cff7fdae17bbc6b079f3aebfbd0a4ee`.

No external model API, default data, settings, or installed binary was used or changed. The full product suite, new Windows/Linux CI, hardware terminal/IME interaction, and real-provider discovery were not rerun for this documentation/catalogue change. Earlier release-wide results below do not prove this source on those platforms. Changes are local commits only; the published 0.1.6 and its installed binary remain unchanged. Staged gitleaks scanning completed with zero findings.

## v0.1.6公開・配布確認・常用更新（2026-10-29）

#19〜#30を含むsource `025388c` を [v0.1.6](https://github.com/tya5/shellboy/releases/tag/v0.1.6) として公開しました。[製品sourceの3 OS CI](https://github.com/tya5/shellboy/actions/runs/40712696984)と[配布workflow](https://github.com/tya5/shellboy/actions/runs/40717582688)の全4 jobは成功。後者のworkflow headはrunner更新の42c9519ですが、全3 buildのログでcheckout/ビルドsourceが025388cbef17a2c7b79be23cdb250d768ec86b13であることを照合しました。tagの書換えはありません。

認証なしで全3 archiveとSHA256SUMSを取得し、公開size・全checksum・Windows PE x64/macOS Mach-O arm64/Linux ELF x64、MIT/README/設定例/ガイドの同梱、private snapshot/開発directoryの非同梱を確認。release notesのDB migrationと内部変換廃止の注意が保持されていることも確認しました。

| 配布物 | SHA-256 |
| --- | --- |
| `shellboy-v0.1.6-aarch64-apple-darwin.tar.gz` | `029b076f089809593f5990846ed4ee76acc095687bf64e2860b1f719873ec178` |
| `shellboy-v0.1.6-x86_64-pc-windows-msvc.zip` | `843644d29283240df8bb099f2955987d17943f565f7ce8d5049a911ab015b90e` |
| `shellboy-v0.1.6-x86_64-unknown-linux-gnu.tar.gz` | `971993631b345e1721532acbfcac62d6adacf539b703a454560ddedf57aef448` |

取得したmacOS版のMach-O minosは14.0。一時HOME/config/data・資格情報とproxyを継承しない環境でversion=0.1.6、help、offline Agent Guideが正常終了し、DB/endpoint/設定fileを作らないことを確認しました。これはmacOS 14実機での操作成功ではありません。

常用 `~/.local/bin/shellboy` は旧0.1.5をGit管理外へバックアップし、取得した公開macOS版へ原子的に置換。SHA-256 `ed1bd720e676e1ba3d4c0f74a268327bf82d04f4a1cf74703253cf26876d0d87` は配布archive内の値と一致しました。配置後のversion/help/offline Guideも成功し、一時環境に保存fileを作りませんでした。既定データ・設定・稼働processは変更せず、次回起動から適用されます。起動前backupが必要な会話DB v10/独立RAG v3への移行は、今回の配置確認では実行していません。

配布workflow更新後のmain `42c9519` の[追加CI 40717576935](https://github.com/tya5/shellboy/actions/runs/40717576935)も3 OSすべて成功しました。公開後のcommitは非同梱の開発/検証記録のみで、製品・ガイド・workflowを変更しないため `[skip ci]` でpushします。

全12 Issueへ採用/見送り・測定の境界・CI/公開結果をコメントし、completedでcloseしました。差分/追加履歴のgitleaksは検出0。会社proxy、外部モデルによる性能・切断率、実機Windows Terminal、macOS 14の操作は未検証です。以下は公開までの経緯で、未公開の表現は当該段階の記録です。

## v0.1.6: native CIと配布runnerの更新（公開までの経緯）

source `025388cbef17a2c7b79be23cdb250d768ec86b13` の [CI 40712696984](https://github.com/tya5/shellboy/actions/runs/40712696984) は3 OSすべて成功。fmt・全target strict Clippyと各76群の全体試験を通過しました。Windowsの専用診断stepの重複実行は合算しません。

| OS | passed | failed | ignored |
| --- | --- | --- | --- |
| Windows | 1,195 | 0 | 18 |
| macOS | 1,296 | 0 | 24 |
| Linux | 1,297 | 0 | 23 |

確認後に同じsourceへ `v0.1.6` をタグ付けしてpush。初回[配布workflow 40715518229](https://github.com/tya5/shellboy/actions/runs/40715518229)はWindows/Linuxのbuild・起動・梱包が成功しましたが、macos-14が10分以上queuedになりました。公式の廃止告知を確認し、`42c9519`で配布runnerをmacos-15へ更新、deployment target 14.0と既存tag用dispatchを追加。初回は公開前にcancelし、[再配布workflow 40717582688](https://github.com/tya5/shellboy/actions/runs/40717582688)を開始しました。workflowのheadは42c9519ですが、全3 buildのcheckoutは検証済みv0.1.6/025388cを指定します。tagは書き換えません。この節の時点ではdraftで、公開/取得/常用配置は未完了です。

## Issue #19〜#30: 0.1.6の統合検証（2026-10-29）

最終の `sh scripts/check.sh` は終了0、fmt・全target strict Clippy・76群1,296 passed / 0 failed / 24 ignoredで成功しました。下記性能用ignored試験6種類は別途明示実行し、すべて成功しています。

macOS ARM64 / Rust 1.99.1、一時DB・合成fixture・ローカルHTTP mockを使用。性能比較はdebug/unoptimized buildで、実ユーザーの会話・既定DB・会社proxy・外部モデルAPIを使っていません。#19〜#23の初期検証は下節、以下は#24〜#30を統合した測定です。

| 対象と測定範囲 | 変更前 → 変更後 |
| --- | --- |
| #25: DbWorker内の末尾1node採用、1,000経路、5回p50 | 956,226 → 216 µs |
| 同3,000経路 | 8,598,210 → 537 µs |
| #26: native Control、50,000 shell/10,000 Run/1,000 Thread、9回、終了済みshell Wait p50 | 30,798 → 12,287 µs |
| 同ThreadList 50件の投影p50 | 116,973 → 10,377 µs |
| #27: 20,000 chunk×1,536次元、検索中の出力commit、5標本の範囲 | 1,581〜1,818 → 0.50〜6.50 ms |
| 同検索中のControl ThreadGet | 1,590〜1,833 → 10.1〜38.8 ms |
| #28: 実shell 10個/終了wait 10個、約1.2秒の出力だけの区間、wait再観測DB job数 | 1,111 → 0 |
| 同出力commit p50/p95 | 4,177/93,965 → 3,260/7,352 µs |
| #29: 127node/保持180,247 bytes、stream 28,258 bytes、120frame layout p50/p95 | 706,690/766,023 → 26,842/51,650 µs |
| 同更新p50/p95 | 103.75/358.25 → 59.92/234.75 µs |
| 同128nodeの幅変更、取得要求数/計測時間 | 128件/32.80秒 → 3件/0.636秒 |
| #30: 実frontendへ10Thread分の合成delta、UI停止250ms×7回、action消費p50/p95 | 256.19/265.83 → 6.41/7.65 ms |
| 同lagged/reload数 | 7/14 → 0/7 |

#25は同じfixtureを毎回rollbackし、receipt照会・replay・HTTP・commit/fsync・UIは含まない。#26は同一現行コードで3索引だけを外した/付けた比較で、schema ledgerは維持。Waitは既に終了したshellの受付・保存・所有照会・cleanupまで、ThreadListはtitle/excerpt投影までを含み、IPCとUIは含まない。別のSQL-only測定とEXPLAINは[履歴採用と索引](../design/history-adoption-performance.md)に記録し、native Control測定へ混ぜていない。

#27は実IndexServiceとsqlite-vec、loopback embedding mock。検索そのものは変更前約1.61〜1.85秒、変更後約1.54〜2.08秒で、高速化の主張ではなくwriterと読取りの待ちを切り離した結果。取消しからdrainは0.50ms、future abortから実処理drainは1.88ms。第2検索はembedding前に拒否。ランキング後・返却前にGCと経路変更を挟む決定的gate試験で、新snapshotが古いwinning witnessを含む2候補を除くことも確認した。

#28の0件は出力だけの計測区間であり、初回・終了の必要な観測は区間外。通常ShellOutput条件の通知、終了/取消し/timeoutも維持する。Thread読取probeのp50/p95は3,889/44,520→3,093/7,636µsだが、これはSQL照会でfull ThreadGetではない。

#29は全量rendererとの文字/style/link/source/行数一致、combined layout予算、heightだけの再構築0回、全可視nodeのresize取得を試験。全体試験で本文window再取得のpreflightが描画時のviewport初期化に消される退行を検出し、既存スクロール試験の期待値を変更せず修正した。最終fixtureではunit再利用14,770回/構築247回、全layout構築120回。p95は約52msなので常時30fpsは保証しない。#30はactionをfrontendが消費する時間で、画面反映時間ではない。残るreload 7回は注入Errorによる通常refresh。重要結果の非coalesce、有限上限、Cancel/Quit/ForceQuit、切断・終了・購読世代も別途回帰試験で扱う。

#24は内部Office/PDF/adapterを除去し、UTF-8 textを直接索引する経路と、外部変換後のingestを統一。17MiB原本の逐次hash、派生本文と原本/申告hashの区別、原本削除後の検索/get、以前のPDF revisionのremovedを確認する。RAG v1/v2はreadonly/dry-runで移行せず、writerでatomicにv3へ移行しfresh v3と索引を照合。会話DBはv10へ移行するため、更新前backupと旧版への直接復帰不可をREADME/guideへ明記した。

最終debug binary（SHA-256 `5d9c18ea11e3c4580221079dda2ca295b174fca9cdaa63232ee60b6e9befc604`）で、オフラインdocs 33項目/13 guides/3 categories/127 links、実RAG CLI 10項目、一覧削除のmacOS PTY 5項目が成功。削除済み内部変換flag 2個も、JSON protocolのinvalid_argument/UnknownArgumentとして拒否し、DBやownerを起動しないことを確認した。最初の手動probeはClap標準stderrを仮定して失敗したため、既存JSON protocolを観測するprobeへ直し、製品の出力は変更していない。

実debug TUIを55行×130列のmacOS PTYで動かし、loopback Responses SSE 10要求を同時生成。最初のdeltaの表示後、PTY排出を約1,172ms止めてSIGWINCH負荷を入れ、10件の保存済み回答の完全一致、表示中の50 tokenの順序と重複なし、非表示9会話の一覧残存、10 Turnのcompletedを確認した。画面はANSI差分を端末cellへ適用して観測し、再描画の累積文字列を重複表示と誤認しない。外部API要求0件。Windows Terminalの対話実機、会社proxy/実モデルの速度と切断率は未実施であり、native CIとは区別する。

## Issue #19〜#23: ローカル検証（2026-10-28、0.1.6公開前）

macOS ARM64 / Rust 1.99.1、一時DBと合成fixtureで検証。`sh scripts/check.sh` は終了0、fmt・全target strict Clippy・73群1,274 passed / 0 failed / 19 ignoredで成功しました。初回Clippyでextract単独試験の未使用helperを検出し、形式判定/上限の意味のある試験を追加。全体試験で旧hybrid失敗の期待値が残っていたため、確認済み未準備のwarning成功へ更新しています。次の全体試験は閉じた一覧の通知を再openで失うケースを検出。通常slash操作のstatus更新を変えず、一覧のnoticeをopen時に保持し、画面に失敗本文が残る試験へ修正した後、上記標準checkが成功しました。

#19は後片付けのSQL失敗/4096件超過を注入し、削除確定を失敗へ戻さないこと、5000件の過去generationが候補を膨らませないこと、閉じた一覧・同一対象の重複・変更キー・画像失敗通知を回帰試験で確認。実debug binaryの40行×130列macOS PTYでも、取消し・背景削除・親だけで子保持・明示子孫削除・現在会話削除後の編集可能な新規下書き、の5項目が成功しました。

#20〜#22はsource内にRAG DBを置く実CLI確認10項目が成功。複数globの永続化/解除、dot/self DB/unsupportedの除外、dry-run無書込み/無API、keyword索引、hybridのdegraded成功、時刻/不足量、削除検知を確認。独立RAG schema 1の読取り/dry-run無移行と初回writerのschema 2移行、failedと旧revision保持のstale、走査失敗時のremoved抑止、失敗一覧上限、準備済みhybrid、providerエラー/不正vector identityを成功へ変えないことも合成DB・ローカルHTTP mockで試験しました。外部モデルAPIは使用していません。

#23は抜粋0/3/512、Unicode、4096超過拒否、cursorの抜粋条件束縛、tombstoneのnull長、不正JSONでも0なら本文解析しないことを確認。固定50件の合成Page JSON（protocol envelopeを除く）は従来相当99,051 bytes（平均1,981 bytes/項目）から抜粋0で20,051 bytes（平均401 bytes/項目）、約80%減でした。provenance/selection/実行情報は保持し、50件は16KiBには収まりません。測定testは`compact_history_listing_size_is_measured_without_removing_provenance`、固定cwd・ID・本文で比較しています。

最終同梱docsはオフライン33 CLI確認・13 guides・3 categories・127 linksが成功。設定/owner/APIは不要です。差分gitleaksは検出0。Windows/LinuxのCI、会社proxy、実機Windows Terminalはこの時点で未実施です。既定データ/設定/稼働process/インストールbinaryは変更していません。追加された#24〜#30の実装と統合検証の後に0.1.6のpush・CI・公開へ進みます。


## 常用0.1.5の配置確認（2026-10-28）

公開済みmacOS ARM64 archiveのSHA-256 `1bb091acc97daf38182d16f52e4091d5dec6311013f2e26e4aa7e009a9f267cd`を再照合し、archiveから実行ファイルを読み出して常用`~/.local/bin/shellboy`へ原子的に配置しました。旧binaryはGit管理外へ保持。配置後の実行ファイルSHA-256は `3730862d1d6b5a5eb999ddfb7bbc5ea1b323099c6cbe932d92fc18a75c35da4c` で、archive内の値と一致しました。

一時cwd/HOME・存在しないconfig/data-dir・資格情報を継承しない環境で、配置後binaryのversion=0.1.5、help、offline Agent Guideのok:trueを確認し、fileやDBを作らないことも確認しました。既定の設定・保存データ・稼働processは変更していません。製品source・公開tag・配布物は変更せず、ローカル配置と結果記録だけのため全体テストは再実行しません。


## v0.1.5公開・配布物と検証記録

2026-10-28、tag `v0.1.5` / source `25d53bd` を[公開](https://github.com/tya5/shellboy/releases/tag/v0.1.5)しました。#16〜#18のレビュー追補と#19の一覧削除を含みます。[native CI 40604076661](https://github.com/tya5/shellboy/actions/runs/40604076661)は各71群、Windows 1,140 passed / 0 failed / 14 ignored、macOS 1,245 / 0 / 19、Linux 1,246 / 0 / 18。fmt・全target strict Clippyも3 OSで成功。Windows診断stepの重複実行は合算していません。

[配布workflow 40605371702](https://github.com/tya5/shellboy/actions/runs/40605371702)は3 OSのnative release build・tag/version/host照合・version/helpの起動確認・packaging、publishの全4 jobが成功しました。公開後、認証headerなしで全3 archiveとSHA256SUMSを取得し、全checksum・公開size・同梱MIT/README/設定例/ガイド、Windows PE x64・macOS Mach-O arm64・Linux ELF x64を確認しました。私的snapshotやローカル開発directoryは同梱していません。

| 配布物 | SHA-256 |
| --- | --- |
| `shellboy-v0.1.5-aarch64-apple-darwin.tar.gz` | `1bb091acc97daf38182d16f52e4091d5dec6311013f2e26e4aa7e009a9f267cd` |
| `shellboy-v0.1.5-x86_64-pc-windows-msvc.zip` | `97f0481bfeb88363989946afa72f655712bdaac968135de4fae828f897cf0574` |
| `shellboy-v0.1.5-x86_64-unknown-linux-gnu.tar.gz` | `99982c524d734996e8f2ec62d96bc7104a0dbb187344287b033489011a47d40c` |

取得したmacOS版は一時HOME/config/data・資格情報とproxyを継承しない環境でversion=0.1.5、help、offline Agent Guideが正常終了し、DB/endpointを作らないことを確認しました。#16〜#19の対応コメントを公開結果へ更新し、返却本文との一致を確認しています。Issueはレビュー用に開いたままにしています。

既定DB・設定・稼働process・インストール済み0.1.4は変更していません。Windows Terminalの対話実機、会社proxyでの切断率改善、実providerでの巨大入力は未検証です。旧tag・配布物は保持。公開後のcommitはstatus/acceptanceの結果記録だけで、製品・同梱guide・workflowは変更しないため、上記tagのCIを根拠に `[skip ci]` でpushします。以下の未公開・未実施の記述は作業当時の記録です。


## Issue #16〜#19: レビュー追補と会話一覧の削除（2026-10-28、未公開）

#16〜#18レビュー修正は`641da47`、#19は`92a875f`と分岐編集下書きの画像所有の追補`68e2f98`。version準備は`c3715ad`、広い一覧の密度を保つfooter修正は`31822c0`。macOS ARM64 / Rust 1.99.1で、一時DB・合成データのみを使いました。初回静的検査で入れ子ifを整理。初回の全体試験はlib 528 passed / 1 failed / 6 ignoredで、低い画面の会話一覧footerからEnter切替の案内が隠れる退行を検出しました。案内順を修正し、同内容の分岐をClippyに従って共通化しました。確認後にRunが始まるケースも、実Coreの削除拒否を通す試験へ補強しています。その後の結合試験で、広い一覧のfooterが2行になり従来の7行表示が6行へ減る退行を検出し、広い幅では案内を1行に収める修正を加えました。既存試験の表示件数の期待は緩めていません。

実binaryをmacOSの40行×130列PTYで起動し、Ctrl+D→Nで変更なし、Delete→Yで背景会話だけ削除、親だけの削除で子を保持、Space切替後の子孫削除、現在会話の削除後に一覧を閉じ新しい下書きへ入力、の5項目が成功しました。CLI照会で削除結果と残存会話を突き合わせ、正常終了も確認。最初のprobeは画面差分・OSC制御文字の除去不足で検証文字列を見落としたため、観測スクリプトを直して再確認しました。これを製品の失敗やWindows端末の実機成功へ読み替えません。外部モデルAPI要求は0件です。

分岐編集下書きの画像取得も元会話へ所有を関連付け、元会話の削除で回収できることを追加確認しました。TUI回帰は7 passed / 0 failed。画面assertは日本語のwide cell継続セルを空白として数えず、実端末の表示文字列へ揃えています。最終0.1.5 binaryのPTY 5項目も再成功。同梱docsのオフライン33 CLI確認・13 guides・3 categories・124 linksが成功し、設定・owner・モデルAPIを使わず検証しました。最終source `31822c0` の標準 `sh scripts/check.sh` は終了0。fmt・全target strict Clippy・71群1,245 passed / 0 failed / 19 ignoredで成功しました。staged差分と公開基点以降の追加履歴のgitleaksは検出0。既定DB・設定・利用中process・インストール済み0.1.4は変更しません。会社proxy・Windows Terminal実機・3 OS CIは現時点では未実施です。利用者の追加指示により、最終ローカル検証後にpush・CI・v0.1.5の公開へ進み、結果を追記します。


## Issue #16〜#18レビューの追補（2026-10-28、未公開）

コミット分割のMediumは判断記録へ理由を残して履歴を保持。共通の保護規則、mode別・prefix専用の正しい境界候補、現在依頼後の作業省略通知、元範囲の先頭IDによる復元、実際の添付除外だけの警告、縮小を採用しなかった理由の監査を修正しました。旧履歴の4文字/tokenの限界と、明示summaryの現在依頼の扱いもガイド・判断に記録しています。

対象確認はmacOS ARM64 / Rust 1.99.1で `cargo test --locked --lib compaction` 56 passed / 0 failed、`cargo test --locked --test response_retries` 18 passed / 0 failed / 1 ignored。実MessageSend後の古いsnapshot拒否、縮小不能からのcommit後復旧通知、context超過の非対象扱いも含みます。後続#19と統合後の全体標準checkは別途記録します。実proxy/Windows実機を成功へ読み替えません。


## Issue #16〜#18: 範囲編集と切断後の縮小（未公開）

2026-10-28、macOS ARM64 / Rust 1.99.1。#16〜#18の本文と22:18 UTCの追加コメントを確認し、途中範囲omitを含めて実装しました。構造移動は`5ee28ba`で独立させました。最初のコンパイル確認でimport漏れとSQL countのRust型を直し、Clippyの単一Range配列の表記も修正。初回の標準`sh scripts/check.sh`はfmt・全target strict Clippy・70群1,215 passed / 0 failed / 19 ignoredで成功しました。

一時DB/合成loopback HTTPで、現在の依頼を残した自動縮小、spillだけの達成、達成不能時の完全未変更、縮小後の再送成功・再失敗から復旧、同Turn/generationの一回制限、途中省略と後続保持の復元、添付excluded維持、GCの原文保持、selection/generation/停止・marker hashの遅着拒否、隠れた原文の要約とraw/cache保持、64k文字の完結group分割、operationの受付/同ID再送/downstreamを確認しました。

最終レビューで旧compactの連続受付のまとめ処理を維持し、復元は最新作成markerを選ぶよう修正しました。L,L,E,Lの受付を2/1/1で処理し、明示編集を越えて結合せず、取消し済みworkerはAPI要求なしで全pendingを失敗へ回収することを検証。隠れた原文の要約時のキャッシュ無効suffixは、展開した原文ではなく編集前のmarker経路で測ります。追加試験のSQL count型もi64へ揃え、最終source `e8153f6` の `sh scripts/check.sh` は終了0、fmt・全target strict Clippy・70群1,219 passed / 0 failed / 19 ignoredでした。旧規則/通常compact/overflow/stop/receiptの既存試験も含みます。

最終実行ファイルで `compact --help` に7個の新しいmode/endpoint flags、offline `docs get --id shellboy/history-and-search --limit-bytes 65536` のok:trueとomit/restore本文を確認。staged全差分と追加2commit（`7dfc96d..e8153f6`）のgitleaksは検出0。処理中・復旧枠の縮小表示、同generationの数値だけの投影、不正/巨大JSON拒否も試験済みです。実機のない環境をこの結果へ合算しません。

既定DB・設定・稼働process・インストール済み0.1.4は変更していません。会社proxy、15万token超の実会話、Windows実機の端末操作、実proxyの切断率改善は未実施です。新規table/worker/queue/event/依存はありません。省略元の要約は一つのmarkerの元全体に限り、部分指定は正しい全体境界へ案内して拒否します。添付は原文復元だけでは再送対象に戻しません。push/releaseは今回の対象外です。


更新: 2026-10-27。最新のv0.1.4検証は以下の節を参照してください。初版・先行commitの結果は当時の記録として残します。

## v0.1.4公開・配布物とローカル更新

2026-10-27、tag `v0.1.4` / source `60847e3` を [公開](https://github.com/tya5/shellboy/releases/tag/v0.1.4) しました。製品変更は `2387144` まで、`30d48d5` と `60847e3` はfixture・CI・記録の変更です。[最終native CI 40516537195](https://github.com/tya5/shellboy/actions/runs/40516537195) のTests stepは各70群、macOS 1,179 passed / 0 failed / 19 ignored、Linux 1,180 / 0 / 18、Windows 1,074 / 0 / 14でした。別stepで再実行したWindows診断は二重計上しません。fmt/strict Clippyも3 OSで成功。対象を減らさず、Windowsは独立fixture同時数を2としました。

[配布workflow 40518207186](https://github.com/tya5/shellboy/actions/runs/40518207186) は3 OSのnative release build、help/version、packaging、checksum、公開の全jobが成功しました。Latest releaseもv0.1.4です。3 archiveとSHA256SUMSを認証headerなしのcurlで取得し、全checksum一致・MIT/README/config/同梱ガイド・Windows PE x64 / Linux ELF x64 / macOS Mach-O arm64を確認しました。SHA-256は次のとおりです。

| archive | SHA-256 |
|---|---|
| `shellboy-v0.1.4-x86_64-pc-windows-msvc.zip` | `851e4f3f495f8dc0ef434a3a7c95a841b43e0780899634983d9db33f247784d8` |
| `shellboy-v0.1.4-aarch64-apple-darwin.tar.gz` | `940db94a34644f3ee2964a6fe6739ffc12241b1c4f59fbf2b2e8dd859bf987ab` |
| `shellboy-v0.1.4-x86_64-unknown-linux-gnu.tar.gz` | `fbc3e6edda422430e87db6ec0239fe820578c8f739f2fd20fa3b47d0911ef3a1` |

公開macOS binaryを実行しversion=0.1.4、help正常終了、docs getの設定ガイドがok:trueかつsession_headerを含むことを確認しました。旧binaryをGit管理外へ退避し、同directory内の一時fileから `~/.local/bin/shellboy` へ原子的に置換しました。配置後SHA-256は `c014c22c4c0b9f09abd57dbbb90f576e5772dd71da48c5e2d662440df76723d4` で配布binaryと一致し、version/helpを再確認しました。設定・既定DB・利用中ownerは変更していません。実行中processには再起動が必要です。

Windows対話端末・会社proxy・GUIでのベル/再描画・15万token超での切断率低下は未検証で、native CIや合成probeをその実機確認へ読み替えません。#4・#9〜#15の8件へ対応内容・最終CI・公開版の結果をコメントし、再取得で本文一致を確認しました（[#15の対応記録](https://github.com/tya5/shellboy/issues/15#issuecomment-6444359131)）。旧tag・配布物は保持しています。公開後の追記はstatus/acceptanceの結果記録だけで、製品・同梱guide・workflowを変更しないため、tagの上記native CIを根拠とし記録更新だけでは再実行しません。

## v0.1.4: Issue #15・追加レビューの検証

2026-10-27、macOS ARM64 / Rust 1.99.1。利用者が指定した#4/#9/#10/#11/#13/#14のレビュア指摘14件と#15を対象に、実装・文書・回帰試験を追加しました。最初の標準checkはstrict Clippy成功、単体484 passed / 1 failed / 6 ignoredでした。失敗は追加したCtrl+L試験が日本語のwide cell継続セルまで文字列化したためで、表示に合わせてUnicode幅で走査する試験へ直しました。製品の再描画処理をその失敗に合わせて変更していません。

並行レビューで使用量SQLのwindow処理に巨大usageを含める問題を見つけ、IDだけを順位付けしてから64KiB制限付きで本文へjoinする構成へ修正しました。SSE後処理中の取消しでも受信済み推論・本文・完了item・最終rawを保全し、最終rawと異なる途中deltaを含む取消し試験で確認します。最終 `sh scripts/check.sh` は終了0。fmt・全target strict Clippy・70群1,179 passed / 0 failed / 19 ignoredが成功しました。SSEの直接2要求/Control会話2Turnとも1 TCP、後続本文の停滞・巨大末尾・取消しでの上限と受信証跡保持、Thread/子/fork/要約/再起動後retryのID、設定独立切替を含みます。履歴全90commitと今回staged差分のgitleaks検査は検出0。Windows対話端末・会社proxy・大contextのEOF低減は未検証です。初回の [CI 40511343475](https://github.com/tya5/shellboy/actions/runs/40511343475) はLinux 1,180 passed / 0 failed / 18 ignored、macOS 1,179 / 0 / 19で成功、Windowsは1,073 / 1 / 14でした（いずれもTests stepのみ、別stepの重複試験は合算しない）。失敗は既存storage_checkpointのidle checkpoint観測待ち1件です。tracer登録前に処理される競合と、待つだけでは新しいidle機会が来ない試験構造を修正し、登録と更新を同じDB jobへまとめました。更新直後の境界を意図的にeligibleにし、read jobを逐次投入して実際のattemptを観測、actor barrierで後処理完了を同期します。製品のcheckpoint処理・間隔・保存契約は変更していません。修正後のfmt・全target strict Clippy・storage_checkpoint 3 passedは成功。先の70群結果とこの試験だけの確認を区別し、[次のCI 40513664128](https://github.com/tya5/shellboy/actions/runs/40513664128) ではcheckpoint試験はWindowsでも成功しましたが、既存management_scope試験のfrontend終了待ちが10秒でtimeoutしました。保存結果・閉じた画面への遅着拒否のassertは通過後のcleanupです。Windows lib全体は100.72秒と初回より遅く、shutdown_graceは完了期限ではありません。12か所のfixture終了待ちを、更新排出とJoinHandleの直接awaitを行う共通helperへ整理し、busy-pollを除去、失敗時に最後のShutdownProgressを表示します。Windows watchdog30秒・その他15秒で強制終了を使わず、製品のshutdownと猶予の試験は変更しません。Windows CIは独立DB fixtureの同時実行を2件に制限し、試験内の並行処理・対象件数・assertは維持します。最終helper（Windows30秒/その他15秒）のfrontend::control_testsは19 passed / 0 failed（472 filtered out）、fmt・全target strict Clippyが成功しました。対象外を成功数へ合算せず、最終3 OS CI・配布結果は確定後に追記します。

最終sourceから生成したunit test実行ファイルを使い、既存ローカルLiteLLMのgpt-5.6-lunaへopt-in合成probeを正確に4要求送信しました（別枠1 passed）。同じ合成入力、各最大出力512・effort low・90秒上限・store:false、API自動再送なし。入力23,267 tokens、header無効の初回/2回目はcached_tokens=0/23,040、header有効も0/23,040（約99.0%）でした。所要時間は順に2984/2600/1374/1507msです。無効でもhitしているため、header単体の因果効果や速度改善は主張しません。本文prompt_cache_keyはこのprobeでは既定false、on/off独立性はmockで検証します。利用者の会話・既定DB・15万token超の負荷・会社proxyを使った検証ではありません。#15に既存の利用者による17.6k入力の報告もありますが、本測定とは別の証拠です。

## ローカル配置の確認（2026-10-27）

利用者の明示指示により、source `90cf354`の検証済みdebug binaryを`~/.local/bin/shellboy`へ配置しました。SHA-256は`6e56157fd7857749fe86dcb39660368e7ecaade15d8819e7de2eaf9bbe4fa38e`で、直前の全体check・PTY確認に使った実行ファイルと一致します。旧版をGit管理外へ保存し、同一directoryの一時fileから原子的に置換しました。

配置後の`--version`は0.1.3、`--help`は正常終了、`docs get --id shellboy/configuration --limit-bytes 200`は有効JSONかつok:trueでした。shellからの短い実行名も同じ配置先へ解決しました。リリースの版番号は変更しておらず、既定config/DBや起動中processを変更していません。今回の差分は配置記録だけで、製品sourceの全体testは前節の結果を参照します。push/releaseは行っていません。

## Issue #11〜#14: 接続再利用後の最終検証（未リリース）

2026-10-27、macOS ARM64 / Rust 1.99.1。#11/#12は`1a1ab57`、#13は`1bcc19a`へ分け、#14の接続再利用を本節と同じcommitへまとめます。既定データ・設定・利用中ownerは変更せず、以下の保存/端末確認は一時DBと合成入力だけで行いました。

- 最終 `sh scripts/check.sh` は終了0。fmt・全target strict Clippy・70群1,164 passed / 0 failed / 18 ignoredが成功しました。最初の標準checkはruntimeの5件が失敗しました。いずれも途中EOF/停滞後の即時復旧を前提とする手動復旧fixtureで、新既定では再試行中だったためです。新しい既定の対象判定は変更せず、これらのfixtureへresponse_retry_partial=falseを明示して全targetを再実行しました。新既定/optout/待機/途中tool非実行はresponse_retriesが引き続き検証します。
- client単体では、catalog→embedding→JSON Responsesを取得する3要求が1 TCP、Control経由のcatalog2要求も1 TCPでした。別profileのURL/header/TLS/期限設定の分離、profile初期化失敗の局所化、サーバーがConnection: closeを返した後の新規接続も確認しました。TLS/期限の分離試験自体は設定値とHTTP接続先の確認であり、2つのHTTPS profileを同時に実行する試験ではありません。実際のTLS/期限の振舞いは既存model_tls/http_deadlines回帰と区別します。
- 別process・current-thread runtimeの限定試験で、同じclient/1 TCPを使った次要求へ新しい環境変数のAuthorization値が反映されました。親test processの資格情報は変更していません。資格情報子試験は親試験内で実行され、別の成功数として加算しません。
- 最終debug binary（SHA-256 `6e56157fd7857749fe86dcb39660368e7ecaade15d8819e7de2eaf9bbe4fa38e`）のmacOS PTYで、HTTP1.1のkeep-aliveを通知しながら初回応答後にTCPを閉じるloopback mockを確認。1回のshell出力を次入力で受け取り、同じTurnの2要求/2 TCPで正常回答し終了0でした。これは予告のない切断後に新規接続できた確認で、必ずstale socketへ書き込むraceを強制した証拠ではありません。
- 最終sourceの実時間outage試験も既定値で明示実行し、61秒のHTTP503障害を越えて63.18秒で成功しました（別枠1 passed、標準ignoredの実行）。外部proxyの障害を発生させたものではありません。
- ローカルLiteLLM `127.0.0.1:4000/v1` / `gpt-5.6-luna` へ合成の短い応答を4要求だけ送るopt-in probeを実行し成功（別枠1 passed）。要求ごとにclient生成した2標本は1485/1358ms、共有した2標本は1440/1466msでした。各最大出力512・45秒上限・store:false。計測はclient取得後から応答完了までで、client生成費用は含みません。4標本では速度向上を確認できず、loopbackでのモデル生成時間をTCP/TLS handshakeの効果と読み替えません。利用者履歴や実会話は送信していません。

最終binaryのベルもPTYで再確認し、起動時0、有効時は復旧でBEL1、無効時0、両方正常終了でした。fmt/diffと文書相対リンク485件（欠落0）は成功。差分の秘密情報検査をcommit前に実施します。Windows/Linux native CI・会社proxy、端末GUIの実音/点滅・マルチプレクサでの体感は未実施です。public releaseとインストール済みbinaryはv0.1.3のままです。push/releaseは行いません。

## Issue #13: HTTP処理の構造整理（未リリース）

2026-10-27、macOS ARM64 / Rust 1.99.1、変更前HEAD `1a1ab57`。#11/#12の動作変更から分け、HTTP層に散らばった再送許可を受信事実へ移し、Driverのclassify_retryへ集約しました。Error.kindやDB保存形式、client生成時点は変更していません。

- `cargo test --locked --lib --test config --test model --test model_http --test http_deadlines --test response_retries --test frontend_activity --test tui_recovery --no-fail-fast` は8群564 passed / 0 failed / 4 ignored。#11の対象試験を同じ動作で確認し、事実型への置換でHTTP試験のassertionを更新しています。新しいDriverの組合せ表試験と、完全なJSON文書のin_progress/incomplete等を通信中断として再送しない境界を追加しました。
- 指数待機・Retry-After・取消し、partial原文保全/非採用、元toolの未実行、metadataと生成出力、HTTP error bodyの期限、完成済みJSONの検証失敗、SSE terminal後の待機打切りは既存回帰で成功しました。既存の61秒outage試験はこのcommitでは再実行せず、#11の結果と区別します。
- 構造計測: http.rsの試験前の製品部分746→308行、response_attempt166→95行、HTTP側の再送許可生成4→0か所、Driverの対象判定はclassify_retryの1か所。新しいresponse.rs397行/body.rs163行を加えており、ファイル分割を総行数削減とは数えません。

最終all-target strict Clippyとfmt/diffは成功。変更差分の秘密情報検査をcommit前に実施します。新しいworker/queue/永続状態/依存、API要求、既定DB/設定の変更、push・release・インストール済みbinary更新はありません。Windows/Linux native CI・会社proxy実機は未実施です。

## Issue #11/#12: 連続失敗の再試行と端末ベル（未リリース）

2026-10-27、macOS ARM64 / Rust 1.99.1、変更前HEAD `09feebe` から本節と同じcommitの変更です。合成fixtureと一時DB・loopback HTTPを使い、既定の利用者データ・設定を変更していません。

- `cargo test --locked --lib --test config --test model --test model_http --test http_deadlines --test response_retries --test frontend_activity --test tui_recovery --no-fail-fast` の対象8群は562 passed / 0 failed / 4 ignored（下記の再実行を含み、重複計上なし）。初回は新規bell試験のDuration import漏れでcompile失敗。修正後は、tool周期を2→3へ増やしたfixtureのspill数期待が旧1のままで1件失敗し、最新を除く旧group2件という実際の対象に合わせて期待を2へ直しました。製品コードは変更せず、response_retries最終13 passed / 0 failed / 1 ignoredを確認しました。
- 連続成功を挟んだ予算リセット、context圧縮だけではリセットしないこと、上限/0、metadata/partial EOF・切断・停滞timeout、partial optout、Retry-Afterの指定待機と600秒超の保留、停止・終了による待機取消しを検証。partial監査nodeは失敗attemptと同時保存し、採用path・次入力への混入と途中toolの実行がないことを確認しました。
- ignoredの `default_retry_policy_survives_a_minute_long_proxy_outage` を明示実行し、既定予算と待機で61秒間のHTTP503障害を越え、63.15秒で正常完了（別枠1 passed）。外部APIや実利用proxyの障害を起こしたものではありません。
- 復旧CASの更新後だけcommit後通知すること、同じ状態/旧generation/停止では通知しないこと、明示再開後の再遷移、準備失敗、非選択/子会話、起動時非再配信を確認しました。UIのbyte backendではBELだけ1byte、2秒集約、設定false、下書き/選択/overlay不変を確認しました。
- 実debug binary（SHA-256 `c25972d458de15cbd83b89e1cab7c143b6ae321cebb62170a72ca87113601036`）をmacOS PTYで起動し、合成HTTP503再試行→400復旧を実行。起動時は無音、有効時はOSC外BEL1byte、無効時は0byte、両方とも要求2回/終了0でした。最初の補助手順は起動完了前に入力し要求0件だったため判定せず、F1表示を待つ手順で実行しました。

最終all-target strict Clippyとfmt/diffは成功。変更差分の秘密情報検査をcommit前に実施します。Windows/会社proxy・端末の実音/点滅・WezTermやtmux等での体感確認、全70群の標準testはこの段階では未実施です。新しいschema/永続状態/worker/queue/依存は追加せず、外部モデルへの要求、push・release・インストール済みbinary更新は行いません。

## Issue #10: 接続先が混在する履歴の再送（未リリース）

2026-10-27、macOS ARM64 / Rust 1.99.1、変更前HEAD `609b244` から本節と同じcommitの変更です。送信用projectionだけでmessage/function_callの直下idを除去し、保存JSON、call_id、phase/status、未知・ネストフィールド、同epoch reasoningを保持します。

- 最終 `cargo test --locked --lib --test replay_cache --test runtime_features --test runtime --test model --test model_http --no-fail-fast` は6群553 passed / 0 failed / 3 ignored。実Compactorによるspill生成→ReplayCache送信変換、保存spill/元nodeの不変、通常/wrapper/直接roleのprojection、同epoch reasoningとepoch切替、旧adapterのstale frame破棄を確認しました。先行4群513件は最終群に含むため重複計上しません。
- 最終all-target strict Clippyは成功。初回は追加テストの固定fixture配列をVecにした警告1件で失敗し、arrayへ直して再実行しました。製品コードは6群成功時から変更していません。最終replay_cache群の再実行も9 passed / 0 failedです（553件に重複加算しません）。fmt/diffと変更文書の相対リンク34件は成功。全70群の標準testとWindows/Linux native CIは未実施です。
- 既存DBを閉じて再オープン後にRunを開始し、mock HTTP400→復旧待ち→明示RunRetry→正常完了、保存済みJSON文字列の不変を確認しました。不正なmessage/function_call idが残ればmockが拒否します。自動HTTP400再試行は加えていません。
- ownerの正常終了・クラッシュ再起動は既存Runを停止する契約です。再起動を挟んだ同じRunへのretry成功は主張せず、同じThreadの保存履歴を再オープンした場合と、生存owner内のretryを確認します。
- `127.0.0.1:4000/v1/responses` の `gpt-5.6-luna` に合成データだけで2要求を送り、どちらもHTTP200/completed（約1.56秒・1.44秒）。実際の回答messageと合成function_call/resultのIDを送信copyから除いた再送が受理されました。この確認はPython製の最小HTTP probeであり、Rust製品のprojectionは別の回帰で検証します。store:false・非stream・最大出力512、ユーザー履歴/configは送信せず、shellを実行していません。proxyからreasoningが返らなかったため、実際のreasoning/暗号化本文の再送は未検証です。

Windows/会社Azure proxy、他モデル、reasoningやfunction_call_output固有IDのprovider互換は今回の成功に含めません。新schema/設定/worker/依存は追加せず、既定データ・owner・インストール済みbinaryは変更しません。push・releaseは行いません。

## Issue #4: 環境別の起動案内とPATH（未リリース）

2026-10-27、変更前HEAD `eab140b`、macOS ARM64 / Rust 1.99.1。今回の差分はガイド・判断記録・検証記録だけです。起動環境の案内はRustのshell adapterとcurrent_exeによるPATH追加に照合しました。

- `cargo test --locked --lib docs::tests` と同じfilterでガイド単体8 passed / 0 failed。最後の差分はmintty URLのfragment除去、未確認環境の表現限定、空白パス例の文言整合だけです。後続の最終 `cargo test --locked --test docs_cli --test docs_proxy --no-fail-fast` はdocs CLI3 passed / 0 failed、実API opt-in1 ignored。最初のfilter付きコマンドで統合群は0件だったため、最終はfilterなしで実行し、0件実行を成功件数に加算しません。
- 設定ガイドのTOMLコードblock3件をPython tomllibでparseし、PowerShell 7/Git Bashのliteral backslashを含む構文が有効なことを確認。配置先や実行ファイルの存在は各環境で設定する前提です。
- 実PATH確認は `eab140b` のdebug実binary（SHA-256 `47cf1aee9abe16c1508b7c946206bb1cb0521dc55f9bc020c01355f83834edfb`）を絶対パスでmacOS PTYへ起動。親PATHを `/usr/bin:/bin:/usr/sbin:/sbin` だけにし、モデル用Bashを `/bin/bash` に固定しました。loopback HTTP mockからshellを呼び、`command -v shellboy`が同じbinaryへ解決し、`shellboy --version`・設定ガイドgetが成功、markerを次のモデル要求が受け取り、mock合計2要求・TUI終了0でした。ガイドの文面変更後に同じ実PATH試験を繰り返してはいませんが、該当Rust処理は変更していません。
- fixtureのdata/configは一時領域で破棄し、既定の設定・DB・ownerは変更していません。モデル/API/embeddingの外部要求はありません。
- Windows/PowerShellのpipe・Job等のnative CIは公開済み版の記録を参照します。Windows Terminal/WezTerm/Git Bash/pwsh/mintty/WSLでの今回の対話実機確認は未実施。mintty公式のConPTY対応とGit for Windows公式のbin wrapper説明に照合し、一律に非対応とする提案は採用していません。

fmt/diff、文書相対リンク、差分の秘密情報検査をcommit前に確認します。今回push・release・ローカルbinaryの置換は行いません。


## Issue #9: 明示的な全画面再描画（未リリース）

2026-10-27、macOS ARM64 / Rust 1.99.1、変更前HEAD `7dc1726` から本節と同じcommitの差分を確認しました。最終 `cargo test --locked --lib --test ui --test tui_process --test terminal_links --test tui_shutdown --no-fail-fast` は5群549 passed / 0 failed / 3 ignoredです。

- TestBackendの描画面だけへ外部文字を書き、通常diffで残ること、Ctrl+Lの実描画経路で元bufferへ一致することを確認。composer/help/threads/details/operations/models/management/confirmation/selection/shutdownの各入力所有者で下書き・anchor・選択・overlay・notice・追従状態を保持します。
- 設定したsend/newlineへのCtrl+L割当が優先され、helpへ無効理由を表示します。Repeat100件をbool一つにまとめ、frame時刻前は描画せず、明示操作だけ選択固定中に描画します。KeyReleaseで予約しません。
- 実binary・macOS PTY・loopback HTTP mockで応答中の入力、完了、外部slaveへの直接文字出力、Ctrl+Lの画面消去、下書き/非追従anchorの再描画、正常終了を確認しました。初回はTerminal::clearの物理cursor照会待ちで失敗したため、固定Ratatuiの同一viewport resize経路へ修正し、全対象群を再実行して成功しました。
- HyperlinkBackendのclear/clear_region後にリンクなしのviewport全体を再描画し、古いOSC 8 URLを出さないことを確認しました。
- 最終all-target strict Clippy・fmt/diffは成功しました。Windows/Linux native CI、Windows Terminal/WezTerm/herdrのGUI視覚確認、実APIは未実施です。PTYの制御列確認をGUI端末の視覚的受入へ読み替えません。

文字幅不一致など元の画面ずれの根因は未特定で、周期的再描画やCore変更は加えていません。今回push・release・ローカルbinaryの入替えは行いません。


## EOF実装監査とSSE framing（未リリース）

2026-10-27、macOS ARM64 / Rust 1.99.1、公開済みv0.1.3から本節と同じcommitの変更を検証しました。報告元はWindows/v0.1.2であり、Windows実機・会社proxyの現象そのものは再現していません。v0.1.2〜v0.1.3で該当parserの終端判定は同じです。

- 修正前の `cargo test --locked --test model sse_framing_ -- --nocapture` は追加2件が失敗。BOM付き単一completedイベントは `EOF before response.completed`、LF deltaの後のCR単独終端は `EOF in partial SSE event` を再現しました。実障害の原因確定とは区別します。
- 修正後の `cargo test --locked --lib --test model --test model_http --test http_deadlines --test response_retries --test frontend_activity --test tui_recovery --no-fail-fast` は7群526 passed / 0 failed / 3 ignored。全byte splitとbyte単位のBOM/CR/LF/CRLF、UTF-8、複数data行、正常completed直後のHTTP close、終端不在/未完了での失敗・再送制限を確認しました。
- その後の変更は混在CRLF/LF・CR/CRLFの明示fixtureとガイド。最終の `cargo test --locked --lib --test model --test docs_cli --no-fail-fast` は3群486 passed / 0 failed / 3 ignored。単体461件・model22件は重複計上せず、先行5統合群43件とdocs CLI3件を合わせて8群529 passed / 0 failed / 3 ignoredです。製品のparserは先行7群成功時から変更していません。
- 最終sourceの `cargo clippy --locked --all-targets -- -D warnings` とfmt/diffが成功しました。文書相対リンク492件の欠落は0件。差分の秘密情報検査をcommit前に実施します。
- EOF診断は受信byte数・解析JSON数・item done数・DONE marker数・残留byte数・固定のevent分類だけです。合成の本文/type値をエラーへ出さないことをテストしました。通常diagnostic logへエラー全文は追加せず、既存のattempt error保存/受信原文経路を利用します。

HTTP受信はchunkを解析後に次を読む単一loopで、EOF追い越しやWindows専用parserは見つかりませんでした。SSEのevent名だけ、DONEだけ、空行未到達のJSONをcompletedへ昇格する変更は行っていません。通常のcreated先行streamならBOMだけでは頻発を説明できず、会社環境での原因は未確定です。全70群の標準test、Windows/Linux native CI、実モデルへの追加要求は未実施。push・release・インストール済みbinaryの変更は行いません。


## v0.1.3公開時の最終検証

2026-10-27、tag `v0.1.3` / commit `bbb9758d8cee85a6a1cc43dfc9e9015508f6f95c` で検証しました。下記は途中の変更ごとの結果を置き換える最終sourceの結果です。後続の公開記録commitは文書だけを更新します。

- macOS ARM64 / Rust 1.99.1の `sh scripts/check.sh` は終了0。fmt・strict all-target Clippy・70群の1,127 passed / 0 failed / 15 ignoredが成功しました。各test executableの初回起動待ちが長いため、別途 `--list` の読み取り専用列挙を8並列で実施しました。列挙結果は成功件数へ加算していません。ローカル試験の完了待ちの間にmainをpushしてCIも並行実施し、両方の成功後にtagをpushしました。
- [CI 40482476917](https://github.com/tya5/shellboy/actions/runs/40482476917) は3 OS成功。all-target試験はWindows 1,022 passed / 0 failed / 10 ignored、macOS 1,127 / 0 / 15、Linux 1,128 / 0 / 14。Windowsの独立したoverflow診断1件も成功し、all-targetの件数へ二重計上しません。
- [Release 40484109212](https://github.com/tya5/shellboy/actions/runs/40484109212) はnativeのWindows x64・macOS ARM64・Linux x64でlocked release buildとversion/help起動を確認し、3 archiveとSHA256SUMSを公開しました。
- 公開URLから認証なしで4 assetを取得し、3 archiveのSHA-256を照合しました。Windows ZIPの整合性と、各archiveの実行ファイル・MIT LICENSE・README・設定例・利用/Agentガイドの同梱を確認しました。非公開snapshot・個人cacheの同梱はありません。
- 公開macOS binaryのversion/helpが成功し、ローカルの `~/.local/bin/shellboy` へ原子的に配置しました。配布版と配置版のSHA-256一致、配置後の `shellboy 0.1.3` を確認しました。旧binaryはGit管理外へ退避し、利用者の設定・既定DB・利用中ownerを変更していません。
- 公開source時点の全履歴81 commitのgitleaks検査は検出0件でした。公開記録commitの差分と全履歴も検査します。

DB schemaはv0.1.2と同じv9でmigration追加はありません。既存設定は新項目を省略でき、CLI・保存先も変更していません。旧版へ戻す場合、新しい接続設定keyを設定ファイルから除く必要があります。実会社proxy/Windows対話端末や実モデルへの追加課金を伴う確認は行っていません。600秒の無受信既定値は暫定であり、company proxyでの測定保証ではありません。


## Thread一覧の状態色（未リリース）

2026-10-27、macOS ARM64 / Rust 1.99.1、変更前HEAD `029c42a` から本節と同じcommitの `src/ui/threads.rs` / `src/ui/theme.rs` の表示差分を確認しました。`cargo test --locked --lib ui::threads::tests` は13 passed / 0 failed。実TestBackendの120列/24列で既知11状態と未知状態のタイトル色を検証し、選択背景を残したまま完了の緑を保持すること、同IDの再取得で復旧待ちの赤へ変わることを確認しました。既存の森林・絞込み・page/世代・狭幅表示の回帰も同じ対象群に含みます。

最終sourceの `cargo clippy --locked --lib -- -D warnings`、fmt/diffは成功しました。差分の秘密情報検査は検出0件です。

一覧の取得済み状態の描画だけを変更し、追加の取得・監視や保存処理は加えていません。全体テスト、今回差分のWindows/Linux native CI、実端末GUIでの配色確認は未実施です。利用者DB・設定・インストール済みbinaryは変更していません。push・releaseは行っていません。

## Issue #6: 解決後の受信記録表示（未リリース）

2026-10-27、macOS ARM64 / Rust 1.99.1、変更前HEAD `3852333` から本節と同じcommitの表示差分で確認しました。`cargo test --locked --lib --test frontend --test frontend_activity --test tui_recovery --test ui --test ui_scroll_refresh --no-fail-fast` の統合5群111件が成功し、その後のfixture/旧文言期待値の修正後に `cargo test --locked --lib` の460 passed / 0 failed / 3 ignoredが成功しました。合計6群571件、重複実行は加算しません。最後の変更は試験fixtureのみで、成功した統合試験から製品コードは変わっていません。

- 元Turnのrecovery_wait→running→recovery_wait→stopped/completedの各保存状態で、previewとF6本文windowの分類を確認しました。DataOwnerを終了・再起動しても完了後は通常通知となり、監査payload・attempt error JSON・採用pathを変更しないことを検証しました。
- 同じroot_revisionの再読でも、スクロール固定中の色を注意→通常→注意→通常へ更新。本文窓・offset100・reasoning・anchorを保持し、再描画と実TestBackendの見出し色・本文表示を検証しました。通常Event省略groupへ受信本文を隠しません。
- 既存の空本文非表示、過去35件の32件ページング、source IDと原文cursor、同Turn配置、遅着・窓revision・スクロールの回帰も成功しました。
- 一覧は同Turnの割当receiptから元依頼を取得し、別Turnの依頼やThread件名へ取り違えません。SQL80字制限、35件/2page、ローカル時刻、状態分類、制御文字の正規化を検証しました。

最終sourceのall-target strict Clippy、fmt/diffは成功しました。差分の秘密情報検査は検出0件、文書の相対リンク500件は欠落0件です。

今回70群の標準check全体は再実行していません。先行#7実装の全体結果は下記と区別します。実provider/会社proxy、実端末GUIのキャプチャ、Windows/Linuxの今回sourceでのnative CIは未実施です。利用者の設定・保存データ・利用中owner・インストール済みbinaryは変更していません。push・release・Issueクローズも行っていません。

## Issue #7: 未受信時の有限再試行とIssue #8の統合（未リリース）

2026-10-27、macOS ARM64 / Rust 1.99.1で実施しました。実装commit `3569524` の `sh scripts/check.sh` は終了0、fmt・strict all-target Clippy・70群の1,120 passed / 0 failed / 15 ignoredです。先行対象9群535件も成功しましたが標準結果へ二重計上しません。

| 確認対象 | 実施と結果 |
|---|---|
| 対象HTTPと待機 | 実Controlとloopback HTTP mockで429/500/502/503/504の本文受信後エラーから回復。秒・HTTP-dateのRetry-Afterを接続別10ms上限で制限。HTTP client単独は依然一要求一POST |
| 回数と停止 | 設定0/2の正確な要求数、上限到達の復旧待ちと保存回数、30秒待機中のRun停止/owner終了で追加要求なしを検証 |
| 受信の境界 | headers前切断・本文0bytes切断・空SSEの正常EOFは対象。1byte、comment、推論、tool引数、平文、JSON断片の受信後は再送しない。未完成toolの実行0件 |
| #8との統合 | 本文未受信の停滞timeoutを再送しない。503エラー本文が途中停止した場合もtimeout/status503/正しい接続設定keyを保ち、再送しない。既存HTTP期限6試験も成功 |
| 予算共有 | 503→tool成功→503→tool成功→明示context超過/spill→503の6要求で、shell/result各2件、spill1件、generation不変、再試行予算2/2のまま復旧待ち。成功/圧縮で補充しない |
| 保存と表示 | 失敗attemptに予約回数・上限・待機・重複課金可能性を保存。TUIの通常色の回数表示、成功後解除、最新同generationへの限定、#8受信時計の切替を検証 |
| 設定と従来復旧 | 既定2/30000ms、範囲0〜5と1〜300000ms、負数/型/上限拒否。明示復旧fixtureは回数0とし、添付・RunRetry・TUI復旧の従来契約を維持 |

全体check後の差分は `src/frontend/activity.rs` の復旧理由の表示投影だけです。回数付きの上限到達診断でも既知のモデル要求段階を示し、保存済み回数から構成した文全体との完全一致を条件にします。metadata欠落・不正値・不一致・古いgeneration・状態不一致は段階不明を保ち、元JSONを変更しません。`cargo test --locked --lib --test frontend_activity --test tui_recovery --test response_retries --no-fail-fast` は474 passed / 0 failed / 3 ignored（455+7+2+10）でした。最終sourceの `cargo clippy --locked --all-targets -- -D warnings`、fmt/diffも成功しました。この小さな表示差分後に70群全体は再実行しておらず、先行実装の標準結果とは区別します。

最終差分の秘密情報検査は検出0件、公開文書の相対リンク499件は欠落0件です。先行公開commit `13b96a1` の [native CI](https://github.com/tya5/shellboy/actions/runs/40449915241) は3 OS成功ですが、今回#7のnative検証には読み替えません。今回は合成入力・一時DB・loopback mockだけで、実provider/会社proxy、Windows/Linuxの今回sourceでのnative CIは未実施です。設定・利用者DB・利用中owner・インストール済みbinaryは変更していません。push・release・Issueクローズは行っていません。

## 公開後のWindows CI起動取消し試験（未リリース）

対象は本節と同じcommitの `src/frontend/startup.rs`（変更前のローカルHEAD `41baf46`）、macOS ARM64 / Rust 1.99.1です。[公開CI 40420705214](https://github.com/tya5/shellboy/actions/runs/40420705214) は文書のみの `211750e` でmacOS/Linux成功、Windowsの `cancellation_during_migration_preparation_keeps_lock_until_writer_finishes` だけが `SaveFailed / SQLite: database is locked` で失敗しました。先行v0.1.2の3 OS成功とは区別します。

- 原因条件の再現：旧試験で外部writerの解除前に一時的に2.3秒待つと、同じエラーで意図した失敗（終了101、2.33秒）になりました。この診断用遅延は除去済みです。製品の2秒busy timeoutを越す試験側競合を確認しましたが、Windowsでの実際のscheduler待ち時間は測定していません。
- 修正後の `cargo test --locked --lib frontend::startup::tests -- --nocapture`：3 passed / 0 failed。Database準備時の同期取消し、実移行、終了とowner解放、endpoint未公開、失敗時rollback、未受取Readyの終了を確認しました。
- 同じlibtest binaryを4プロセス並列・各 `--test-threads=3` で20回実行：20回とも3 passed / 0 failed。独立した60試験が増えたとは数えません。
- `cargo test --locked --test storage --test app_ipc --test tui_process`：24 passed / 0 failed / 0 ignored。受付済みwriterが全呼出し元drop後もlockを保持する既存storage試験、移行の原子性、実IPC、local mockに接続する実TUI processを含みます。

最終sourceの `cargo clippy --locked --all-targets -- -D warnings`、`cargo fmt --all -- --check`、`git diff --check` は成功しました。staged秘密情報検査は検出0件です。

push前に `305e088` で `sh scripts/check.sh` を再実行し、終了0、fmt・strict all-target Clippy・全69群の1,103 passed / 0 failed / 15 ignoredを確認しました。Windows/Linux native CIの今回sourceでの結果、release・Issue #3のクローズは本記録時点では未確認・未実施です。利用者の保存データ、設定、実行中owner、インストール済みbinaryは変更していません。

## Issue #8: HTTP期限・取消し・受信経過（未リリース）

2026-10-27、macOS ARM64 / Rust 1.99.1で実施しました。`6aeac45` の `sh scripts/check.sh` は終了0、fmt・strict all-target Clippy・69群の1,102 passed / 0 failed / 15 ignoredです。先行の対象テスト8群576件も成功しましたが、標準結果へ二重計上しません。

その後の `32e3359` は `src/model/http.rs` だけの差分です。HTTPエラー本文の受信も非永続の活動観測へ渡す補強とネットワーク回帰1件を加えました。`cargo test --locked --lib --test model_http --test http_deadlines` は473 passed / 0 failed / 3 ignored、最終 `cargo clippy --locked --all-targets -- -D warnings` とfmtも成功しました。この小さいHTTP差分後に69群全体は再実行していません。最終コードの検証範囲と、標準check対象を区別します。

| 確認対象 | 実施と結果 |
|---|---|
| ヘッダー・空本文・部分本文・エラー本文の停滞 | 接続を維持するlocal TCP mockで期限切れを検証。kind timeout、header/body、経過ms、正しい接続設定keyを保持。401/503の既知statusも失わない |
| 長い正常stream・無効化・接続別設定 | heartbeatが続くstreamは全体/idle設定より長くても完了。0では取消しまで待機。別clientの期限へ影響しない。モデル一覧は設定と20秒の短い方 |
| 索引 | `stalled_embeddings_retry_then_pause_and_manual_retry_restores_pending_work`：期限切れ→有限retry→pause→明示retry→次の入力も採用 |
| 検索の取消し | caller tokenとowner停止に加え、`history_search_ipc_disconnect_closes_embedding_http_and_next_observation_succeeds`で実LocalServer/IPC切断→60秒を待たずHTTP閉鎖→次の検索成功→receipt増加なし |
| 会話の保全 | `stalled_stream_enters_durable_recovery_and_preserves_received_text_without_replay`：実mock streamの部分受信後に停止、本文・timeout診断を保存して復旧待ち、追加要求・shellなし |
| 活動表示 | 単調時刻を使う閾値、再受信、終端、Thread/generation/attemptの遅着拒否、履歴layout不変を自動確認。HTTPエラー本文の各受信も観測通知として到達し、assistant本文を作らない |
| 設定 | 既定・接続別override・0・min1000、負値/不正型/範囲外拒否、Debugのredactを確認 |

指定の実proxyへ合成入力・high effortで1要求を送りましたが、HTTP400、header待機約0.893秒で拒否されました。正常streamの最長無受信時間は測定できておらず、600秒の既定を実測保証したとは扱いません。測定は本文・資格情報を保存せず、再試行・fallbackは行いませんでした。実provider互換、会社proxy、Windows/Linuxの今回sourceでのnative CI、実端末で30秒待つ表示確認は未実施です。

最終ビルド `32e3359` の同梱docsは、空cwd・環境変数なし・設定なし・owner/APIなしで33 CLI操作、13ガイド・3カテゴリ・117同梱リンクが成功しました。状態ファイルは作成しません。staged差分と全履歴の秘密情報検査は検出0件、公開文書の相対リンク493件も欠落0件です。下記のv0.1.2 CI/配布成功は当時のsourceの記録であり、今回の未リリースsourceの3 OS合格には読み替えません。

## v0.1.2の標準チェックとnative CI

対象は `77da7e3`（製品修正 `d3cfa9b` にREADMEのみを追加）です。macOS ARM64 / Rust 1.99.1で `sh scripts/check.sh` が終了0となり、fmt・strict all-target Clippy・全68群の1,083 passed / 0 failed / 15 ignoredを確認しました。標準チェック完了後にpushしました。

[3 OSのnative CI](https://github.com/tya5/shellboy/actions/runs/40418072576)もfmt・Clippy・全68群が成功しました。

| OS | passed | failed | ignored |
|---|---:|---:|---:|
| Windows x64 | 979 | 0 | 10 |
| macOS ARM64 | 1083 | 0 | 15 |
| Ubuntu x64 | 1084 | 0 | 14 |

Windowsのoverflow単独試験は別枠で成功し、上表へ二重計上していません。Issue #3のreadfree・期限切れ・照会統合、Quit受付後の起動抑止、macOS一時領域と管理画面の表示回帰を含むsourceです。

`d2d53de` のCI失敗は、readfree契約に対する古い期待値と、管理画面の補完情報・配列の`items`が欠ける製品の退行に分けて修正しました。読取りの`request_operation_id`は保存receiptが存在しないため表示しない仕様を維持し、mutation側の表示は残しています。先行失敗を今回の成功でなかったことにはしません。

一時DB・固定HTTP応答・合成データによる検証です。会社proxy・任意provider・Windowsの実IME/GUI・本物のtmpfiles経年清掃は再検証していません。既定の利用者DBを開く、移行する、ownerを停止する操作は行っていません。

## v0.1.2の配布とローカル更新

公開tag `v0.1.2` は `77da7e3` を指します。[配布workflow](https://github.com/tya5/shellboy/actions/runs/40419581339)でWindows x64・macOS ARM64・Linux x64のrelease buildと各OS上のhelp/version確認が成功し、[Release](https://github.com/tya5/shellboy/releases/tag/v0.1.2)を公開しました。全archiveとSHA256SUMSを認証なしの公開URLから取得し、HTTP 200、checksum一致、実行形式（PE AMD64 / Mach-O ARM64 / ELF x64）、README・MIT License・設定例・公開docsの同梱と、私的資料を含まないことを確認しました。

| 配布物 | bytes | SHA-256 |
|---|---:|---|
| Windows x64 ZIP | 9582492 | `e9073ae4861901a66d22b5991c50931f552dda7ee3f43228b553eb33987c7533` |
| macOS ARM64 tar.gz | 9171993 | `9d48f1c782bfdde030f07306fbfed811cf9e264ff2d8c7a1824880dc276f4a99` |
| Linux x64 tar.gz | 9916360 | `834a5d10785b32950a98b2f14be8a64aff67a4a366fda826319db2929af0f2a0` |

公開macOSバイナリをローカルへ原子的に配置し、配布版とのSHA-256一致、`shellboy --version`の0.1.2、helpの成功を確認しました。旧バイナリはローカルに退避しました。既定DBや設定を開く操作、実行中ownerの停止は行っていません。新バイナリは次回起動から使われます。

公開バイナリの同梱docsも、空cwd・環境変数なし・設定なし・owner/APIなしで33 CLI確認、13 guide・3 category・117同梱リンクが成功しました。バイナリSHA-256は `dfd87fc0cfb622f66459c0fe45f52ab7d66228758bc91f02ff1e314746dbd4d6` です。

schema v9へ更新したDBはv0.1.1で開けません。ロールバックは更新前の整合backupを別data-dirへ復元する方法を案内し、user_versionの手動引下げは勧めません。配布後の完了記録は文書のみの変更で、検証済みtagのコード・同梱guide・依存・workflowを変更していません。

## 初版のローカル標準チェック

更新: 2026-10-26。macOS / aarch64、固定Rust 1.99.1で、改名後の公開用ディレクトリから実施しました。この表のローカル標準検証対象は公開Gitの `8de8699` です。後続のプラットフォーム修正とGitHub CIは下記の別記録で区別します。非公開snapshot・過去検証asset・作業台帳を置かない構成を使っています。

| 確認 | 結果 | 手順・対象 |
|---|---|---|
| 固定依存でのビルドとCLI名 | 成功 | 全targetをビルド。実binaryの `shellboy --help` と `shellboy --version`（0.1.0）を確認 |
| fmt・全target Clippy・テスト | 成功 | `sh scripts/check.sh`。67テスト群、982 passed / 0 failed / 15 ignored。修正後の通し実行は約7分 |
| オフライン同梱ガイド | 成功 | `scripts/check-docs.py`、33 CLI確認、13 guide・3 category、同梱リンク115件。空cwd・環境変数なし・存在しない設定・owner/API不要、状態ファイルの作成なし |
| 新しい既定保存先・環境変数 | 成功 | config / CLI / runtime試験。新しい名前・OS既定path・保存先解決を確認。旧名・旧データ形式への互換は対象外 |
| 一時owner・CLI・正常終了 | 成功 | cli_process / runtime / tui_process試験。IPC接続、同ID再送、実shellからのCLI呼出し、PTYでの入力・固定HTTP応答・終了を確認 |
| TUI描画・スクロール | 自動試験成功 | TestBackendと自動PTY。実IME・OS clipboard・各端末GUIの再確認は今回未実施 |
| 実provider互換・意味検索の品質 | 今回未実施 | 通常試験はloopback固定応答・固定vector等。実API/性能/実環境のignored試験を合格数へ含めない |
| 公開用checkoutの独立性 | 成功 | 公開対象320ファイルのみで上記build/test。literal include 63件の欠落なし、旧名称残存なし、公開文書の相対リンク欠落なし |
| 公開Gitの全履歴 | 検査実施 | gitleaks v8.30.1、全ref・全履歴、allowコメント無効、redact有効。検出0件。非公開ファイル・旧履歴commitを含まない |

`CARGO_BUILD_JOBS=2`、`CARGO_INCREMENTAL=0` を指定し、ビルド生成物だけローカルcacheへ出しました。ソース・guide・migration・fixtureは公開用ディレクトリのものです。この表の実施時点ではGitHub Actionsはpush前でした。現在のGitHub実行結果は [Actions](https://github.com/tya5/shellboy/actions) を参照してください。実会話・資格情報・個人path・私的画像・生ログは検証文書へ含めません。

初回の通し検証では、名称変更で長くなった合成トークンの出力を19 bytesと正しく取得していましたが、テストに旧名の15 bytesという固定期待値が残っていました。期待値をtokenのbyte長へ変更し、該当回帰単独と上記の通し検証で成功しました。製品の出力処理は変更していません。追加のガイド検証scriptにも旧cursor期待値があったため、最終ページでcursorを省略する既存契約へ合わせました。

採用範囲は [適用仕様](../design/effective-spec.md)、現在地は [status](../development/status.md)、標準手順は [setup](../development/setup.md)、公開対象と履歴の扱いは [公開構成](../development/publication.md) を参照してください。実端末の追加確認は [手順](manual-terminal.md) と実施結果を区別します。

## 初回のネイティブCIとリリース検証

[初回CI](https://github.com/tya5/shellboy/actions/runs/40241942578) で、LinuxのClippyがUnix PTY試験の引数を拒否し、Windowsではstdin pipe作成中のC構造体がawaitを跨ぐためfutureのSend要件を満たしていないことが判明しました。前者はUnix共通4箇所をraw pointerで渡す形へ、後者はC構造体を同期block内へ閉じる形へ局所修正しました。プロセス操作順序・handle所有・待機期限・保存形式は変えていません。

修正後のローカルmacOS確認はPTY関連5件と対象strict Clippyが成功しました。Linux/Windowsの成功は再実行したネイティブCIで確認し、Releaseに対応する実行結果と分けて扱います。リリースworkflowはWindows x64・macOS ARM64・Linux x64のnative release buildと実行ファイルのhelp/version確認、配布物とSHA-256を作成します。

## TLS設定追加とWindows fixture修正

先行候補 `a67c162` の [native CI](https://github.com/tya5/shellboy/actions/runs/40243159749) はmacOS/Linuxで成功しました。WindowsはClippy成功後、管理UIの5試験でfixtureのcwdがOSの絶対パス条件を満たさず失敗したため、合格とは扱いません。fixtureだけをOS別絶対パスへ変更し、macOS上の管理UI6件は成功しました。Windowsでの確認は変更後のCIを使います。

接続ごとの `ssl_verify` 追加後、ローカル設定13件と既存HTTP12件が成功しました。合成自己署名HTTPSの2試験も成功しました。Responses・モデル一覧GET・Embeddingsについて、既定/明示trueの拒否とfalseの成功、他Connectionへ影響しないことを実要求で確認しました。課金API、利用者設定・保存データ、OSの信頼ストアは変更しません。変更後の3 OS CIの結果は下記「公開済みv0.1.0の結果」に記録します。

## 公開済みv0.1.0の結果

初版の製品コードは `1e581fc` の [native CI](https://github.com/tya5/shellboy/actions/runs/40245585424) で次を確認しました。全OSでfmt・strict all-target Clippyも成功しました。

| OS | suite | passed | failed | ignored |
|---|---:|---:|---:|---:|
| Windows x64 | 68 | 891 | 0 | 10 |
| macOS ARM64 | 68 | 986 | 0 | 15 |
| Ubuntu x64 | 68 | 986 | 0 | 14 |

Windowsの先行単独overflow試験1件は上表へ二重計上していません。添付/AGENTSのpath fixture失敗は解消し、TLS2件、native PowerShell5件、overflow単独と全体の成功を実ログで確認しました。

配布tag `v0.1.0` は `96e65bf` です。上記からの差分は配布workflowのversion検査と判断記録だけで、製品Rustソース・依存・同梱guideは同一です。最初の配布runはUbuntu 22.04のPython 3.10にtomllibがなくbuild前に失敗したため、固定Cargo metadata JSONによるversion取得へ変更しました。Release/assetの公開前に限り初期tagを更新し、旧commitと失敗runは履歴に残しています。

[配布workflow](https://github.com/tya5/shellboy/actions/runs/40246376176) は3 OSすべてでrelease build・実binaryのhelp/version・archive作成を成功させ、[Release](https://github.com/tya5/shellboy/releases/tag/v0.1.0) を公開しました。公開後に全配布物をダウンロードし、SHA256SUMSとの一致、実行ファイル・MIT License・設定例・guide・既知の制限の同梱、非公開資料を含まないことを確認しました。Windows ZIPは認証なしでHTTP 200/9,395,798 bytes、PEヘッダはAMD64でした。

| 配布物 | bytes | SHA-256 |
|---|---:|---|
| Windows x64 ZIP | 9395798 | `9756f870e25d3fc090681f25d90ff78fd5bfa92fd7a8f3d0c82eec10ba5946b2` |
| macOS ARM64 tar.gz | 8986811 | `164f313bc64d284076f48b75de21bae2f1ce54fe1c34a4cd4383c30a7ab0e897` |
| Linux x64 tar.gz | 9707525 | `c1ee749ae916fa036c8e95bc8f77f5db2f5df69bd314acdeb7f63438145b81f8` |

ダウンロードしたmacOS release binaryでもオフラインdocsの33 CLI確認/13 guides/3 categories/115 linksが成功しました。空cwd・環境変数なし・設定なし・owner/APIなしで状態ファイルは作りません。binary SHA-256は `8179b21d6c2ab60dc927cf16a239b9e39a4126710be247b57b3824ff12d02111` です。

### Windowsのテスト待機上限の追補

`96e65bf` の [再CI](https://github.com/tya5/shellboy/actions/runs/40246369812) はmacOS/Linux成功、Windowsは890 passed/1 failed/10 ignoredでした。前回成功したoverflow試験が全体実行時だけ再発し、追加診断では最初のshellがrunning・raw未到着のまま5秒を超過していました。overflow処理やsummary/retryへ進む前の待機であり、製品の5秒完了契約違反とは扱いません。

`6e45ac1` はWindowsに限りそのfixtureの待機上限を既存native shell試験と同じ15秒へ揃え、Unixの5秒・保存状態診断・全ての意味検証は保持しました。製品コード・guide・依存は配布tagと同一で、公開済みtag/assetは変更しません。[追補CI](https://github.com/tya5/shellboy/actions/runs/40247172429) も全3 OSで成功しました。fullは各68 suite、Windows 891 passed/0 failed/10 ignored、macOS 986/0/15、Ubuntu 986/0/14です。Windowsの先行単独1件は別枠で成功し、全体でも同じ回帰が成功しました。OS内部の起動遅延原因までは断定していません。

## ストリーム差分・受信本文保持の修正

初版公開後のローカル修正であり、上記v0.1.0の配布物とは別に検証します。会社Windowsの社内proxy・実モデルのSSEは未取得で、その環境の再現成功とは扱いません。固定応答のloopback HTTP、一時DB、UIのメモリ状態で確認し、資格情報・会社の会話・利用者の保存データは使いません。

回帰対象は、metadata/本文/ツール引数/推論の途中・最終差があっても検証済み最終responseを採用すること、deltaだけ異なる場合の保全、同chunkの後続不正JSONで先行受信本文を失わないこと、最終応答自体の不正は復旧待ちになることです。差分ログに本文を入れず、保存証跡を採用path・索引へ混ぜないことも照合します。

UIでは復旧待ちになった瞬間の本文保持、再起動後の復元、35件の保存記録を32件pageで辿って原文を取得すること、metadataだけの成功で重複表示しないこと、古いThread/世代の更新を採用しないことを確認します。GCでは生存Threadのpath外監査nodeを保持し、Thread削除後に失敗・成功の両監査本文とattempt内の回答/平文推論/完了item/最終rawを回収することを確認します。

修正source `d170da6` に対するmacOS/aarch64の `sh scripts/check.sh` は成功しました。fmt、strict all-target Clippy、全68 suiteの1,008 passed / 0 failed / 15 ignoredです。最終表示コードに対するstrict lib Clippyとfmt/diff再確認も成功しました。以前の受信表示full-lib確認ではattempt_id追加に伴う既存fixtureの期待値差が1件ありましたが、期待値へ同fieldを加え、キャンセル状態や旧保存値を守るassertを維持した上で今回のfullでも成功しました。

実binaryによるオフラインdocs確認は33 CLI/13 guides/3 categories/116 linksが成功し、空cwd・環境変数なし・設定なし・owner/API不要で状態ファイルを作らないことを確認しました。公開差分はgitleaks v8.30.1でallowコメント無効・redact有効で検査し、検出0件でした。会社Windows・社内proxy・実端末GUIでの再検証と実providerへの課金要求は未実施です。公開済みv0.1.0のtag/assetは変更していません。

## Issue #1: Windowsの隠し環境変数

`environment_block`の検査だけを継承環境の読込み規則へ揃えた局所変更です。macOS/aarch64で `cargo test --locked --test shell_windows_command` の4件が成功しました。`=C:`/`=::`/`=ExitCode`/`=ExitCodeAscii`と通常変数を混在させ、並び・上書き・二重NUL終端を含む環境blockの完全値を照合しました。空名、名前/値のNUL、先頭以外の`=`による注入を拒否し、診断の値漏洩防止・名前長制限・escape、値中の`=`と非Unicode UTF-16保持も確認しました。

`cargo clippy --locked --lib --test shell_windows_command --test shell_windows -- -D warnings`、全体fmt/diff確認が成功しました。前節の全1,008件は本修正前の結果であり、この差分では対象試験と対象Clippyを実施しています。PowerShellが隠し変数入りの環境blockで起動・正常終了するWindows専用回帰を1件追加しましたが、macOSではcfg対象外のため実行成功に含めません。親processの環境や利用者データは変更していません。Windowsでの起動確認、push、新配布物は未実施です。

## Issue #2: XDG設定不要のUnix IPC

macOS/aarch64で `cargo test --locked --lib --test app_ipc --test cli_process --test runtime --test runtime_hierarchy` が全5 suite、430 passed / 0 failed / 3 ignoredで成功しました。内訳はlib398、app_ipc3、cli_process4、runtime21、runtime_hierarchy4。OSが先に一時領域を回収した場合の正常清掃も含みます。直前の初回lib397件は最終430件へ二重計上していません。全targetの標準チェックを繰り返した結果ではなく、IPC・起動・Control/shellの変更対象とライブラリの検証です。

保存先の末尾slash付きsymlinkを追ってchmodしない最終調整後、専用subprocessでumask002・XDG_RUNTIME_DIRなし・group書込み可の親を組み合わせた起動/正常終了、保存先0700、新規/既存保存先の扱い、親権限の不変、symlink先の権限不変を1件の対象試験で追加確認しました。この再実行は430件へ加算しません。最終調整後の対象strict Clippyも成功しています。

旧locatorしかない段階・空directory・bind済みsocketのクラッシュを模擬し、再起動で旧領域を回収し新しいランダムpathになることを確認しました。これはDB/診断workerを停止してmetadataを残す合成fixtureで、実OSの強制終了や電源断の網羅ではありません。他の生存owner、未知file、socket/parentの置換、symlinkを保全する試験も成功しています。

実CLI subprocessではendpoint.jsonを削除した状態でもSHELLBOY_SOCKET経由で接続し、別data-dirへの明示切替ではそのmetadataへ接続、不正instanceではprotocol_mismatchとなりfallbackしないことを確認しました。実モデルtool相当のshellへsocket環境が伝わり、そのshellからCLI操作・子Run・待機が動く既存回帰も成功しました。APIはローカル固定応答で、課金要求や利用者データは使いません。

peer照合は実Unix socket pairのcredentialsを使い、同uid成功と期待uid不一致の拒否・protocol byte未送信を確認しました。実際に別uidへ切り替えたprocessやLinux SO_PEERCREDの実行は未実施です。対象strict Clippy、fmt/diff、実binaryの同梱docs33 CLI/13 guides/3 categories/116 linksは成功しました。Linux/WSL実機・公開CI・新配布物は未実施であり、macOSの合成条件をそれらの成功とは扱いません。

## Enter直後の送信表示と診断（2026-10-26）

macOS / aarch64、Rust 1.99.1で、一時DB・合成メッセージ・loopback固定応答を使いました。UI送信の7回帰は、Enter直後の描画/時計、保存とsource/履歴の順序逆転、複数revision、受付拒否時の入力保持、会話切替、窓外の完了済み送信、復旧時の静止を確認します。frontend試験は実送信のqueued/processing/savedと保存receiptのoperation ID/message IDを照合し、本文がログに入らないことを確認します。診断の2追加試験は固定分類・IDの上限/制御文字除去も確認します。

最終ソースの `cargo test --locked --lib --test frontend --test frontend_activity --test frontend_ordered --test diagnostics --test tui_recovery --test tui_process --test tui_shutdown` は8 suite、441 passed / 0 failed / 3 ignoredでした。対象のstrict Clippy（`-D warnings`）、fmt/diff、同梱docsの33 CLI確認・13 guides・3 categories・116 links、staged秘密情報スキャンも成功しました。

今回の確認には会社のWindows・実端末GUI・実provider APIを含めず、標準全targetチェックや公開版のCI結果と区別します。利用中owner・既定DB・公開済み配布物は変更していません。

## v0.1.1候補のnative CIと終了待ちfixture（2026-10-26）

候補 `7c888db` の [native CI](https://github.com/tya5/shellboy/actions/runs/40332152109) はLinux/macOS各68 suite・1,025 passedで成功しました（ignoredはLinux14、macOS15）。Windowsはfmt・Clippy・overflow個別試験を通過しましたが、既存のstale_mutation_report試験が最後のbridge終了待ち5秒でtimeoutしたため不合格です。操作結果・古いreport抑止・通知所有のassertは完了し、他のtargetも成功しました。

fixtureの終了待ちは製品既定の停止猶予5000msと同じで、DB・IPC・ログworkerの終了までを含む外側期限として余裕がありません。Windowsの試験用期限だけ30秒へ延長し、busyなyieldポーリングを5msの待機へ変更します。製品の停止期限・強制終了・保存処理は変更しません。ログから停止段階は確定できないため、原因を期限不足と断定せず、修正後のnative CI成功で区別します。

## 公開済みv0.1.1の結果（2026-10-26）

tag `v0.1.1` は `e55da58` です。[修正後のnative CI](https://github.com/tya5/shellboy/actions/runs/40333964684) は全OSでfmt・strict all-target Clippy・全68 suiteが成功しました。Windowsのoverflow個別試験は下表へ二重計上していません。

| OS | suite | passed | failed | ignored |
|---|---:|---:|---:|---:|
| Windows x64 | 68 | 925 | 0 | 10 |
| macOS ARM64 | 68 | 1,025 | 0 | 15 |
| Ubuntu x64 | 68 | 1,025 | 0 | 14 |

Windowsの先行候補の終了待ちfixture失敗は上記成功と区別します。修正後の試験は正常終了まで成功しており、製品の停止猶予・強制終了仕様は変更していません。

[配布workflow](https://github.com/tya5/shellboy/actions/runs/40335468427) は3 OSのnative release build・help/version・パッケージ生成を成功し、[v0.1.1](https://github.com/tya5/shellboy/releases/tag/v0.1.1) を正式公開しました。全3 archiveとSHA256SUMSを認証なしの公開URLからダウンロードし、SHA-256一致、単一prefix配下の構成、必要文書、各OSの実行形式を確認しました。公開macOS版をこのマシンでも実行し、help/version、同梱docsの33 CLI確認・13 guides・3 categories・116 linksを確認しました。同じ実行ファイルをローカルへ配置し、配置後versionも0.1.1です。

会社のWindows・社内API proxyによる実provider互換、実IME/clipboard/端末GUI、WSL実機の再検証は今回の成功に含めません。既定設定・履歴・実行中ownerはリリース検証で変更せず、v0.1.0のtag・配布物も保持しています。配布ソース以降の文書commitはこの完了記録だけで、検証済みtagを移動しません。

## レビューgoal再開後のローカル検証（2026-10-26、未配布）

公開v0.1.1から後続のレビュー是正を進めています。全て一時DB・匿名fixture・固定応答で、利用者の既定DBや設定を変更しません。旧レビューの証拠監査はローカル専用Gitに置き、ここには製品の新しい検証だけを記録します。

- 索引選択維持と巨大rawの第一段階はindex49件・rag_store16件・raw decoder/failure分類の単体各1件、対象strict Clippyが成功しました。
- 続く停止予算・query fingerprint・末尾cursorの4 suiteはcontrol_db20件・index51件・rag_store16件・work_status11件が成功しました。停止の試験はDB予約と保存/再送経路を検証し、OS processの停止を実行した証拠とは分けます。
- 検証済みquery markerを再読取時に書き直さない補強後、lib407件（既存ignored3）・index51件・rag_store16件と、対象strict Clippyが成功しました。SQLite trigger/total_changesでmarkerの再書込と不一致時の破壊を拒否することを確認しました。

G7の可視性判定は本番と同じsearchable SQL・bundled SQLite・専用DB workerを使い、256sourceと共有置換深さ0/8/64、live rootあり/なしで計測しました。順に6,961/8,237、20,529/20,916、162,735/159,950µsでした。これは1マシンの診断標本であり、全検索のlatency保証や実データ性能の証明ではありません。追加の常駐reachability cacheやworkerは導入していません。

ここまでは10月26日時点の段階検証です。最終コードの確認は次節へ記録します。Windows/Linuxのnative検証は今回のローカル結果に含めず、v0.1.1の過去CIと区別します。今回の修正はpush・新配布物・ローカルインストールへ反映していません。

## 最終コードの実端末・負荷確認（2026-10-27、未配布）

`f488196` の単体試験は428 passed / 0 failed / 3 ignoredでした。正式な親子終了記録のkind・詳細参照が一般fieldsの上限で落ちる条件も回帰へ追加しました。以降の確認はmacOS ARM64、Rust 1.99.1、release、専用一時DB・loopback fixtureです。利用者の設定・履歴・インストールは変更していません。

実WezTermの120×40・60×24セルを取得しました。復旧理由と操作案内から `/retry` へ進み、同じTurnのgeneration 0/incomplete→1/completedをDBでも確認しました。実shell2件は番号とcommandが対応し、出力ONE/終了0とエラー側TWO/終了7を区別できました。親子処理の終了（6f8f78a）では両shellのstopped・group_cleanup_confirmedを確認し、f488196で開き直した5詳細＋summaryが一つの「原文6件」へまとまること、F6の25項目には各詳細が残ることを確認しました。別の静的scene（6f8f78a）では中断・構造化error・索引・code caption、全8項目とO原文まで確認しました。foreground処理の途中stdoutを待つ補助手順はtimeoutし、正常終了処理後の保存出力で確認しています。画像取得はscreencapture失敗とScreenCaptureKit -3811で未実施です。これらは実端末セルの確認であり、色・pixel描画・実IME/clipboardの合格とは扱いません。

同じsourceのrelease測定は他のCargo・試験・captureを止めて実施しました。1024 delta／80,908byteのSSEを3回、製品の32件/4ms更新経路と120×32・30fps TestBackendで処理しました。snapshotは73〜74回、clone総量は2.69〜2.73MiB。source送信からpump後の表示観測までのp95は10.45〜11.75ms、Paste→TestBackendの21標本は1.82〜37.97ms、DB往復21標本の最大は1.05msでした。表示観測間のskipは更新の集約を含み、通知配送件数や純粋なqueue待ち時間ではありません。保存確定本文も確認しました。

実release binaryのPTYでは空の新規DB／同DB再openを各7回、120×32で測定しました。起動p50/p95は新規41.16/41.59ms、再open41.17/42.06ms、日本語bracketed pasteのp95は各36.31/37.73msでした。約2秒idleのRSS観測最大は17,776KiB、CPU区間値は0〜0.50%でした。マシンは8core/8GiB、開始時load averageは139.12/117.83/89.56。OS cacheは消去せず、履歴・model・embedding未設定の限定条件です。旧baselineとの非退行、大規模履歴、SLO、pixelや物理入力の性能は主張しません。

最終コードf488196の `sh scripts/check.sh` はfmt・全target strict Clippy・68群の試験が成功しました。1,058 passed / 0 failed / 15 ignoredで、上記の単体428件を含みます。明示したrelease負荷試験は別枠です。`python3 scripts/check-review-plan.py --require-complete` も成功し、178 original＋22 PR1＋19 PR2は217完了・Windows実機なし2環境対象外、396子条件の対応を確認しました。文書検査だけを製品の検証とは扱いません。Linux固有lease/実tmpfiles、Windows GUI、会社proxy、実providerは今回未実施です。

## v0.1.2タグ前の追補（2026-10-27）

文書commit `dcdbab3` でも `sh scripts/check.sh` を実行し、fmt・全target strict Clippy・68群 1,058 passed / 0 failed / 15 ignoredを確認しました。先行する `f488196` の結果と同数ですが別の実行です。この後のバージョン更新・再レビュー修正の成功へ読み替えません。

最初の準備commit `2cec582` の [native CI](https://github.com/tya5/shellboy/actions/runs/40406278067) はmacOS/Linuxが成功、WindowsはUnix専用テスト補助関数のcfg漏れによるstrict Clippy失敗でした。Windowsテストは未実行であり、成功に含めません。テスト側cfg修正と受信表示・raw検証・起動診断の追加修正は、後続commitの検証結果を別途記録します。


## Native CI follow-up after explicit RAG lease release (2026-10-29)

Run [40802346281](https://github.com/tya5/shellboy/actions/runs/40802346281),
source `bfdf7f5` (same Rust source as `f659197`), passed Linux
(**80 groups / 1,367 passed / 0 failed / 24 ignored**) and macOS
(**80 groups / 1,366 passed / 0 failed / 25 ignored**). All RAG registration,
selection and migration cases passed on both. Windows failed one IPC frontend
cleanup fixture and two attachment-provenance incoming-request watchdogs; the
new RAG lease and earlier FIFO fixtures passed there. This run is not an
all-OS success.

The IPC fixture completed its bounded listener notification, local DB mutation
and Quit submission, then exceeded a two-second whole-owner join. The fixture
now separates that notification bound from eventual owner cleanup using the
existing 15/30-second cleanup watchdog, drains UI delivery, retains duplicate
notice/error checks and additionally verifies the DB is closed. Production
lifecycle and timeout settings are unchanged. The focused macOS test passes.
The individual slow Windows cleanup step was not measured.

The attachment failures only identified the shared `incoming()` watchdog, not
which request or saved failure. A further bounded local probe of the unchanged
binary ran six complete eight-case suites (two concurrent processes, eight test
threads per process): **48 passed / 0 failed**. This does not establish Windows
health or explain the failure. Subsequent diagnostic/barrier changes and native
results are recorded separately below.

The follow-up attachment fixture keeps its five-second bounds, captures the
actual incoming call site and bounded saved run/turn/attempt diagnostics after
failure, and checks the exact accepted Turn's intended invalid-UTF-8 recovery
condition before editing synthetic history. No production change or proven
explanation for the original native timeout is claimed.

Focused macOS checks of this test-only follow-up pass: IPC server 11 cases and
attachment provenance 8 cases. Format, strict all-target Clippy and diff checks
pass. The production Rust source is unchanged from `f659197`; the earlier
whole-suite result is not counted as a new run of these modified fixtures.


## Final pushed native gate for #34 and review follow-up (2026-10-29)

[Run 40804700576](https://github.com/tya5/shellboy/actions/runs/40804700576),
source **b8f1f84d0ed3e26cbeefabda058dab2451b15cfa**, completed successfully.
Production Rust is unchanged from `f659197`; `b8f1f84` changes only test fixtures
and records. Every OS passed format, strict locked all-target Clippy and locked
all-target tests with no-fail-fast.

| Native runner | Test groups | Passed | Failed | Ignored |
| --- | --- | --- | --- | --- |
| Linux, job 123220740977 | 80 | 1,367 | 0 | 24 |
| macOS, job 123220741206 | 80 | 1,366 | 0 | 25 |
| Windows, job 123220741249 | 80 | 1,263 | 0 | 19 |

The dedicated Windows overflow-recovery step separately passed its one filtered
case before the main test step; the table excludes that duplicate execution.
Both attachment cases and the IPC listener-failure/owner-cleanup case pass on
all native runners. The Windows Tests step also compiles/link targets (5m22s in
this run), so its elapsed time is not pure test runtime.

This final success does not prove the cause of the earlier attachment timeouts;
the stronger exact-Turn precondition and bounded caller/state diagnostics remain
for recurrence. RAG ownership, FIFO, receipt/autosave and index-stop regressions
also pass. No retry-until-green loop, production timeout relaxation or weakened
semantic assertion was added. The IPC fixture cleanup bound change is explained
separately above and in decisions; its notification deadline is unchanged.

Documentation audit passes 617 relative links, 38 heading fragments, four TOML
blocks, 13 English guides and one Japanese User Guide, with zero errors. Staged
secret scans pass. Final evidence-only documentation commits do not change the
Rust/configuration/guide source covered by this native run. Company Windows GUI,
provider/proxy behavior and representative operational retry traces remain
unmeasured; the documented provisional defaults/transport limits still apply.


## #32–#34 additional reviews and v0.2.0 local gate (2026-10-29)

The follow-up is tracked in [the disposition table](../development/issue-32-34-follow-up.md).
This is new source after `37bbdb1`; the previous native success is baseline only.
The working package is 0.2.0, protocol 2, conversation schema 11 and RAG schema 3.
On macOS 26.3.1 / ARM64 / pinned Rust 1.99.1, with CARGO_BUILD_JOBS=2,
CARGO_INCREMENTAL=0 and RUST_TEST_THREADS=2, `sh scripts/check.sh` passes format,
strict locked all-target Clippy and **80 groups / 1,380 passed / 0 failed /
26 ignored**. The fixture-only obsolete trim wrapper and its duplicate case
were removed; real Driver tests retain original-path/content/no-replacement/noise
checks. This complete result supersedes the earlier 599-pass library-only run.

Focused HTTP/retry/upload/scoped-RAG/store cases pass **56 / 0 / 2 ignored**.
They include durable trim success followed by a tool, 429 retry and later context
overflow; actual connection refusal beyond two attempts before proxy startup;
large partial upload with one reduction and disabled-trim two-strike recovery;
overlapping scopes and Thread deletion during query embedding. All traffic is
local fixture traffic. Shutdown stages use the existing diagnostic queue;
IPC tests validate fixed fields, completed stages and per-scope monotonic times.

The explicit ignored `keyword_thread_scope_thousands_of_nodes_measurement` case
also passes: full keyword search is **105.670ms** for 1,000 original nodes,
1,000 foreign nodes and eight replacement layers, and **717.309ms** for 5,000
original nodes, 5,000 foreign nodes and 40 layers. Each result identifies exactly
one omitted original with its Thread scope, with zero embedding calls. These
are debug-build, local synthetic observations without cold-cache control or an
old-code timing baseline. They do not establish a latency SLA or constant-time
behavior for deep/shared DAGs or many unrelated Threads.

Initial compilation found a fixture temporary borrow, a SQLite fixture numeric
type and a collapsible condition; these were corrected before the complete gate.
No semantic assertion/deadline was relaxed. The old recurring attachment
watchdogs remain unexplained; repeated native observations, release builds,
public-download verification and local installation are pending.


### Portable fixture and fixed native observations

Run [40809307248](https://github.com/tya5/shellboy/actions/runs/40809307248),
source `1e9d9b2`, passes Linux (80 groups / 1,381 / 0 / 25 ignored) and macOS
(80 groups / 1,380 / 0 / 26 ignored). Windows Clippy cannot compile the new
optional-preflight unit fixture because it referenced a Unix-only shell fixture;
Windows tests did not run. Replace that dependency with the test's own minimal
portable temporary owner and direct ThreadCreate request. It needs no shell,
HTTP listener or unrelated seeded Runs. The same cancellation/state/right
assertions remain. No product code or test deadline changes.

After this fixture fix, `sh scripts/check.sh` again passes format, strict
all-target Clippy and **80 groups / 1,380 / 0 / 26 ignored** on macOS. CI YAML
and its shell repeat-step syntax also parse. The next native gate runs the full
suite then exactly two additional fresh attachment test processes per OS,
stopping on any failure. Count these separately, not as distinct new cases or
proof that the old symptom is resolved.

The verified 0.2.0 debug executable for `1e9d9b2` has SHA-256
`7f02a1385c15e9dc3b67b87f583a74e87a71c31715ca16783b8890142d0c399c`.
It passes ten-conversation HTTP+PTY: early delta, exact ten canonical outputs,
ordered/unique selected tokens and hidden Thread presence, all ten completed,
with raw PTY drainage paused for 1,158ms. Offline docs33 (13 guides/3 categories/
127 bundled links) and RAG CLI13 also pass, with zero external model requests.
The subsequent fixture-only fix does not change production behavior; do not
claim this hash is the optimized official release binary. Release download and
installed-binary verification remain pending.


### Closed-endpoint observation budget correction

Run [40810247300](https://github.com/tya5/shellboy/actions/runs/40810247300),
source `895d76b`, passes Linux/macOS all-target tests (1,381 / 1,380 passed,
zero failed) and their two extra fresh attachment processes (16 / 16 repeated
executions). Windows compiles and runs the full suite: **80 groups / 1,276
passed / 1 failed / 20 ignored**. Only the new three-failure closed-endpoint
watchdog fails; both attachment cases and all other production/library/integration
cases pass. Its extra observations are skipped after that failure.

That fixture allows three-second requests but gave three failed attempts only
five seconds total. Derive its aggregate observation budget from the configured
requests/retry delays plus fixture preparation (14,300ms), and its listen-to-
completion observation from one already in-flight failure plus success (11,100ms).
Production/request deadlines and all semantic/count/recovery assertions stay
unchanged. Add bounded saved-state diagnostics on failure and print per-attempt
kind/phase/source/approximate wall time after successful recovery, with no extra
await before binding the restored listener. Local macOS records three transport /
before_body_source / zero-source-byte failures of about 1ms each; this is not
proof of Windows refusal timing.

The initial budget correction passes `sh scripts/check.sh` again (80 groups /
1,380 passed / 0 failed / 26 ignored). The final fixture-only diagnostic/budget
refinement passes its three integration cases; strict all-target Clippy is
recorded separately. Native CI adds one explicit closed-endpoint diagnostic
execution before the two fixed attachment repetitions, so standard totals and
repeated observations must remain separate. No retry-until-green or suppressed
recovery assertion is used.

Final fixture refinement also passes locked strict all-target Clippy and format/
diff checks. No non-test Rust code changed after the complete standard run.


## v0.2.0 tagged-source native gate (2026-10-29)

[Run 40812283328](https://github.com/tya5/shellboy/actions/runs/40812283328),
source/tag target **4a5eb3ee90f324d3e905a2a6c3dff269fae4f05f**, succeeds on all
three native runners. Format, strict locked all-target Clippy and all-target
tests pass. Standard counts exclude repeated diagnostics:

| Native runner / job | Groups | Passed | Failed | Ignored |
| --- | --- | --- | --- | --- |
| Linux / 123245173692 | 80 | 1,381 | 0 | 25 |
| macOS / 123245173727 | 80 | 1,380 | 0 | 26 |
| Windows / 123245173424 | 80 | 1,277 | 0 | 20 |

Each OS separately passes the existing closed-proxy recovery case once with
uncaptured diagnostics. Its failed-attempt approximate wall times are Linux
3/20/21ms, macOS 1/3/2ms, Windows **2,024/2,023/2,029ms**. All report transport /
before_body_source / zero source bytes consumed. Three Windows failures alone
therefore exceed the earlier five-second aggregate assumption. This is direct
native evidence for the fixture-budget correction; do not equate wall time with
TCP delivery or provider prefill. The exact earlier failing run had no timing
trace, so its individual delays are not reconstructed as measured facts.

Two additional fresh attachment processes also succeed on each OS: Linux16,
macOS16 and Windows14 repeated executions, zero failures. Together with the
normal suite, tagged source has three successful attachment processes per OS.
The dedicated Windows overflow diagnostic also succeeds once separately. These
repeat observations are not distinct new test cases and do not establish the
original intermittent preparation-watchdog cause. No such recurrence is observed
in these samples; the preserved caller/state/reason diagnostics remain.

Tag `v0.2.0` is pushed at the exact verified source, with no later product change.
[Release workflow 40813637451](https://github.com/tya5/shellboy/actions/runs/40813637451)
is building the three native optimized binaries. Public archive checks and local
installation are recorded after they actually complete. The six new commits
from `37bbdb1` through the tag pass history secret scanning (119,279 bytes,
zero findings). No paid provider call or default-data migration is part of QA.


## v0.2.0 publication, public downloads and local installation (2026-10-29)

[Release v0.2.0](https://github.com/tya5/shellboy/releases/tag/v0.2.0) is published
at **2026-10-29 03:51:11 UTC**, stable/latest and not a draft. Tag points to
`4a5eb3ee90f324d3e905a2a6c3dff269fae4f05f`; no tested product code changed after
the successful native gate. [Release run 40813637451](https://github.com/tya5/shellboy/actions/runs/40813637451)
succeeds: macOS job123249520636, Windows123249520843, Linux123249520939 and
publisher123252097808. Each native build prints the exact source and
`shellboy 0.2.0`, passes help/version execution, and packages public docs/license.
This establishes native executable smoke checks, not company-proxy or GUI use.

All four public HTTPS assets are downloaded independently after publication.
The three archive SHA-256 values match the published SHA256SUMS:

| Archive | SHA-256 |
| --- | --- |
| shellboy-v0.2.0-aarch64-apple-darwin.tar.gz | `4f9d08cd9d327296177a4553ba45cff56c01edce708039418ed0750099010666` |
| shellboy-v0.2.0-x86_64-pc-windows-msvc.zip | `5d95a0cb4c03aafb461afd0a623b6908c5bc89e01391e33a350041587389851d` |
| shellboy-v0.2.0-x86_64-unknown-linux-gnu.tar.gz | `90668225a71dac37d3768e6d6af18f9f1e65de0b161b1fac681a2eaf60ef0722` |

Windows ZIP passes CRC integrity checks and contains the expected AMD64 PE
executable (machine 0x8664); Linux is ELF64/x86-64 and macOS is ARM64 Mach-O.
Official release sizes are 10,723,096 bytes (Windows ZIP), 10,256,612 (macOS),
11,081,943 (Linux), and 333 bytes (SHA256SUMS). These are real public downloads,
not merely successful upload metadata.

The macOS executable is extracted from that verified archive. Its binary hash is
`756969dcc5758c7e9e76c3a782ad07c32a11c5633cf89e69dd89694e7443bb48`.
It passes --version/--help, offline docs33 (13 English guides, three categories,
127 bundled links) and RAG CLI13. After a read-only old-owner activity check,
the existing 0.1.6 executable is backed up locally and the verified 0.2.0 is
staged beside `~/.local/bin/shellboy`, chmod755 and atomically renamed. Installed
hash exactly matches the official artifact and installed help/version succeed.
No running user owner is stopped, no user configuration is edited and no default
DB migration is performed. A previously running 0.1.6 owner still requires normal
exit/restart; protocol2/schema11 upgrade notes are part of the release.

The installed official optimized binary also passes the original ten-chat
HTTP+PTY pressure check: initial delta visible, raw PTY drainage paused1,152ms,
exact ten canonical outputs, selected tokens ordered/unique, hidden Threads
visible, all ten completed, ten mock requests and zero external requests. Only
fresh synthetic temporary stores are launched. This is separate from the earlier
debug-binary result and does not claim physical terminal pixel/IME/clipboard,
company Windows proxy or paid-model validation. Provider retry-count/prefill
measurements and the original intermittent attachment cause remain the explicit
follow-ups above.
