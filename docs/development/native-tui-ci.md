# Manual native TUI measurements

The optional `native-tui-measure` job in `.github/workflows/ci.yml` records issue #41 P2/P5 observations. It does not change the existing Windows test-timing comparison. Dispatch with `native_tui_performance` enabled (`performance` separately enables the existing Windows test comparison), an explicit `native_tui_baseline_ref` and the candidate ref (empty means the workflow commit).

Each Ubuntu/macOS job builds both subjects before measuring them sequentially on that runner, at 80×40 or 140×40. P2 observes an actual background shell blocked on stdin for 600 seconds after its Turn completed. P5 hides a real stdin-gated producer, then observes its flushed output for 60 seconds. A separate P5-list window keeps List mode visible while that other Thread produces output. The synthetic Responses endpoint binds loopback only. Each scenario has a fresh synthetic data directory and config; no user store or paid API is used. Fixture setup, CLI/SQLite checks and warmup precede the acknowledged observation window. No CLI/SQL probes run during the window.

Windows currently uploads `unavailable.json`: a real ConPTY fixture has not been implemented. This is an open verification gate, not a simulated success. Ordinary CI does not run these long windows. The execution history below distinguishes fixture qualification from formal measurement.

## Witness and failure policy

`scripts/tui-measure.py prepare` obtains actual HEAD/tree and tracked-file hashes from each clean checkout. It validates every hook before writing a patch into that disposable checkout. `build` records the fixed toolchain, build command/profile, exit and binary hash. Before each scenario the harness verifies original/modified source hashes, HEAD, harness hash and binary hash; the executing binary returns embedded source/harness hashes. Unsupported source layouts fail with an artifact instead of zero counters. The generated Rust module is a CI template, never part of product modules or release binaries.

Artifacts include manifests, instrumentation diff, build logs, binary hashes, fixture/terminal captures, metrics, summary and same-job CPU/draw deltas. Raw terminal captures retain at most 64 MiB of the true tail and report total/truncated bytes. The decoder keeps a fixed screen, with no scrollback. Only synthetic fixture data is uploaded. Source refs, toolchain/profile, dimensions and same-runner identity must agree when interpreting deltas; hosted runners across jobs are not comparable hosts.

CPU uses the Shellboy process's `RUSAGE_SELF` (all its threads, excluding fixture children). Writer-worker admission/execution/work counters distinguish UI, output, maintenance, index and unknown callers. All production `Connection::open_with_flags` sites are instrumented, including dynamic flags, and readonly sites are listed in the manifest. Readonly counts are actual connection-open boundaries; successful UI read-task admission is recorded separately. Readonly work time is lexical connection-scope lifetime, not pure SQLite execution time. Work completing after the end signal is excluded. Draw bins and the first 4096 draw timestamps are bounded; the aggregate count remains available. Unknown worker callers remain visible rather than being silently classified as UI.

CPU, timing, draw and read values are **record-only**. They never decide job success. Wrong source/binary witnesses, build failure, missing counters, short windows, unexpected model requests, invalid fixture state, missing producer output or unsuccessful cleanup are errors. A producer below 40 committed batches/second is recorded as an unqualified load observation, not a timing failure or a passed P5 load gate. Missing before/after artifacts remain missing in the summary; no result is invented.

The Unix job timeout is 90 minutes: each subject requires 720 seconds plus setup, and both release builds occur on the same runner before measurement. Artifacts upload with `always()` even on failure.

## Fast qualification

These commands do not build or execute Shellboy and do not require the CI-only decoder:

```sh
python3 -m unittest discover -s scripts/tests -p test_tui_measure.py
python3 -m py_compile scripts/tui-measure.py
```

For an explicitly requested short native fixture smoke test, prepare two **clean disposable** checkouts, install `scripts/measurement/requirements.txt`, build both with the harness, then run:

```sh
python3 scripts/tui-measure.py prepare --subject BEFORE --output observations/before
python3 scripts/tui-measure.py prepare --subject AFTER --output observations/after
python3 scripts/tui-measure.py build --subject BEFORE --output observations/before
python3 scripts/tui-measure.py build --subject AFTER --output observations/after
python3 scripts/tui-measure.py run-pair --baseline BEFORE --candidate AFTER --output observations --cols 80 --smoke
python3 scripts/tui-measure.py summarize --output observations
```

Smoke windows are two seconds each, explicitly labelled and never P2/P5 acceptance. Full windows are rejected outside GitHub Actions. `pyte==0.8.2` provides the fixed VT screen; `wcwidth==0.2.13` pins character-cell width behavior. These are CI-only Python requirements; no Rust product dependency is added. Review artifacts before using observations for acceptance.


## Local harness qualification (2026-10-31)

On native macOS ARM64/Rust1.99.1, the instrumented release build of sourcee5870d0
and three80-column2s fixture smoke windows pass. This source predates the later
#75 merge; these observations qualify the harness only, not the proposed E
candidate or the600s/60s acceptance windows. Silent-background smoke records two
draws and zero UI worker jobs/readonly opens; hidden-output and visible-List
smokes each record zero draws/UI jobs/readonly opens with102/101 committed
batches (about50Hz). No provider request occurs inside a window. All fixture
owners exit cleanly. At this initial checkpoint no full CI or native Windows
measurement had run; the later CI report below supersedes that execution status.

The first smoke attempt failed in pyte's wide-character `display` exporter,
before metrics existed. The controller now exports the finite grid directly;
a regression tests a clipped wide glyph. Keep that initial failed proof. Fast
Python tests20/0 and Ruby YAML syntax validation pass. Both the full historical
baseline1fec7f0b2000b7dadbcec518e493cdfec169eeee and the E+#75 source support hook
planning without source writes; baseline compilation/execution remains pending.


## Bounded P5 producer follow-up (2026-10-31)

The initial formal run41160746679 compared dispatch-time main912b2e2 withCBC.
See [the full report](https://github.com/tya5/shellboy/issues/41#issuecomment-6538392801)
for all12 same-runner pairs, ordinary-CI failures and performance gaps. Its Mac
P5 windows reached16.62–17.77 saved commits/second despite about50 lines/second.
These lower-load observations remain valid records but do not qualify40commits/s.

`native_tui_producers` now selects1–4 independent gated shells for both subjects'
P5/P5-list windows; default1 reproduces the original load. P2 remains one silent
shell regardless of that option. Use3 explicitly for the proposed follow-up CI,
with matching before/after count and actual saved-commit rates in the report.
The CLI equivalent is `run-pair ... --producers 3`; `--smoke` remains2s. Readiness
requires all distinct executions under one saved completed Turn/live Run, and all
are released before warmup. Record per-producer50 target lines/second separately
from the aggregate150 target lines/second and actual saved commits. Multiple
producers are a load-fixture hypothesis, not a Core throughput fix or guarantee.

A native macOS ARM64 release build of exact E source0b7bd73 completed using only
measurement-target cache reuse. Its three80-column2s smoke windows pass:
P2 uses1 producer/2 draws/0 commits; P5 and P5-list use3 producers,0 draws and
306/305 commits, respectively (150.28/150.13 commits/s). All record0 UI read-task
admissions,0 UI writer jobs,0 readonly opens and0 model calls inside the window;
all owners exit0 with no remaining live shells. This qualifies the bounded
fixture at short duration on this Mac, not a60s hosted macOS/Linux result, a
before/after comparison, P3, Windows support or main admission.

The initial local build launch lacked Cargo in PATH and failed before invoking
the compiler; using the documented toolchain path completed the build in3m57s.
This environment failure is distinct from product/test failures. Python22
controller tests pass; repaired-source three-OS CI and formal follow-up are not
run by these local changes. No user store, installed binary or provider is used.
