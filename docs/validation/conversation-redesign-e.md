# E conversation-list qualification (#41)

This record distinguishes current evidence from remaining admission gates.
It does not qualify F Watch/descendants or G management/forms. See the
[delivery ledger](../development/issue-41-plan.md) for those independent units.

## Repaired-source ordinary native CI

At7c6f4d6e4ee12f0e6bd906504626d781ba751c13, workflow41166258538 passes ordinary
Linux1905/0/31, macOS1904/0/32 and Windows1780/0/24
(passed/failed/ignored). Artifact source_sha and actual harness success lines
were checked. The16-mutation shared-snapshot test passes on all3, including
Linux/Windows where CBC failed. The original failed runs remain available.

P2/P5 are separate formal windows in that workflow, not implied by these checks.
The follow-up uses baseline0d4f63a and candidate7c6f4d6, P5 producers3 on both
sides, P2 producer1. Keep the producer1 comparison41160746679 separate. No main
admission, release, install or user-data mutation follows from test success.

## P3 direct-read characterization, not the final gate

Native macOS26.3.1 ARM64,8GiB/8logical CPUs, Rust1.99.1, ordinary release builds
without cfg(test). Disposable sources: before0d4f63a6bcf90fa9d8b30d8598f7b8e8ca8e52bd,
after0b7bd73421ece40b9e593d3b4f196dceb029face. The latter has the same Rust/Cargo
bytes as CI7c6f4d6; the source identities are retained separately, not relabelled.
Other builds were active on this host, so timing differences do not establish a
causal or stable source regression. These are off-thread read-pipeline timings,
not UI frame or commit-to-paint latency.

Temporary normal-binary hooks count executed DB-worker closures and their total
closure duration, plus E's readonly connection lexical duration. Work completion
uses notifications and the shared60s watchdog, not sleeps or machine-speed
assertions. No product probe, worker, queue, dependency or flag is committed.
Source/modified-file, controller/harness and copied-binary hashes plus runtime
source identity are retained with the private raw observations.

Fixture:50 Threads; selected Thread with128 saved canonical nodes,50 successful
saved tool results and50x64KiB raw output. Saved Run is seeded/asserted live and
Turn completed. The ordinary current-page query returns50 nodes, all50 tools;
128 is the saved dataset count, not the returned-page count. No raw preview is
loaded. Before each sample, a fixture-only title write alternates automatic and
explicit title; its cost is outside the observation. Explicit titles are checked
in the returned header.10 warmups precede30 samples,15 per title mode.

Before executes the actual current-view refresh and the legacy explicit Thread
list request with50 rows. After supplies one50-ID Browser ticket to current-view
refresh, checks50 patch rows, shared read sequence and ticket completion. Both
retain50 returned tools. The legacy list does not automatically follow Changed;
its missing reactive behavior is not a speed advantage. These direct calls do
not execute the UI event-admission loop. The independent16-mutation integration
test supplies functional admission evidence, not a measured latency distribution.

| Title mode | Subject | Writer jobs/sample | RO opens/sample | Writer work p95 ms | RO lexical p95 ms | Pipeline p50 / p95 ms |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Automatic | Before |4|0|2.1208|0|2.9580 /3.2981|
| Automatic | After |0|1|0|5.5704|5.6338 /8.4079|
| Explicit | Before |3–4|0|1.8204|0|2.6345 /2.9446|
| Explicit | After |0|1|0|4.7173|4.9106 /6.5973|

Nearest-rank p95 with15 samples is that group's maximum. Readonly lexical time
includes connection acquisition, preparation and transaction scope; it is not
pure SQL execution time. Zero writer work does not mean zero database work.
The observed after pipeline is slower here; do not advertise a latency or CPU
improvement based on the reduction in writer jobs.

This is useful characterization but **does not close E's P3 gate**. The legacy
request calls Control.start and wakes the index through apply_effects, while the
new direct-refresh fixture does not explicitly start Control. Current counters
include all executed worker closures during the sample and have no caller
classification, so not every3–4 legacy job can be called a UI read. A final E
comparison must start the same services before both workloads, distinguish the
actual UI/CLI read closures from index/maintenance work, and verify saved lifecycle
facts after observation. Full combined-consumer P3 remains staged until F/G exist.

## Preparation failures and rejected evidence

The first libtest-only build was stopped before accepted observations when the
method moved to normal release binaries; it is not a passing measurement or a
zero-test success. The first native after attempt failed an invented expectation
of128 returned nodes: real query_current_tx uses a50-node current page. Corrected
only the fixture expectation/report fields, preserving the128-node SQL seed and
all50-tool/row/shared-sequence assertions. The failed binary/manifest/controller
and stderr are retained; no product page size or source assertion was changed.

A shared measurement-target build switch also reported a fresh0.44s build without
recompiling the after source. That build was not accepted as a source-matched
observation. Forced recompilation by refreshing source mtimes, without changing
file bytes; the runtime after source/kind and copied binary hash were checked.
The public CI builds use separate subject targets and are unaffected. Future
local runs must retain runtime source/harness witnesses and never trust target
cache freshness alone.


## Formal producer3 P2/P5 follow-up

Workflow41166258538 completed successfully at exact7c6f4d6. The
[full issue report](https://github.com/tya5/shellboy/issues/41#issuecomment-6539050643)
records all12 pairs/24 complete windows. Source/harness/binary/profile hashes,
full600s/60s windows, all producer identities/counts,0 window model requests and
normal owner cleanup were checked. No observations are missing or relabelled.
The artifacts are native-tui-{Linux,macOS}-{80,140}-41166258538 in that run.

CPU is RUSAGE_SELF across app threads, one-core percent, excluding tool children.
Each row compares sequential subjects on the same runner; there is no cross-host
average. Deltas use unrounded inputs. P5-list retains the actual visible List.

| Runner / columns | Window | Main CPU % | E CPU % | Δ percentage points | Draws / max1s-bin main→E | UI tasks main→E | UI writer jobs main→E | RO opens main→E | output Hz main→E |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| Linux-140 | p2 | 3.7897 | 3.7852 | -0.0045 | 600/1→600/1 | 0→0 | 0→0 | 0→0 | 0.00→0.00 |
| Linux-140 | p5 | 7.1894 | 7.1482 | -0.0412 | 0/0→0/0 | 0→0 | 0→0 | 0→0 | 150.00→150.00 |
| Linux-140 | p5-list | 7.4105 | 7.1216 | -0.2889 | 0/0→0/0 | 0→0 | 0→0 | 0→0 | 150.00→150.00 |
| Linux-80 | p2 | 3.7568 | 3.7550 | -0.0017 | 600/1→600/1 | 0→0 | 0→0 | 0→0 | 0.00→0.00 |
| Linux-80 | p5 | 7.3909 | 7.2631 | -0.1278 | 0/0→0/0 | 0→0 | 0→0 | 0→0 | 150.00→150.00 |
| Linux-80 | p5-list | 7.1076 | 7.1918 | +0.0842 | 0/0→0/0 | 0→0 | 0→0 | 0→0 | 150.00→150.00 |
| macOS-140 | p2 | 0.3389 | 0.3823 | +0.0434 | 600/2→600/2 | 0→0 | 0→0 | 0→0 | 0.00→0.00 |
| macOS-140 | p5 | 3.7471 | 3.4966 | -0.2505 | 0/0→0/0 | 0→0 | 0→0 | 0→0 | 55.08→55.72 |
| macOS-140 | p5-list | 3.3998 | 3.7411 | +0.3413 | 0/0→0/0 | 0→0 | 0→0 | 0→0 | 54.57→53.98 |
| macOS-80 | p2 | 0.3112 | 0.2605 | -0.0507 | 600/1→600/1 | 0→0 | 0→0 | 0→0 | 0.00→0.00 |
| macOS-80 | p5 | 3.5860 | 3.8144 | +0.2285 | 0/0→0/0 | 0→0 | 0→0 | 0→0 | 57.05→55.62 |
| macOS-80 | p5-list | 3.6816 | 3.7600 | +0.0783 | 0/0→0/0 | 0→0 | 0→0 | 0→0 | 55.12→55.06 |


All16 P5/P5-list windows reach40 saved commits/second: Linux150, Mac53.98–57.05.
All have0 draws/UI read-task admissions/UI writer jobs/readonly opens. Lines are
not substituted for commits. The original producer1 run41160746679 remains a
separate main912b2e2→CBC comparison: Linux about50commits/s, Mac16.62–17.77
(unqualified40Hz load). Workload and sources changed, so cross-run CPU differences
cannot be attributed to E. The original report6538392801 retains its full values.

P2 has600 draws/600s and0 UI reads in all8 windows. Mac140 nevertheless has
max1s-bin2 on both subjects (main1 such bin, E88); shortest intervals0.753213 /
0.844620s. The other three pairs have max1s-bin1. Average1Hz is not a strict-bin
pass, and phase/scheduler contributions were not separated. P2 Mac140 CPU rises
0.0434percentage points; other pairs decrease. Mixed P5 CPU differences and one
pair per host establish neither stable improvement nor a no-worse guarantee.
P2's strict redraw/CPU policy therefore remains open across D/F work.

All DB work is not zero: raw output/index/maintenance remain active. Both subjects
record AttachmentService::collect_managed20 times per P2 and2 times per P5 in
the unknown-caller field. Windows TUI jobs upload explicit unavailable.json; no
ConPTY performance result is inferred. Initial short-ref checkout failure
41166177038 remains separate. Main admission still needs later reviewer approval;
P3 matched-service/caller follow-up and full #41 remain unfinished.
