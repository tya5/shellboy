# Conversation structure and inline interaction (#41 B+C)

Runtime/test source: `342cb6ef230538b2c659ce63446901a963589b51`, based on
main8fb885d (including #50, #61 and 0.3.0 package preparation), after standalone #58. This qualifies only
B+C; D–G remain open in the [delivery ledger](../development/issue-41-plan.md).
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
reported separately below. Main three-OS CI is a separate pending gate.

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
tools,64KiB stdout each (3,276,800 raw bytes total), saved live Run, no model
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
