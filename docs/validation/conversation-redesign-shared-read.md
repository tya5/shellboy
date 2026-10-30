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

The job-count goal is met for this selected current-view consumer. Worker time
in this sample increases by0.265ms at p95; this is not evidence of a SQL-time
speedup. The combined raw-source projection validates identities before rendering;
keep its bounded extra work visible in later combined E/F comparisons. Both runs
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
