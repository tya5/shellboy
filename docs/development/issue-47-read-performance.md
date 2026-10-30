# Saved Usage Read Measurements (#47)

These are shared read-query projection measurements, not completed CLI/IPC/UI
latency. The initial projection is not yet attached to those surfaces. No schema,
producer/state/driver hook, persisted total or paid/user-data operation is used.
The latest target is advisory100ms, with roughly500ms acceptable at100k after
obvious query waste is removed; no absolute wall-time assertion enters CI.

## Environment And Fixture

macOS26.3.1(a), Darwin25.3.0 arm64, Apple M1/8GiB, Rust/Cargo1.99.1,
LLVM23.1.1; pinned bundled SQLite3.53.2. Release profile uses thin LTO and one
codegen unit; build jobs2/incremental0, one test thread. Full logs are local
artifacts `/tmp/shellboy-47-read-scale-release.log`,
`/tmp/shellboy-47-read-varied-release.log` and environment
`/tmp/shellboy-47-read-scale-env.log`.

Each temporary file has100k attempts (60k completed/20k failed/20k incomplete),
1k completed Runs/Turns and10k terminal shells. Model/Run saved duration is10ms,
shell elapsed5ms. Root plus100 children: hot layout puts all Runs at root;
spread uses Run index modulo101. Turn u0/Run r0 have100 attempts/10 shells;
spread root has1k attempts/100 shells. Standard JSON reports input100/cache75/
output10/reasoning2/total110. Legacy case replaces the first256 rows with the
same standard fields plus128KiB attribution (approximately32MiB extra).

First-source `fc122c9` has identical usage/date values. `0f2e6ad` additionally
varies input by rowid modulo97, output by modulo13, and appends distinct rowid to
the256 legacy appendices. This matters: bundled SQLite jsonCacheSearch has four
entries and compares JSON text contents, so identical rows can share parse work.
Legacy key ordering also differs between fixture generators; do not attribute
all timing differences to caching. Normal correctness tests cover invalid/missing
fields separately; no fabricated requests come from cache records.

Each scope has one first read plus21 fresh-connection repeats and21 repeats on
one connection. Fresh connection means cold SQLite page/statement cache, NOT
OS-cache cold; fixture writes and other probes can warm OS caches. Report median
and nearest-rank p95. Timing includes transaction/projection/query preparation,
read-local date function registration, SQL and result building, not connection
opening, fixture writes, EXPLAIN, transport or rendering. VM steps use statement
counters; all plans come from the actual generated query. Warm timing is separate.

## Initial Exact Results

Exact `fc122c9edad5a6d6455df957c26ae3d8d10d163f`: scale probe exits0, all16 cases
complete (96.06s execution after release build). Fresh-connection median/p95 ms:

| Layout | JSON | Turn | Run | Thread | Thread+Children |
| --- | --- | --- | --- | --- | --- |
| Hot | Small | 2.15/2.23 | 2.20/2.63 | 334.31/339.66 | 336.53/345.09 |
| Hot | Legacy256 | 13.91/14.62 | 14.26/14.62 | 363.12/366.59 | 369.23/373.73 |
| Spread | Small | 2.25/2.71 | 2.19/2.28 | 5.27/5.99 | 335.00/338.65 |
| Spread | Legacy256 | 14.10/14.35 | 14.19/14.43 | 16.90/17.50 | 367.32/373.43 |

Warm medians: hot/small Thread334.77ms, children333.86ms; spread/small root4.43ms,
children332.66ms. Full first/fresh/warm/p95 data are in the artifact, not omitted
failures. Warm results do not turn this into OS-cold evidence.

Exact `0f2e6adaeda38aacfbabcae3c378666399b269bd`: varied probe exits0 (113.56s
execution). Hot varied small Thread median402.15/p95413.63ms, children401.13/
426.65ms; distinct-legacy Thread430.78/439.87ms, children400.38/405.00ms. Small
Turn/Run remain about2ms, legacy about9ms. This variability/fixture shape does not
support claiming the identical-value fixture represents all histories.

## Plans And Cause Probe

Models seek attempts_by_turn through Turn PK/turns_by_run/turns_by_thread;
initial terminal and active model reads are separate source passes. The large
projection materializes values and uses a temporary grouping B-tree. Run reads
use PK/run_thread_created, and active Runs use one_live_run. Terminal shells
SCAN shell_terminal_time: it is a time-only partial index, NOT a scope seek.
The initial active-shell plan also scans shell_active_owner without an owner
predicate. Bounded lineage uses Thread PK and threads_updated, not a persisted
parent index. No new index is silently added.

Varied small hot Thread statement medians (21 warm repeats): model terminal385.94ms
and14.51M VM steps; active-model second pass15.53ms/610k steps; all terminal
shells8.15ms/337k steps; terminal Runs0.44ms; empty active-shell query0.014ms.
Diagnostic-only variants replace data-dependent work with constants: no JSON97.42ms,
no timestamp parsing373.43ms, neither79.45ms. They are cause probes, NOT valid
production queries or claimed feature performance. JSON validation/extraction
cost dominates; date parsing and redundant active scans are smaller contributors.

## Query-Only Optimization And Remaining Work

A new query factors shared #49 numeric checks, validates/extracts only five fields
into transient compact JSONB once, combines terminal/active model traversal,
borrows timestamp arguments without String copies and seeks active shells by
saved owner Run where the existing index permits it. Terminal shell scope seeking
has no suitable schema12 index; retain complete saved-shell coverage instead of
assuming every row has a tool-call join. Re-measure this new exact source before
accepting performance. Firstlast/own-Turn metadata, CLI/get/TUI attachment, broader
edge regressions and full standard/native checks remain separate completion gates.
