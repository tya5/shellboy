# Conversation shared-read qualification (#41 D0 / E foundation)

## Source and scope

Local commits f43f7dd and55cdc67 implement the coherent current-view read and
cached Thread tree, respectively. Merge2fd4197 integrates mainbb7e29b, including
#59 sender evidence and #69 setup. Commit67f6f09 updates integration observers
for atomic header/history and exact stream-to-canonical mappings. These are local
commits; no main admission, tag, release or installed update is implied.

The collector observes receipt states, selected Thread, saved page/paired pending,
source/tool/recovery/usage, received audits and activity in one DB transaction.
One selected-consumer start deadline is500ms; one in-flight refresh retains a
dirty follow-up. Paused tool facts share that read without retaining message bodies
or triggering repeated refresh feedback. Prepared history, pending, received and
paused source facts share the read witness. Raw failure tails use the already
validated source/execution facts and existing finite read-only output facility.

Atomic UI application rejects stale/malformed child scopes before header changes,
retains stream commit mappings before canonical history, and performs one cache
retention pass. Existing shrink/follow/stale diagnostic filtering remains active.
The Thread tree is rebuilt only after item/filter/collapse changes. Selected IDs
and screen rows survive reorder/deletion; preferred first load does not invent a
viewport anchor, and viewport growth reveals newly fitting rows without a rebuild.

## Checks and observed failures

Environment: native macOS arm64, pinned Rust1.99.1. All fixtures use temporary
stores, synthetic identities and local mocks. No paid provider or user data.

The initial library run failed2 tests (818 passed/13 ignored): a Thread-only
notification observer and a genuine preferred-first-load viewport regression.
Both were corrected; a paired-diagnostic audit also found and fixed an early-return
path that skipped existing history diagnostics. The first combined standard run
on2fd4197 passed layout90/Python6/fmt/strict Clippy and826 library tests; integration
had873 passed/2 failed/17 ignored. Both failures were observers waiting for the
former separate header/history delivery. Their replacements validate the coherent
frame and its exact mapping rather than dropping those assertions. All20 frontend
integration tests then pass at67f6f09.

Final `sh scripts/check.sh` at67f6f09 with CARGO_BUILD_JOBS=2,
CARGO_INCREMENTAL=0 and RUST_TEST_THREADS=2 succeeds: **1,711 passed /0 failed /
32 ignored** (library826, integration875, termination7, crash3); layout90,
Python6, formatting and strict all-target Clippy pass. The integrated suite includes
all4 real separate-CLI/GC reactivity tests,826 library regressions including the
new coherent-frame/diagnostics/scope/tree cases, and existing real-TUI PTY tests.
Those PTY tests do not replace a new native visual review of the entire D UI.

## Actual worker observation

The same128-node/50-successful-tool fixture has50x64KiB stored raw output,
10 warmups and30 measured observations. Private ignored worktrees instrument
actual DB-worker Run closures and elapsed work; instrumentation is not a product
feature. Both versions return all50 canonical tools and0 raw-preview blocks.

| Observed source | Actual jobs per refresh (min/p50/p95/max) | Worker work p50 | Worker work p95 |
| --- | --- | --- | --- |
| Before7aea088 |7/7/7/7 |3.345ms |3.675ms |
| After2fd4197 |1/1/1/1 |3.809ms |3.940ms |

The one-job observation holds for this selected current-view consumer. Worker time
in this sample increases by0.265ms at p95; this is not evidence of a SQL-time
speedup. The combined raw-source projection validates identities before rendering;
keep its extra work visible in later combined E/F comparisons. Existing helpers
still contain per-node SQL inside that worker job. Their actual SQL batching is
unfinished, so a1-job count is not proof of the adopted all-surface batch-work
requirement. The reused ThreadDetails helper can also derive an empty title by
scanning/parsing saved user payloads on the DB worker. Extracting that UI title
read/preparation is an explicit remaining D0/D1 boundary task; this local check
does not establish SQL-only work for that path. Both runs
are synthetic local observations, not end-to-end provider latency.

One attempted after-run reused the before executable from a shared target cache;
its7-job result is excluded as a measurement-identity failure. The final after-run
forces package compilation from the intended worktree, prints the source witness
2fd4197 and asserts exactly1 job on every sample. Future cross-worktree Cargo
runs must invalidate the package source fingerprint before using this shared
cache. Do not treat an exited-success process without its source witness as
valid comparison evidence.

## Limits and remaining gates

This unit does not finish D or E. Visible source-detail refresh remains a separate
bounded consumer, and independent activity polling remains until D's live-work
and clock policy. One-job current-view measurement must not be presented as
all-surface planner consolidation. Live rows/output focus, silent-background P2,
hidden-output P5, E patches, F Watch/descendants/P4 and G popups/P6 remain pending.
Native Windows/Linux and physical terminal behavior have not been verified for
this local increment. Prior B+C native CI and PTY evidence do not qualify it.


## E prerequisite: external deletion notifications

Commit161b4c9 returns exact successfully deleted Thread IDs in the existing
mutation effects. Control emits existing Changed notifications after commit;
saved-operation replay, domain rejection and SQLite rollback have no notification
effects. No new event type/schema/lifecycle/queue is introduced. Two focused
library tests pass, covering singleton/cascade targets and actual Control
subscriptions, independently observed committed deletion, replay, rejection and
partial-cascade rollback. The follow-up standard check at161b4c9 succeeds: **1,713 passed /0 failed /
32 ignored** (library828, integration875, termination7, crash3), layout90,
Python6, formatting and strict all-target Clippy, with the same native macOS and
bounded build/test settings above. This
fixes the producer gap only; browser patch consumers remain unfinished.


## D1 actual SQL batches and bounded raw headers

Commits2f2e39b/5000b05/ee369f6/92e9450 separate tool sources, sender facts,
recovery evidence and raw headers. Merge46963c9 includes main1b559b9, including
submitted-copy retention/Thread-scoped fences and setup follow-ups. Commit558b870
fixes physical join order and avoids running the title byte-budget window over
non-user rows. Final native standard check at558b870 passes **1,745/0/32**
(passed/failed/ignored; library859, integration876, termination7, crash3), layout90,
Python6, formatting and strict all-target Clippy. Same bounded build/test settings
as above; no provider call, user data or installed update.

The test-only shared SQLite trace observes real statement executions. It is not
compiled into the product. Bundled-SQLite regressions establish:

- Modern tool nodes:2 statements for50 direct results;3 with whole groups, using
  at most129 actual ordinal seeks per distinct group. Sparse ordinals, repeated
  call IDs/groups, duplicate requests, spoofed/missing/collected sources, foreign
  shell ownership and the first output-item's null source mapping remain correct.
  Projected initial-page/scroll tool facts also use2 statements, rather than a
  per-block loop; independent old queries remain only as test oracles. Explicit
  one-source preview validation still uses its existing exact query.
- Common sender facts:2 statements for50 tools;4 for50 distinct user timestamps.
  Every target/state/timestamp range is capped at129 matching receipts before
  requested-node filtering. Any overflowing state or ambiguous source is Unknown;
  legacy-null's Human band convention, typed unknown sender facts, original
  parent/self identity and non-authoritative external names remain distinct.
  Only unique sender witnesses are copied, at most8193 Unicode characters each.
- Recovery:1 statement for50 eligible events, deduplicating saved Turn/time facts.
  Historical observation time, latest failure/source selection, byte limits,
  duplicate request order and raw-error decoding after DB exit match the old
  independent oracle. Empty requests and scope-bound failures execute no SQL.
  Exact counts still read each distinct indexed attempt range;1 statement is not
  a constant-work claim.
- Headers:1 statement for explicit names;3 for an automatic name/witness. Thread,
  Run/Turn IDs, direct owned counts, context and original settings survive DB
  shutdown before preparation. Prefixes bound inspected membership before user
  filtering; source bytes share512KiB, with128KiB per candidate. Earlier omitted
  sources, row/byte exhaustion and pending tie ordering cannot justify choosing a
  later input. Empty evidence, unreadable users and limited evidence stay distinct.
  CLI title semantics remain unchanged; initial draft decoding is also off-DB.
- Native VM-step/query-plan tests scale unrelated source/node records by10,000
  without a linear lookup scan. The existing migration2 node-source index is used;
  no new index/migration/schema or authority rule is needed.

### P3 trace: identical source workload and current-main baseline

Private baseline00d8472 merges the pre-D1 sourcee70b729 with the same main1b559b9
used by final558b870, isolating the D1 change from concurrent main work. Both
are built explicitly from their intended worktrees, print full source witnesses
and assert1 actual worker job for every sample. Fixture:128 saved nodes,50 closed
successful tool results and50x64KiB retained raw output;10 warmups,30 samples.
All50 tools return in every sample and raw preview blocks remain0.

| Source | Actual worker jobs | SQL executions (min/max) | Worker work p50/p95 | SQLite VM steps (min/max) |
| --- | --- | --- | --- | --- |
| Pre-D1 +same main00d8472 |1 |176/177 |4.813/9.826ms |16,602/16,607 |
| Intermediate46963c9 |1 |26/27 |4.591/4.937ms |61,255/61,260 |
| Final558b870 |1 |26/26 |3.334/4.359ms |22,632/22,632 |

The intermediate result exposed a physical-plan regression despite fewer SQL
statements: a kind index drove repeated requested-ID scans. The final selector
forces requested IDs before node-PK lookup; the bounded title prefix filters
users before its window while retaining an empty overflow witness. Keep the
intermediate observation as evidence, not as final qualification.

Final VM work remains higher than pre-D1; do not report a universal SQL/CPU
speedup. Worker time here includes instrumentation and local scheduler/load
variation, and excludes queue waits/presentation/render/provider time. Occasional
existing worker maintenance appears in the statement/VM hook. SQLite full-scan
counters98→724 include bounded materialized/temporary CTE scans; they are not
physical table-row or disk-I/O counts. Native scaling tests and EXPLAIN separately
prove the intended PK/source-index lookups. This measurement qualifies the
selected current-view workload, not all popup/list/Watch consumers or global P3.

An earlier minimal Python schema omitted the already-shipped node-source index.
Its linear-scan curve is explicitly invalid for the current store. The corrected
full-schema probe and bundled regression support retaining schema14. No proposed
migration15 was implemented. See the decision log for the rejected proposal.

D live work/clock/output, E patches, F Watch/descendants and G catalogue/forms/
popups remain open, including applicable P2/P4/P5/P6 and exact-source native
Windows/Linux/visual gates. Previous B+C captures or CI do not qualify D1.
