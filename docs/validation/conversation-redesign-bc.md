# Conversation structure and inline interaction (#41 B+C)

Runtime/test source: `342cb6ef230538b2c659ce63446901a963589b51`, based on
main8fb885d (including #50, #61 and 0.3.0 package preparation), after standalone #58. This qualifies only the shipped
B+C structural/interaction subset; the freshness follow-up below is separately
qualified and D–G remain open in the [delivery ledger](../development/issue-41-plan.md).
No schema, protocol, dependency, worker or queue is added.

## Standard checks

Native macOS arm64, pinned Rust1.99.1, `CARGO_BUILD_JOBS=2`,
`CARGO_INCREMENTAL=0`, `RUST_TEST_THREADS=2`:

```sh
sh scripts/check.sh
```

Passes: **1,658 passed / zero failed / 32 ignored**. Breakdown: library786/0/13,
integration862/0/17, termination7/0/1, crash recovery3/0/1. Layout88, Python6,
formatting and strict all-target Clippy pass. Ignored timing observations are
reported separately below. Main native CI41057195099 succeeds on exact SHA
b0e3c29d86dddaed1af4319a8404e3bf9fa9b9e2. Downloaded timing artifacts name that
source SHA and report Linux1,659/0/31, macOS1,658/0/32 and Windows1,543/0/24
(passed/failed/ignored; Windows threads4). These are native standard checks,
not physical terminal or unimplemented-freshness qualification.

## Freshness gaps at the initial b0e3c29 admission

At the b0e3c29 admission, review6523314574 identified missing code/evidence beyond
the shipped structural subset. No external-client commit-to-header1s measurement
had been performed. Open F6 mutable execution/availability refresh and
source-change-driven F7 invalidation were not qualified. Immutable original/finished-preview read times
and the F2 frozen-at time are not fully displayed. Remaining footer/foreign
queue mutation routes also need inventory-driven latency coverage. Request-ID
unit tests and current PTY focus/original checks do not establish these routes.
Track implementation and fresh evidence in BC-R1–R6 of the delivery ledger;
these remained B+C requirements, not silently deferred general G work. The
follow-up below supplies new implementation and evidence for those routes.

Functional coverage includes true shared stdout/stderr suffix budgets, indexed
chunk/row bounds, encoding uncertainty, CR progress normalization, zero raw reads
for closed success, validated group/call/source identity, sender facts, exact
Unicode anchors and mixed/nonadjacent prose, inline focus/click/F6/F7/original/copy
targets, popup link hydration without off-page history installation, preview
eviction/reacquisition/admission, quiet summaries and forward/backward wheel
traversal, preserved pending sends and #58 retention/stale-response regressions.

The coalesced-notification regression identified a real backward boundary error:
quiet facts painted after later prose cannot supply the beginning of the actual
canonical projection. Navigation now reuses the same128-node/192KiB projection
bounds while row anchors retain real Unicode positions. A stale preview reply
also releases its own admission slot without cancelling a newer token.

## Native PTY observations

`cargo build --locked --bin shellboy` produces the tested debug binary SHA256:

`2dc2341d51590013e36bc810291fd7b049a05e7ba2487e93666d506a181eab64`

Actual macOS PTYs at80x40 and140x40 pass in dark, light and NO_COLOR modes:
all six cases exit0, contain no harness/proxy errors and use that exact binary.
NO_COLOR emits zero chromatic SGR sequences in both widths. Dark/light emit the
expected distinct surfaces. Private terminal matrices, ANSI/style streams and
result manifests are retained locally; screenshots, paths and synthetic DBs
are not published. A private pyte terminal parser reconstructs the real stream.
This is not a physical WezTerm/Windows GUI claim.

The isolated loopback Responses fixture supplies400 lines of synthetic prior
context, two real local shell scripts producing more than40KiB each (stdout
success and stderr failure), and a held next model response. It verifies:

- Successful output starts closed; failed output shows its actual final marker.
- Tab enters history focus; click/Up/Space close failure and expand success.
- Explicit success expansion reaches its true suffix beyond8KiB, and closes
  without resurrecting the failed preview.
- Original/F6 popups preserve the selected saved source. Copy is not invoked
  against the host clipboard; exact copy targets are covered by UI tests.
- Ctrl+End resumes follow; the final answer is visible after canonical adoption.

The held-response captures also verify that an accepted send does not remain
as a stale operation notice. Typed send progress remains reactive, a missing
activity observation is not labelled idle, history/send failures stay visible,
and rejected UI/read-queue actions release their own loading state (unit tests).

The harness waits for the ready composer, not merely endpoint publication. It
selects actual tool rows rather than assuming the last quiet notification is a
tool. An earlier candidate run incorrectly expected new text while explicitly
paused; the corrected harness resumes follow before checking the final answer.
Those harness mistakes are distinct from the product regressions above.

## P1: native UI handling and drawing

Same128-node/50-tool synthetic fixture and140x40 TestBackend, debug profile,
20 warmup frames followed by500 timed frames per run. Baseline main1fec7f0 has
no inline focus/expansion, so it performs cached redraw; candidate timings also
include Up/Down and one toggle every four frames. The fixture uses short saved
canonical text/tool captions; large raw suffix correctness is tested separately.

Three interleaved baseline/candidate pairs give p95:

| Round | Baseline redraw | Candidate interaction + draw |
| --- | ---: | ---: |
| 1 | 1.733ms | 3.206ms |
| 2 | 1.725ms | 3.172ms |
| 3 | 1.776ms | 3.132ms |

The exact final0.3.0 runtime source repeats at3.407ms p95. Both the<=8ms absolute
and<=2ms additional-cost targets pass. Focus-only iterations assert no layout
or item rebuild. Across520 frames:131 composed layouts,258 item builds
(128 initial +130 toggles) and16,510 item-cache hits. Cache-hit paths do not
reallocate outcome/source metadata before testing the cache. This is local
synthetic debug evidence, not a general terminal/OS latency guarantee.

## P3: actual shared DB-worker jobs

Identical private instrumentation/fixture in detached baseline1fec7f0 and
candidate216b213 worktrees:128 canonical nodes,50 validated successful saved
tools,64KiB stdout each (3,276,800 raw bytes total), saved completed Run, no model
driver/network. Execute the actual visible `frontend::refresh::run` path.
10 warmups and30 measured refreshes, drained512-entry UI channel. Count actual
`Command::Run` executions and total closure work time; exclude queue wait,
checkpoint time and independent readonly presentation I/O. The subsequent
primary changes do not alter this measured read pipeline.

| Observation per refresh | Baseline | Candidate |
| --- | ---: | ---: |
| DB jobs, every measured sample | 656 | 7 |
| Total DB work p50 | 36.660ms | 2.846ms |
| Total DB work p95 | 37.816ms | 3.006ms |
| Returned saved tool identities | 50 | 50 |
| Raw preview blocks loaded | 50 | 0 |

The count reduction comes from avoiding successful closed-output reads/read pins;
metadata remains available. Instrumentation is private/test-only and is not a
new production diagnostic, worker or dependency. D–G must still qualify shared
coalescing and their additional consumers; seven jobs are not advertised as the
future one-job-per-consumer target.

Correction recorded2026-10-31: the original retained instrumentation in both
detached subjects inserts `measure-run` as `completed`, with an ended timestamp;
the previous phrase "saved live Run" was incorrect. The counts/times above remain
observations of that completed-Run refresh fixture. They do not establish live
Run/background activity cost, current E List coalescing or F consumers. A new
P3 fixture must explicitly assert and record its actual saved Run/Turn states.

## P7: shared preview accounting and lifecycle

The128-item test actually allocates16KiB per accepted preview, with64KiB total
admission. It checks retained key/entry/block/vector data and conservative map
overhead, not just text payload. Older payloads are evicted and a new token
rehydrates the selected item; duplicate old tokens cannot resurrect evicted data.
Root view retention includes this storage in `cache.body_bytes`. The documented
finite active working exception allows up to64KiB for current previews when the
configured cache is tiny/zero; it is not128 independent16KiB exemptions.

Four open16KiB items with20KiB admission, followed by100 visibility passes,
cause exactly four automatic requests despite one-preview capacity. After
leaving/reentering, another100 passes bring the total to eight. Explicit reopen
can retry; failed reads do not poll. Logical pending reads are capped at three
and Thread changes cancel their tokens. Retained known output survives a failed
repeat read. These allocation/admission checks are not a process-RSS limit.

## Remaining gates and scope

Main three-OS CI is pending for this unit. Physical Windows terminal/IME,
clipboard/browser actions, actual providers and the company proxy are untested
here. No paid API, default user store, installed binary, release or tag changes.
D live work/timers, E browser patches, F watch/descendants, G operation forms/
popup wheel/general popup freshness, and their remaining P2/P4/P5/P6 measurements
remain tracked. #41 must stay open.


## B+C freshness follow-up (review6523314574)

Runtime/test/embedded-guide source: `4bf14a9f782be518bccd20f189d8f0d829cec3c9`, including current
main ae0eccc installer/release documentation and the independently shipped #58.
Local macOS arm64/Rust1.99.1 standard checks pass **1,671 / 0 / 32**
(passed/failed/ignored): library795/0/13, integration866/0/17,
termination7/0/1, crash recovery3/0/1; layout89, Python6, formatting and strict
all-target Clippy pass. Main admission/three-OS CI is still a separate pending
gate for this follow-up; the earlier b0e3c29 CI does not qualify this new code.

### External CLI and collector routes

Four integration tests use actual separate CLI executables, temporary owner
stores and IPC credentials, loopback-only mock HTTP, the production frontend,
and real UiState/TestBackend drawing. No user action follows each observed
mutation (the detail popup is opened during fixture setup). Measured local values:

| Route | Start of measurement | Correct paint |
| --- | --- | ---: |
| thread set title/connection/model/cwd | Observer receives postcommit Changed | 4.645ms |
| Foreign first message / waiting footer | Same | 15.805ms |
| Foreign queued input | Same | 10.769ms |
| External pending-message cancellation | Same | 9.485ms |
| Saved recovery / footer | Same | 13.687ms |
| External Run stop / footer | Same | 8.478ms |
| Raw GC / already-open F6 availability | Before spawning CLI storage collect | 504.875ms |
| History GC / already-open F7 links | Before spawning CLI storage collect | 471.240ms |

Every route asserts the1s unloaded-local target. The Changed measurements start
at notification **receipt**, not an instrumented SQLite COMMIT timestamp; GC
measurements conservatively include CLI startup, IPC and collection. This is
local synthetic evidence, not a physical Windows terminal or provider claim.
The pending-message CLI measurement paints the compact strip; the queue popup
uses the same pending state and existing UI tests, without a separate popup
latency measurement.

Raw GC really removes2KiB, advances retention generation and updates the open
F6's exact source without rewriting canonical payload/hash or hiding paid text.
History GC uses a real CLI fork through the complete call/result boundary.
Deleting the parent preserves both sources because the live child roots them;
deleting the final child then collecting makes the same open F7 unavailable and
removes cached URLs. A proposed call-only fork fixture was rejected during test
design because real fork validation forbids it; no invalid fork is counted.

State/reader regressions additionally cover running→exited/code changes, exact
group identity with repeated call IDs, source-local unavailability, oversized
payload/ID bounds, source-kind preservation, selected field/URL mode isolation,
manual R during a pending read, stale source replies and reference-only ToolFacts
changes that add a later result to an already-open item. Physical reads retire
before a dirty follow-up; visible requests are coalesced at500ms. Lost/failed
current-view reads and detail refresh failures are labelled stale/unavailable.

### Snapshot observations and native terminal behavior

Original pages show source creation plus their fixed UI receipt time and O/P
re-read controls. Explicit and automatic failure-tail previews show fixed UTC
read times with close/reopen to re-read. Reopening genuinely requests a new read;
failed rereads retain known paid text and its old timestamp, with a separately
budgeted bounded refresh error. F2 shows its fixed entry time and resume controls;
Ctrl+L repaints available state without recapturing that entry time.

The final debug binary SHA256 is
`6eaf6e5df4d29a2e1b62127494d233cc7a09a7a20c907641e76d9feace5e3b0b`.
All six actual macOS PTYs (80/140 columns × dark/light/NO_COLOR) pass seven checks
each with that exact binary and no fixture errors. They include the prior
open/close/original/follow/real-tail checks plus frozen-time stability and preview
read-time/reopen labels. NO_COLOR has zero chromatic SGR sequences in both widths.
No clipboard/browser invocation, paid API, default data or installed binary is
modified. Physical WezTerm and Windows GUI remain untested.

An initial new PTY assertion compared a trimmed captured header with padded
terminal cells and falsely reported clock movement. The corrected assertion
compares equally trimmed rendered headers; raw ANSI/style streams are retained.
That harness issue is not counted as a product fix. A new F7 fixture initially
mistook cached visible URLs for completion of the source read; it now waits for
its observed-time label before asserting the received snapshot.

### Performance and limits

Same controlled128-node/50-tool fixture: fresh baseline1fec7f0 p95 redraw1.855ms;
final interaction+draw p95 **3.324ms**, p50 2.608ms, maximum15.545ms across500
samples. Both p95<=8ms and baseline+2ms pass; this is a p95 target, not a maximum
per-frame guarantee. Layout builds131, unit builds258 and unit hits16,510;
focus-only frames still assert zero layout/item rebuilds. The historical P3
656→7 measurement above is not rerun or relabelled as a measurement of this
follow-up's new popup consumer. That consumer has one bounded DB call/read and
one physical request in flight/500ms, no hidden/F2 polling. Combined current-view
one-job consolidation and its fresh DB-time/count measurements remain D work.
P2/P4/P5/P6 and D–G remain unqualified by this record.


## Latest #59 integration qualification

Comment6524373262 is adopted on merged main ed40f33. Latest runtime/test and
embedded-guide source: `fd968e531e5cdc5c94b7e9c670977573e38559f7`. The earlier4bf14a9
observations above remain pre-merge evidence, not a substitute for this run.
Preserve explicit trusted-human versus external_cli bands, parent/self/Revisit
identity and the documented historical-null display convention. Self-asserted
names stay in F6; a mutable-source snapshot must not erase loaded immutable
sender kind/name/IDs. The merged sender regression verifies that order.

Fresh native macOS arm64/Rust1.99.1 standard: **1,679 passed / zero failed /
32 ignored** (library801/0/13, integration868/0/17, termination7/0/1,
crash3/0/1), layout89/Python6/fmt/strict all-target Clippy. Same final-source
four external-CLI/GC tests pass. Measured postcommit Changed-observer-to-paint:
header4.322ms, first-send/footer113.207ms, queue9.986ms, cancel9.805ms,
recovery15.478ms, stop8.586ms. Conservative CLI-start-to-paint:
raw GC/F6 **506.856ms**, history GC/F7 **464.325ms**; all assert the1s local target.
These starts/physical-terminal limits are the same as documented above.

Fresh interleaved baseline/current P1: p95 **2.110ms / 3.974ms** (delta1.864ms),
current p50 2.934ms/max5.068ms,500 samples; both p95 targets pass. Layout/unit
counters remain131/258/16,510. Final binary SHA256:
`4a89ba055ff3ccfcc717c63938330b7eb9d97c17f6668e4cdbb5f47b4bf64d77`.
All six actual PTYs pass seven checks each using those bytes; NO_COLOR emits0
chromatic SGR in both widths. Private fixtures/captures are retained locally.
No paid API, user store, installed binary, tag or release is changed.
Main three-OS CI for the combined follow-up remains pending admission.


### Main admission and native three-OS gate

Follow-up main `17c42710f1be41289fb2e0c478b1dff7b24d1f09` passes
[CI41068514157](https://github.com/tya5/shellboy/actions/runs/41068514157).
Downloaded native-test-timings artifacts report that exact source SHA and no
failures: Linux1,680/0/31, macOS1,679/0/32, Windows1,563/0/24
(passed/failed/ignored; Windows test threads4). B+C freshness BC-R1–R6 is now
qualified through main. D–G and their shared-refresh/performance gates remain
open; no physical Windows GUI, paid API or installed-binary claim is added.
