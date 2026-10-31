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
