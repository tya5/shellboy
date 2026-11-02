# Validation record

## Index-source GC scan (#164, 2026-11-02)

macOS, rust-slot: unit test `gc::tests::index_source_scan_seeks_deleted_nodes_and_owned_rows`
asserts the result (job- and chunk-owning deleted nodes plus a payload-less node)
and EXPLAIN QUERY PLAN `SCAN n USING INDEX nodes_index_gc` with `jobs_source` /
`chunks_source` seeks, no other SCAN and no temp B-tree. Synthetic measurement
(ignored test `index_source_scan_measure`, debug build, in-memory DB, 58,902 live
~1 KB nodes, 5,098 deleted nodes, 130,000 jobs, 44,000 chunks, zero result rows;
single run each, recorded facts not bounds): before 60.66 s (`SCAN embedding_job`
per candidate), `jobs_source` only 20.5 ms (full node scan), both indexes 5.5 ms.
The user's DB was not opened; the 2m33s figure came from a copy measured earlier.
Schema upgrade/fresh-equality tests are listed in the commit's check run.
## Dead delivery retention (#170)

On native macOS, `cargo check --tests`, five `gc::event_retention` unit tests,
the schema-validation test, five `storage_hot_queries` tests, public schema
convergence, and the History GC integration test passed against synthetic data.
The integration test initially saw stale shared-target output; it passed after
`touch src/lib.rs` and rebuild in rust-slot. The actual correlated eligibility
query must use indexed range searches for both node and delivery references;
its plan is checked in the GC unit tests. Three-OS CI is pending. No
production database mutation or real conversation data was used.

The later restack onto 72f163d5 retains schema20's index-source GC migration
and moves event retention to schema21. Synthetic 20-to-21 coverage checks both
index sets and ledger names; Python unittest discovery passed 169 tests on macOS.
The rebased branch's fresh Rust checks and CI are reported separately; the
previously passing CI did not test this schema21 tip. Real-database capacity
savings, throughput and long-term behavior remain unmeasured.

## Bounded index wake-delay read (#164, 2026-11-02)

macOS, rust-slot: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`
and `cargo test --lib index::` (50 passed, 2 ignored) passed. New unit tests cover
a paused profile with family None/Some returning 30 s without touching queue
tables, earliest selection across active/candidate/paused/other-splitter spaces
and bootstrap rows with auto_history true/false, the 256-row bound fallback
(last examined time clamped to 1..30 s), and byte-exact bootstrap key ranges with
`_`, `%` and mixed case (the old LIKE also matched other profiles).

Synthetic measurement (ignored test `next_delay_synthetic_measurement`, release,
in-memory DB, 84,948 pending ineligible jobs, 84,948 ~1.8 KB `index.input.*`
rows; single run each, recorded facts not bounds): before 19.5 ms (auto_history
true), 28.3 ms (false), 28.2 ms (paused profile); after 0.19 ms, 0.11 ms and
0.004 ms. EXPLAIN QUERY PLAN on the user's database (read-only, plans only): old
job MIN walks all of jobs_ready unconstrained and the LIKE read is `SCAN
app_meta`; new reads use `jobs_ready (space_id=? AND state=? AND
next_attempt_at>?)` and `sqlite_autoindex_app_meta_1 (key>? AND key<?)`. The
fix was not run against the real owner or its disk-resident database.
## DB job timing (#164)

Native macOS synthetic tests exercise a blocked DB job, queued work, distinct
call-site names, fast-job silence, privacy and the worker's event limit. The
post-change `cargo check --tests`, `cargo test --lib slow_job_tests` (2 tests),
and `cargo test --lib db_job_slow_tests` (1 test) passed via rust-slot on macOS.
Three-OS branch CI is pending; this does not establish latency or log
completeness under a production workload.
## Automatic compaction activity (#165, in progress)

Before the change, a disposable macOS PTY with a loopback mock reported input
203,000 against the default 200,000 threshold and gated the actual summary HTTP
request. After the third Turn's activity update, the footer showed `Preparing`
and the existing context bar, not an automatic-compaction label. This is a
synthetic provider/PTY observation, not a physical-terminal or live-provider
claim. On the candidate, the same loopback fixture held summary requests and
observed the auto-compaction label with the exact trigger values in English and
Japanese, followed by its disappearance on completion. A synthetic HTTP 400
rejection and Ctrl+C during a gated request also cleared the label and recorded
one bounded start/finish event pair with matching Thread/Run/Turn IDs. Focused
macOS check, Clippy, timing/recovery unit tests, activity witness/PTY tests and
seven proactive tests passed; Python tooling 168 and test layout also passed.
Physical terminal, real provider, Windows-local and post-integration coverage
remain unverified. Fresh task-branch CI is pending.

After rebasing onto 99d10a78, the first shared-build attempt returned zero
matching tests and is not validation; it was stopped. On macOS 26.3.1 arm64,
Rust 1.99.1, source tip 4c821816, a fresh source and integration-harness build
passed `cargo check --tests`, the automatic-compaction separator unit test (1),
and the bounded run/turn witness integration test (1). The runner rejected
zero-test results. A binary copied from the same locked build passed isolated,
literal-loopback synthetic PTY scenarios for English success, Japanese success,
summary rejection, and cancellation. Python PTY helper tests (9) and integration
layout (102 suites) passed. Local synthetic logs are retained in the task
worktree; no physical terminal or live provider was exercised.

## Proactive retry gate (#153, 2026-11-02)

Review follow-up includes a read-only inspection of the user's W3 compaction and
usage records (no private text, mutations or provider request). At 18:35 UTC,
44 spill replacements committed despite the summary's `invalid_state`; the
result recorded `rejected` and omitted the spill counts. Trigger input was
202,622 tokens; the next completed request reported 151,601 (51,021 fewer),
though intervening changes prevent assigning the full difference to spill.
The rejected summary's `summary_usage`: 1 map / 0 reduce, input 2,895,
cached 0, cache-creation unknown, output 1,051. This is provider-reported usage,
not a cost in currency. The rejected source was 1,741 characters and its
candidate summary 1,840 characters (not a reduction). By the trigger, that
Turn had 193 completed conversation requests with reported output sum 74,417,
of which reasoning tokens were 30,759. These are generated-token totals, not
measurements of replayed or retained tokens: output includes other item types,
and reasoning replay is encoded/provider-dependent. They illustrate potential
growth, not an exact remaining footprint. The synthetic acceptance below
additionally tests that partial adoption retains failure usage and that the next
actual request shrinks.

Review regression on native macOS: rust-slot execution
`af851bd4-f1b9-4dbd-aaef-938e084937c2` passes all six proactive integration
tests. In the partial-adoption case the summary source is below 3,000 characters
and two safe current-Turn tool outputs are spilled; the next ordinary request's
input JSON decreases from 39,706 to 7,123 bytes. The fixture uses the existing
`summary_max_chars=4000` setting to make the candidate longer than its source
without issuing a reduce request. The rejected summary reports one mock map
request, input 321 and output 123 tokens; partial adoption retains that usage
and the growth gate. A separate HTTP 400 rejection makes one mock summary request
with input and output unknown, not zero; repeated 400s also do not report usage.
Earlier fixture runs exposed tool output clipping, mistook
a reduce request for a conversation request, and produced an accepted summary;
these were corrected before the passing result. The earlier post-rebase
`cargo check --tests` passed. CI and focused unit results are reported separately.

Test scope: synthetic temporary data-dir and loopback Responses mock; no user data,
real provider, installed binary or paid API. Every new mock loop is capped at
nine shell tool calls and four summary requests. Acceptance covers same-Run
rejected -> adopted at input 150 -> 160 with threshold100, repeated rejection
counts at 150/160/170/180, and no retry at unchanged/decreasing/unknown/159 usage.
Unit coverage checks rounding, u64 overflow and the ordinary summary boundary
before/after spilling a large current-Turn tool group. Existing threshold
and opt-out behavior remain covered by focused tests. Post-adoption stale usage
is an existing guard and is not independently exercised by these new tests.

On macOS, rust-slot `cargo check --tests`, the retry arithmetic unit test and
`cargo test --test integration runtime_features::proactive_` passed (five tests,
execution a859273f-e050-45ac-a0ea-8eb1e7d9cdc1). The strengthened integration tests also passed on macOS in execution
8f733d3b-adc2-418c-b674-8dccfae9f327; its policy test filter matched zero
tests because the shared build directory reused another worktree's binary.
After a forced rebuild in rust-slot, execution 3a8af7fd-d438-429b-8dc9-9e8ca30b5c63
passed `cargo check --tests`, both focused unit tests (one each), and all five
proactive integration tests after rebase on the earlier branch revision. The
review follow-up's tests and CI are reported separately. The W3 record is a
read-only observation, not an execution of the fix with the real provider or
reconstruction of private conversation content. Native Windows remains the
integrator's three-OS CI responsibility.

## v0.5.0 preparation boundary

Preparation base is main `cdcb70da`, including #101, before #117 main integration.
Package/lock version, release-note draft and guide version/description wording
are changed; conversation schema remains17, RAG3 and local protocol2.
The Agent Guide's obsolete optional-generation/normalization wording is corrected
to the adopted model-HTTP requirement and existing verbatim replay behavior.
No runtime implementation or migration is added by this preparation.

Build-free checks on the preparation tree pass: integration layout95/watchdog,
Python document-role1 and candidate-installer unit2 (3 tests total), `git diff
--check`, and TOML parsing confirming package/lock both identify shellboy0.5.0.
Execution315e7d61 exits0 with group cleanup confirmed. Installer unit tests use
synthetic fixtures; they do not establish a real candidate installation pass.

Final-source Rust checks and three-OS CI remain pending. Release-artifact builds,
installers and public v0.5.0 download checks have not run. The private macOS
build and synthetic upgrades are qualified separately below. No paid provider,
user-data or installed-binary change is part of this validation.
The upgrade helper now accepts public0.4.0, schema17 and `--start-schema 16`.
The authoritative v0.4.0 tag `dae2ba1f` contains migrations1-14; schema16 is a
separately labeled synthetic fixture prepared with existing derived SQL15/16,
not an unmodified published0.4.0 store. Nine Python SQL/WAL unit tests pass:
public14 -> 17, staged16 -> 17, staging preservation, rejection of rewritten
originals, changed lineage/ledger prefix or a wrong parent index, checkpointed
no-sidecar reads and preserving committed WAL. SQL tests alone do not qualify
the startup runner. The private PTY results below do; final release-byte checks
remain separate in the [candidate plan](../development/publication.md#candidate-validation-plan-not-results).

### Private macOS synthetic upgrade qualification

On native macOS ARM64, explicit slot07027a07 built source
`0c9535fead401e5d4ff2670b0b927870aa3dcfd8` / base`cdcb70da` using
`cargo build --locked --bin shellboy -vv`, JOBS2, clean/SHA/base guards and
own-path compilation evidence. Only own entry-file mtimes changed. Build exited0,
cleanup confirmed; the executable was copied immediately within the slot to
`.local-bin/shellboy-v050`, reporting `shellboy 0.5.0`, SHA-256
`3bb68ed0da956bfcb8457a4247cfe19a09975e1014d8648be28a9d8fe2601291`.
The shared Rust slot was then released. This is a private debug build, not a
release archive, tag, installation or exact final-main qualification.

Public v0.4.0 archive SHA-256
`5e37608162d526825d15af945e418a54cd7b09e4c6234c9c3f209c6f181dcb34`
was checked against a freshly downloaded public SHA256SUMS before extraction;
its macOS binary reports0.4.0, SHA-256
`286f9d1701aae742ea02f82afa5cdd99b36a5c44000ea49215b0f6972b49b19f`.
Actual old-owner startup confirms schema14. Stopped SQLite backups preserve WAL;
only the separately labeled16 fixture applies production15/16 SQL before startup.

Final helper SHA-256
`944a984e404e5248e3a857d2a7b0f92f3c46bd7a82925dd4f08abc036e88d509`
passes both public14 -> 17 and synthetic16 -> 17 (executionb0ca54e8 exit0/cleanup).
Each run checks the exact ledger prefix/suffix, parent index, integrity/foreign
keys, original21 nodes,16 edges,19 path rows,3 output items,1 tool call/result,
1 output chunk and3 Thread lineage rows. There are no attachment rows in this
fixture; attachment preservation is not qualified. Original sender, held model
input, completed usage and RAG3/search remain available. New loopback mock exec
succeeds; normal owner exit is0, then original-row/ledger checks pass again on
the stopped store. Each run used5 mock Responses and12 mock embedding requests,
not paid providers. Private evidence is under
`/tmp/shellboy-v050-upgrade.05J3be/closed14` and `closed16`.

Failure history is retained: the added post-stop readonly check initially failed
with `unable to open database file` after checkpoint removed WAL sidecars
(execution2a568efa). Explicit connection close and stopped/no-WAL immutable reads
fix that fixture access; existing WAL is never ignored. A subsequent95-test
Python run failed because the new no-sidecar fixture assumed automatic WAL unlink;
a standalone9-test run reproduced it. The corrected fixture explicitly checks
TRUNCATE checkpoint, closes handles and copies the checkpointed main file into a
fresh synthetic path; it does not delete WAL or weaken preservation assertions.
Earlier successful startup-only runs do not replace these post-stop results.
Corrected-source focused9 and full Python95 tests pass (executionbda9f9bf,
exit0/cleanup), with layout95/watchdog and diff checks successful. Full Rust
standard/native3OS CI and exact final-candidate upgrade runs remain pending.
## #108 current-main preflight (Rust unrun)

Task branch rebases without conflicts onto maincdcb70da; original G delivery base
is811852f. Permanent author-owned module/target/dispatcher/dirty/test registration
follows the reviewer2026-11-01 update, not the superseded coordinator-only draft.
Rustfmt is applied to task changes; original captured values and observation
contracts are unchanged. Current planned positive filters are
`frontend::inspection_retained::`14, `history::raw_witness::`3 and registered
integration `retained_sources::`2, total19. Two IPC cases label test-side GC,
output growth/terminal/tombstone as synthetic changes and use real IPC for the
notification/inspection path. Existing cursor replay is explicit test-side work,
not automatic notification refresh. No timing-performance gate is introduced.
Compile, targeted Rust and native3OS CI remain unrun. Source-ready is not a
passing result; final #107-main rebase and new exact-source checks are required.

## G latest-main fixture correction and final gate records

At main4035457 integration351e5f4, standard v32 passes Python86/layout95/
watchdog/fmt/strict all-target Clippy and library1359/0/15, then fails integration
957/1/17 in work_overview_follows_the_saved_execution_to_get_and_raw_output.
The diagnostic v33 reproduces Inspect success followed by a raw-read Status:
original shell output is no longer fully retained. The fixed2026-10-25 output
crossed the normal seven-day GC cutoff at the2026-11-01 run. Neither run is
claimed successful; v32/v33 logs and trace are private local evidence.

Test-onlyfbf0d5c replaces that fixture's calendar literals with one captured
current UTC instant and a one-minute-earlier start. Native focused verification
exits0:1 passed/0 failed (v34). GC/retention/watchdog/assertions and product source
are unchanged. No test is ignored, deleted or weakened; temporary trace removed.
The final full-standard and exact-integrated-SHA3OS outcomes, main admission and
transferred residual report are recorded on
[#41](https://github.com/tya5/shellboy/issues/41). Older checkpoint successes below
do not qualify this new merge or the fixture correction.

## G54e377b exact-source CI and final integration boundary

Ordinary native3OS CI41251630243 succeeds at exact source
54e377b62dd234a6935af46e634ed7b381f408b3: Linux2329 passed/0 failed/34 ignored,
macOS2328/0/35 and Windows2191/0/26. Python71 and each platform's configured
layout/watchdog/fmt/Clippy checks pass. gh run watch exits0; all three native
jobs are terminal success, measure is intentionally skipped. Counts subtract
the listed ignored cases from each binary's discovered tests, with exit0 for
every test binary. Private complete CI logs are preserved locally. This proves
native CI, not physical Windows terminal screenshots or paid API behavior.

Main4035457 (#72/#97/#112/#114) merges cleanly atf96f5a2; the previous CI does
not qualify its new code. The final local standard and exact-SHA admission CI
are reported separately on [#41](https://github.com/tya5/shellboy/issues/41).
Reviewer/user authorize main only after those gates. #115 is an explicitly
nonblocking followup for lifetime-growing reads and maintenance; #107–#109
registration remains after G. No new schema/table/worker/queue/dependency,
release, installation or user-data change is claimed.

## Historical G profile-integrated standard6c8eeca and latest-main qualification

Native macOS shared-target `RUST_TEST_THREADS=2 sh scripts/check.sh` v30 exits0:
Python71, layout95/watchdog, formatting, strict all-target Clippy and2326 Rust
passes/0 failures/35 ignored (library1350/15, integration957/17,
termination12/1, crash7/2; main/three examples0). This qualifies source6c8eeca
including #105 line-table dev profile, not just the earlier production profile.
Evidence: private issue-41-g-final-main-e830-standard-v30.log. Subsequentdeeb2b0
only reconciles the ledger. G-only65-commit gitleaks8.30.1 scan reports0 leaks.

New main1ac2277/#106 changed control driver retirement and stop ownership;
merge8f0ad72 is conflict-free. Its separate announced sequential standard v31
exits0: Python71/layout95/watchdog/format/strict all-target Clippy, Rust2328
passed/0 failed/35 ignored (library1352/15, integration957/17, termination12/1,
crash7/2; main/three examples0). Evidence: private
issue-41-g-main106-standard-v31.log. All product/test/build/workflow files remain
identical to8f0ad72; subsequent commits only update non-embedded development and
validation docs. Native run termination is confirmed; no duplicate run is live.
An independent read-only audit found no additional retained-G blocker, with the
same limits on P3 fixture counts/native empty-DB smoke/physical Windows evidence.
Exact-source3OS, G publication/reviewer approval and main admission remain
unperformed; no release/install/user-data authority is inferred.

## G production5ba1cdd and external IPC7984c0c qualification

Native macOS/Rust1.99.1, root shared CARGO_TARGET_DIR, job1/incremental off and
test threads2; announced sequential standard v27 exits0. Python71, layout95/
watchdog, fmt and strict all-target Clippy pass. Rust2326/0/35: library1350/15
ignored, integration957/17, termination12/1, crash7/2; main/three examples0.
Private issue-41-g-combined-standard-v27.log and the17-file hash witness bind
this result to the unchanged source before source commits339223e/5244ebe/
e7e4602/5ba1cdd. The v26 attempt failed only new-test macro resolution; no
assertion or count gate was weakened to obtain v27 success.

The test-only7984c0c helper calls real public ipc::call against the fixture owner,
forwards exact origin/operation ID and asserts returned identity. Eleven existing
mutations cross transport, including retained HistoryGet/List refresh, lifecycle,
index/probe and attachment/receipt cases. Focused frontend::inspection_tests::
passes14/0 and strict all-target Clippy exits0 (v28 logs). This is separate-client
IPC evidence, not subprocess CLI or real provider/GC evidence. Synthetic initial
source/path mutations remain distinguished from the subsequent real IPC commit.

P3's production read_current_and_views_tx is used by refresh::run and the native
coupled test. One readonly WAL connection/transaction/sequence carries current,
browser, descendants and same-Thread inspector. The fixture's standalone Thread
inspector uses7 collection statements; sharing the header removes exactly1.
Combined collection statements <=64 for this fixed fixture (not a universal
budget for every source category); readonly setup/transaction overhead is
separate. Cancelled browser/count/inspector add0 SQL to current-only collection.
Existing view_read cancellation/admission and inspection_state quiet/hidden/
coalescing/late-result tests retain their own source coverage. No extra timing
study or universal own-cardinality constant-VM requirement follows.

Header tests enforce SQL-before-copy caps and canonical parity, preserve exact
indexed counts and0 SQL for invalid input. Run/context tests record scoped own
row growth and compare optional tied-Turn index extension in disposable DBs.
The current indexed-scope gate does not require that optional migration; no
schema change is adopted by G. Existing explicit row/copy/channel caps remain.

## G native popup wheel smoke7984c0c

Private driver v29 uses public completed-frame VT/PTY helper and the shared
normal development binary. Native80x24 and140x40 each pass Help down/up, F8
catalogue selection down/up, unsubmitted HistorySearch form down/up and submitted
embedded docs-list structured metadata down/up. No search/model/shell/message
was submitted. Original draft marker survives, synthetic node count remains0,
no OSC52 is emitted. Captures retain raw/text/cells, helper/driver/source hashes
and binary SHA256
`9fef9ea6f1649abdbf508b1d391f7eec0455b05258d287262b79a1ca327b0fe2`,
verified unchanged. These are native Unix frames, not physical screenshots or
Windows qualification. Nonempty-history offset/edge/outside/F2/noop ownership
remains covered by the existing unit matrix; do not infer it from an empty DB.

The failed v28 capture stopped at a driver's draft-ready predicate requiring
Ctrl+T even after the product correctly replaced empty-composer key guidance
with send/newline hints. Raw failed frames/manifests remain. The v29 predicate
uses the preserved draft and empty-history body; actual wheel movement/counter
assertions are unchanged. No product behavior was modified for this harness.
Existing cb1d480 visual/effect/Enter form matrix and a8bbcf0 P6 evidence remain
separately source-qualified; current original reader/cache/dispatch code is
unchanged. Merge7071932 adds #105 build debug profile only; integrated standard6c8eeca
now passes as recorded above. Later #106 source8f0ad72 also passes its separate v31 standard as above;
exact-source3OS/main review remain pending.


## G native P6 checkpointa8bbcf0: count gates pass

Native macOS/Rust1.99.1, shared debug target only. Sourcea8bbcf0 (Rust source
78abbf8) plus private atomic instrumentation builds and runs four sequential
Unix PTY cases. Each case opens a saved64KiB original, warms the reader, sends
600 keyboard/wheel events at60Hz for10s, then exits cleanly. Synthetic loopback
Responses only: exactly one request per case; no user data/provider/clipboard.

| Input | Columns | Accepted events | Draws | Rebuilds during window | UI+draw p95 observation (ms) |
| --- | --- | --- | --- | --- | --- |
| Keyboard |80|600|238|0|8.599333|
| Keyboard |140|600|228|0|8.777375|
| Wheel |80|600|241|0|6.131876|
| Wheel |140|600|219|0|12.485084|

All cases retain65536 source bytes; observed reader peaks are1493 rows and446375
charged bytes, below65536 rows/8MiB. Actual frame counts fit both configured30fps
total allowance and changes plus elapsed seconds. Sample arrays are complete;
readable source changes after the first second; the binary is unchanged during
each capture. No strict1s-bin, latency, variance or CPU threshold is used.
The full binary SHA256 is
`aaf6271f6db823927ebb31d89908b0fa5ff7f47b36bd044c4c27057a436cac99`.
Private p6-a8bbcf0-{keyboard,wheel}-{80,140} results retain terminal bytes, source
manifest, fixture/script/binary hashes and raw metrics. Private instrumentation
patch/originals/manifests are preserved; restoration verifies all touched files
and leaves the source tree clean. Shared debug executable remains the private
measurement build: rebuild ordinary source before its next ordinary use.

Original baseline1fec7f0 keyboard p95 values are36.836459ms (80) and39.427083ms
(140); current deltas are-28.237126ms and-30.649708ms. Comparisons require the same
fixture/input duration/event count. Durations are observations only. Baseline
wheel support is unavailable; zero old wheel frames are not a speedup claim.
Original scripts/results, including the historical rejected-config preflight,
are unchanged. The candidate's first preflight used system Python without pyte
and stopped before product launch; its failure log remains. Existing terminal
review venv then runs the successful four cases; no dependency was installed.

This is a P6 source-qualified count result, not the remaining all-popup wheel
matrix, P3 shared-consumer proof, physical terminal/Windows success or full G
admission. All G5 saved-route and G9 query/source obligations remain in the ledger.
No product instrumentation, push, installed binary or user data change.


## G integrated native standard78abbf8

On native macOS/Rust1.99.1, `RUST_TEST_THREADS=2 sh scripts/check.sh` exits0
using the existing shared root target (job1, incremental disabled). Python71,
layout95/watchdog, format and strict all-target Clippy pass. Rust2308 passed,
0 failed,35 ignored: library1332/15 ignored; integration957/17 ignored;
termination12/1 ignored; crash7/2 ignored; main and three examples0. Private
issue-41-g-mainca317-standard-v21.log records the actual run. Check source78abbf8
includes mainca317a8 and preserves #88/#97;21342a9 changed documentation only.

This closes that integration check, not G as a whole. Native P6/wheel/P3,
remaining reactive route/source/SQL inventory and exact-SHA three-OS/review/main
admission remain open. The candidate private P6 driver now has count-only gates
and shared-target/fail-closed restoration guards. Synthetic policy checks reject
wrong event/source/frame/sample/cache counts and do not reject arbitrarily long
durations; they are harness checks, not native product performance evidence.
Historical baseline scripts/results are unmodified; unsupported baseline wheels
remain unavailable, not a zero-cost baseline. No paid API/user data/install/push.


## G latest-compaction resource envelope checkpoint18b2b5f

Native macOS/Rust1.99.1; one shared root CARGO_TARGET_DIR, incremental disabled,
job1/test threads2; announced in #41. Sequential `cargo test --locked --lib
readback::` passes15/0; `cargo test --locked --lib
frontend::inspection_tests::` passes12/0; `cargo clippy --locked --all-targets
-- -D warnings` exits0. Integration layout95 and watchdog policy pass. Private
logs use issue-41-g-compact-bounds-{,runtime-,clippy-}v20 names. Source18b2b5f
implements design7e87880; earlier standards do not qualify this source.

Five new domain tests prove canonical parity, forced owned interruption, oversized
source/scalar handling, aggregate copy exhaustion and propagation of unrelated
SQL errors. The first four also preserve canonical source/results; guarded
transaction cases prove the original length limit, a large subsequent scalar,
a recursive query exceeding the previous hook budget and an open shared
transaction after the scoped read. The actual frontend route retains foreign
Thread identity/body availability under a held writer; after release, actual
ThreadSet updates its title despite oversized compaction metadata and exposes
`last_compaction` as limited. Full CLI restore readback remains available.

The prior v19 failure was a fixture UPDATE without a key predicate, which collided
with other app_meta primary keys; v20 qualifies the corrected source without
weakening assertions. No paid API, user DB, installed binary or host clipboard
was touched. The2500 progress-callback allowance uses SQLite's approximate100
instruction cadence, not an exact VM/time bound. Broader indexed SQL/query
plans, all remaining reactive routes, P3/P6, native wheel, latest-main integration,
standard/three-OS/admission checks remain open.


## G Thread compaction raw/preparation checkpoint35f45ce

Native macOS/Rust1.99.1, existing shared root target/job, incremental disabled/two
test threads, build notice in #41: `cargo test --locked --lib readback::`
passes10/0; actual `frontend::inspection_tests::` passes12/0; strict all-target
Clippy succeeds. Source35f45ce implements adopted design5c8dd98. This is focused
qualification, not a current full-standard/three-OS/native-matrix/P3/P6 result.

The existing canonical mixed receipt/automatic/replacement fixture keeps its
manual event/metrics/order/period assertions; full and filtered raw facts then
prepare after the DB is closed and match those results. Nine spill-count types
exercise the existing integer semantics (including SQLite's boolean projection,
float/string/negative/null/overflow/object cases). Actual statement counts are4
without spill coverage and5 with it; no per-row decoder query is added. The real
foreign-Thread F8 fixture receives a nonempty restore event while another
connection holds BEGIN IMMEDIATE, with exact event identity and retained latest
answer availability/limit facts. No provider or user-data changes.

The raw reader still uses the canonical source queries and candidate caps;
these do not prove finite source-byte copying or bounded total SQL work. G9
source/query/plan qualification remains open. No row/time gate was weakened,
and no successful stub, worker, queue, table, dependency or public API was added.

## G latest Thread messages: bounded raw/preparation checkpointc3608f0

Native macOS/Rust1.99.1, one shared root target/job, incremental disabled/two
test threads, announced in #41: `cargo test --locked --lib thread_observation::latest::`
passes8/0; `cargo test --locked --lib frontend::inspection_tests::` passes12/0;
strict `cargo clippy --locked --all-targets -- -D warnings` succeeds. The
domain refactor and display integration are separate commits6280bca/c3608f0.
No full-standard/current-source3OS/native-form/P3/P6 claim follows these checks.

Assertions retain canonical natural text, latest ordering, sender whitelist and
512-character excerpts; raw facts prepare after closing the DB. Actual statement
counts are4 for complete input+answer,3 when a source exceeds the shared64KiB
allowance,1 for a rejected saved timestamp and0 for invalid display scope.
An opaque2MiB answer never enters its excerpt query; its body remains available,
display excerpt is unknown and the canonical CLI still returns its retained
text. Individually legal30KiB/20KiB/30KiB sources exercise the aggregate budget;
an oversized sender does not consume the remaining budget or hide a legal input
excerpt. A1MiB timestamp rejects display copying without changing the CLI value.
Existing bundled-SQLite plan assertions still require exact Thread/node index
seeks without history scan/temp sort.

The actual UI/form/frontend fixture opens a foreign Thread while a separate
connection holds BEGIN IMMEDIATE. It receives the exact answer ID, true saved
availability and limited-field witness, without changing the viewing Thread;
after releasing the writer the canonical excerpt is still intact. This proves
the readonly display route and retained source, not physical-terminal/Windows
performance. Last-compaction/header work and the remaining G5/G9/P3/P6 gates
remain open; there are no provider calls or user-data changes.

## G plus main805efc9: full local standard at Rust source51772d2

Native macOS/Rust1.99.1, `sh scripts/check.sh`:2276 passed/0 failed/35 ignored.
Named harnesses: library1304/0/15; integration953/0/17; termination_signals12/0/1;
tool_group_crash_recovery7/0/2; main and three example harnesses0 tests. Python70,
layout95, watchdog policy, fmt and strict all-target Clippy pass. One shared root
target, incremental disabled, one build job/two test threads; build notices in
#41. Private named-harness log/audit retain this run separately from earlier
failures. Later design-only0f771b3 does not change this Rust source; no current
source3OS/native-matrix/P3/P6 or G admission is inferred. No paid API/user data.

The merge initially failed strict Clippy because preserving #102's wait facts
enlarged a private enum. A boxed Lifecycle payload fixes that without a lint
exception. New passing regressions cover the canonical saved Run wait reason,
its removal and exact4/5-query bound, plus the actual painted code chooser's
wheel selection, outside/edge no-ops and exact original/block/hash copy binding
at80/140 columns with TestBackend. These do not establish physical-terminal or
Windows UI success.

After standard success, an actual macOS process/Unix PTY using only disposable
synthetic data reproduced an additional semantic #87 overlap: F8 -> RunGet ->
F1 opened general help, not the form's explanation. Captured binary SHA256
dd10352b1ac0f01dcac65b0cf7fd4f367c241405a84953a063f940f145b86233;
private screen retained. Form-help and cleared-target follow-ups are qualified
separately below, not covered by that standard.
Focused source54dac64 now passes UI449/0/3, actual frontend inspection11/0
and strict all-target Clippy. The first focused run passed UI448/0/3 but failed
one of11 runtime fixtures: it observed the intentional pending-operation frame
before the final ThreadGet result. The corrected fixture waits for that exact
result title and still asserts canonical missing-target failure, no saved
inspection and unchanged viewing Thread. The corrected F1 dispatch tests also
keep pending client confirmation/general-help layering and refuse execution.
The fresh native binary18f9a81318cf5d2a0a359b9d2276f43a183b8a2e20ab3486d028b0f85c513ef1
passes the original form/help matrix8 cases x4 forms and canonical-copy8 cases
x4 exact intercepted OSC52 payloads. Each matrix covers80/140 columns and
dark/light/NO_COLOR/vt100, with disposable fixtures and no paid API/host
clipboard. Private manifests, text/raw frames and failures are retained.

Manual inspection of the captured80-column Run help reveals a further issue:
the common bottom hint still says Enter/send on a read form and advertises
field editing while help is open. This was outside the initial driver's
predicates; do not call that initial matrix complete visual acceptance.
Follow-upcb1d480 reuses the catalogue EnterVerb in the hint/cache identity,
classifies form explanations as Help and separates F1 ownership from hint
classification. New regression coverage includes custom composer bindings and
same-verb cache reuse; the strengthened native driver asserts bottom-row verb
and help semantics. Atcb1d480, focused UI450/0/3, strict all-target Clippy,
Python70 and current layout95/watchdog policy pass. A fresh normal build has
SHA256cd7e087f85a4607fdff1e7b17160c00720f15d92cd0d5a86470a347511d9faa8;
the strengthened eight-case32-form native matrix passes. Captured80-dark Run
form/help and140-light execute/send forms were inspected: their bottom hints
respect the catalogue verb and help navigation. This qualifies actual Unix
process/PTY frames, not physical screenshots/Windows or the remaining G native
wheel/P3/P6 inventory. The earlier canonical-copy matrix remains scoped to its
actual54dac64 source; it is not silently attributed to later code. No complete
standard or three-OS success is claimed forcb1d480.

## G saved-inspection composition: native library checkpoint

Native macOS, Rust1.99.1, working source based on1479a59 with the G5/G6
composition: `cargo test --locked --lib` passes1282/0/15 and
`cargo clippy --locked --all-targets -- -D warnings` succeeds. One root
CARGO_TARGET_DIR, incremental disabled, one Cargo build job and two test
threads were used. The build was announced in #41. The composition is now
committed at5f17374. The first full `sh scripts/check.sh` passes Python70,
layout95, watchdog/fmt/strict all-target Clippy and library1282/0/15, then
fails integration951/1/17. The single failure is the existing structured-history
end-to-end fixture waiting for a generic Status on an expired detail request;
the implementation now emits the correctly correlated ManagementDetailFailure.
Its corrected assertion keeps the exact result ID, detail request ID and pointer
and rejects replacement bytes masquerading as the old original. Native exact
rerun passes1/0 (968 filtered out), retaining all original/cursor/hash/wait
assertions. Later process/example harnesses were not reached;
no complete-standard success is claimed. No paid API or user database was used.

The actual frontend tests cover foreign Run stop without viewer retargeting,
usage periods/scope, pending receipt visibility before held external work,
live deletion blockers, retained index coverage windows, attachment removal and
managed-file collection, plus operation/downstream cancellation. Unit evidence
also covers late field pages, hide through saturated input, superseded show
rejection before target installation, shared initial metadata/usage admission,
and index commit/rollback/no-op/replay/partial-startup notifications.

Earlier broad runtime runs exposed real missing notifications, corrected through
existing commit paths. Their remaining fixture corrections preserve the current
contract: real owner-instance pins survive recovery; managed-file unavailability
takes precedence over selection exclusion; usage form navigation includes the
since/until fields. A failed new index window retains the prior counts and
observation identity under last_observation, without a stale actionable cursor.
The full library result includes all these regressions.

This is the original twelve-target composition, not complete G5. Thread's
additional canonical last-input/answer/compaction reads still need bounded
source/scalar and off-transaction preparation work. Status/list/history/readback
coverage, G9 query costs, native UI/P3/P6, latest-main reconciliation, three-OS
CI and complete G admission remain unverified.

## Exact-SHA CI reuse (#92, reuse-only qualification pending)

On 2026-10-31 the reviewer approved reuse and explicitly dropped Windows
partitioning: two-job wall times7.3/8.5min (other samples7.8/8.1min) did not
materially improve the earlier8.9min unsplit job, because each job repeated
all-target build/link work. The final tree preserves the main baseline's full
single-job Windows execution, timing runner, layout checks and Python tests;
there is no partition manifest, selector, batching helper or shard audit.

Retain experimental provenance, not as proof for the final source:
- Measurement efc10e5/run41182264568 passed; library103.812s and integration146.219s.
- Candidate39577ff/run41184106926 failed Python Unicode subprocess sibling imports.
- Candidate7f339a7/run41184263185 rejected unassigned visible support modules.
- Candidatefa1e26b/run41184815121 rejected ambiguous libtest substring skips.
- Candidate1fa0e95/run41185378870 passed three OSes and the former inventory audit
  (1846 Windows identities,24 ignored); execution128.032/182.828s, independent
  all-target builds173.578/182.484s. This was not the final reuse-only layout.
- Rebalanced4355953/run41186034007 passed Linux/macOS/Windows1 but failed Windows2:
  frontend_reactivity::selected_header_and_open_browser_share_one_changed_snapshot_without_reload
  observed read sequences44/43; response_retries::stalled_permanent_error_body_is_not_retried
  observed request counts3/1. Per reviewer, #95 owns their separate diagnosis;
  no assertion was weakened, excluded or retried into qualification.

The reuse gate's Node fixtures cover exact-SHA success, wrong SHA/repository,
reruns, skipped/incomplete/Windows-only native jobs and API failures. Require
new first-attempt native three-OS checks in the original unsplit configuration
before main integration. Actual main-push reuse and the no-prior-success main
push fallback remain untested live until authorized integration; mocked gate
cases do not establish those paths. No local Rust build/test was performed on
the8GB integration host. Final exact-source results will also be linked in #92.

## #41 D2 owned work and output (local)

Final source807ac97 passes native macOS arm64/Rust1.99.1 standard checks:
**1,777 passed/0 failed/32 ignored**, layout90/Python6/fmt/strict Clippy.
The [qualification](conversation-redesign-live-work.md) separates actual pipe/DB
commit ordering, same-snapshot timing, typed ownership/selection, finite decoder
and outbox tests from native six-condition PTY shell interactions. NO_COLOR
chromatic output is absent in both widths. Synthetic fixtures and a loopback
Responses mock avoid the default store, credentials and paid model APIs.

D's P2/P5 and full multi-consumer P3 remain open, along with E/F/G and applicable
native OS/GUI gates. Short PTY byte counts are not draw/DB-job/CPU counters and
old B+C CI does not qualify this source. No push/release/install was performed.

## #41 D1 SQL batches and raw headers (local)

Source558b870 integrates current-main1b559b9 and passes native macOS arm64/
Rust1.99.1 standard checks: **1,745/0/32**, layout90/Python6/fmt/strict Clippy.
Actual SQLite traces, independent old-query oracles and10,000-unrelated-record
scaling regressions verify bounded tool/sender/recovery reads and PK/source-index
shape; raw-header/title/draft preparation is off-DB. No schema/migration/authority
change is introduced. The [detailed record](conversation-redesign-shared-read.md#d1-actual-sql-batches-and-bounded-raw-headers)
retains before/intermediate/final evidence, the rejected missing-index assumption
and remaining limits. P3 selected workload SQL176→26, worker p50 4.813→3.334ms;
VM16,602→22,632 remains higher, so no universal CPU claim. D/E/F/G, global planner,
performance and native Windows/Linux/visual gates remain incomplete. No paid API,
real store, installed binary, push or release was involved.

## #41 D0 shared-read and E tree-cache foundation (local)

Source67f6f09 integrates mainbb7e29b and qualifies locally with
`sh scripts/check.sh`: **1,711 passed /0 failed /32 ignored**, layout90,
Python6, formatting and strict all-target Clippy on native macOS arm64/Rust1.99.1.
The [detailed record](conversation-redesign-shared-read.md) retains the observed
initial failures and distinguishes shared current-view/paused facts, atomic
application/diagnostics, tree caching and viewport corrections from unfinished
live-work, list patches/Watch and general popup work. Actual selected-view worker measurement is7→1 jobs, with worker p95
3.675→3.940ms (no SQL-time speedup); full multi-consumer P3 and native
Windows/Linux qualification remain pending. Follow-up161b4c9 additionally fixes
external deletion notifications through existing postcommit effects. Its standard
check passes **1,713/0/32** (library828, integration875, termination7, crash3),
including exact singleton/cascade targets and effect-free replay/rejection/rollback.
Browser patch consumers are still pending. No paid API, real user data, installed
executable or release is involved.

## #59 sender inspection and labels

2026-10-30, native macOS arm64/Rust1.99.1. `sh scripts/check.sh` succeeds:
**1,666 passed / 0 failed / 32 ignored**, layout88, Python6, format and strict
all-target Clippy. Tests use temporary databases/directories, loopback-only
configuration and synthetic identities; no installed executable, real user store
or paid API is used. The build cache is shared with the coder's earlier local
validation target; source/worktree/branch are isolated for #59.

Verified regression coverage:

- Store receipt tests compare `message get` and `history get` provenance for
  external labeled/unlabeled input and model input appended to an existing Run.
  Top-level sender IDs remain after the source Thread is marked deleted.
- Trusted internal submission records `human`; selected-path `history list`
  exposes it. Saved null becomes unknown in sender facts. Complete historical
  OriginIdentity can be inspected without credentials; partial/null evidence is
  never upgraded into a fabricated identity.
- Owner-issued Origin passes Control validation; a forged token does not.
  A model labeling itself human cannot resume recovery-wait work. Own-Thread
  cancellation still works with additive kind/label; a different model Thread
  remains forbidden. Existing ownership/stop/retry tests pass unchanged.
- 64 multibyte Unicode characters are accepted; 65, C0/C1 controls, newlines,
  and Unicode line/paragraph separators are rejected without echoing input.
  Real CLI cross-data-dir IPC strips Origin but keeps the unverified name and
  records `external_cli`; no credential appears in sender output or receipts.
- F6 TestBackend checks separate kind/name, exact `名乗り：…`, stale-reply
  rejection and unchanged conversation presentation fields. A narrow existing
  metadata-classifier regression preserves recorded-human categorization and
  proves an external label 'human' does not change that category.

Development checks initially caught two stale protocol/persistence expectations
(the new required sender output and additive model kind) and a TestBackend
assertion that did not account for wide-glyph continuation cells. Those fixtures
now assert the adopted contract without dropping credential/context checks.
The existing real-TUI PTY integration test passes unchanged. Its earlier #62
intermittent repaint failure remains unresolved; this work does not claim a fix.

Untested separately: native Windows/Linux execution pending ordinary main CI;
physical-terminal F6 sender interaction, real providers, and a real cross-owner
team session. TestBackend/mock/synthetic results do not establish those results.
No conversation renderer/layout or index-refresh work is qualified by #59.

## #41 structural unit and B+C freshness follow-up

Initial structural/interaction main b0e3c29 passes native three-OS CI41057195099
(Linux1,659/0/31, macOS1,658/0/32, Windows1,543/0/24).
Follow-up sourcefd968e5 includes main ed40f33/#59 and addresses review6523314574.
It qualifies locally on macOS arm64/Rust1.99.1: **1,679 passed / zero failed / 32 ignored**,
layout89/Python6/fmt/strict Clippy. Four genuine separate-CLI tests cover header,
foreign input/cancel/recovery/stop and real raw/history GC while F6/F7 stay open;
local target1s passes. Snapshot read/freeze times, real reread with paid-copy
preservation, source membership, late replies, selection and bounds are covered.

The [detailed record](conversation-redesign-bc.md) separates exact source,
measurement starts/values, the final binary's six real PTYs, P1 p95 3.974ms,
initial P3/P7 evidence and untested physical/real-API cases. Main17c4271 passes
CI41068514157: Linux1,680/0/31, macOS1,679/0/32, Windows1,563/0/24
(passed/failed/ignored; Windows threads4); downloaded artifacts name that exact
SHA. This closes the B+C freshness gate. D–G and shared one-job consolidation remain open. No installed binary,
release/tag, paid API or user data is changed by this #41 increment.

## v0.3.0 preparation (#62)

Single-conversation preparation, macOS arm64/Rust1.99.1, initially based on
96bcd16. Local standard: Rust7groups1611pass/0fail/31ignore, layout88/Python6,
fmt/strictClippy, exit0; version0.3.0. Latest-main rebase/standard and exact-SHA
native CI precede candidate artifacts. Tag/publication/installed update are not
part of this permission. Package Cargo.lock changes only its own version.

### Public v0.2.0 upgrade fixture

Anonymous curl (--disable, no authorization header) downloaded the public
v0.2.0 macOS archive and SHA256SUMS from GitHub Releases. Archive SHA256:
`4f9d08cd9d327296177a4553ba45cff56c01edce708039418ed0750099010666` matches
the downloaded checksum. Executable reports0.2.0 and creates conversation DB11.
The verification uses no user DB, real conversation, credentials or paid API.

A private bounded Python harness owns real120x40 macOS PTYs and a local
Responses/embedding server. It clears inherited SHELLBOY_* context and supplies
explicit temporary config/data paths. The old binary itself creates synthetic
conversations, indexes and queued machine input to a recovery-wait Thread.
Before stopping:21 saved nodes,8 observed indexed chunks,1 held input and1
recovery-wait Turn. A registered synthetic text folder is keyword-indexed into
standalone RAG3. The initial manual-ingest attempt used an unregistered source
scope; the corrected fixture uses public source add/index commands.

To preserve committed recovery/pending facts, hard-stop the old owner after
fixture adoption rather than let normal shutdown terminalize them. No OS side
effect is left active. After exit, use read-only SQLite backup to include committed
WAL in a separate upgrade copy; explicitly close/finalize backup connections and
use DELETE journaling on that copy before the new owner establishes WAL. An early
read-only backup probe lacked finalized WAL side files; this was a harness issue,
not a failed product migration. Original fixture stores are retained separately.
This is restart/crash-recovery qualification, not a clean old-TUI shutdown test.
No attachments/managed-file migration or real provider behavior is covered.

With old compact_hint_tokens still present,0.3.0 PTY startup reports that key
and schema remains11. Remove the key: owner starts, DB becomes14,
integrity_check=ok/foreign_key_check empty. Retained conversation semantic search
and scoped history keyword search pass. Held input remains held. usage get counts
old usage11 input tokens, then2 completed requests after a new mock send. Independent
RAG keyword search passes and its schema remains3. New owner shuts down normally
with exit0; old0.2.0 read-only history retrieval refuses upgraded schema14.
All five Responses and twelve embedding calls hit the synthetic loopback only.

### Native archives and publication boundary

The release workflow candidate_ref accepts an exact main SHA, with release_tag
only its version label. Candidate build/assembly have read-only repository
permission; publish is conditional on absence of candidate_ref. Static YAML,
embedded Python syntax and guard checks pass; actual skipped-job/native results
are confirmed by the preparation run. Python3.12 is selected for safe tar extraction.
Each runner extracts its own archive and invokes --version/--help/docs list from
that packaged binary, checking files; assembly checks exactly3 archives and writes
SHA256SUMS. These are native CLI smokes, not physical-terminal/UI qualification
on every OS. Ordinary main three-OS tests are a separate exact-SHA gate.
Final SHA/run links/archive hashes belong to issue62 and the final report; a
rebuild changing bytes needs requalification. No v0.3.0 tag/Release is created.

## #61 documentation qualification

Single-conversation docs-only worktree, base16c8934, macOS arm64/Rust1.99.1.
No child delegation, code/test/workflow/schema/config/dependency or installed
binary change. Source checks use cli.rs MessageSendOptions/MessageCancelOptions,
Control pending-message cancellation/foreground-release rules, shell spawning
and inherited environment, Origin boundaries, existing TUI confirmation paths
and raw retention/startup recovery contracts.

- Standard `sh scripts/check.sh`: exit0; Rust7groups1611pass/0fail/31ignore,
  layout88/Python6/fmt/strictClippy. Later wording was checked with a rebuilt
  candidate; latest-main rebase and full standard are repeated before push.
- Candidate debug binary `docs get --limit-bytes 65536` reconstructs User Guide,
  Agent Guide, CLI reference and TUI guide exactly with matching source SHA256,
  no continuation. This uses the bundled owner-independent docs, not installed
  old-guide output or a live owner modification. EN/JA usage sections each have
  exactly three user bullets; both safety sections have the same five topics.
- Candidate message send/cancel and usage get --help confirm the flags/wording.
  No real send, cancellation, paid API or user-store mutation is used for help.
- EN/JA self-review distinguishes shell approval from existing TUI confirmations
  and CLI state/ownership checks. cwd/worktrees are not sandboxes; stopping is
  not rollback, output retention is not permanent, and restart does not revive
  terminal Runs. Origins are not an OS hostile-model boundary. No future #59
  sender-interface feature is claimed as already implemented.
- WezTerm Shift+click is a macOS user report in issue61, not new developer or
  other-terminal validation. Three-OS sandbox recipes/profile/proxy examples
  are deferred: no current macOS/Linux/Windows sandbox execution or boundary
  qualification is claimed. Seatbelt/bubblewrap/Landlock and Windows dedicated
  user/NTFS/Firewall are named only, with owner-mediated read caveats.

Final exact-SHA main CI is reported separately after push. No issue close,
release, install or #59 work is part of this change.

## 2026-10-30: #58 diagnostic transition filtering

On main c6397b1 plus this isolated correction, native macOS arm64 with pinned
Rust1.99.1, build jobs2, incremental0 and test threads2 passes
`sh scripts/check.sh`: **1,610 passed / 0 failed / 31 ignored**, layout88,
Python6, formatting and strict all-target Clippy. The focused
`cargo test --locked --lib ui_history_tests` also passes (2 tests).

A real UiState feeds 100 ordinary history and 100 ordinary window updates,
then growth; none emits a record. Shrink, stale rejection, user follow change
and body eviction produce exactly four records via the real diagnostic writer.
A held admission mutex confirms silent updates neither contend nor increment
the dropped-record count. Diagnostic filtering occurs before queue admission;
no new worker, state, schema, dependency or UI I/O is introduced.

Main CI41041115046 at exact source75152eb passes all standard jobs on native
Windows, Linux and macOS. Issue58 is closed with the result linked. Earlier actual PTY evidence
belongs to the original retention source and was not repeated for this
logging-only change. No paid API, user store or installed binary was touched.

## #50 review items 1-4

Single-conversation follow-up on c6397b1; macOS arm64, Rust1.99.1, no child
conversations or production data. `sh scripts/check.sh`: exit0, Rust7groups /
1,610 passed / 0 failed / 31 ignored, layout88, Python6, fmt/strict Clippy.
Focused retries23pass1existingignore and runtime_features23pass.

The new only-protected-input fixture proves recovery with one saved request,
zero automatic resends, no queued next request and the factual unavailable flag.
The fallback fixture keeps only the request and latest complete tool pair: the
first no-target maximum edit goes directly to a summary request, not an unchanged
conversation resend. The stage-exhaustion fixture retains retry limit8 but stops
with stages spent rather than consuming the remaining6 slots. Existing transient
no-candidate/budget, pair integrity, stop and duplicate-tool protection stay tested.
An initial fallback test incorrectly assumed a maximum edit had no target; the
fixture now explicitly selects the protected path rather than depending on size.

Sample config comments and the Agent Guide no longer describe removed settings/
estimates. Manual maximum's operation poll has a 60-second watchdog, not an
unbounded yield loop. Native Windows pre-push and exact main CI are confirmed
separately in the completion message. Real providers/physical terminals remain
unverified. No release or installed-binary update is included.

Release-note follow-up (#62, recorded only): remove compact_hint_tokens before
upgrading, and explain the change from the old partial-upload two-strike stop to
the normal finite retry budget. This review does not implement that release task.

## Reactive History Retention (#58)

Runtime/test source747807450f09cf0f9935d5a8e04327256b7f5d94, including
post-tool-result notification80de5f0, passes `CARGO_BUILD_JOBS=2
CARGO_INCREMENTAL=0 RUST_TEST_THREADS=2 sh scripts/check.sh` on native macOS
arm64/pinned Rust1.99.1: **1,614 passed / 0 failed / 31 ignored**, layout88,
Python6, formatting and strict all-target Clippy. Gitleaks8.30.1 finds no leaks.
Subsequent changes are nonembedded development/validation records only.

Regression coverage includes overlapping follow/non-tail windows, delayed
canonical body installation, active and paused committed previews, same-source
stamps independent of queue age, stale rebase retries retaining wheel movement,
visible mixed-kind protection beyond3nodes and offscreen hydration, disjoint
protected edges, offscreen committed-preview reclamation, same-predecessor audit
retention until fresh membership, and postcommit-only tool-result notifications.
Rollback emits no such notification. Diagnostics use fixed scalar-only records
and a nonblocking queue; tests verify serialization and contention behavior.

A real80-column PTY candidate exposed a missing notification: its completed
result stayed absent35seconds while the next HTTP response had not sent a byte.
The shell collector Changed preceded the result commit, and no notification
followed that commit. Commit80de5f0 adds the existing coalesced postcommit edge;
no poller, worker or new business event is added. Earlier candidate failures are
not overwritten by later successful source checks.

Final debug binary SHA256
`39738baeca2572b0d1249941dff949c5c15d79329aff60f94a2fce6a9558c871`
passes actual native PTYs at80x40 and140x40, driven by a private local harness
against temporary data and a loopback Responses server. Each case uses a250k-char
prior Turn, three fixed synthetic shells producing about120KiB each, a20KiB
shared body budget, and five mock API requests with279–333KiB request JSON.
At each of three next-response gaps, the server withholds all response bytes;
already-painted current messages and exited tool results persist through four
250ms steady samples. Full reconstructed cells and raw ANSI are retained outside
Git. PageUp keeps the exact prior source line across the next request, new-output
indication appears without following, and Ctrl+End restores the latest final
message/results. Both owners exit0 with no recorded fixture errors.

This is native macOS PTY/cell evidence, not a WezTerm pixel screenshot or physical
Windows terminal qualification. No external/paid API, user DB/settings or installed
binary changes are included. Exact main e8c1ff22162cd11c54ed89aa34463ab3b13c57d4 passes native three-OS
CI41034892332: Linux1,615/0/30, macOS1,614/0/31, Windows1,499/0/23 (threads4).
All artifact source SHAs match. Issue58 is closed with final run/results. These
results do not validate later #41 implementation.

## #50 solo first version

Environment: macOS arm64 (M1), Rust 1.99.1, bundled SQLite, dedicated worktree
based on 1fec7f0. Design 01bbd3d precedes implementation 37a43a9. No child
conversation, real provider or installed-owner/data modification.

- `sh scripts/check.sh`: exit 0, 7 Rust groups / 1,588 passed / 0 failed /
  31 ignored; layout 88; Python 6; fmt and strict all-target Clippy pass.
  Background shell completion observed with shellboy wait, elapsed 79,219 ms.
- Focused model retries: 22 passed / 0 failed / 1 existing ignored. This covers
  first failure unchanged, second eligible failure reduction even on small input,
  all 5xx, connection/EOF/header timeout, retry exhaustion after eight resends,
  zero/configured retry limits, opt-out/rate limit, explicit context reduction,
  same-Turn re-growth/reduction, stopped waits, partial-output safety, manual
  maximum with no model call and model change on explicit retry.
- Library: 717 passed / 0 failed / 12 existing ignored. Preserves restoration,
  instruction snapshots, scope/generation/cancellation fences, original nodes and
  immutable side effects. New marker-vs-user-request protection regression and
  existing nested summary boundary tests pass without changing summary policy.
- Runtime fixtures retain explicit-overflow stage/pair integrity and three saved
  tool executions, with a configured two-resend budget. Native upload fixtures
  now expect three requests under that same budget (two unchanged attempts,
  then reduced retry if enabled), instead of the removed two-strike rule.
- Earlier local runs exposed removed-policy expectations, missing CLI completion
  entry, a now-obsolete stopping-metrics expectation, test indexes/pointers,
  strict Clippy findings, and the repeated-marker fence defect. They were fixed;
  no test skip, retry-limit inflation or timeout extension hides failures.
- Pre-push manual Windows CI will cover changed native upload/tool fixtures;
  actual provider context-code encodings, physical terminals and model behavior
  are unverified. Final exact-SHA main CI is reported separately, not inferred
  from this local run. No OS process/encoding implementation changed.

Presentation is deliberately limited to existing recovery/F8 surfaces. Manual
actions remain in existing receipts/history rather than a new action ledger;
there is no ranked recovery dashboard, pre-POST override or new lifecycle state.

## Review Correction Qualification (#52)

Review6517077748 is addressed by582ec70 (unfinished interrupted group uses
Idle/nudge-or-stop) andbeace3d (native observation watchdogs/notifications and
guide typo). The added regression preserves unresolved pairs in running and
recovery_wait, creates no further model attempt, and retains full Run stop.
Submitted snapshot retention remains deferred with recorded replay/watermark
constraints; no live startup repair or new product machinery is introduced.

The combined source including the subsequent large-upload fixture diagnostic
correction passes the complete macOS arm64/Rust1.99.1 standard check:
**1,572 passed / 0 failed / 29 ignored**, layout88, Python6, formatting and
strict all-target Clippy. The final correction is committed as6e62349204254e534acf758e520c870cc20bb801.
Its manual native Windows qualification
[41019801385](https://github.com/tya5/shellboy/actions/runs/41019801385) passes:
**1,457 passed / 0 failed / 21 ignored**, threads4. Artifact source SHA matches;
the new busy-loop regression is active and passes, and both previously failing
fixture cases pass. Ignored sets remain unchanged. Only development/validation
records change after that qualified source; runtime/tests/embedded guides and
workflow remain identical for main publication. Main CI is still a separate
exact-source gate, with final result linked from issue52.
The earlier fixture-only run41018286041 passed shutdown registration but failed
the existing15-second upload-prefix observation; its failure is retained.
Product header timeout remains3000ms and real1024-byte prefix/backpressure,
partial-consumption and exact two-attempt assertions remain unchanged.

Main1aa591b CI41016702414 attempt2 passes Windows1,456/0/21 (artifact SHA
verified), with the earlier successful Linux/macOS jobs retained. That reviewer
rerun does not erase attempt1 failure or validate these later review corrections.

## Main Windows Shutdown-Fixture Follow-up (#52)

Main source `1aa591b` CI41016702414 passes Linux1,572/0/28 and
macOS1,571/0/29, but Windows records1,455 passed/1 failed/21 ignored. The
failure is `recovery_paths::shutdown_context_save_failure_is_cached_but_wait_and_run_still_drain`
at its three-second saved-wait-registration barrier, before shutdown begins.
All new native interruption cases passed. Preserve that failure separately from
the successful pre-main manual Windows run. No request response was captured at
the failing barrier, so its cause is not proven to be runner load.

A fixture-only correction explicitly starts Control before synthetic owner facts,
observes the unchanged durable registration predicate with a60-second watchdog
and premature-response diagnostics, and keeps the observed wait alive120seconds.
Its5ms local polling does not add a product observer; initial human registration
emits no Changed notification. All capture-error caching, stopped reason, wait
cleanup and restart assertions remain. The fresh macOS standard check passes
1,571/0/29, layout88, Python6, format and strict all-target Clippy. Native Windows
qualification of the correction remains pending at this checkpoint.

## Pending Messages And Human Turn Interruption (#52)

The unchanged product/test tree committed as `74592a9` / `47e6751` passes
`CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 RUST_TEST_THREADS=2 sh scripts/check.sh`
on macOS arm64 / pinned Rust1.99.1: layout88, Python6, format, strict all-target
Clippy and Rust **1,571 passed / 0 failed / 29 ignored** (lib700, aggregated
integration861, isolated signal7 and child3). Logs are private under .local.
That local check is not a Windows/Linux result. The branch then merged main
`b686ec1` as `33936cf5d6f074d01adb44028e6a58e2e31af7a6`; the merge changed only
non-embedded development/validation records, leaving the checked product, tests,
embedded guides and workflow identical.

Before main admission, the required manually dispatched Windows-only CI
[41015832578](https://github.com/tya5/shellboy/actions/runs/41015832578) passed on
that exact33936cf source: **1,456 passed / 0 failed / 21 ignored**, four test
threads. Downloaded timing artifacts identify the same source SHA and successful
native harnesses. All six new Turn-interruption/quiet-delivery integration cases
and the three native-gate/binding cases ran and passed; they were not skipped.
Formatting, strict Clippy, layout and Python runner gates also passed. The next
main push still requires its own three-OS CI; this manual Windows run does not
establish Linux success or physical terminal UX.

Temporary stores, synthetic files and loopback HTTP exercise typed cancel
outcomes/authority/replay/rollback, explicit now versus ordinary quiet delivery,
frozen submission and newer editable draft survival, prior-owner/file-deleted
replay, raw-text versus serialized-metadata budgets, managed-file GC and unknown
historical input list/cancel/restore across restart. Schema14 fresh creation and
migrations1..14 have exactly matching99 sqlite_schema objects in Python
SQLite3.53.0; EXPLAIN uses the two new draft indexes for current bindings and
submission watermarks. That query-only observation is separate from Rust/native
runtime validation.

UI tests cover paired read ordering, both pre-ACK notification orders, stale
headers/generations/read epochs, post-reconciliation ACKs, fixed confirmation IDs,
restore cursor/folds/attachment positions and CAS, deleted archives, escaped
controls and bounded4MiB accepting previews. Ratatui TestBackend establishes
rendering/state behavior, not physical Windows terminal compatibility.

Six real local HTTP/shell scenarios cover interrupted partial retention without
retry, completed-tool then interrupted-stream provenance, exact/stale/recovery
Turn targeting, foreground/background survival and same-Run queued handoff,
actual shell_ended after interruption leading to a new Turn and Run completion,
and ordinary queued input allowing both serial calls to finish with exactly two
model attempts. The owned-child liveness record is explicitly synthetic; no new
native child process is claimed by that scenario. Native gate and binding tests
fence both Turn replacement and driver publication, including a reserved call
whose OS side effect must not occur. Their Windows command variants pass the required native Windows CI recorded
above; physical Windows terminal interaction remains untested.

Candidate checks exposed typed ACK/internal-assignment projection mismatches,
validation-order regressions, an absent-owner compaction fence and stale legacy
UI/wake/selection/autosave assumptions. They were corrected before the final
passing check. Queued input does not invent selected-path adoption; compaction
uses existing Run/Turn absence fences rather than a broad metadata counter.
No paid API, default user data, existing owner process or #52 installed binary
was used or changed by these checks.


## Installed Main Binary After #53/#56

Main `b686ec1835a23ebfbbe0d3b297c2f2593bc489ea` passes native CI
[41005281669](https://github.com/tya5/shellboy/actions/runs/41005281669): Linux
1,527 / macOS1,526 / Windows1,411 passed, zero failures (Windows threads4).
Artifact source SHAs match main. #52 is absent from that source.

The same main source was built on macOS arm64 with
`CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo build --release --locked --bin shellboy`
in an isolated worktree, sharing only the target cache. The optimized build
finished in4m37s. After help/version and an offline `docs get --id
shellboy/agent-guide` check against a temporary unused data path, the installed
`~/.local/bin/shellboy` was backed up outside Git and atomically replaced.
Installed/source SHA-256 matches:
`7bbc7a0264ffe993fa8a66741c311f6adf2f2d6c4ee5351de6bc66cca55fc3b9`.
Installed help/version pass (`shellboy 0.2.0`); the release version number is
unchanged. Existing processes and user settings/data were not changed; the new
program applies on the next launch. This installation contains #53/#56 only.


## Combined Main Verification For #53 And #56

Exact combined source `b6621bf` (including main `343c2d6`) passes
`CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 RUST_TEST_THREADS=2 sh scripts/check.sh`
on macOS arm64 / Rust 1.99.1, using the primary target cache from an isolated
worktree: layout87, Python6, format, strict all-target Clippy, Rust **1,526 passed /
0 failed / 29 ignored**. Gitleaks8.30.1 scans origin/main..HEAD without findings.
This source excludes #52 queue/cancel/now/Turn-interrupt implementation.

Temporary stores, synthetic attachments and loopback HTTP exercise admission
facts, actual Unix owner death, repeated startup, explicit recovery authority and
ordering, immutable assignments, operation replay, legacy uncertain evidence,
transaction rollback and source loss. Real Store draft sends test zero/one file
and stale selection CAS. Frontend tests preserve applied versus answered versus
held facts. Three real-shell vertical cases compare committed cancellation facts
to model replay and prove later cancelled calls did not start executions.
Incomplete-output and saved CLI-wait reasons use unit tests, not full vertical
replay. Existing candidate test/Clippy failures were fixed before this result.

The later documentation-only publication commit does not add #52. This macOS
check is not a Windows/Linux result for #53/#56; fresh main CI remains a separate
gate. #52 specifically requires manual native Windows CI before main admission.
No real credentials/user data, paid API or installed binary is used.




## Interrupted Input And Tool Cancellation (#53, #56)

On macOS arm64 / pinned Rust 1.99.1, source through `68568bf` passes
`CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 RUST_TEST_THREADS=2 sh scripts/check.sh`:
layout84, Python6, format, strict all-target Clippy and Rust **1,496 passed /
0 failed / 29 ignored**. Logs remain private under .local. Gitleaks8.30.1 scans
the four new commits with no finding. No installed binary/default data/paid API
is used. Native Windows/Linux results for this new message work are unverified.

Temporary SQLite, synthetic files and loopback HTTP cover admission facts,
repeated startup, actual Unix owner SIGKILL, explicit human/owning-parent versus
unrelated-caller recovery, immutable old assignments, reception order and replay,
legacy unknown admission, capacity/transaction rollback and source deletion.
The real TUI send Store path tests zero/one attachment and retained selection CAS.
UI tests distinguish saved applied from answered and keep held receipts tracked.
Three real-shell vertical cases verify nonzero exit, new-input release and human
background release, exactly one execution and committed result/model replay
agreement for later cancelled calls. Incomplete-output and saved CLI-wait causes
are classifier unit tests, not native vertical scenarios.

Candidate checks exposed an actual stale attachment-registration revision, an
incorrect terminal-Turn frontend inference, obsolete existing expectations and
one Clippy finding. These were corrected before the final passing check; the
failed candidates are not native-success evidence. The later main merge and
planned #52 behavior require fresh combined checks.



## #47 Saved Totals: Implementation Gates

Latest comment6515082156 supersedes the former design/DDL evidence. Read-only
saved-state SQL is wired to usage get and existing gets, with snapshot-based F8/F6
fields. No schema/state/driver/producer/HTTP/summary-ledger changes are claimed.

Exact be76c12 (before the newest mandatory throttle/log addition) passes the local
standard check: layout82, Python6, format, strict Clippy and Rust7groups /
1,488 passed / 0 failed / 30 ignored. Its following release rebuild was stopped
for the new requirement, not counted as a completed final measurement. Earlier
surface candidates exposed outdated operation-count expectations and an unnecessary
read-time timestamp on empty active sets; these are corrected without weakening
Core atomicity assertions or renaming old test identities.

The latest focused usage suite passes25/0/3 ignored: exact slow-log boundary,
all four scope labels plus open/changed/cli request-task propagation, temporary
SQLite/log dirs with private-content sentinels absent, 1,000 Changed bursts per
window yielding one pending query, fast reads yielding zero aggregation logs,
close/epoch/inflight guards and an actual worker-backed UI Changed refresh.
The new cooldown initially missed idle views because the old progress tick was
disabled; the same existing tick is now enabled only for pending usage too, without
introducing extra activity reads. Tests use injected clocks/elapsed values and
long deadlock watchdogs, not sleep-based timing correctness.

See [measurement evidence](../development/issue-47-read-performance.md) for the
initial and optimized shared projection results, layouts, JSON variation/cache
limits, EXPLAIN/VM steps and fresh/warm definitions. These are not IPC/render
latencies. Exact46eabea metadata-inclusive release scale passes both probes and remains
below the adopted approximate500ms extreme-size goal (varied Thread median412.21ms,
legacy432.51ms; p95425.85/449.57). Its standard check passes7groups1492/0/30,
Python6/layout82. This does not approve later UI changes or native main CI.
Subsequent independent UI findings are fixed: actual result-popup Esc/F8 emits
captured-owner close before clearing, ActivateDraft closes the cache, lag rechecks
one pending snapshot without bypassing cooldown, and ordinary Thread selection
uses the lightweight DB metadata helper. Focused usage27/0/3ignored and explicit
selection test1/0 pass. Exact c4b233728a64cb7100280c28778c77f708525d58
passes the final standard check: Rust7groups1495/0/30ignored, layout82, Python6,
format and strict Clippy. The independent reviewer confirms those counts and the
actual command/cwd/commit-before-check, exit0 and complete output in execution
1b180353 (89,370ms is the entire shell, including prior commit/notification).
Independent usage27/0/3, selection1/0 and all-UI78/0/1 checks resolve the reported
UI P2s and the non-usage index-confirmation regression.

The parent approves documenting the existing surface distinction: explicit F8
usage/get forms use coalesced Changed refresh; followed references query once and
F6 pages only inspect cached fields, with explicit re-query and no freshness
deadline. This documentation clarification changes no runtime code. Independent
confirmation of that wording and both surfaces remains the final review gate;
new exact documentation checks and latest-main/native CI are separate evidence.
Summary/embedding/RAG remain not_recorded/out_of_scope, never known-zero usage.
No user DB or paid provider call is used. #50 permission is withdrawn; after #47
main exact CI succeeds, stop and await a new instruction after owner restart.

## Standard Usage Persistence And Scalar Reads (#49)

On macOS arm64 / pinned Rust 1.99.1, clean exact source
`ad9343ce50465d6a97684f087ed0d02a4df6de89` passes
`CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 RUST_TEST_THREADS=4 sh scripts/check.sh`
with `$HOME/.cargo/bin` on PATH: layout82, Python6, format, strict all-target
Clippy and Rust **7 groups / 1,474 passed / 0 failed / 28 ignored**. The recorded
command commits the final test-only correction before running the check;
its shell exits 0 after 117,955 ms. Later documentation records do not inherit
that exact-SHA result; independent review, latest-main checks and native main CI
remain separate verification. No paid API, user DB, push or installed binary is
used by the coder.

Synthetic regressions cover independent known zero / unknown fields, five-number
whitelisting, huge top-level attribution and nested appendices, signed 64-bit
maximum, maximum+1, u64 maximum/+1, negatives, floats including exponent notation,
booleans, strings, invalid nested structures, malformed/missing JSON and null.
Rust normalization agrees with guarded SQL extraction for these cases. Legacy
>64KiB rows feed context usage, the same-model estimation baseline and exact
adopted-attempt F6 rendering without modifying saved JSON. Only nullable scalars
are selected into Rust; SQLite still parses the old JSON internally.

Loopback HTTP completion preserves measured input characters while retaining
only standard usage numbers. Failed/incomplete response envelopes preserve
identity, output and error diagnosis while removing usage appendices from both
usage_json and audit raw_response. Existing received-failure classification
remains incomplete rather than being changed to failed. SSE tests cover all three
terminal envelope kinds; the real summary/cache/adoption fixture verifies compact
usage with reasoning/raw provenance and originals retained. Writer tests cover
transaction rollback and repeated terminal adoption without replacing measurements.

Earlier development checks are not successful evidence: one compile used the
wrong ThreadGet argument type, one new test incorrectly expected failed rather
than the existing incomplete recovery classification, and the first clean-SHA
check exposed a test-only &String/&str inference mismatch after enlarging the F6
fixture. All are corrected without changing production lifecycle or timing.
The final exact-source standard result above supersedes those candidate checks.

The pre-existing non-2xx error path still exposes only diagnostics within its
64KiB limit and does not supply usage to the attempt writer. This is an explicit
#49 limitation, not a complete #47 outcome. #47 acceptance must add agreed typed
non-2xx known/partial/unknown usage tests while preserving body limits, diagnostics
and non-disclosure of the full error body. Oversized/malformed/unreported usage
must remain unknown. No existing raw/usage-row rewrite migration is introduced.
The Agent Guide's once-per-task-boundary review uses actual last input, protects
unfinished work/unapplied instructions and restores access to original facts;
mock checks do not establish real-model cleanup behavior. Dogfood is unverified.


## #51/#48 Windows repair: separate failed and corrected evidence

### Additional History Expectation From Repair-Branch CI

Repair-branch run `40991269818`, exact source `0cf9c405`, is still failed evidence:
Windows clears the earlier two lib failures and the runner completes all harnesses
with failure names, but
`history::persistence_fork_returns_inherited_agents_and_rejects_explicit_overrides_atomically`
fails at the final inherited-path comparison. The actual path contains native
`project\AGENTS.md`; the expectation used `join("project/AGENTS.md")`.
These are the integrator/reviewer's native-log observations, not local Windows
execution by the coder. Main is not updated on this result.

The separately approved test-only correction builds the expectation with
`join("project").join("AGENTS.md")`. It retains exact inherited-path equality,
both explicit-override validation paths, zero operation receipts and the unchanged
Thread count after rejection. No production code, runner, test inventory, retry,
ignore or timeout changes. A read-only search of #51/#48 additions across Rust
source/tests finds no other unresolved same-kind path expectation; remaining
raw CLI/form strings, mock-returned paths and filesystem-write inputs are intentional.

On macOS arm64 / Rust 1.99.1, the focused command
`cargo test --locked --test integration history::persistence_fork_returns_inherited_agents_and_rejects_explicit_overrides_atomically -- --exact`
passes once with four test threads. The same pre-commit working tree passes
`CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 RUST_TEST_THREADS=4 sh scripts/check.sh`:
layout82, Python6, format, strict all-target Clippy and Rust 7 groups /
1,468 passed / 0 failed / 28 ignored. This local result is not native Windows
validation or the integrator's required latest-main post-rebase check.

Under the latest explicitly authorized #57
rule, independent review and latest-main fetch/rebase plus successful local
standard checks precede the integrator's normal main push; exact-SHA main three-OS
CI follows. Branch CI is optional, not a mandatory pre-push gate. The earlier
main `c0f1ad9` and repair-branch `0cf9c405` failures remain separate failed evidence.
No push is performed by the coder; workflow-trigger changes are out of scope.

The [#51 failure report](https://github.com/tya5/shellboy/issues/51#issuecomment-6513119814)
and [#46 console diagnosis](https://github.com/tya5/shellboy/issues/46#issuecomment-6513118002)
identify two Windows failures at exact main SHA
`c0f1ad984be7a5d64d9dfbab4189cd3ba0438151`, run `40988162209`, attempts 1 and 2.
Both downloaded results records identify that source and the same native lib
exit 101/failure names:

- `control::db::agents_reference_tests::explicit_agents_resolution_inheritance_validation_and_receipt_replay`:
  actual native project/backslash/role.md versus expected mixed project/role.md.
- `ui::chrome::tests::empty_conversation_explains_actual_bindings_cwd_and_unobserved_agents`:
  actual cwd.join-generated AGENTS path versus a literal slash-only label.

Both records also contain the secondary runner charmap encoding error. Raw
harness logs are present UTF-8 artifacts; the failure was printing their Unicode
tail to the Windows cp1252 console before appending/saving the harness record.
These two failed attempts remain failed evidence; no further same-SHA rerun is
used to hide them. Local macOS success at the old source did not prove Windows
expectation portability.

The repair changes only the two fixture expectations, timing-runner Unicode
observability and its existing self-check suite, plus these records. It does not
change Rust production paths/rendering, native test inventories, ignored tests,
parallelism, deadlines, retries or dependencies. The fixture tests each pass
once on macOS arm64 / Rust 1.99.1, using the existing four-thread configuration.

The new Python regression launches a real child runner with
`PYTHONIOENCODING=ascii:strict`, `PYTHONUTF8=0` and an uncoerced C locale. A real
Python fixture process emits UTF-8 Japanese/emoji output exceeding the console
tail size and exits 101; subcases use ASCII and non-ASCII test names. Before the
runner repair it reproduces secondary encoding/decoding failures. After repair,
all six Python self-checks pass: the full raw log bytes match, the harness record
retains exit 101/counts/names, every name is printed as an ASCII-safe line before
the tail, the inventory retains Unicode identities, no runner-stage encoding
error is added and overall exit remains 1. This is a synthetic failure-path
check, not native Windows execution of the repaired Rust suite.

### Local Repair Checkpoint

At exact clean `ff84d81682e477db91aa34be751609081cdb535b`, the coder's macOS arm64 /
Rust 1.99.1 standard command
`CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 RUST_TEST_THREADS=4 sh scripts/check.sh`
with `$HOME/.cargo/bin` on PATH exits 0: layout82, Python6, format, strict
all-target Clippy and Rust **7 groups / 1,468 passed / 0 failed / 28 ignored**.
The background shell took 168,648 ms including its preceding runner commit/status
steps; this is not isolated test-runtime evidence. Its saved command/cwd/exit and
complete log were checked, test results independently re-aggregated, and
`shellboy wait` observed completion. Sources were unchanged during that check.

Independent focused review repeats both fixture tests, all six Python checks in
normal and strict-ASCII parent environments, and the old-runner negative check
(two failed subcases). Those are separate focused results, not native Windows
repair-branch success. Independent review formally approves exact `ff84d816`
after confirming the standard log and saved shell facts. That checkpoint did not
establish native success. The earlier mandatory repair-branch CI gate is superseded
by the #57 rule recorded above: latest-main reconciliation and successful local
standard checks precede normal main push and exact-SHA main CI; branch CI is
optional. Only the integrator is
authorized to push; recording this plan does not authorize force pushes, issue
operations, release or installed-binary replacement. Physical-terminal/real
provider verification is not claimed.


## Issue #48: outbound working-instruction provenance

### Combined Local Integration Checkpoint

Exact `702cef9eacf45ed628fcf8125c37b6eda4287640` applies only #48's two approved
commits onto approved #51 documentation `f46fa2e`; both patches are identical in
range-diff to `96454e5` / `120218c`. Independent re-review repeats the header
unit tests (2 passed) and unified runtime_features suite (22 passed), with no
findings. The combined worktree's standard command
`CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 RUST_TEST_THREADS=4 sh scripts/check.sh`
passes layout (82 suites registered once), five Python checks, format, strict
all-target Clippy and Rust **7 groups / 1,468 passed / 0 failed / 28 ignored**.
Environment: macOS arm64 / pinned Rust 1.99.1; temporary stores and loopback mocks.
The background command shell takes 264,078 ms including the preceding ff,
cherry-picks and range-diff. Its saved initial cwd is the #51 worktree, but the
command explicitly changes to the isolated #48 integration worktree before any
integration or checks. `shellboy wait` observes completion; full logs and command
metadata are independently checked rather than inferring success from acceptance.

The source-specific result is distinct from the coder's earlier `120218c` run.
Later documentation-only commits do not acquire that exact-SHA test result.
The two validation-record insertion conflicts preserve both #48 and #51 sections.
Origin/main is not reconciled, new three-OS CI is not run, and real-model behavior
or physical-terminal evaluation remains unverified. Future main adoption and a
separately authorized push still require base comparison and required rechecks.

### Environment And Exact Source

macOS arm64 / pinned Rust 1.99.1, separate issue-48 worktree. The initial
provisional #51-on-#46 base `144721f` was fast-forwarded, before #48 edits, to
`872c4b8af5bda81801a077a4217b5e9b25f2a9fa` with the integrator's approval.
That base fixes six #51 fixture calls for #46's helper signature and was later
independently reapproved. #48 commits remain separate from #51. This is local
integration on the exact #46 branch, not a claim that origin/main already
contains #46/#51. No other role's worktree or earlier evidence was modified.

At exact clean implementation SHA `120218cb79063bc97012cb809d422ec837b5a507`,
`CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 RUST_TEST_THREADS=4 sh scripts/check.sh`
with `$HOME/.cargo/bin` on PATH passes: layout **82 suites**, Python **5 passed**,
format, strict all-target Clippy and Rust **7 groups / 1,468 passed / 0 failed /
28 ignored**, exit 0. The background shell took 131,067 ms including its preceding
local commit/status preparation; this is not an isolated test-runtime measure.
Saved command/cwd/exit metadata and complete output were checked and
`shellboy wait` observed completion. Sources stayed unchanged during the check.

### Focused Evidence

- `cargo test --locked --lib instruction_header_tests`: 2 passed. The formatter
  preserves every body byte after the first LF, quotes/round-trips path data and
  escapes quotes, backslashes, CR/LF/tab, DEL, C1 and Unicode U+2028/U+2029. A
  present empty file retains an empty body after its metadata line.
- `cargo test --locked --test integration runtime_features::`: 22 passed using
  synthetic temporary stores and loopback providers. #51's existing assertions
  now parse the metadata line, decode and compare the exact path, and compare
  original body bytes; they were not weakened to substring checks. Same-Turn
  later requests/retries retain old header/path/body despite file deletion or
  reference changes; the next new Turn uses the new selected source.
- Additional mock requests verify empty and nonempty file bodies, saved raw body/
  hash/observed-size (including Some(0)), same-source edits reread on a new Turn,
  and immutable prior snapshot bytes. Missing instructions produce no instruction
  message; invalid UTF-8 retains recovery and repaired-source retry behavior.
  On Unix where mode bits deny reading, unreadable instructions retain io_failed
  recovery with no snapshot/attempt/HTTP. Privileged mode-bit bypass is explicitly
  conditional, not reported as denial success on every runner.
- The adopted #46 unified harness and existing runtime_features module are used;
  no test binary/file, table, worker, queue or dependency was added. The only
  production change is outbound developer-message presentation.

### Remaining Gates

Independent review approved exact `120218cb79063bc97012cb809d422ec837b5a507`
after separately passing the two header tests, all 22 runtime_features tests,
layout82 and Python5, and confirming the standard log's counts and shell facts.
Post-integration validation on the newer #51 documentation base remains a
separate exact-SHA gate. If the #51/#46 base changes,
review/check the resulting exact SHA again. Main reconciliation and explicit
push authorization remain pending; no GitHub post, new CI run or installed
binary update is performed. Native Linux/Windows, physical terminals, paid/real
providers and improved human/model conflict-handling behavior were not tested.
Mocks establish factual request transmission only; a later authorized dogfood
exercise must establish whether real replies identify the path, constraint and
safe next-Turn edit procedure accurately. No new priority/permission rule or
automatic exception to higher-priority instructions is claimed.


## Issue #51: advance integration on exact #46 source

Base: `a94968898c704b4ee7b78be1cda860ee6d057b04`, from the completed #46
measurement branch, not origin/main. Existing native CI run
[40982193189](https://github.com/tya5/shellboy/actions/runs/40982193189)
was checked through the current API: exact source matches and Linux/macOS/Windows
check jobs succeed; optional measurement is skipped. That run does not validate
#51's rebased changes. No new CI or push was requested for this local integration.

The old `0469ca0..7f425e8` series is preserved and maps one-to-one onto
`a949688..144721f`. Only documentation insertion contexts differ in range-diff;
source patches retain their original content. However, textual patch equivalence
was not build compatibility: the first post-rebase standard check at `144721f`
failed Clippy with six E0061 errors in `runtime_features` incoming calls. #46's
helper now requires Control for bounded diagnostic facts. Test-only `872c4b8`
adds that argument at those six calls, preserving request assertions and the
Changed-notification guards. IPC/attachment overlap is only constructor fields;
#46's pending-wait and attachment fixture changes remain intact.

Environment: macOS arm64, Rust 1.99.1, isolated integration worktree. At exact
`872c4b8af5bda81801a077a4217b5e9b25f2a9fa`, the background command
`CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 RUST_TEST_THREADS=4 sh scripts/check.sh`
completes with exit 0. Layout requires each of 82 suites exactly once; five
Python failure-gate self-checks, format and strict all-target Clippy pass.
Rust all-target results: **7 harness groups / 1,464 passed / 0 failed / 28 ignored**.
The command shell elapsed 154,417 ms, including local log setup/result summation;
this is not a native CI timing benchmark. It was observed with `shellboy wait`.
The earlier failed source and later successful source are separate evidence.

The ordinary test files remain in place and are imported by
`tests/integration/main.rs`; #51 does not introduce an extra integration binary.
Historical 86-group evidence below remains attributed to its original sources.
The additional idle-effects unit test raises the historical 1,463 passes to 1,464;
independent inventory/list review is separate from the full integration command.
No real-provider, physical-terminal or new native Linux/Windows execution is
established here. Reconcile with origin/main after #46 lands, re-review any
changed SHA and rerun the required checks before a separately authorized push.
#48 is a separate worktree/commit boundary and is absent from this source.

## Issue #51: selectable per-Thread instruction references

Environment: macOS arm64, pinned Rust/Cargo 1.99.1; isolated worktree based on
`0469ca03`. Tests use temporary stores, synthetic instruction files and loopback
mocks. No real provider traffic, private user-data migration, installed-owner
replacement, push or GitHub post is part of this validation.

### Production implementation

Exact clean production SHA `bfd96f5238943b57d94ccd678704592c00481edf` passes
`CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 RUST_TEST_THREADS=2 sh scripts/check.sh`
with `$HOME/.cargo/bin` on PATH: format, strict all-target Clippy and all-target
tests, **86 groups / 1,462 passed / 0 failed / 28 ignored**, exit 0. The
background shell took 306,587 ms including its preceding review notification;
`shellboy wait` observed completion and shell metadata confirmed command/cwd/
exit. Sources remained unchanged during that final run. Earlier exploratory
runs are not substituted for this result.

Independent review at the same SHA passes library-wide tests (641 passed,
10 ignored), focused agents tests (8 passed), History integration (14 passed)
and agents runtime integration (2 passed), with no remaining findings. The
reviewer independently confirmed the standard log's counts and shell facts.

### Covered Boundaries

- CLI create/set/fork preserves relative paths until the target effective cwd
  is known. Root defaults, child/fork inheritance, strict explicit empty/missing/
  directory/invalid UTF-8/size failures, exact 8 MiB acceptance, readable-mode
  denial on nonprivileged Unix, same-path validation without invented audit,
  receipt replay after deletion, and revision-CAS rejection are covered.
- Preflight uses the same bounded InstructionLoader permits as Control; the
  existing stat/read growth test covers the shared limit-plus-one read. Failed
  validation and CAS do not partially update cwd/model/title/reference. Audit
  retains real caller IDs and old/new paths/time without the Origin token.
- Loopback requests cover active and recovery changes, old body/source path on
  same-Turn tool continuation and explicit retry, new snapshot on the next Turn,
  unrelated settings at their normal boundary, retained notification after a
  failed request, one saved event node/adoption, idle changes without wake and
  deletion after selection retaining missing-means-no-instructions behavior.
- TUI form/slash/reference rendering, unsaved creation through frontend and back,
  preservation of newer text, origin-draft adoption after switching away,
  stale-generation/duplicate rejection and next-send receipt targeting the same
  created Thread are covered. Real ActionSender Full/Closed rejection restores
  slash input and permits retry; direct-form rejection preserves text and
  permits normal send. Stale rejection cannot release a later request.
- DB-only History fork helpers reject explicit Some references, including empty,
  before writes and leave no Thread/receipt. Inherited path returns truthfully;
  full explicit overrides belong to Control's validated, audited thread.fork.

### Additional Regression And Limits

After the production-code approval, one src-only test explicitly distinguishes
parent `parent.md` from child/fork `child.md`, so ignoring an override cannot
accidentally pass an inheritance assertion. It passes with
`cargo test --locked --lib child_and_control_fork` (1 passed); production code
is unchanged. Independent review approved the test/documentation commit
`9ddb49d4609e6874dddcd335b9a05f3f472d6eb7` after repeating that focused test.

### Final Post-Integration Check

Exact integrated SHA `9ddb49d4609e6874dddcd335b9a05f3f472d6eb7`, in the separate
integration worktree on macOS arm64 / Rust 1.99.1, passes
`CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 RUST_TEST_THREADS=2 sh scripts/check.sh`
with `$HOME/.cargo/bin` on PATH: format, strict all-target Clippy and all-target
tests, **86 groups / 1,463 passed / 0 failed / 28 ignored**, exit 0. This result
includes the additive distinct-reference test and does not replace the earlier
1,462-test production checkpoint. Integration was fast-forward only, preserving
reviewed SHAs without conflicts. The complete background shell took 306,042 ms,
including its prior-check observation and fast-forward steps; this is not an
isolated test-runtime measurement. Saved execution metadata confirms the actual
command, integration cwd, exit 0 and complete output; the final standard log's
counts were independently re-aggregated for this documentation update.

These are local integration results, not three-OS CI or physical-terminal
success. Main and remotes remain unchanged; no push, GitHub posting or installed
binary update is authorized by recording this evidence.

Exploratory failures exposed stale JSON fixtures, old path labels/positional
form assertions, an empty-CLI-value parser expectation, a Unix permission literal
rejected by Clippy and a test's incorrect receipt column name; they were fixed.
Independent review found and drove the three P2 corrections recorded above; no
commit was amended. Real providers, physical-terminal evaluation and native
Windows/Linux execution or three-OS CI are not established by local mock tests.
File validation does not pin future bytes: later deletion/edit/access failures
retain the adopted Turn snapshot/preparation boundary. A privileged runner may
bypass Unix mode-bit denial, so that assertion is conditional on actual denial.


### External Review Follow-Up: Deterministic Idle Evidence

Sources: [external review](https://github.com/tya5/shellboy/issues/51#issuecomment-6511989087)
and [follow-up](https://github.com/tya5/shellboy/issues/51#issuecomment-6512018897).
These comments are review findings and publication requirements, not independent
execution evidence. The three Low observations (profile_revision coupling,
initial-reference audit wording and constructor Default ergonomics) are
record-only and do not change the adopted production behavior or closing gates.

The #51 idle regression no longer infers inactivity from the absence of a proxy
request within 100 ms. It awaits the existing RunEnded observation and asserts
saved Runs/Turns are completed and attempts terminal. A single DB-worker read
captures each target Thread's ordered Run, Turn and attempt identities/states;
after the settings request completes its committed effects, the same saved rows
must remain identical, including their counts. The new reference audit remains
unbound to a Run, without delivery or an event history node before the new Turn.
A src-only effect regression separately asserts empty wake_threads,
quiet_wake_threads, stop_runs and compact_threads, guarding a delayed driver
schedule that an immediate DB snapshot alone would not exclude. No sleep or
quiet-interval extension is used as proof of inactivity; positive progress
watchdogs remain bounded failures, not success criteria.

The 1,463-test exact-SHA integration record above remains the pre-follow-up
checkpoint. On macOS arm64 / Rust 1.99.1, the follow-up passes
`cargo test --locked --lib idle_agents_set_only` (1 passed),
`cargo test --locked --test runtime_features explicit_agents_changes` (1 passed)
and `cargo clippy --locked --lib --test runtime_features -- -D warnings`
(exit 0), with CARGO_BUILD_JOBS=2, CARGO_INCREMENTAL=0 and RUST_TEST_THREADS=2
for the tests. Format and diff checks also pass. These are focused results,
not a new whole-suite count; full validation on the rebased #46 layout is pending
that task's landing.
No test file is added. Expected overlap includes runtime_features,
attachment_provenance, ipc_admission and shared records. #46 lands first; #51
then rebases and moves tests into the adopted layout before standard rechecks.
Push instructions in review comments are not permission to push or start CI;
physical-terminal and native Linux/Windows validation remain unperformed here.


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


## 2026-10-30: #46 main integration verification

After the latest review reported no Medium findings, the user authorized main
integration. Fast-forwarded `0469ca0..a949688` (15 commits) without rewriting the
measured commits and pushed main. Exact main source `a94968898c704b4ee7b78be1cda860ee6d057b04`
succeeds on macOS, Linux and Windows in
[CI 40987103680](https://github.com/tya5/shellboy/actions/runs/40987103680).
Commented on #46 and closed it after that result, as required by the review.
The #42/#43/#55 working changes remain on a separate local branch and were not
included. No release or installed-binary update was performed. The original
benchmark and deliberately deferred timer/sleep cleanup remain separate evidence.


## 2026-10-30: #42/#43/#55 local index acceptance

The tested implementation tree was committed as `7a8e547` (shared Core/request
modes), `930786e` (batch regressions), `83d2be7` (dimension/probe regressions),
`31a8f31` (explicit inactive-space GC), and `f9d3ee7` (TUI recovery). The adopted
design is `37d2d78`. Later guide/evidence changes are recorded separately.

On native macOS, with pinned Rust 1.99.1, `CARGO_BUILD_JOBS=2`,
`CARGO_INCREMENTAL=0` and `RUST_TEST_THREADS=2`, `sh scripts/check.sh` succeeds:
85 suites registered exactly once, five Python failure-gate tests, formatting,
all-target Clippy with `-D warnings`, and 1,482 passed / zero failed / 28 ignored
across seven Cargo targets. Breakdown: lib636, integration836, termination7,
crash-recovery3. Ignored tests were not counted as verified.

Verified with fresh synthetic stores and loopback HTTP:

- Saved pause timestamps/identity survive reads and service recreation; unchanged
  per-Thread observations deduplicate; absent historical time remains unknown.
  Explicit retry clears current state without deleting the historical notice.
- Six batch regressions verify identical snapshot grouping, Unicode/count caps,
  request reduction, default singleton JSON, full-response index/count/vector
  validation, recorded per-input fallback even with zero retry budget, preserved
  singleton length-split children, auth pause and vector-transaction rollback.
  Retry preview mirrors actual current targets and marks unread admissions
  unavailable without file/API reads or new source authorization.
- Seven dimension/probe regressions verify one real first candidate input even
  with concurrent workers, first-vector reuse, old active search during build,
  rejection/wrong-length stopping, transient retry with the same N, changed-model
  native reset, persisted query/ingest mode after reopen, inherited rebuild mode,
  superseded failure fencing, one probe request, bounded32 tuple observations,
  and operation replay/conflict without another HTTP request. Four HTTP boundary
  regressions also check exact native/explicit wire shape and malformed arrays.
- Three GC regressions verify multiple bounded32-space transactions, active/
  candidate/running-work protection, invalid identifier rejection and atomic
  rollback including vector-table DDL. Payload counts are float32 arithmetic;
  physical file/WAL shrink is not asserted.
- Native PTY uses the real binary, temporary durable401 pause and local proxies.
  It opens the saved stop record, shows time/action, gives P-without-target
  guidance, obtains global T preview, displays source1/character29 in one full
  repaint, requires Y, refreshes S to unpaused and completes a normal conversation.
  Existing streaming animation/input/scroll/manual-repaint/idle/shutdown checks
  still pass. Full paint uses existing Ctrl+L; no terminal emulator was invented.

Earlier checks failed on stale test fixtures (wrong management mode, old command
counts, missing synthetic Thread cursor, and assertions over terminal diff cells).
They were corrected rather than accepted as passing evidence. The PTY also found
a real S-key collision with generic output navigation; Index mode now routes S
to IndexStatus, with regressions for default/all/explicit-space scopes. Read-only
review found mutable request-mode lookup and superseded failure adoption races;
claims now freeze the mode inside their DB transaction and late errors recheck
the rebuild group before changing pause/failure state.

Not performed: new Linux/Windows native CI for this branch, company proxy/model
validation, paid APIs, default-user data reindex/GC, installed binary replacement
or release. Main CI 40987103680 validates #46 at a949688 only. A mock success
does not establish provider dimension support or retrieval quality.


## 2026-10-30: current-main and index combined verification

After the index-only local verification, fetch found origin/main `c0f1ad9`
containing #48/#51. Merge `80caf38` combines that source with index branch
`3be41be`. Conflict resolution retains both adopted designs, decision/status
records and both CLI regressions; no test or instruction-reference behavior is
discarded. This combined check is separate from either branch's earlier result.

With the same native macOS/pinned-toolchain environment and explicit two-thread
fixture setting, `sh scripts/check.sh` succeeds: 85 suites registered once, five
Python failure-gate tests, formatting and all-target strict Clippy; 1,498 passed,
zero failed, 28 ignored across seven Cargo targets (lib647, integration841,
termination7, crash-recovery3). The strengthened real PTY pause/retry/status and
existing animation/stream/input/scroll/repaint/exit test passes in the combined
suite. `index probe --help` and `index rebuild --help` also succeed without an
owner or API call. No code changed after this check; subsequent evidence edits
are development/validation documentation only.

The combined branch has not been pushed; its new three-OS CI is awaiting explicit
push authorization under AGENTS.md. Do not substitute #46's main CI at a949688
for this combined source. User data, installed binary and release remain
unchanged.


## 2026-10-30: canonical Windows repair and #57 in the index branch

Another session published the canonical Windows repairs `509daa5`, `ff84d81` and
`5234f94` before the proposed isolated recovery push. The non-fast-forward push
was rejected without changing main. Adopted the published source, whose
[main CI40992985197](https://github.com/tya5/shellboy/actions/runs/40992985197)
succeeds on all three OSes. The duplicate local alternative `34ca8d3` was not
merged or published. Keep it only as an abandoned local investigation source.

Only #57's five-file policy was reapplied to repaired main, producing `16fe4aa`.
That exact proposed tree passed macOS standard checks (1,468 passed, zero failed,
28 ignored; six Python tests, formatting and strict Clippy). User-authorized
main push fast-forwarded 5234f94 to16fe4aa and created
[push CI40995235000](https://github.com/tya5/shellboy/actions/runs/40995235000).
All three OS jobs actually started; its Windows result remains pending at this
checkpoint. No index feature was included in that main push.

Index merge `5341512` combines b22a30a with canonical16fe4aa, rather than the
duplicate fixes. `sh scripts/check.sh` on native macOS with Rust1.99.1 and
the same two-thread fixture setting succeeds: 85 suites, six Python gates,
formatting, all-target Clippy -D warnings and 1,498 passed / zero failed /
28 ignored (lib647, integration841, termination7, crash-recovery3). No source
changed after that check; the next commit updates development/validation records
only. Existing real-PTY recovery remains included.

Next publish the authorized index branch, observe that #57 does not automatically
start its push CI, then explicitly dispatch the retained three-OS workflow for
that exact branch. Record source/event/run and per-OS results separately from
b22's failed Windows result and main's baseline successes. No default data,
paid provider API, installed binary, tag or release is changed by these checks.


## 2026-10-30: final native index verification and #57 trigger observations

Exact main source `16fe4aa27ec41ef05c803fb12808557117618e0f` succeeds in
[push CI40995235000](https://github.com/tya5/shellboy/actions/runs/40995235000):
Linux1,469 / macOS1,468 / Windows1,356 passed, zero failed; ignored27/28/20.
Artifacts match that SHA and have empty failure arrays. This verifies #57's
main-push event actually starts and completes the ordinary three-OS matrix.
Canonical Windows fixture/Unicode runner repairs remain those from main5234f94;
the unpublished duplicate34ca8d3 was never used as publication evidence.

User-authorized branch push advanced b22a30a to exact
`ae9bc193947882ce1d4b99ac4765c7056a45565e`. Multiple run-list observations
before manual dispatch show no push-triggered run for this SHA; only the prior
failed b22a30a push run exists. Explicit `gh workflow run ci.yml --ref
issue-42-43-55/index` then starts
[workflow_dispatch CI40996181881](https://github.com/tya5/shellboy/actions/runs/40996181881)
with diagnostics=false, performance=false, windows_test_threads=default and
baseline_ref5234f94. All retained inputs are accepted and all three native jobs
complete successfully:

| OS | Passed | Failed | Ignored | Recorded test threads |
| --- | ---: | ---: | ---: | --- |
| Linux | 1,499 | 0 | 27 | libtest default |
| macOS | 1,498 | 0 | 28 | libtest default |
| Windows | 1,386 | 0 | 20 | 4 |

Downloaded timing artifacts match ae9bc19 and contain no failures. For each OS,
compare full normalized identities against the main16fe4aa artifact: every
baseline identity is retained, exactly30 active cases are added, and ignored
sets are identical. Windows now runs and passes the complete integration
harness, including dimension/probe/batch/GC regressions previously not reached
when b22's lib diagnostics aborted. Native OS success does not claim physical
company-terminal/IME/clipboard or actual provider dimension/quality support.

#57 is therefore verified for main push, branch push with no automatic CI, and
explicit branch dispatch. Optional enabled diagnostics/performance workloads
were not rerun solely for this trigger edit: their inputs/matrix/job bodies
are byte-identical to the previously adopted workflow. No test tag or release
was created; tag exclusion is verified by the branches-only configuration and
GitHub's documented filter semantics.

Post-result changes are development/status and acceptance records only, beyond
the exact native-verified ae9bc19 code. Verify their links/diff/secrets before
publishing; no Rust, bundled-guide, dependency or workflow source changes and
no duplicate native run are needed for that evidence update. Index features
remain on the work branch; main contains the authorized CI policy/repairs only.
No user data, paid API, installed binary or release was changed.


Final fetch also observes origin/main5350fc8, containing another session's usage
work after the verified main16fe4aa. Its run40997452704 is still in progress at
this checkpoint. It is not merged into the index branch merely to chase a moving
main during review. The 30-case inventory comparison and native successes above
are explicitly against16fe4aa/ae9bc19, not that later combination. A future main
integration must reconcile and validate the then-current main; index remains
unmerged and publication authorization does not include that future integration.


## 2026-10-30: index review integration with main5350fc8

Reviewed #42/#43/#55 contain no Medium findings. The Low dispositions are recorded
in [the review follow-up](../development/index-review-follow-up.md). Main usage
work5350fc8 is merged at51a819f, retaining both model HTTP test groups and both
documentation histories. The configuration guide now explicitly couples larger
batches with review of the whole-request timeout and proxy caps.

Native macOS `sh scripts/check.sh` with Rust1.99.1, build jobs2, incremental0 and
test threads2 succeeds: 1,504 passed / zero failed / 28 ignored, six Python gates,
85 registered suites, formatting and all-target strict Clippy. This is the new
combined source result; do not substitute ae9bc19's earlier native CI for it.
Subsequent changes here are development/validation records only. The user
authorizes main push; confirm its exact-source three-OS CI before issue closure.
#53/#56/#52 code and any user-data operations are excluded.

## Guide follow-up (#67)

2026-10-30. Source: runtime `ed40f33`, initial guide snippets `3d65edd`, with final
snippet/config rechecks before push. Native macOS 26.3.1 ARM64, Python 3 and
`/usr/bin/sandbox-exec` (confirmed deprecated in its local manual). Extract the
Scheme/profile and sh wrapper from `docs/guides/shell.md`, substitute disposable
physical work/scratch/profile paths and a fixture proxy port, and configure
`[shell] executable` to that wrapper. No real user owner or paid provider is used.

A temporary Python HTTP Responses fixture requests one actual background model
shell from a separate PTY owner; a separate local proxy fixture answers only
`http://allowed.test/` with 200 and rejects `http://denied.test/` with 403. Checks:

- Direct writes to work/scratch succeed; a sibling path outside both is denied.
- HTTP through the fixture proxy returns the expected allow/reject codes; direct
  TCP to a different local service fails even with proxy bypass requested.
- The model shell's real `message send` succeeds through its issued owner socket,
  and `wait` observes a known-completed independent fixture Turn without a cycle.
- The real `shell stop` operation terminates the wrapped model shell and its
  sleep descendant; the descendant PID disappears. The fixture owner and HTTP
  servers are stopped afterwards; all storage is disposable.

This is an HTTP allowlist fixture, not a production forwarding proxy or HTTPS
CONNECT/DNS-rebinding qualification. Reads and other OS IPC are not confined by
the allow-default profile. This is not malicious-code escape resistance or
all-macOS-version certification. No real provider interoperability was tested.

[Linux capability run 41068155623](https://github.com/tya5/shellboy/actions/runs/41068155623)
at branch-only SHA `60a4749` succeeds on Ubuntu 22.04 x64 / kernel 6.8.0-1070-azure
with bubblewrap 0.6.1. It installs bubblewrap in the ephemeral runner and checks
read-only `/`, writable work/temp binds and direct TCP denial in `--unshare-net`.
The validation-only workflow is **not merged into main**. A complete Linux
proxy bridge, owner CLI connectivity and stopping were not verified together;
Landlock and WSL2 are also unverified. To keep this follow-up bounded, the guide
defers that executable recipe rather than extrapolating from the capability test.
Windows ACL/Firewall/AppContainer/VM options are conceptual, unverified here.

The three cleanup lines were checked against the existing `thread delete-preview`
and `thread delete` contract (no live/stopping Runs, matching descendant choice)
and usage's exclusion of deleted Threads. The User Guide typo and its Japanese
safety-section link were checked for alignment. Rust and workflow changes are
excluded from the main-bound documentation commits; standard checks and exact
main CI remain separate validation gates.

### Related #63 validation history

The Windows running-image fix is recorded in [publication](../development/publication.md#windows-running-image-replacement-follow-up-63).
At `b16f414`, local full standard check first failed the existing TUI repaint
assertion `real_tui_accepts_text_renders_proxy_response_and_exits_cleanly`;
the unchanged same-SHA full rerun passed. Main CI
[41063015359](https://github.com/tya5/shellboy/actions/runs/41063015359) passed
macOS/Linux on attempt 1. Windows attempt 1 timed out in
`a_long_turn_can_reduce_again_after_successful_tool_work`; failed-job rerun
attempt 2 passed that test but timed out in
`partial_uploads_reduce_on_the_second_failure_and_keep_the_retry_budget`.
Same-SHA failed-job rerun attempt 3 passed. No Rust/test changes or automatic
retry suppression were introduced; the timeout causes remain unresolved.

## Windows retry fixture watchdog follow-up (2026-10-30)

Scope: response_retries and response_upload_recovery fixture-only guards and
Changed-driven waits. No product timeout/retry policy or success predicate is
changed. Main run41063015359 attempt1 failed long-Turn post-tool HTTP arrival
(already60s); attempt2 failed partial-upload Control shutdown (5s). Logs were
inspected separately; load remains a hypothesis. Attempt3 is observed separately.
Native macOS arm64/Rust1.99.1 full standard check passes1666/0/32, layout88,
Python6, formatting and strict all-target Clippy. The exact test/document tree
3b45c3c passes independent native Windows four-thread manual runs
[41067174706](https://github.com/tya5/shellboy/actions/runs/41067174706) and
[41067200942](https://github.com/tya5/shellboy/actions/runs/41067200942), both
first attempts, including both named failures. No product source or workflow
changed. Original b16f414 attempt3 succeeded; that does not erase attempts1/2.
This follow-up changes validation prose only; final main CI remains pending.

After conflict-free rebase onto main17c4271 (#41), exact combined source d72155f
passes Windows four-thread runs41069488710 (attempt1) and41069483605 (attempt2).
The latter's attempt1 failed an unrelated existing lib fixture,
model::http::clients::tests::control_conversations_reuse_owner_client_for_chunked_sse,
at its five-second completed-Turn observation guard (clients/tests.rs:589); its original log
is retained and only that same-SHA failed job was rerun. The fixture was not
changed. Local full check on d72155f first failed unchanged tui_process at its
post-index model-wait footer, then passed without source changes:1679/0/32,
layout89, Python6, fmt and strict Clippy. Neither failure is claimed fixed.
Final main CI remains a separate gate.


## #59 D-006 review clarification

The follow-up changes CLI/design prose plus F6 sender_fields only. A regression
requires the unknown kind unchanged, the old-record/missing-evidence explanation
in both fields and original-page lines, and no fabricated self-asserted label.
No conversation metadata/band/renderer code is modified. Native macOS arm64,
Rust1.99.1: RUST_TEST_THREADS=2 sh scripts/check.sh passes1680/0/32, layout89,
Python6, fmt and strict all-target Clippy. The added F6 regression and existing
sender-only/TestBackend preservation test pass. Two prior default-concurrency
full checks failed unchanged real_tui_accepts_text_renders_proxy_response_and_exits_cleanly:
one lacked the unpaused index fact, the other missed the subsequent model-wait
footer. Their original logs remain outside Git; no test was skipped or weakened,
and a lower-concurrency success does not establish the failure's cause/fix.
Final main CI is a separate gate; physical-terminal F6 remains unqualified.

## First-run configuration (#69)

2026-10-30. Native macOS ARM64, pinned Rust 1.99.1. Local standard check
`export PATH="$HOME/.cargo/bin:$PATH"; sh scripts/check.sh` passes after rebase.
Seven focused integration tests in `tests/config_setup.rs` cover standalone
subprocesses with temporary HOME/USERPROFILE/APPDATA/LOCALAPPDATA/XDG paths,
explicit/relative configuration paths, missing files/defaults/keys, unknown
`compact_hint_tokens`, unreadable directory-as-file, endpoint suffixes, template
identity, refusal to overwrite existing/dangling symlinks (Unix), and no owner
storage creation. The report and owner cases also check source metadata and
localized first-send or missing-key recovery details without an HTTP connection.

A bounded loopback catalog mock requires `GET /v1/models` and rejects any second
request. Cases include keyed/keyless success, unadvertised model, HTTP 401/404,
malformed JSON and connection refusal. Synthetic key sentinels are absent from
all command output even when returned in error bodies or catalog IDs. No actual
provider, user configuration, paid model/embedding request or user key is used.

[Manual native branch CI 41074920861](https://github.com/tya5/shellboy/actions/runs/41074920861)
at initial implementation SHA `45600e0` passes all three ordinary OS jobs. Native
subprocess tests check each OS's actual default-path environment resolution;
Linux additionally checks HOME fallback without XDG_CONFIG_HOME. The later exact
template-placeholder follow-up is locally checked, and final combined-source
main CI is still a distinct gate, not replaced by this older branch run.

TestBackend covers the configured/no-notice case and missing-configuration
facts, template and next actions, including after a rejected first send. An
actual disposable macOS PTY (no provider) verifies missing default config and
an explicitly selected empty config, source paths, Japanese first-send failure,
and retained setup text after a full repaint. ANSI/OSC cursor/hyperlink updates
are accounted for rather than requiring repeated unchanged text in raw diffs.
Paths with spaces are exercised. Empty-key presence and keyless behavior are
also checked independently of UI rendering.

No Windows/Linux physical terminal or particular Ollama/LM Studio release is
qualified. The keyless example requires a server supporting catalog plus
Responses; a catalog GET cannot prove chat compatibility or billing behavior.
AGENTS existence display, OAuth/keychain, provider-specific templates and an
interactive wizard remain outside this first version. Explicit missing/unreadable
config files retain the existing strict loader error; use `config init` with the
same `--config` before startup. No existing configuration or installed binary
was changed by these checks. Installer guidance is a source/output change for
the next release; old v0.3.0 assets are not modified.

### Path-safety follow-up and local rerun history

The initial seven setup tests are followed by a Unix-only non-Unicode path test
(eight on Unix, six on Windows). A disposable-HOME reproduction found a JSON
serialization panic when init reported a non-Unicode configuration path.
`eed67fe` rejects that unsupported path before file I/O and uses display-safe
UTF-8 for diagnostic directory paths. All eight focused tests pass locally;
no user path/configuration was used or modified.

The local combined check at `453c5e0` failed the existing unpaused-index repaint
assertion in `real_tui_accepts_text_renders_proxy_response_and_exits_cleanly`;
`bb7e29b`, differing only in documentation reconciliation, passed the full
standard rerun without Rust/test changes. Its exact-source three-OS main CI
[41077938813](https://github.com/tya5/shellboy/actions/runs/41077938813) also passes.
The same pre-existing repaint assertion recurred in the default-concurrency
local full check at `eed67fe`, while setup tests, fmt, Clippy and other tests
passed. No timeout, assertion or test skip is changed. A bounded full recheck
uses `RUST_TEST_THREADS=2`; report that environment/result separately and do not
claim it diagnoses or fixes the repaint issue. Final-source ordinary main CI
remains a distinct gate for the path-safety follow-up.

### #69 review follow-up

Replace short elapsed-time assumptions with 60-second watchdogs and committed
state notifications. Count pending mock connections only after the CLI finishes
or the fixture owner and all owned work shut down; no sleep loop or 100/150ms
negative observation remains. Partial template-write injection verifies cleanup,
retry and non-overwrite. Catalog aliases not advertised by the mock now succeed
with `model_advertised=false` and warnings, per #68 D-009. Eight focused Unix
integration cases and the partial-write unit test pass locally; ordinary final
three-OS CI is still required separately. No paid/provider probe is added.

## #60 submission retention qualification

Synthetic macOS temporary DB/loopback fixtures only; no installed binary, user DB
or paid provider. Full `RUST_TEST_THREADS=2 sh scripts/check.sh` passes1688/0/32, layout89,
Python6, formatting and strict all-target Clippy. Exact frozen-copy metadata validation, source-revision fences and
bounded four-field receipt markers preserve cancellation archives. Sixteen repeated
submitted/admitted sends each release at least8192 logical bytes and leave no
submitted row; exact replay and reopen retain the markers. Old rows are collected
128 then2 through the real history GC path. Unknown/submitting copies survive DB
reopen. Cancelled text and attachment position restore without a model send after
copy collection and ordinary GC; late autosave is suppressed, newer edit accepted.
The initial full check failed two old frontend expectations that all accepted
copies remain forever. Their replacements retain canonical-text/trace/edit/zero-
shell assertions, assert receipt binding and admission, and require copy retention
when admission is still uncertain. Final exact main CI remains pending. No
physical-terminal crash or real-provider qualification; receipt watermark scan scalability is unmeasured.


Combined with #69 base bd75c9a, exact #60 source7d34cbe passes the full standard
check1698/0/32, layout90, Python6, fmt and strict all-target Clippy on native macOS
arm64/Rust1.99.1 with RUST_TEST_THREADS=2. The first combined full run failed the
existing tui_process unpaused-index repaint at tests/tui_process.rs:137; the
same-SHA/same-environment full rerun passed without Rust/test changes. Preserve
that failure independently; no timeout/assertion is weakened or failure cause
claimed fixed. Only appended documentation conflicts arose during rebase and
both #69 and #60 histories are retained; runtime patches are unchanged.


## #60 indexed fence follow-up

Thread isolation and EXPLAIN QUERY PLAN are checked against the actual generated
fence SQL, requiring SEARCH binding USING INDEX receipt_target with a target-
Thread equality and rejecting a binding table scan. No schema/index is added.
Native macOS arm64/Rust1.99.1 RUST_TEST_THREADS=2 standard check passes1699/0/32,
layout90, Python6, fmt and strict all-target Clippy. Native main CI remains
a separate pending gate; prior #60 results are not substituted.

## Chat Completions dialect (#64)

2026-10-31. Native macOS ARM64, Rust 1.99.1. Six focused integration tests in
`tests/chat_completions.rs` and synthetic recorded wire traces in
`tests/fixtures/chat/` pass. `RUST_TEST_THREADS=2 sh scripts/check.sh` passes all
local tests, layout, Python gates, fmt and strict all-target Clippy, including
the existing Responses suites. Dependency versions/lockfile are unchanged;
test-only Tokio test-util enables controlled time rather than a short wall-clock
negative assertion. Core schema/tables/workers/queues are unchanged.

Observed with no provider credentials and only loopback mocks:

- Request projection preserves instructions, images, assistant text plus grouped
  calls, associated tool results and effort; reasoning/encrypted replay is absent.
  Responses projection is unchanged. Default API, strict enum/config validation,
  API-root suffixes and unchanged catalog/embedding paths are exercised.
- Literal SSE traces are fed across byte/chunk boundaries; multiple calls arrive
  interleaved by index and arguments concatenate. Text/JSON success, incomplete
  length and EOF, explicit context error, invalid JSON message/choice, metadata-only
  empty text and ignored post-terminal junk retain the correct terminal facts.
- Actual HTTP requests target /chat/completions with existing Thread session headers.
  HTTP 400 context codes, 429 and 503 preserve status/classification. A socket held
  open after a text delta uses a paused/advanced test clock to prove the shared
  inactivity timeout; 60-second watchdogs and notifications bound waits.
- A usage footer received before a lost DONE remains on the failed attempt. Valid
  counts map to existing scalar fields; missing counts and missing usage stay
  unknown rather than zero or a synthesized total. No catalog/error sentinel is
  used as a credential or real model.
- Real Core handles 503/429, executes two harmless echo tools, sends their results,
  then handles a second Turn's explicit context error via the existing adopted
  reduction path. Tool groups remain atomic. Saved cached usage is 200 with one
  unknown completed-attempt counter, as supplied by the fixtures; usage get and
  the common F6 field projector expose that distinction. Persisted input-character
  sizes match actual serialized Chat messages, not discarded reasoning items.
  Model-list output exposes the chosen API and unavailable reasoning features.

No real Gemini, DeepSeek, Mistral or any other provider, paid/cache-effectiveness
probe, native Anthropic/Gemini API, physical Windows/Linux terminal, or installed
owner/binary update is performed. Gemini's guide example is unverified. Vendor
reasoning_content/opaque tool metadata and generic extra_body/cached_content are
not implemented; only the documented standard shape is qualified. These mocks
cannot establish real-provider compatibility, caching or billing behavior.
Final combined-source three-OS main CI remains independent evidence.

### Related #69 reviewed main gate

Review fixes and the path-safety follow-up landed at `bd75c9a`. Main CI
[41081171111](https://github.com/tya5/shellboy/actions/runs/41081171111) passes:
macOS/Linux succeeded initially; Windows attempt 1 timed out in three existing
`revisit.rs:109` waits (`pending_revisit_waits_for_owned_background_shell_then_fires`,
`registration_racing_normal_completion_is_either_delivered_or_rejected_as_terminal`,
`stopping_a_held_request_drops_pending_without_firing`). Same-SHA failed-job rerun
attempt 2 succeeds. No unrelated test/assertion was changed; rerun success does
not diagnose or fix those failures. Earlier local TUI repaint failures and the
separate initial main gate are retained in the #69 record.

## #65 noninteractive exec qualification

Nine native CLI loopback-owner/mock cases pass locally on macOS ARM64 using
only temporary stores: stdout exact UTF-8 JSON with stdin/Schema instruction,
reasoning excluded, default external_cli sender label, two saved repairs and raw
failure, repair success, recovery without implicit retry, timeout leaving the
pre-existing Run and queued input alive, and input appended before a real shell
tool boundary on an active Turn. Absent owner and invalid/unsupported Schema
have distinct stderr-only failures and create no owner data directory.
Native macOS arm64/Rust1.99.1 RUST_TEST_THREADS=2 full standard check passes
1713/0/32, layout91, Python6, fmt and strict all-target Clippy. Four Schema unit
regressions cover unsupported semantics/depth, shape/Unicode constraints, exact
integer lexemes, escaped keys/duplicate semantics and work exhaustion. Final-only
pagination/cursor separation and unchanged ordinary history/serialized defaults
pass. The native UTF-8 case checks the complete70KiB exec output while ordinary
history get remains64KiB-paged. An earlier expanded fixture incorrectly compared
that first ordinary page to the whole answer; its corrected assertion requires
the true prefix/cursor while retaining complete exec JSON equality.

Numeric spelling is preserved verbatim instead of reserializing through f64.
A temporary local raw_value-feature experiment was discarded to avoid affecting
Core serde behavior; Cargo.toml/Cargo.lock are unchanged. The final bounded
scalar-span index follows complete existing serde_json syntax validation and
checks decimal integrality exactly. Out-of-range parser numbers/resource limits
fail explicitly; no unresolved Schema keyword is silently accepted.
No real provider or physical-terminal execution is used. PowerShell7 example is
documented; native Windows/Linux qualification and exact main CI remain pending.


## #65 combined-source and native qualification (restored)

Source6b2f5c9 combined with Chat maine65baaa passed the complete local standard
check1719/0/32, layout92, Python6, formatting and strict all-target Clippy on native
macOS arm64/Rust1.99.1 with RUST_TEST_THREADS=2. This reconstructs the intended
872fbf7 qualification from its status delta and the retained exact-source local
check evidence, not from the corrupted acceptance copy.
Main872fbf7 CI41098996000 passed Linux/macOS on attempt1. Windows attempt1 failed
storage::cancelling_shutdown_observer_still_closes_and_joins_writer at its2s
shutdown guard; only the same-SHA failed job was rerun and attempt2 passed.
The failure is retained, not diagnosed as fixed. Separate GitHub API502/status
observation lag was not a workflow rerun or test result. No live provider or user
store/binary was used or changed.

## Fact-preserving Chat tolerances (#64 review / D-010)

2026-10-31, native macOS ARM64, Rust1.99.1. Ten focused Chat integration tests
pass, including four new credential-free synthetic SSE traces: complete calls
ending with stop, repeated full shell names, missing call ids and usage chunks
without choices. Feed variants at different byte boundaries; check original
finish_reason and normalization markers, parse complete arguments, preserve
reported usage and unknowns, and keep generated ids stable across chunk sizes
and JSON/SSE decoding. A golden id locks the response-id/index derivation, and
another response or index produces a different id. Replay sends the same ids
for assistant calls and tool results; synthesis is not mislabelled provider data.

The real loopback Core flow now uses stop-terminated complete calls, executes
both harmless echo commands, and verifies the accepted original finish/marker
in persisted assistant_response output, not only a temporary HTTP envelope.
Normalization metadata stays in the normal saved output. An empty usage-only
answer uses an empty message as its fact carrier, not invented text.

Negative cases cover truncated/empty/non-object arguments, empty names, both
response and tool identity absent, a no-choices chunk without usage, and partial
name fragments that must not be repaired into shell. Existing Responses and
Chat retry/reduction/cache-usage/timeout tests remain unchanged in behavior.
Local full standard checks use RUST_TEST_THREADS=2; final rebased source must
also pass the ordinary three-OS main CI gate. No live provider, billed request,
real user config/data or installed binary is used. Real-provider compatibility
is still unverified. Capacity classification is intentionally unchanged: only
recognized string codes, not numeric Gemini/generic OpenRouter/message guesses.


### Bounded Chat audit identity and finish reason (#64)

Source3733142 bounds response IDs to1024 UTF-8 bytes before their audit facts can
be repeated across up to1024 tool calls. Source4e2b71c similarly bounds finish
reason strings to64 bytes before duplication. Both extend the existing negative
case d010_does_not_accept_incomplete_calls_or_guess_missing_response_identity:
a1025-byte response ID and a65-byte finish reason must report input_limit.
These commits did not originally append separate acceptance sections; this entry
reconstructs their coverage from their reviewed design/test diffs, without claiming
an unrecorded standalone run. Exact source4e2b71c main CI41100386417 passed all
three OSes. The prior7c91f15 validation appendix is preserved verbatim above.
No real provider, paid model request, user configuration or installed binary was
used or modified for this synthetic qualification.

## Validation document restoration (#65 High follow-up)

The complete3078-line acceptance record from6b2f5c9 is preserved as an exact byte
prefix, with intended #65 combined-source qualification and the later #64 review
appendices restored after it.872fbf7 had copied status into acceptance; this was
a documentation write error, not intentional pruning. Status was audited against
its6b2f5c9/872fbf7 deltas: only intended overview/qualification counters and the
#64 overview were present, not the displaced detailed validation sections.
Standard Python checks now enforce separate document role headings and content.


## #65 old-owner/final-answer support guard

The client probes final_answer_only decoding with a random missing-node read
before creating a Thread or sending work. Expected target_unavailable proves
argument acceptance; unsupported/closed-wire failures remain owner errors.
There is no Core compatibility parser, automatic downgrade, owner start, transport
mutation replay or schema-repair resend after a final-read rejection.

Synthetic loopback RPC rejection modes cover an old-style owner refusing the
argument before work (exit6, zero Threads/receipts/model attempts), and support
lost after sending (exit6, complete message/Run/Turn/answer IDs, original answer
still readable through ordinary history get, one receipt/attempt and no resend).
The existing nine exec cases still pass, including real UTF-8/paging, active-Turn
append, recovery and non-stopping timeout. A unit check covers InvalidArgument,
ProtocolMismatch and IoFailed final-read mapping while keeping ordinary input
errors as2 and missing-node errors as4. Native macOS arm64/Rust1.99.1 full
RUST_TEST_THREADS=2 sh scripts/check.sh passes1726/0/32, layout92, Python7, fmt and
strict all-target Clippy. Native main CI remains a separate pending gate.
This is a portable simulated legacy rejection, not an installed0.3.0
owner or real-provider run. User data and installed binaries are unchanged.

## OSC7501 program status (#54 / D-011)

2026-10-31, native macOS ARM64, Rust1.99.1. Source initially based on main4e2b71c,
then rebased onto d5f38d9's restored Validation record before appending this entry.
The erroneous status-as-acceptance file is not restored or reused. Rebase to this
point has no code conflict; terminal_links.rs, conversation draw/status-row code
and activity::classify are unchanged.

Eight focused library tests pass: standard-base64 and decoded UTF-8 bounds,
whole4096/title192/msg2048 limits, strict id grammar/length/depth, C0/C1/control/
bidi/default-ignorable removal, exhaustive ActivityPhase mapping, root priority,
32-child overflow and exact all-thread counts, wire diff/idle/eviction/root clear,
strict on/off configuration, visible-only completion acknowledgment, stale auth
attempt generation and actual HyperlinkBackend byte capture outside cell frames.
No progress/query/heartbeat is emitted. Seen metadata is session-only and pruned
when latest completion or stored Thread changes.

Native Unix PTY integration (temporary HOME/config/data-dir and a loopback-only
Responses mock) passes on macOS. It holds the first mock response until working
is observed, executes a real shell that prints a forged OSC7501 error/app/id,
and sees the shell bytes in saved tool-result input, never as terminal records.
It observes hidden completion done, clear on selecting that Thread, HTTP401
blocked with auth kind, normal-exit id-less clear and clean process exit. The
same flow with ui.program_status=off emits no OSC7501 at all. Condition-based
PTY readiness/oneshots and60-second watchdogs are used; no short negative window
or sleep polling is used in these regressions. Cell-frame capture independently
shows sanitized before/after content and no forged report through the renderer.

An installed tmux3.6a was checked with a dedicated temporary socket/server,
empty config and disposable HOME: print before, valid OSC7501 working, after;
wait-for notifications delimit the capture. capture-pane contains beforeafter
and no protocol payload; the isolated server is then released/stopped. This is
real tmux parser smoke, not a GUI/whole-Shellboy receiver qualification. No user
session/config is changed. Installed herdr and TERM_PROGRAM=WezTerm were observed,
but receiver status UI/visual behavior was not qualified. kitty/cmux/herdr,
Terminal.app and WezTerm physical receiver behavior remain explicitly unverified.

Local full check RUST_TEST_THREADS=2 sh scripts/check.sh passes before the
metadata-record rebase; final rebased check and ordinary three-OS main CI remain
independent gates. No new package/version, schema, worker/poller/process or queue
is added to production. All request/owner/data fixtures are synthetic; no real
provider, installed shellboy binary, real owner or private conversation is used.


## OSC7501 persistence isolation and failure retry (#54 review)

Native macOS ARM64, Rust 1.99.1, 2026-10-31; source based on main fdaec1c.
`RUST_TEST_THREADS=2 cargo test --locked --lib frontend::program_status -- --nocapture`
passes all three tests. This includes the existing overflow/priority/seen test
and two new regressions:

- A read-only open failure does not create a missing database, preserves seen
  identities, emits no replacement snapshot and marks status dirty. Retrying at
  the next simulated coalescing admission, without a Changed event, publishes
  the repaired store and prunes obsolete seen identities. The production result
  arm uses this same acceptance helper; existing tick admission remains intact.
- A temporary WAL store contains exactly 10,000 nondeleted Threads: 9,992 historical
  completions plus live/stopping/recovery/failed/fresh/seen/stopped/never-run cases.
  There are 9,999 Runs, Turns and model attempts. With the real persistence worker
  deliberately occupied by an uncommitted Run update, ten production read-only
  projections complete. Each returns working 2 / blocked 1 / error 1 / done 1 / idle 9995,
  five child records and the exact acknowledged completion identity. The
  uncommitted update is never seen; after release/commit, the next read returns
  working 1 / error 2. Writer shutdown and a subsequent TRUNCATE checkpoint return
  successfully with no retained reader. A oneshot and a channel delimit the
  writer hold; 60-second watchdogs, not sleeps or negative timing windows, guard
  the flow. Holding a writer job open is a synthetic test technique only.

The ten observed per-read durations in microseconds are 100828, 109280, 109936,
112886, 106621, 110635, 110225, 115314, 120397, 111846: mean 110796us, minimum 100828us,
maximum 120397us. The timer includes presentation admission/scheduling, read-only
connection open, transaction, full SQL/projection, retained-child title reads,
commit and connection close. This is the ordinary debug test profile, repeated
against the same temporary database, with concurrent local compilation activity;
it is not a cold-disk or release-binary benchmark and no strict timing threshold
is asserted. It still inspects 10,000 Thread rows per projection (O(Thread count));
this fix chooses the review's read-only-connection alternative rather than a
candidate-index migration. It removes that scan from the persistence queue, not
all CPU/I/O contention. The existing two-slot blocking-pool permit stays with
admitted work until it completes, even if its async observer is cancelled.

Projection uses SQLITE_OPEN_READ_ONLY plus query_only, a 250ms busy timeout and
one deferred read transaction; there is no writer-pool call, migration or new
worker/queue/process/poller. Existing classifier, output encoder, frame boundary,
conversation/status-row rendering and off-mode admission are unchanged. All
fixtures use temporary data; no real provider, private conversation, installed
binary or user settings are used. Physical terminal receivers remain unverified.
The full local standard check and final rebased three-OS CI are separate gates.

On source 2b9bd88, `RUST_TEST_THREADS=2 sh scripts/check.sh` passes all targets:
1,737 passed / 0 failed / 32 ignored, integration layout 93 suites, Python 7,
fmt and strict all-target Clippy. The native disposable-PTY on/off/model-shell
OSC isolation regression passes again through the new read path. The final
rebased full standard check and ordinary three-OS main CI are still independent
gates, not inferred from these local observations.


## v0.4.0 preliminary release preparation (#70)

Public v0.3.0 macOS archive and SHA256SUMS were downloaded anonymously with curl
--disable over HTTPS. Manifest hashc8a139f704027220fd51bc173e54aa0e2fbb67fd35e0f9b4eb3859714b447611,
archive1e0010db3a34cd6c159e591d92ffac64062ce793f767972f0518346522d8f5d4,
executable2e9b1c76b6cb959852eaf2f7283051f186f6ca19acf7e4b2168f0b52e09372bb.
It reports shellboy0.3.0; it is public bytes, not a tag rebuild or old candidate.

On native macOS ARM64, the real0.3.0 owner created21 synthetic nodes, completed
work/indexes, one recovery-wait Run and a queued message sent by its real model
shell/issued Origin. Standalone keyword RAG was also created by that executable.
After a fixture-only hard stop and WAL-aware backups, a staging0.4.0 debug binary
opens the copy with schema14 and the complete migration ledger unchanged, RAG3,
conversation search, legacy unknown/model sender facts, held pending input,
old/new saved usage and JSON stdout exec through the loopback mock. New exec
sender is external_cli/self-asserted shellboy exec; owner shuts down0. Five model
and12 embedding calls are local fixtures, not providers or user data.

Initial harness attempt stopped during read-only backup of a checkpointed RAG
file marked WAL but lacking sidecars, before starting0.4.0. The fixture now uses
immutable read-only access only for stopped files with no WAL; conversation WAL
is never ignored. A fresh fixture passes all checks. That initial harness error
is not classified as a product migration failure or hidden as a passing run.

The local macOS installer prototype passes fresh/replacement/backup/checksum
rejection using a real debug executable and stamped scripts. Its other-platform
archive entries are explicit nonnative placeholders, so it is not candidate or
cross-OS qualification. The final matrix must use the actual assembled release
archives and repeat native execution. New Python unit cases reject damaged or
rehashed-but-unbound/template script sets. Ordinary local checks pass1735/0/32,
layout92, Python9, format and strict Clippy on the initial staging tree; the final
rebased source, packaged upgrade, three native installer OSes and exact main CI
remain pending. No tag/GitHub Release/publication/installed-binary change occurs.

## v0.4.0 publication verification (#70)

2026-10-31 JST. Source/tag target is exactly
`dae2ba1fcc46c635edf1b88973ad306dd80960d4`; ordinary three-OS CI 41115910158
and tag-triggered build/publication 41138064021 both pass without reruns.
User-approved issue #70 revised notes equal the published body and retained file.

Native macOS 26.3.1 arm64/Rust 1.99.1 `sh scripts/check.sh` passes before the tag
push: 1,737 passed / 0 failed / 32 ignored, layout 93/Python 9, fmt and strict
all-target Clippy. Isolated validation branch also passes the full standard check
before push. It changes only the read-only manual workflow and public smoke
fixtures, not runtime code, and is not merged into main.

Anonymous curl downloads with curl configuration disabled retrieve all six
public files and match the manifest and independently pinned hashes. All three
archives extract (ZIP integrity checked); the local public macOS binary reports
0.4.0. The actual latest shell installer passes fresh/replacement installation
in a temporary directory with spaces; its backup matches previous binary bytes.
Both latest install scripts equal version-specific v0.4.0 assets.

Native public-byte CI 41139816831 at isolated source
`b28fa7b0d1f8f55f83272bf502d510c49990db86` passes Linux x64, macOS ARM64,
Windows PowerShell 5.1 and PowerShell 7. Each job downloads all six public files
anonymously, verifies pinned SHA-256 plus manifest, extracts its native archive,
runs --version/--help/docs list, checks latest scripts, and invokes the actual
public one-line installer twice in a disposable install path. Every installed
and extracted native version is shellboy 0.4.0, and replacement preserves a
byte-identical backup. There are no failed jobs or reruns.

Exact hashes and observed environments are retained in the
[publication record](../development/publication.md#v040-publication-2026-10-31-jst).
No owner is started/restarted, no real database/configuration is accessed and
no paid provider is called. These public-byte checks are not three-OS synthetic
upgrade tests or qualification of all physical terminals/real providers.
A later separately authorized user installation must not restart the live owner.


## Semantic retrieval guidance and omission notices (#71)

2026-10-31, native macOS ARM64/Rust1.99.1, source4711c89 on base fcb8d78.
`cargo test --locked --lib compaction::edit::` passes28 tests, including saved
marker checks for configured/unconfigured embeddings and explicit/maximal
omission. Default/unconfigured notices remain keyword-only; configured ones add
the exact history search command, embedding-request cost and coverage caveat.
Tests preserve original node endpoints and side-effect warnings. Valid embedding
settings use a synthetic loopback URL but make no HTTP/model/embedding request.
`cargo test --locked --test integration docs_cli::agent_semantic_search` passes
with the guide extracted through the actual worktree executable's owner-free
`docs get --id shellboy/agent-guide --section when-semantic-history-search-helps`.
The five situations, cross-conversation safety/cost notes and all three examples
are checked; commands from both saved notices and the bundled guide are parsed
by the actual CLI argument parser (including global search, Thread scope, hybrid
history RAG and rag get --ref). The binary needs neither config nor a data-dir.
Both new async tests have only a60-second watchdog; no sleep, timing comparison
or real-provider experiment is introduced.
`RUST_TEST_THREADS=2 sh scripts/check.sh` passes1,739/0/32, layout93, Python9,
fmt and all-target strict Clippy. Rebased full check and ordinary three-OS CI
remain independent gates. Search efficacy is not established by a mock: the
reviewer's read-only one-week plan in issue71 comment6535408537 separates
attempts/results, explicit cross-Thread searches, global cross-Thread usage,
post-reduction retrieval and evidence actually used in decisions. No telemetry,
indexing-policy change or user-data scan was added for this observation plan.

Initial guidance-only line-count audit against fcb8d78: status1258 -> 1274 (+16/-0);
acceptance3335 -> 3366 (+31/-0). Both originals remain exact byte
prefixes; role headings are preserved. No validation history was replaced.


## Independent auto-history policy (#71 required4)

Added after the initial guidance gate, on native macOS ARM64 / Rust1.99.1.
Two new60-second-watchdog integration tests exercise real temporary SQLite/vec
and a loopback embedding mock. No latency or negative-observation window is a
pass condition. With auto_history=false, new node/raw admission and previously
pending automatic work send zero embedding requests. Existing vectors survive;
search explicitly returns indexed-only/degraded coverage, including empty results.
Explicit backfill reauthorizes matching held jobs/bootstrap; rebuild creates a
new active space while preserving old vectors; probe remains available. Mock
request counts distinguish these explicit charges from prohibited automatic work.
Standalone material semantic indexing/search remains functional with embedding
settings retained; history-scoped hybrid RAG warns of disabled coverage, whereas
standalone material search is not degraded by this history-only setting.
Config tests check strict bool/default true and config check's flag; IndexStatus
reports it. Existing TUI search and caller-disconnect suites pass with the new
HistorySearch result object, whose hit/provenance fields are unchanged.

The first full run failed in the claim unit's minimal in-memory DDL because it
omitted the existing authorized_watermark column now read by the policy filter.
A focused run initially still used that earlier fixture; the DDL was then aligned
with current schema and the focused test passed. No runtime schema, migration,
compatibility fallback, test timeout or retry suppression was added.
Full `RUST_TEST_THREADS=2 sh scripts/check.sh` passes after the fix (1741/0/32),
layout93, Python9, fmt and strict all-target Clippy. Rebase and final three-OS
main CI remain separate gates. Real provider/cache/billing behavior and actual
one-week usefulness are unverified. Existing indexing policy and metadata fields
are reused; no new telemetry, queue, worker, dependency or user-data modification.

Final record audit against base fcb8d78: status1258 -> 1289 (+31/-0);
acceptance3335 -> 3400 (+65/-0). The old files remain byte-for-byte
prefixes; role headings and all prior histories are retained.

## #73 bounded explicit summary qualification

2026-10-31, native macOS 26.3.1 arm64/Rust 1.99.1. Full standard check
`sh scripts/check.sh` passes: **1,745 passed / 0 failed / 32 ignored**,
layout93, Python9, format and strict all-target Clippy. New configuration, CLI
mapping/owner validation and config-check diagnostics pass in the normal suites.
Only temporary synthetic stores, local HTTP mock responses and disposable
JSONL request logs are used. No user data/configuration or real provider call.

Verified explicit-summary boundaries:

- Four chronological source parts (about 248,000 source text characters) become
  one summary via three intermediate reduce batches and one final reduce.
  The mock JSONL record grows from 0 to 8 lines (four map + four reduce); result
  model_requests is 8, final purpose/constraints appear once, latest published
  state/decision/reason/reference n3 survive, and replay input shrinks. Each
  reduce input is at most 64,000 Unicode characters. Semantic behavior here is
  the mock contract and inspected prompts, not proof about a real model.
- A 2,001-character nonconverging response with effective 2,000 limit is retried
  through four stronger reduce rounds, then rejected with original path and
  source/summary/limit/request facts. Log 0->5 lines; identical explicit retry
  reuses validated raw audits, log stays 5 and model_requests is 0. No truncation
  or partial adoption. A nonshrinking tiny-source summary is also rejected.
- Both 65 map segments and 63 map segments followed by an over-limit reduce
  stop at exactly 64 calls/log lines. The shared budget includes map and reduce;
  original paths remain selected. No 65th request is sent.
- Default 20,000/5% minimum, absolute-only, percent-only, combined limits,
  fractional percentage, 2,000 floor, Unicode character-vs-byte accounting and
  per-call overrides are checked. Zero/negative/nonfinite/out-of-range/unknown
  settings are rejected; limits on nonsummary actions cannot be silently ignored.
  Config check exposes resolved values and rejects invalid limits before a
  catalog request. No paid chat probe is added.
- Existing hidden-source provenance/late-adoption cancellation tests pass; the
  whole-tool-group fixture now includes its final reduce request. Omission
  notice tests assert leave-omitted guidance, bounded history lookup, capped
  opt-in summary and the context-expansion warning. Automatic policy is unchanged.

New summary tests use only watchdog deadlines, no elapsed-time thresholds or
sleep-based correctness claims. Development checks caught stale old notice-text
expectations and a CLI Boolean-group requirement that alone did not enforce
summary-only flags; explicit local and owner checks plus updated assertions fix
those cases. Later full checks pass; these are not suppressed or ignored tests.
Native Windows/Linux results are tracked in issue #73 separately from local checks. Real-provider
summary accuracy, very large real histories and actual paid request billing are
not qualified by these loopback mocks.


After rebase onto `a5a08b1` (#71), the full standard check passes locally again:
**1,749 passed / 0 failed / 32 ignored**, layout 93/Python 9/fmt/strict Clippy.
Config check retains both auto_history and compaction facts. Omission guidance
retains #71's keyword baseline, conditional semantic hint/cost/coverage warning,
and new #73 capped opt-in summary. All old conditional-guidance tests remain.
Append-only documentation conflicts and adjacent single-line output/notice
conflicts were resolved by keeping both contracts. The first integration edit
accidentally removed file tails; parse checks caught this before push. Complete
upstream files were restored with only scoped #73 changes; the successful full
integration check follows that repair. No truncated code or tests are accepted.

Record audit against the new base `a5a08b1`: status 1289 -> 1298
(+9/-0), acceptance 3400 -> 3464 (+64/-0).
The complete previous status body and byte-for-byte acceptance prefix remain;
no historical evidence or role heading is replaced. Exact-source native CI is
recorded in issue #73 separately from these local checks.

## #41 D2-only main7393ccb integration qualification (2026-10-31)

Candidate37e0929 contains D2 through4ce6913 only, merged onto main7393ccb.
No E/F/G modules or schema15/16 migration is admitted. macOS arm64, Rust1.99.1,
`CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 RUST_TEST_THREADS=2 sh scripts/check.sh`:
first run passes library911/0/13 and integration906/0/17, then fails termination
6/1/1: `actual_parent_shell_pty_close_kills_term_ignoring_owned_groups_without_force`
finds no shutdown scope=signal diagnostic (all preceding saved-cleanup assertions
passed; bash/zsh retired after about2.02s). That run is not a successful standard.

At identical source the focused native PTY rerun passes1/0, then a full standard
rerun exits0: library911/0/13, integration906/0/17, termination7/0/1, crash3/0/1,
combined1827/0/32, layout93/Python9/fmt/strict all-target Clippy. Private logs are
`.local/issue-41-d2-admission-standard.log`, `issue-41-d2-termination-recheck.log`
and `issue-41-d2-admission-standard-recheck.log` in the development root. The
intermittent diagnostic cause is unproven; rerun success is not a causal repair.
Historical D1/D2 P2/P5 figures retain their original source scope and do not
qualify latest-main CPU behavior or another OS. Main CI/Windows terminal/new
long measurements are unexecuted. #71's actual auto_history-off integration
tests are retained and included in the passing standard.

## Held automatic-index status and wire defaults (#71 Low review)

2026-10-31, native macOS ARM64 / Rust 1.99.1, based on main 7393ccb.
`cargo test --locked --test integration index::auto_history -- --nocapture`
passes 2 tests; `cargo test --locked --test integration protocol::index_status_additive`
passes 1. The mixed fixture contains a pending source authorization, bootstrap
input, automatic vector job and explicit work. On/off projections distinguish
pending from held_auto_history, including selected-space exclusion of pre-space
metadata. Explicit reauthorization moves applicable entries back into pending;
completion yields zero held and pending entries. Synthetic running/failed rows
retain their counters. Status observations do not increment the loopback proxy's
request count; only the four explicitly authorized inputs send requests.
The existing automatic-off/bootstrap regressions are retained, with held entries
now asserted separately instead of as pending. All async additions have only a
60-second watchdog, no latency condition or short negative observation window.

Wire tests cover omitted/default fields, explicit true/false and nonzero held
counts, round-trip output and invalid value rejection. Configuration still
defaults true; missing response fields use only the explicitly requested serde
defaults, not a fabricated observation of an old owner's effective policy.
Unrelated required fields remain required. D-016's accepted HistorySearch object
shape is unchanged. No real provider/user data, installed binary, owner restart,
new worker, queue, schema or timing suppression is used. Full standard checks,
final rebased source and native main CI remain independent gates.

The full local `RUST_TEST_THREADS=2 sh scripts/check.sh` passes: 1751 / 0 / 32 passed/failed/ignored,
layout93, Python9, fmt and strict all-target Clippy. Rebase and ordinary native
main CI remain separate gates. Record audit against starting main7393ccb:
status1298->1310, acceptance3464->3496; both original byte prefixes
are preserved, with zero deleted lines in either historical record.

## #73 review corrections / D-017

2026-10-31, native macOS 26.3.1 arm64 / Rust 1.99.1. Full standard check
`sh scripts/check.sh` succeeds: **1,755 passed / 0 failed / 32 ignored**,
layout 93, Python 9, format and strict Clippy. All summarizers in these tests are
loopback mocks; no real provider, installed binary, owner or real data is changed.

- Stable pre-#73 map prompt/key/raw validation are reused. Raising output limits
  after an over-limit attempt reuses its map cache: log remains 5 lines and the
  next result reports zero model requests. Reduce policy identity remains separate.
- Map/reduce request defaults are 128/16 and independently configurable (1..4096).
  Preflight with 3 segments / 1 cached / 2 uncached and map limit 1 refuses with
  counts and guidance, original path and zero requests/log lines. After another
  legacy-key cache seed, 2 cached parts plus 1 new map and 1 reduce succeed:
  log 0->2. Cached maps do not consume map budget. A 65-part range succeeds
  under defaults with log 0->66 rather than failing after paying 64 requests.
- A one-map/one-reduce budget stops at log 0->2 independently. Context-limit split
  attempts consume runtime map budget; unknown provider capacity cannot be
  predicted by preflight. Four reduce rounds remain a separate convergence bound.
- Automatic stage 2 uses identical size/reduce limits. A real temporary owner and
  loopback proxy show a 2,001-character map/reduce response rejected at limit
  2,000: two summary request-log lines, zero summary nodes, recovery_wait and
  exactly two conversation attempts (no unchanged resend). No new table/worker.
- The omission notice drops hardcoded size flags and preserves conditional #71
  semantic guidance. Config check exposes both request budgets; validation rejects
  zero/negative/noninteger/excessive values. A request-budget-only section retains
  D-015 output defaults; specifying a single output limit keeps its old meaning.

New tests use 60-second watchdogs, not elapsed-time pass thresholds. Initial
checks correctly rejected old tiny-source success fixtures under the newly shared
nonshrinking rule; those fixtures now use longer synthetic originals or shorter
mock summaries. Their raw/refusal/provenance/tool-once assertions remain. The
existing automatic overflow success cases and new rejection case all pass.
Exact-source three-OS CI is recorded separately in issue #73. Real-provider
quality and billing remain unverified.

### D2-only requalification after current-main #71/#73 corrections

Source2e6973d includes mainb581283 plus reviewed D2 and watchdog-only679774b
(asbd0ff5b). Same native environment/standard command as above; exit0,
layout93/Python9/fmt/strict all-target Clippy pass. Rust915/0/13 library,
910/0/17 integration,7/0/1 termination,3/0/1 crash:1835/0/32.
Private log `.local/issue-41-d2-latest-main-standard.log` in the development
root. The previously failing parent-shell PTY test passes both bash and zsh;
retain the initial failure as unexplained instead of claiming causal repair.
No E/F/G source, migration15/16, paid API, installed update or push is included.
Native main three-OS CI and CI long measurements remain separate unexecuted gates.

### E-only readonly admission: pending native qualification

New focused tests exercise actual browser page/ID-patch and paired current-view
reads while the writer worker is held, plus one WAL snapshot's stale sequence,
pre-open cancellation and retryable query contention. They are prepared, not yet
run on this exact source. This follow-up adds no F/G feature or migration.
Applicable E P3/P5 and native 80/wide checks remain open; existing D2 tests or
moving reads off the persistence queue do not demonstrate those targets.


### #41 E isolated candidate: native proof and qualification still open

Environment: macOS ARM64, Rust1.99.1, native PTY plus fixed-screen decoder;
synthetic temporary data only. E worktree includes main4a6b839; no F/G migration,
paid provider request, default data change or installed binary update.

Six native cases (80/140 columns × dark/light/NO_COLOR) pass external CLI rename,
addition/deletion without explicit reload, selected screen anchor retention,
selectability of the added row and normal exit. NO_COLOR emits no chromatic SGR.
Raw terminal bytes, screen text and binary hashes remain private local artifacts;
the binary digest is75f34ac64f4a00ca6ae84c0ac0795d2d265be0ddf242d54b9f4f3a07fec1cceb.
These observations cover E product source20f424f plus UI-deadline plumbingca27ff3;
subsequent changes are test helper/admission-fixture changes, not a new native
binary claim. No Windows GUI/terminal success is implied.

Initial standard:1880 passed/0 failed/32 ignored, layout93/Python9/fmt/strict
Clippy. Later qualification failures are retained: duplicate helper registration
(Clippy), then `frontend_first_send_is_saved_and_later_draft_survives_shutdown`
(the automatic title was visible before path admission). The latter is being
replaced with an actual admission wait plus a deterministic intermediate-state
regression. A final standard run is required after that correction.

#77 helper bytes are copied exactly fromc2a5c9e into the lib/integration harness
scope. Its current policy scanner finds zero raw test guards in E browser,
snapshot, refresh and frontend_reactivity files. Existing unrelated guards in
`tests/frontend.rs` remain owned by #77; this increment replaces the affected
first-receipt/shutdown and shared-next-update guards, not a file-wide exemption.
P3 actual cost/baseline comparison and P5 CI observations remain open. E is not
main-integrated or pushed, and no ordinary E three-OS CI exists yet.


## #75 saved readback qualification

2026-10-31, native macOS 26.3.1 arm64 / Rust 1.99.1. Local standard check
`sh scripts/check.sh` succeeds: **1,845 passed / 0 failed / 32 ignored**,
layout93, Python9, format and strict all-target Clippy. Exact-source native CI
is recorded separately in issue #75; this local result is not an OS/terminal or
real-provider qualification.

- Compact list combines retained operation results, automatic failure records and
  replacements. Synthetic summary, ordinary spill, automatic omission and restore
  show correct sizes/ranges/current-request omission count. Shared replacement
  operations coalesce with representative ID/count; restore does not merge into
  its old omission. Legacy summary text including Unicode/NUL measures exactly;
  missing metrics remain null. thread get includes the latest retained observation.
- Usage [since,until) selects requests/shells by start and Runs by creation, not
  overlap/proration. Boundary, offset-equivalent/nanosecond, empty-window, active
  and unknown-start cases preserve saved-state aggregation/unknown rules. Read-only
  total_changes is unchanged. Existing slow-only usage logging/trigger tests pass.
- Shell list checks scope, state, exit code, literal command substring, pagination,
  insertion watermark and query-bound cursor rejection. A 2,001-row fixture has an
  empty filtered first page at the 2,000-candidate budget, then a matching final
  page; no false terminal page. Snippets are at most 512 Unicode characters, remove
  controls/bidi, suppress obvious credential shapes, and expose no output body.
  Timestamp keys test offsets, lowercase t/z, nanoseconds, leap seconds and unknowns.
- A loopback endpoint records **requests.jsonl 0->0** while all six heavy-read
  commands complete with the writer deliberately gated. One pre-existing receipt
  stays **1->1**, model attempts **0->0**; no observation receipt/provider call.
  Tests use 60-second watchdogs only, not speed pass/fail thresholds.
- EXPLAIN QUERY PLAN seeks every persistent source for compact/usage/shell queries,
  including spill association, both shell scopes and period filters. SCANs of
  materialized/JSON iterators are not shared-table scans. Scalar time ranges were
  added after the tuple-only expression-index query correctly failed this test.
- dbstat on 5,000 rows per indexed source / 4KiB pages: shell_started_readback
  118,784 B; shell_owner_started_readback 139,264 B; compact_operation_thread
  221,184 B; automatic_compaction_turn 217,088 B. Fixture IDs are short (t/other,
  sNNNNN, opN, aN); UUID/other real datasets differ. These are B-tree sizes, not
  whole-file/schema-text overhead or a storage guarantee. Two shell entries and
  one eligible partial receipt/automatic-attempt entry are the explicit write cost.
- Startup migration15 is index-only; no source rows/body copies or new workers.
  Partial JSON predicates tolerate malformed metadata during index construction.
  Existing quick_check still rejects CHECK-corrupt owner startup after preserving
  its rows; it is not weakened. Old-version fixtures remove the new indexes when
  deliberately constructing old schemas, rather than making migration idempotent.
  Public-v0.3.0 upgrade helper now has explicit expected-new-schema14/15 handling;
  the public-binary upgrade script was not rerun as part of this readback task.

Initial checks found and corrected new-field/exhaustive-match compile errors,
index-seek and credential-header cases, plus old-schema fixture preparation.
The existing frontend::persisted_output_identity_is_delivered_before_each_history_window
watchdog also failed once (Elapsed/RecvError); unchanged isolated and full reruns
pass. Cause remains unproven, not claimed fixed by this task. No retries/timeouts
were relaxed to hide it. No user DB, installed binary, owner or public tag/Release
is changed. Real-provider billing/quality and giant real-history latency remain
unverified; readbacks report retained facts, not recovered deleted data.


### E+#75 complete local standard (a0d8865)

Native macOS ARM64, Rust1.99.1, `CARGO_BUILD_JOBS=2`, `CARGO_INCREMENTAL=0`,
`RUST_TEST_THREADS=2`: `sh scripts/check.sh` passes1891/0/32, comprising lib965/13
ignored, integration916/17, termination7/1 and crash3/1. Layout93, Python9, fmt
and strict all-target Clippy pass. This follows the deterministic pending-title
regression1/0 and the earlier exact-source3e09e48 standard1881/0/32. The two
intermediate finalization failures remain in the preceding record.

The merged main15037c1 includes #75/schema15 and the shared finite presentation
pool. #71 and Program Status source paths have no E changes; #54's conditional
tick guard is identical to main. #75's run41157762817 is green on three OSes,
not an E CI result. E adds no migration and no F/G implementation. E full P3/P5,
physical Windows terminal UX and E main CI remain open.


### E+#75 native follow-up

The explicit `cargo build --locked --bin shellboy` at sourcea0d8865 and all six
native macOS PTY cases pass after the #75 merge (80/140 × dark/light/NO_COLOR).
The binary SHA-256 is8cd9c247d4deb445c0789eaa7ed499b88d1830a7ff86000e78d300a46a599bbf.
Real CLI rename/add/delete updates the open List without manual reload; adding
an earlier row retains the selected screen anchor, changes the count, and makes
the new row selectable by Up. Every fixture exits0; no paid/default-store or
clipboard action occurs. This supersedes the pre-#75 native scope for these
specific behaviors, not physical Windows UX or full performance acceptance.


The measurement-review branch63a7f17 standard run passed lib965 and Python20,
but integration had915 successes/1 failure: the paired-browser fixture saw
browser sequence77 and current-view78. Its initial gate accepted a metadata-only
patch without proving the selected-row excerpt observation had completed.
Initial-work overlap is the working explanation, not a proven product cause.
Wait for the actual selected excerpt result instead; retain the exact one-observation/shared-sequence/no-reload
assertions. No product cadence, timeout or sequence fence is relaxed. New focused
and complete qualification is pending; the original failure log is retained.


Focused follow-upc19e5c1 passes1/0 for the exact paired-header/browser integration
case after requiring the selected excerpt result. Its exact shared-sequence,
one-observation and no-reload assertions remain intact. A full check on the
final review branch is still required; one focused success does not prove the
intermittent cause or complete E's performance gates.


### Stronger E selected-observation gate: complete check

Measurement candidate d3b611d passes the complete standard check1891/0/32,
layout93/Python20/fmt/strict Clippy, including the corrected paired-observation
fixture. Its Rust sources/tests/migrations exactly match E cut42bab84. This is a
new complete result, not an isolated rerun used to hide branch63a7f17's failure.
The initial-overlap explanation remains provisional; all strict assertions stay.

A final guide audit found the old TUI sentence calling the list a fixed snapshot.
Correct that sentence and align both User Guides with reactive saved updates,
selection retention and explicit resync. Guides are embedded, so the final review
candidate receives another full standard check after this documentation change.
No Rust UI behavior changes, F/G implementation, push or release is introduced.

## #75 small redaction and usage-boundary follow-up (2026-10-31)

Native macOS arm64, Rust 1.99.1: targeted command_redaction test passes with
synthetic secret-access-key flags (separate/equals), sshpass -p (bare/path),
mysql attached -p (quoted/unquoted), mixed-case credential-suffix assignments,
and PowerShell environment syntax. Ordinary mkdir -p/mysql --port remain visible.
The existing 60-second watchdog applies; no performance deadline is asserted.
Tests inspect returned strings only: no subprocess, provider, receipt or request
log is produced (zero new request-log rows). Usage guidance now explicitly puts
whole boundary-crossing lifetimes on their start side. This does not extend the
heuristic into arbitrary-secret detection. Exact-source standard/native checks
are recorded in issue #75; no installed binary, user DB or owner is changed.

## Noncontrolling EOF diagnostic observation (2026-10-31)

Main 912b2e2 run 41160166609 attempt 1 failed on macOS because the EOF test required
a scope=signal diagnostic row; Ubuntu/Windows passed. The same-SHA failed job was
rerun as attempt 2 before changing the test; its result is retained in issue #75.

Diagnostics enqueue uses sender.try_lock and try_send, incrementing a dropped
counter on failure. Orderly shutdown closes the sender and joins the writer, so
accepted rows are drained, but rows dropped before admission cannot be recovered
by waiting. The EOF test therefore keeps all read-only DB state, saved tool result,
success exit and native process-group cleanup assertions. It validates any retained
signal diagnostic as terminal_lost/begin, without requiring a row or substituting
log text for durable facts. The other termination tests retain their existing
assertions; this scoped change avoids the worker's separate #77 work. No runtime
logging contract, timeout, transport retry or shutdown implementation changes.

Native macOS arm64/Rust 1.99.1: the exact EOF test passes. Full standard-check,
role-test and exact-source native CI results are recorded in issue #75. Fixtures
use temporary stores/processes, not the installed owner, real data or providers.

## #78 selected README overview (2026-10-31)

The user's latest final-source comment supersedes the old issue-body/attachment
hashes. User selection is candidate C: reduced to 89 colors; mean absolute channel
error from the original is about 1.0/255 (measured 1.0024/255). The user compared
the images visually and confirmed no discernible difference. That is user visual
acceptance, not an assertion that palette quantization is pixel-lossless.

| Image | Bytes | SHA-256 |
| --- | ---: | --- |
| Original final source, outside Git | 1,739,810 | `951579fba21b4c402b08aaf6ad1ad202e3c95ec78478d2ca65e557e7a46c7216` |
| A: lossless comparison, outside Git | 1,444,851 | `2a31b67e6757b4ea27fa4cd3ad6765c781a0994784dbbeb3932e9fe70d6fd66c` |
| B: high comparison, outside Git | 827,044 | `7ad3fc3663799d0998e0d84fb148982689390fb0a179ed1717ec9bc9e85f792f` |
| C: docs/assets/shellboy-overview.png | 621,395 | `eccf659fb93d7751afbc2d886a51ca163f03da24808a73bf36ac1413e2119627` |

Local pngquant 3.0.3: B uses --quality=85-100 --speed 1 (256 colors, reported Q96);
C uses 128 --quality=60-85 --speed 1 (89 used colors, reported Q90), with default
dithering. Dimensions remain 1672 x 941; all candidates are fully opaque. Copying
selected C into docs/assets preserves its SHA-256 exactly. The source and unused
comparison candidates are not committed. No external image/model service is used.

The image is directly after the README title with descriptive alt. Removing only
the inserted image and endpoint sentence reproduces the previous README exactly:
intro and Features are retained. The nearby sentence applies configured embedding
requirements to both history and explicitly configured local documents. The
reviewed fork/source/delegation relationships and final wording are unchanged by
layout edits. No embedded-model, offline/free API, all-files indexing or full
external-document-copy guarantee is added. Models still have one shell tool.

On native macOS 26.3.1, local Chrome 156 headless uses GitHub-like image max-width
CSS on light (#ffffff) and dark (#0d1117) backgrounds. Desktop viewport 1000px
renders the image at 850 x 478; CDP mobile viewport 390px renders 358 x 201. All
four cases load the 1672px-wide asset, retain alt/aspect ratio and have no horizontal
overflow. The opaque light image is not inverted in dark surroundings. Local
on-device Apple Vision OCR retains major headings and the final endpoint note.
Actual GitHub GUI, physical phones and screen-reader behavior are unverified;
mobile fine print may require zoom or the retained text. OCR/layout checks alone
are not subjective visual acceptance; the user's comparison supplies that check.

Standard-library tests check PNG hash, dimensions, palette, chunk CRCs,
decompression, placement and textual alternatives without image-library/network
requirements. Exact-source standard/native CI and text-file line-count audits
are recorded in #78. No runtime, user configuration, DB or installed binary change.


### E formal CI / locally corrected E cut (2026-10-31)

[Issue41 comparison report](https://github.com/tya5/shellboy/issues/41#issuecomment-6538392801)
contains the full12-pair table for run41160746679, source main912b2e2→CBC. All
required source/build/window/cleanup witnesses pass; ordinary checks fail on
Linux(two cases) and Windows(one), macOS1891/0/32 passes. Windows native
measurements are explicitly unavailable. No unchanged-CBC failed-job rerun is
used to hide the initial failures.

P2 both sources:600draws/600s,0UItasks/writerUIjobs/ROopens. Maximum1s-bin draws
are1 except macOS80 both2; the latter has6/35 bins with2, minimum draw intervals
0.826665/0.820072s. P5/P5-list both sources:0draws/UItasks/writerUIjobs/ROopens.
Linux produces49.90–50.00commits/s; Mac16.62–17.77 does not qualify the40load.
Mac140 P5-list retained stream totals (including setup/cleanup) show3171lines /
1208chunks(main),3167/1240(E), so lines and commits are not interchangeable.
CPU values/deltas, unknown attachment-GC callers and measurement-scope limits are
preserved in the linked report/artifacts. No full-P2/P5 acceptance is inferred
from nominal job success or average1Hz alone.

The final E-only local cut68304cc merges current mainc950b44 and passes the full
standard:1891/0/32 (lib965/13ignored, integration916/17, termination7/1, crash3/1),
layout93, Python11, fmt and strict all-target Clippy. The16-change shared-snapshot
case and stronger post-drain raw-save assertion pass. This source is not pushed
or run in new native CI. P3 and laterF/G are not established by these results.

## State-based test watchdog migration (#77)

2026-10-31, native macOS ARM64, Rust1.99.1, based on main b581283.
The single tests/support/watchdog.rs WATCHDOG is60seconds. Async guards label the
awaited operation and caller while returning the original Result; existing
saved-progress diagnostics remain. Synchronous PTY loops and DB barrier receives
reuse that budget. Controlled-clock tests check the helper, Retry-After policy,
heartbeat idle refresh and file-worker timeout ownership without native latency
assertions. Production timer/grace/busy settings and runtime behavior are unchanged.

Non-#41 integration/unit files were migrated in separate Core/transport,
presentation, HTTP, lifecycle and native-process commits. The four reported
flaky fixture paths use the shared helper (recovery_paths, response_retries,
response_upload_recovery, attachment_provenance). The ordered invalid-AGENTS
preparation pause subscribes before inspecting saved state and reinspects on
Changed/lag rather than polling the DB every5ms. Existing notification-based retry
and upload waits keep their saved-phase assertions. Negative HTTP observations
count queued connections after the operation's completion signal; channel checks
follow completed operations/persisted phases, with final attempt counts retained.
Explicitly blocked native/DB work uses future polling instead of20/30ms absence.
The former ignored minute-long outage fixture now checks seven explicit failures
and the preserved retry budget with a small test-configured cap; exact delay
policy is checked on a virtual clock, not by counting attempts that fit in61s.
No product limit was changed.

Native shutdown qualification varies five named persisted boundaries: before
Control shutdown, while shutdown awaits a held DB job, while an uncommitted
transaction is held, after Control drains, and while owner close awaits the DB.
The fixture child is really killed (SIGKILL on Unix; TerminateJobObject on Windows).
It contains synthetic completed/running/pending tool facts, not a real provider
or a model request. Reopen verifies original result identity/payload byte equality,
rollback, interrupted running output, not_started pending output, integrity/FK
checks, unchanged shell count and no repeated repair on a second reopen. This
samples transaction/shutdown boundaries, not every CPU instruction or real power
loss. Existing real-native-shell SIGKILL and native signal cases remain.
macOS focused forced-boundary test passes all five positions. Windows is a
separate required native gate, not inferred from the Unix result.

Native disposable PTY coverage uses output predicates, not700ms color samples or
500ms silence as success. Static-idle frame behavior is tested via the deterministic
draw budget. The fixture continues draining its master while waiting for saved
index retry status and runs its deliberately foreign slave writer concurrently
with master draining, avoiding test-created PTY backpressure deadlocks. Escape
uses an unambiguous CSI-u key frame, not a sleep to disambiguate a lone ESC.
The revised real_tui_accepts_text_renders_proxy_response_and_exits_cleanly passes
locally, as do8 attachment-provenance cases, the virtual blocking-worker case and
841 library cases on the intermediate tree. Final all-target/rebased validation
and native CI remain independent gates.

Implementation observations retained separately from product diagnoses: the new
heartbeat test initially held an immutable deadline borrow too long (fixed by a
future scope); the crash expectation initially called a pending result interrupted
(the correct retained fact is not_started); explicit virtual advancement must
pass the timer wheel's tick while a real blocked worker prevents auto-advance.
The first virtual file-worker experiments stalled and their owned shells were
stopped, then explicit advances were corrected. PTY experiments exposed paused
index polling that stopped draining the terminal and a blocking foreign-writer
call; these were fixture coordination failures, not evidence of a product deadline
regression. Clippy rejected duplicate includes of the connection-count helper;
it is now loaded once per integration harness. No test timeout suppression,
automatic retry, reduced assertion or production timer patch hides those failures.

scripts/check-test-layout.py runs the watchdog policy on all native CI jobs and
both standard check scripts. Its seven Python tests cover short/long raw guards,
production exclusion, nested/platform test modules, path overrides, aliases,
comments/raw strings, manual deadlines, and exact allowance growth/staleness.
The checker is lexical, not a full Rust macro expander. The committed reason list
pins existing file/function/fingerprint counts; copying a deferred timeout adds
an unapproved occurrence, and deleting one requires removing its stale allowance.
At the current snapshot135 #41-owned occurrences are deliberately deferred and14
intentional helper/virtual-clock/non-gating workload occurrences remain allowed.
This is not a claim that #41's legacy short waits are already migrated. The
initial #41 diff did not include src/frontend/control_tests.rs; a later read-only
recheck did, so that file's #77 changes were withdrawn. Only src/lib.rs test-module
wiring overlaps the latest #41 file set. No other worktree was edited.

All database/process/provider fixtures use disposable data and local mocks. No
paid API, user configuration, installed binary, live owner, schema/worker/queue or
new package is used. Successful local checks do not replace the required two
three-OS manual branch CI runs and the final main gate.


### Rebased D2 and admission facts

The branch rebased onto main4a6b839, including D2's shared readers. Only appended
status/acceptance sections conflicted; the complete upstream histories were kept
before #77's sections. The exact deferred allowance inventory was reinspected
against the integrated #41 sources:134 deferred occurrences plus14 deliberate
exceptions. A new parser regression prevents cfg(test) arguments/parameters from
incorrectly capturing the following production function. Production-token audit
(excluding test scopes) found no differences in31 changed src files; the remaining
src/lib.rs change is only the cfg(test) shared module registration.

One all-target run exposed a fixture mistake in the observer-cancellation
migration: polling a spawned JoinHandle as pending does not prove its inner owned
worker has been admitted (receipt count0 instead of1). The unchanged substantive
fixture/assertions now live in control::ipc_admission_tests, where it can inspect
existing private active-request/admission-closed facts without a new production
hook. It waits for admission before dropping observation and for shutdown closure
before releasing the blocked DB, then verifies exactly1 receipt and no live
Run/active Turn. The focused test passes. This is not a weakened count assertion
or another scheduling delay. The original integration test was moved, not copied
or dropped. The source checker also rejected an injected1ms test timeout in a new
support file; the temporary poison file was removed after that failure check.


### First native candidate observations

Candidate c2a5c9e passes the rebased local standard check:1852 passed/0 failed/
32 ignored, layout93, Python16, fmt/strict Clippy. Manual branch runs41156089976
and41156092253 both passed macOS/Linux but failed Windows Clippy before Tests:
old Duration and timeout imports remained in Windows-only tests/shell_windows.rs.
These are #77 migration errors, not flaky timeouts; both imports are removed.
Neither failed run counts toward the required two all-OS successful observations.
Azure log download temporarily returned503 ServerBusy/egress limits; a later
job-log read confirmed the exact unused imports. No job/test retry hid the errors.
Native Windows runtime/Job-object execution still requires the corrected gates.


### Corrected candidate native gates

Candidate3460132 passes manual all-OS runs41157071532 and41157073871, both on
attempt1 with macOS/Linux/Windows checks successful. The Windows-only imports
are fixed, and the native Job-object branch is included in its passing standalone
crash target. Rebased local standard check remains1852/0/32 with layout93,
Python16/fmt/strict Clippy. Before integration main advanced to15037c1 (#75,
readback/schema indexes), so these gates are retained as candidate evidence and
the combined final tree will be checked independently rather than assuming the
older pair qualifies new production/schema code.


### Readback combined-tree guard alignment

Main15037c1 added three new raw readback test guards outside the deferred set;
CLI/readonly observations now use the shared async helper and the synchronous
readback test wrapper delegates to shared blocking_recv. Its new storage migration
guard is already60s but src/storage.rs is #41-owned, so that exact occurrence is
recorded as deferred (now135 deferred +14 deliberate). No #75 production code or
migration was changed. Rebase conflicts were only src/lib.rs's appended module
declarations (readback plus cfg(test) test_watchdog both retained) and validation
appendices (complete upstream prefix retained). New combined native gates follow.


### Final repaint capture observation

A local latest-main preflight retained all runtime assertions but caught one
manual-repaint fixture failure: capture stopped at draft bytes before the
footer's pause arrow had arrived. The capture predicate now includes that same
pre-existing arrow requirement, so the assertions inspect the full required
facts rather than race a partial PTY read. No350ms wait or relaxed assertion was
restored, and production rendering is unchanged. Final combined-source native
qualification is repeated after this fixture correction.


### Output-driven repaint refresh

On candidate b387323, run41161117185 passed all three OSes; run41161114836
failed Linux's native PTY while waiting for the post-retry unpaused management
frame. The first requested repaint can precede the asynchronous status reply;
later cursor-diff bytes need not repeat an unchanged label. The observer now
requests a further repaint after actual received output, retains only the latest
clear-frame bytes, and requires all expected labels within that frame. It does
not reinstate120ms settling windows or join labels across unrelated frames. The
same60s guard protects liveness. This fixture failure is retained, not relabelled
as an unrelated Windows flake. Main's independent noncontrolling-EOF diagnostic
follow-up ad6c54b is preserved during rebase; it is not a #77 production change.

## #79 managed clipboard path qualification (2026-10-31)

Scope: keep the existing absolute path field for add/list, expose it in current
request metadata, and report managed lifecycle/missing-file facts with ordinary
unavailable_reason semantics. Existing managed preparation already used the same
successful-send hash comparisons as external files; tests establish that parity
rather than adding a second baseline or immutable-file rule. No permission,
schema/index, export, lease, table, worker or queue is added.

On native macOS arm64/Rust 1.99.1, 25 attachment integration tests pass. A temporary
RGBA PNG is created through save_clipboard_pixels, registered with its ordinary
reference, listed, and read by an actual native shell (sh on Unix; the native CI
Windows fixture uses PowerShell). The path is absolute; Unix permissions remain
0600. Replacing its bytes produces changed=true, before/after hashes and a separate
change notice; disappearance produces unavailable metadata and list reason. After
selection removal and release of existing pins, the same reclamation code deletes
it, with path and clipboard-retention/deleted reason still visible in the list.
Three actual prepared manifests are recorded in a synthetic observations.jsonl
(0 to 3 rows). No provider/HTTP request is made, and list does not create operation
receipts (0 to 0). This is not a real OS clipboard or real-provider format test.

The actual list SQL's EXPLAIN QUERY PLAN requires SEARCH, not SCAN, for selection,
attachment-ref and managed-file aliases, using existing Thread/ID/unique-path
indexes. No extra index is needed. A deliberately held DB writer does not block
the read-only list; ordinary missing external files use the same observation path.
File observations run after ending the read-only transaction through the existing
two-slot presentation admission; they do not pin files or guarantee future access.

New tests use #77's shared 60-second watchdog, with no speed threshold or retry.
Initial validation caught a test argument-type typo and a regression where copying
huge detached paths into new metadata starved the existing 64-KiB exclusion notice
budget. Detached references now keep their pre-existing bounded path notice without
duplicating that path. The unchanged bounded-notice/progress regression test passes.
Full standard/role/bundled-doc checks and exact-source native CI are recorded in #79.

Before editing, origin/main...origin/issue-41/e-ci-review was checked: F6 rendering
and frontend files overlap. Per user authorization, F6 path presentation is deferred
to #41 and recorded in #79; no F6 rendering files are edited here. That is remaining
scope, not a claim of a completed TUI test. The old #78 unpublished draft branch was
removed after user authorization; no tags, releases, installed binary or user DB
are touched by this task. Status/acceptance old lines are preserved and audited.

## E after canonical #77/#79 main integration (2026-10-31)

Exact local source0b7bd73 merges main0d4f63a through79d8be2. On native macOS,
`CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 RUST_TEST_THREADS=2 sh scripts/check.sh`
completed with1904 Rust successes,0 failures,32 ignored, Python18 successes,
layout93 registered suites, watchdog policy, fmt and strict all-target Clippy.
Library969/0/13, integration918/0/16, termination10/0/1, force-kill7/0/2;
other binary/example targets contain0 tests. The raw log is retained privately
as issue-41-e-main77-standard.log. New documentation-only ledger edits are
separate from this exact-source test claim.

Canonical #77 helper wiring occurs once. Removed9 obsolete exact allowances;
retained6 explicit supplied-Instant samples in3 pure UI debounce/coverage tests
with fingerprint/count reasons. They do not sleep or assert a real-time response
budget. #71 auto_history, #54 conditional tick and #79 managed attachment path
contracts remain intact. E adds no schema migration, worker or queue.

This result does not overwrite CBC's failed native Linux/Windows CI, qualify
repaired-source performance, or repeat the earlier6 native UI captures at this
new source. E's remaining gates include repaired-source three-OS CI, Mac P5
40commits/s, P3 and the P2 strict-bin/CPU review. Main remains unmodified by this
work; no new push, release, install or user-data change is implied.

Gitleaks8.30.1 redacted directory scan reported one existing synthetic fixture:
`src/readback/tests.rs:97`, rule `curl-auth-header`, literal
`SECRET-DO-NOT-RETURN` used to assert command redaction. Inspected the fixture and
its negative assertion; it is not a credential. No broad ignore or source change
was added. The E documentation diff scan reports0 findings. Directory scan is
not described as zero findings, and this is not a full-history scan.

## Repaired E CI invocation and source freeze (2026-10-31)

Approved branch issue-41/e-ci-followup77 was pushed at exact7c6f4d6. Corrected
manual workflow41166258538 uses full baseline0d4f63a6bcf90fa9d8b30d8598f7b8e8ca8e52bd
and candidate7c6f4d6e4ee12f0e6bd906504626d781ba751c13. Ordinary3OS and formal
Unix80/140-column windows run together, P2=one producer600s, P5/P5-list=three
producers60s each before/after. Checkout/instrumentation have succeeded; build/
tests are pending. Record final source-matched values later, not as this entry.

The first attempt41166177038 failed checkout because the short ref0d4f63a was
matched as a branch/tag. Saved the failed native job log and cancelled remaining
duplicate checks; it is not a product/P3/P5 failure or a successful test run.
Explicitly corrected to the same full SHA in a new workflow. Preserve both runs.
Windows native jobs may complete successfully by uploading unavailable.json;
that never qualifies ConPTY performance. Main and the installed binary remain
unchanged. #41 comments6538653546/6538662493 distinguish launch/correction.

## Repaired E ordinary three-OS branch gate (2026-10-31)

Workflow41166258538 at exact7c6f4d6 now has all3 ordinary checks successful.
Downloaded native-test-timings artifacts and verified each source_sha, empty
failures and actual harness logs: Linux1905/0/31, macOS1904/0/32,
Windows1780/0/24 (passed/failed/ignored). The real16-mutation test
frontend_reactivity::selected_header_and_open_browser_share_one_changed_snapshot_without_reload
passes on all3, directly addressing CBC's Linux/Windows paired-snapshot failure.
Linux runtime::raw_save_failure_stops_admission_reaps_shell_and_reports_failed_shutdown
also passes, addressing CBC's old8s fixture guard failure without relaxing saved
attempt/shutdown facts. #41 interim report6538791783 records the same evidence.

Native Unix P2/P5 jobs have finished both builds and entered formal observation;
no performance values are available at this checkpoint. The Windows native jobs
upload explicit unavailable.json at both widths, not a ConPTY measurement.
Main admission and main CI remain separate later gates; main is unchanged.

## E P3 direct-read characterization limits (2026-10-31)

The [E qualification record](conversation-redesign-e.md) now contains actual
normal-release before0d4f63a/after0b7bd73 observations:30 samples each, automatic/
explicit titles,128 saved nodes,50 returned nodes/tools and50 List rows. After
uses0 writer closures/1 readonly connection per sample; observed pipeline p95
is8.4079/6.5973ms versus3.2981/2.9446ms before, not a speedup claim.

This does not close P3: legacy CLI request starts/wakes services while the direct
new path does not explicitly start Control; all-worker counters are unclassified.
Record that lifecycle/caller asymmetry and require a matched-services follow-up.
No UI event-loop, future F/G, full-consumer, Windows or pure-SQL-time success is
inferred. Source/binary/controller/fixture failure records remain private and
separate from the frozen public CI candidate. Main remains unchanged.

## Formal repaired E producer3 CI result (2026-10-31)

Workflow41166258538 completes success at7c6f4d6. The
[E qualification record](conversation-redesign-e.md) and #41 comment6539050643
contain all12 same-runner pairs/24 source-matched600s/60s windows and3OS counts.
All16 P5/P5-list windows meet40 saved commits/s: Linux150, Mac53.98–57.05;
0 draws/UI read-task/UI writer jobs/readonly opens,0 model requests. Original
producer1 run41160746679 is separate and not retrospectively qualified on Mac.

P2's average1Hz/0 UI reads does not satisfy its strict policy: Mac140 max1s-bin2
on both subjects,2-draw bins1→88; CPU0.3389→0.3823%. Do not infer the cause or
promise no worsening. P3's lifecycle/caller asymmetry remains open. Green job
status is a fixture/source success, not blanket performance acceptance. Windows
TUI unavailable records, initial short-SHA checkout failure and raw maintenance
work remain explicit. No main integration, release/install or user-data change.

## E matched P3 and real CLI/frontend qualification (2026-10-31)

The [E record](conversation-redesign-e.md) adds matched-startup30-sample direct
costs and actual frontend16-mutation evidence at0b7bd73. Old2 UI writer closures
plus1–2 index next_delay jobs are distinguished; E's corresponding read is1 RO
snapshot. E component pipeline times increase and are not sold as a speedup.
The original unmatched observation remains retained with its limitation.

The actual128-node/50-tool/50-Thread workload uses real CLI/IPC/Changed/frontend,
drawn TestBackend180x40,4 warmups then16 samples:1 executed RO read each,
1 current observation and identical browser sequence,0 page reload/generation
change,50 retained tools. RO p95=4.9858ms; observed Changed receipt-to-draw
p95=496.6643ms. CLI mutation1/index next_delay2 are separate, with exact caller
count/time checksums and0 overflow. Normal owner shutdown completes. No native
PTY, physical Windows or future F/G/full-consumer proof is inferred.
Private prep/example errors are retained and not passing measurements; the
accepted source/harness/file/binary witnesses are checked. Frozen7c6f4d6 and main
remain untouched. E own P3 scope is qualified; later combined P3 remains open.

## #79 read-only connection/file worker separation (2026-10-31)

The initial implementation ended its transaction but retained the read-only
connection while checking paths on a presentation closure. Review correctly
identified that this did not separate DB and filesystem lifetimes. The corrected
reader returns owned AttachmentDetails and explicitly drops the connection. A
second await then runs metadata/open through InstructionLoader's existing bounded
control-file admission, shared with AGENTS/cwd preparation. No pool is added; its
configured deadline and actual-task permit lifetime remain in force. Timeout error
text is generalized from AGENTS-specific wording to control-file preparation.

All 25 attachment integration tests pass locally, including the actual list SQL
EXPLAIN, held-writer progress, current path reads/changes/deletion/retention and
bounded detached notices. A new shared-admission test holds the actual file closure
behind a channel gate, observes the existing permit in use, releases it, and waits
for idle; it uses #77's 60-second watchdog and no speed criterion. The file phase
receives only owned rows, not a connection or transaction. Full standard checks,
role checks, exact-source native CI and record-line audit are tracked in #79.
Tests use temporary files/databases and no provider. F6 remains deferred to #41.


## Unix owner-death lifeline (#82)

2026-10-31, native macOS ARM64 / Rust 1.99.1, main 0d4f63a base. Temporary HOME,
data and loopback Responses fixtures only; no installed binary, real owner,
user data, release or paid provider is changed.

The Unix adapter adds a fixed native guardian, not a runtime worker/queue/table.
Its live PID reserves the model PGID, with the guardian outside both model and
owner groups. A startup-only child creates its private group and is reaped before
publication. Steady overhead is two owner FDs and two guardian FDs per execution;
three close-on-exec socket pairs are launch-only, and group setup temporarily
uses a pipe/two FDs. The status handshake is required to retain the natural-exit /
late-stop contract. No FD is allocated while waiting for the Turn gate. See the
Shell design for post-fork operations and descriptor closure responsibility.

Evidence on this source:
- Native shell suite passes stdin/encoding/profile/env/argv, natural exit,
  nonzero/signal status, backpressure/late stop, descendant cleanup and raw bounds.
  A direct Bash oracle compares SIGPIPE, SIGTERM and nonzero exit/outputs.
  Missing executable failure and no-shebang executable-script fallback preserve
  the launch result. The private mechanism does not add a public CLI helper.
- A subprocess owner starts two real shell groups with sleep descendants. Tests
  KILL only its PID, then separately its whole owner group; both model groups
  must disappear by native ESRCH observation. Test fallback cleanup runs only
  after observation/assertion and cannot make a passing outcome. Cross-execution
  inherited descriptors therefore cannot keep the lifelines alive in this fixture.
- The real DataOwner/Control crash regression now kills only the owner, not the
  shell group in the test. It observes the group gone, reopens the same DB, checks
  failed/unverified shell get with no fabricated exit/signal/forced/group facts,
  retains adjacent call/result repair and confirms no repeated shell execution.
- Full ordinary local checks pass normal Ctrl+Q, SIGTERM/HUP, terminal loss and
  forced shutdown fixtures. Their native identities are observed with getpgid
  rather than assuming shell PID=PGID or PPID=owner after the guardian change.
  Windows Job and its startup repair path are not modified.

Two development failures were retained and fixed: the first shell suite found
that reaping before relaying status could let late stop relabel natural exit;
waitid(WNOWAIT), owner receipt and reap acknowledgment fix the native handoff
without weakening the existing assertion. The first crash observation rejected
transient EPERM as though it must already be ESRCH; it now keeps observing unknown
states and still requires actual ESRCH before success. No short timeout, settling
window, latency assertion or new watchdog allowance was added. New test waits
use the shared #77 watchdog only; existing product stop/drain budgets are reused.

Local full checks passed twice during development (the second: 1,866 passed /
0 failed / 33 ignored, layout 93, Python 18, fmt and strict all-target Clippy).
The subsequent direct-launch oracle passed separately. Final source after native
FD-admission placement/documentation and rebase needs its own standard check and
native three-OS CI, not inferred from these earlier gates. Linux process behavior
and Windows build/tests remain unqualified until that CI completes. Full cleanup
of deliberately escaped groups, protection from deliberate guardian tampering,
absolute stop-time guarantees and stale-PID recovery are not implemented.

## Typed command prefixes and pasted arguments (#84)

Source: task branch `issue-84/typed-command`, based on `72f7889`; macOS arm64,
debug build, synthetic temporary data and loopback SSE only. No provider, user
data, installed binary or release asset is used. The focused composer contract
suite passed 11/11 before final gate qualification.

Repeat the native screen observation with Python 3 plus the test-only `pyte`
package: `python3 scripts/check-paste-commands.py --binary target/debug/shellboy
--out /tmp/issue-84-pty-UNIQUE`. This opens a real Unix PTY and decodes its cells;
80/140 columns each cover dark/light, NO_COLOR and TERM=vt100. It observes
paste-only, pasted arguments followed by a manually entered leading command,
typed command followed by pasted arguments, folded long slash-containing paste,
a literal pasted slash warning and an explicit-send synthetic SSE reply. Only
the #77 60-second hang watchdog bounds waits, not a response-time assertion.

Initial harness observations exposed pyte's differential wide-cell display
exception and blocked shutdown when output was not drained. Those harness
failures are not passing product observations. Final PTY results, standard
checks and native CI must be recorded independently after they complete.
Unqualified: physical terminal/IME/clipboard operation, Windows ConPTY, and
matched-baseline P1 timing (no unnecessary release build for this task).

PTY result (2026-10-31): all eight sessions passed, with 10 screen observations
each and exactly one explicitly submitted synthetic SSE request per session.
The PTY bytes in `/tmp/issue-84-pty-third` were decoded again at Ratatui's
end-of-frame cursor sequence to replace partial-repaint snapshots with 80
complete-frame text captures, without rerunning the TUI. At 80 columns both
warning/retyping lines and the 41-line/1326-character paste chip are readable.
The second harness attempt deadlocked on a blocking master write; bidirectional
nonblocking delivery with the same 60-second watchdog fixes that fixture issue.
No product change was required for these harness corrections. Raw bytes,
`results.json` and `*-complete.txt` remain local synthetic evidence, not user logs.

The first standard check caught an unnecessary transient-prefix difference on
ordinary non-command text in the existing attachment/restart equality test;
tracking is now restricted to leading slash drafts. The same run also hit the
existing persisted-output observer watchdog. Neither failure is erased; the
final standard gate runs with two test threads to bound concurrent memory use.

Final local source `869fa2e` (rebased on `72f7889`):
`CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=2 sh scripts/check.sh` passes layout 93,
watchdog policy, Python 18, fmt, strict all-target Clippy, and Rust 1,875 passed /
0 failed / 33 ignored. Both previously failing native identities pass.
The final debug binary SHA-256 is
`6179bc85d8146955bc5b01481ee0e8b37ec7b43e951130cd3d147a3993ac2ec3`.
The completed-frame PTY driver passes all eight sessions/80 observations again
on that binary (`/tmp/issue-84-pty-final`); manifest, screen text, raw bytes and
request counts remain synthetic local evidence. No timing threshold is asserted.
Subsequent evidence-only documentation changes do not alter the tested Rust tree.
Native three-OS CI and integration status are tracked in issue #84.

## F isolated session-state primitive

Dedicated F preparation branch based on E10d6df8 ports pure ui/watch from archived
a410fb3 and registers the ordinary UI module. Native macOS/Rust1.99.1 focused
`cargo test --locked --lib ui::watch::` passes7/0/0 (990 filtered out): bounded
seen LRU/uncertainty, completion identity, stale/replaced action targets, stable
category order and local visible ages/sanitization. Formatting passes. No I/O,
new Core state, command, worker or table is added. Runtime Watch/page/lineage
integration, later archive corrections, full standard and F P2–P5/native proof
remain pending. No new feature is advertised or admitted to main by these tests.


## F parent-index preparation (2026-10-31)

On the isolated F branch based on E10d6df8, inspected origin/main e933054's
schema15 and added local candidate0016_thread_parent. Native macOS/Rust1.99.1
`cargo test --locked --test integration storage` passes32/0/0 (907 filtered),
including injected ledger failure rollback, original rows, fresh/upgraded schema
equality, two reopens and covering successor seek. The focused storage library
run passes3/0/0. Synthetic historical fixtures drop the added index before
downgrading; the readback corruption guard now uses the common60s helper and
its old allowance is removed. Formatting/layout93/watchdog policy pass.

The release-note draft includes stopped-owner/full-data backup and schema
rejection guidance. Number16 remains provisional until F admission. This is not
a full standard/strict Clippy/three-OS result; incomplete F reader consumers
currently produce dead-code warnings without suppression. No main push,
installed binary, default-data change, paid API or release.


## F lineage reader primitive (2026-10-31)

The F-only lineage module uses one indexed bounded recursive statement per
actual root, at most two roots, and separates saved Thread ancestry from Run
ownership. Depth8/discovery256 plus one sentinel bound traversal, including
deleted bridges. Lightweight saved native states classify counts; no descendant
title/body/tool/attempt reads are introduced. Changed identities use cached
positive membership or one bounded upward PK query for new/frontier children.

Native macOS focused `cargo test --locked --lib frontend::lineage::` passes7/0/2
(997 filtered). This includes a physically read-only SQLite connection, one
statement per root, unavailable versus exact zero, deleted bridge, cycle/depth/
wide-frontier semantics and query VM work unaffected by10,000 unrelated Threads.
The two explicitly ignored P4 timing/million-sibling observations remain unrun.
The existing presentation-pool/shared consumer broker is not wired to this new
module yet; this is not a working Watch screen or all-consumer P3 qualification.
Formatting/layout93/watchdog pass. Primitive diff secret scan has0 findings.


## F bounded visible Watch details (2026-10-31)

The local F reader now extracts exact-ID saved details, reusing supplied summary
and parent-title facts only within the caller's snapshot. Command JSON, recovery
and answer sources share pre-copy16KiB/row and64KiB aggregate admission; stored
parent labels independently cap at4096 bytes. Existing live-work age/sanitization
helpers are shared with sibling frontend modules, without a public API change.

Native macOS `cargo test --locked --lib frontend::watch_read::` passes9/0/0
(1006 filtered), including large/malformed command JSON, UTF-8 byte accounting,
combined per-row sources, whole-command aggregate charging, unavailable previews,
shared-query reuse and unchanged VM work after10,000 unrelated Threads. Formatting,
layout93 and common watchdog policy pass. Initial private-helper visibility
compile failure and missing required Turn in the new synthetic shell fixture
were corrected; earlier logs are retained separately from the successful run.

This is still local F preparation: no Watch UI/action exposure, new worker or
writer call. Actual presentation-pool wiring, same-snapshot consumer broker,
strict Clippy/full standard, native three-OS CI and F P2–P5 remain unqualified.
No main update, installed binary/default-data change, paid API or release.


## F readonly Watch discovery primitive

The local F candidate now has canonical Watch discovery on a readonly WAL
connection through the existing finite presentation pool/progress admission.
Its sequence is assigned before admission; cancellation is checked before open
and after collection. The transaction ends before title/text projection.
Discovery retains at most4096 candidate metadata rows and128 title witnesses;
this does not bound SQLite's global priority-sort work to4096 rows. Descendant
scopes use the same bounded lineage and explicit truncation/unavailability.
Private cursors bind filter/root/seen identities/watermark; no public wire or
Core operation is added. Diagnostics are supplied by the existing owner, avoiding
a new Core accessor just for this frontend read.

On the combined prepared F source, native macOS `cargo test --locked --lib watch`
passes30/0/0, including9 discovery fixtures (priority outside the first List page,
seen overflow,51st match, Unicode derived titles, deleted bridge, metadata/
witness continuations and cursor root/watermark fences). `diagnostics::` passes
6/0/0. These results describe the complete prepared primitive tree, including
the separately committed session and diagnostics changes, not each earlier
intermediate commit in isolation. Formatter/layout93/watchdog policy and diff
gitleaks pass.

The async page adapter is not exercised through a running Watch frontend yet;
its UI, pool contention/cancellation and shared-consumer P3 proof remain pending.
Four lib-test dead-code warnings currently expose the unwired reader entry and
fields; ordinary non-test builds may identify more. No suppression/full standard/
strict Clippy success is claimed. P2/P4/P5, Watch actions and three-OS native UI
qualification remain open. E main admission still needs separate approval; no
main push, installed-binary change, default data, paid API or release.


## F browser backend integration over E

The existing List observer now owns explicit mode/detail/parent/saved-lineage
scopes for future Watch consumers. Exact-ID summary/title/detail batches reuse
same-transaction evidence; detail cancellation is separate from page scope.
The output-hint helper tracks only already-visible shell owners. Existing List
runtime keeps E's readonly canonical page helper, progress admission, sequenced
shared current-view snapshot and same-deadline priority rule. List skips Watch
summary/detail projection. No UI-thread I/O or new worker/queue is added.

Native macOS checks: browser/read helpers19/0/0 (1019 filtered); shared refresh
5/0/0 (1033 filtered); actual frontend_reactivity6/0/0 (933 filtered), including
16 actual CLI title mutations sharing CurrentView/BrowserPatch sequence with no
page reload, unloaded create/delete, queue/recovery/Run stop and F6/F7 GC paths.
The integration result precedes only the test-only virtual-clock cleanup; product
logic is unchanged by that cleanup. Two imported synthetic deadline tests now
advance paused Tokio time, with no real-time wait/allowance. Formatter, layout93
and watchdog policy pass. No new performance/three-OS qualification is claimed.

The current UI still produces only List observations: Watch summaries/details
are not exposed to users by this commit. The prepared internal detail DTO still
needs actual Watch UI delivery/sequence adoption through the forthcoming shared
browser/count broker. Descendant observer/current-view admission, Watch overlay/
actions, cancellation/contention tests and F performance/visual gates remain
pending. Dead-code warnings for these unwired components remain unsuppressed;
this is not full standard/strict Clippy success or F admission. No main push,
installed-binary/default-data change, release or paid API.


## F runtime candidate9d3494f — local standard proof

Native macOS/Rust1.99.1; product/test Rust source9d3494f (including private Core
guard8206c20). `CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 RUST_TEST_THREADS=2 sh
scripts/check.sh` passes: lib1042/0/15, integration922/0/17, termination10/0/1,
forced-shutdown7/0/2 and three zero-test harnesses, totaling1981 pass/0 fail/
35ignored. Python18, layout93, common watchdog policy, formatter and strict
all-target Clippy also pass. Diff gitleaks8.30.1 reports0 findings. The source
remains unchanged by this proof-only documentation update; no installed binary,
main, default data or external provider is changed.

Prior failed attempts are retained: formatting whitespace after watchdog wrapping;
a collapsible match lint; then standard3 lib1040/1/15 exposed an actual resize
race. Invalidating Watch paint/detail scope fixed it, and all10 focused widget
tests plus standard4 pass. A separate new footer regression reproduced clipping
of positive descendant counts behind long facts; reserving the count width
before fitting fixes that test in standard4. No assertion was weakened to hide
either product failure.

The existing combined-view test keeps its identity and now also observes an
unowned child Thread while the writer is blocked: selected history/header,
browser patch and descendant count share the same read sequence with no writer
admission. This is functional same-snapshot proof, not native F-wide P3 cost or
latency measurement. The ordinary E actual CLI/reactivity suite also remains
green in the full check; a real-CLI scoped Watch/retry-race fixture is still to
be ported and run.

Remaining F admission gates: complete guides/help/completion, actual scoped Watch
CLI lifecycle and cancellation/contention cases, bounded retained-memory account,
lookup-index evidence/final schema numbering, native80/wide theme/terminal review,
and applicable P2/P3/P4/P5 with native/manual CI. The2 explicit P4 timing/stress
tests are ignored, not qualified. E main admission still awaits separate review
approval; F/G are not admitted. Windows/Linux ordinary CI and physical terminals
are untested for this source. #83–#87 remain outside this work.

## E combined with main #79/#82/#84: local admission candidate (2026-10-31)

Exact merge10d6df8 contains current main23276368ad2461ab2b053e57e87a3bb1be50a2eb
(main CI41171514740 green). Native macOS standard completes with1915 successes,
0 failures,33 ignored: library977/0/13, integration921/0/17,
termination10/0/1, force-kill7/0/2. Python18, layout93, watchdog policy, fmt and
strict all-target Clippy pass. Target crate-root mtimes were refreshed before
building to avoid accepting shared-target artifacts from another checkout;
tracked source bytes remained unchanged. The private standard log is retained.

The upstream #84 checker also passes on the combined source: actual Unix PTY
80/140 columns x dark/light/NO_COLOR/vt100 =8 cases,80 captured stages,8 loopback
mock requests. It checks the existing typed-leading-slash/pasted-argument and
literal-paste behavior after E integration; it does not implement assigned #84
work again. Raw synthetic captures/results and binary digest remain private.
Diff from main2327636 gitleaks8.30.1 reports0 findings. No broader full-history
scan or physical Windows result is implied.

P3 and formal producer3 witnesses remain their original0b/7c sources; exact-source
timings are not relabelled as10d6df8. The new merge's ordinary Linux/Windows CI
and main CI have not run. E publication requires the promised separate review
approval. Only the approved measurement branch was pushed; main, installation,
user store and transferred #83–#87 features were not changed by this work.

## #80 on-demand owner memory qualification (2026-10-31)

The minimal interface is status --memory: read-only, owner-wide, and exclusive
with activity filters. It returns existing cache budget accounting (body views,
layout/unit entries, attachment paths and replay Threads) plus configured limits.
Existing live/pinned counts remain separate. This is not a full heap measurement,
not an additive partition of OS memory, and not a leak diagnosis. No new retained
byte counter, cap, table, worker or work queue is added.

UI counters respond to a request on existing EngineEvent/UiUpdate delivery with a
single completion responder. No periodic sampler or log exists. Process and other
cache reads reuse existing bounded control-file admission, not the DB/UI lane.
MacOS uses native rusage_info_v2 footprint/resident bytes; Linux parses bounded
proc RSS/PSS kB fields with checked conversion; Windows uses current-process
working-set/private usage. OS failure/absence, unsupported platforms and absent
UI ownership yield null plus reasons, never a guessed zero.

Synthetic tests on native macOS arm64/Rust 1.99.1 verify body/layout growth after
use and attachment/replay growth after preparation, without exceeding configured
retention caps in those fixtures. The actual request completes with the DB writer
held. Process sampling stays at 0 until requested; attached UI replies precede
sampling, and two requests produce exactly two samples. Private observation JSONL
rows are 0 to 2; operation_receipt/model_attempt remain 0 to 0. Native-field tests
require a byte value or a concrete failure reason for each platform's expected
metrics, and malformed/overflow proc units stay unknown. CLI conversion and scope
conflicts are tested; no model/embedding provider is used. Waits use #77's shared
60-second watchdog only, not performance thresholds.

Initial local checks caught a fixture cursor/path mismatch at shutdown, private
method use in an external CLI test, and an unchecked server-shutdown Result. The
fixture and tests were corrected without changing production recovery semantics.
Full standard/role/bundled-doc checks, native OS results and record-line audits are
tracked in #80. The earlier #79 adopted-design paragraph is aligned with its
review-corrected connection/file-admission separation; historical evidence stays.

The published #41 branch overlaps F8/frontend/UI files. Only minimal request/reply
plumbing is included here; F8 display, leak investigation, untracked SQLite/heap
estimation and request-copy/peak optimization are deferred. No real owner/data-dir,
installed binary, tag or Release is modified by verification.

The first #80 branch CI (dd416b7, run 41168775635 attempt 1) passed Windows/Linux
but macOS failed the existing cli_process durable-retransmission test's index
preview metadata assertion: (0,0,1) versus baseline (0,0,0). New memory tests passed.
There are no index-preview implementation/test changes in this task; the cause is
unproven. Preserve the failure and any same-SHA failed-job rerun as separate facts
in #80. A follow-up also qualifies CLI help's default saved-state description with
its live owner-memory exception. No test guard, assertion or retry policy is relaxed.

The same-SHA failed-job rerun of first candidate dd416b7 succeeded (41168775635,
attempt 2). After #82 integration/help correction, candidate 3e43896's run
41170243551 attempt 1 passed Windows/Linux but macOS failed the existing parent-PTY
termination test's required scope=signal diagnostic row. Its unchanged failed-job
rerun also succeeded (attempt 2); no timing/assertion/runtime relaxation is made.
The attempted main push after qualification was safely rejected non-fast-forward
when other work landed during the pre-push standard check. Rebase retains the
complete upstream records and requalifies the combined SHA; run links/results
remain in #80 rather than changing qualified source recursively.

### 2026-10-31: #83 row vocabulary, pre-rebase native observations

macOS 26.3.1 (a), build25D771280a, arm64 debug; CARGO_BUILD_JOBS=2 and
RUST_TEST_THREADS=2. No release build, installed-binary change or user data.
- Focused presentation 34/34 and existing history-interaction 8/8 passed before
  the additional explicit notification-navigation regression was added.
- `python3 scripts/check-row-kinds.py --binary target/debug/shellboy --fixture
  target/debug/examples/visual_fixture --out /tmp/issue-83-pty-fifth`: exit0,
  actual Unix PTY + pyte + synthetic loopback SSE, 8 sessions / 56 completed
  screens, 80/140 columns, dark/light/NO_COLOR/TERM=vt100. A real background
  read coexisted with two consecutive foreground successes; saved-start seconds,
  marks, dense tools, factual delivery notices, Enter source jump and explicit
  output expansion/close were observed. Static summary/input/child notice/
  unavailable rows used a separate synthetic conversation, not model replay.
- All16 table entries have Unicode EAW=N and ordinary/CJK width1 (including
  repeated source arrow). Canonical text, source anchors and item identities
  remain independent of generated nonselectable blanks.
- First PTY attempt fed a deliberately unmatched visual tool result into model
  replay and correctly entered recovery. Separate static/live scenes fix the
  fixture. Attempts2/3 misselected or misqualified the baseline HTTP-only F7
  overlay and hit shutdown watchdogs. Enter navigation supersedes that approach.
  Attempt4 had a whitespace-sensitive wrapped-label assertion; normalized text
  fixed the driver. No failed attempt is counted as product acceptance.
- First standard check: Python18/layout/watchdog checks passed; strict Clippy
  rejected a nested if and the new navigation test had usize vs u64. Corrected;
  full standard/rebase validation and matched-baseline P1/3OS CI remain pending.
- Physical terminal glyphs, real IME/clipboard and Windows ConPTY are unverified.
  Native OS CI is not physical terminal UX. Missing name/source enrichment: #88.

The first rebased standard run passed Clippy, then had948 unit passes/2 failures:
a remaining old unknown-code glyph expectation and the new off-window fixture
incorrectly reused a view that intentionally retained its old source. Both tests
are corrected. Self-check also separated Enter following from Space/mouse
expansion; exact loaded source, independent unavailable source and child Thread
navigation now pass the focused regression without adding a read/poll operation.

The next full run passed950 unit tests and918 integration tests, with two
remaining integration golden-label failures (`●` answer and `↳` background).
These are updated to the adopted `▸` and `⏵ 背景（利用者）` contracts without
changing source-order, command-identity, cancellation or watchdog assertions.


Final local source7028c3b is rebased on accepted main e933054 (including #80).
The three append-only documentation conflicts retained both tasks' records;
Rust/UI integration had no conflict. The pending standard/local qualification
statements above are superseded by the following independent final results:
- `CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=2 sh scripts/check.sh`: exit0,
  Rust1,887 passed / 0 failed / 33 ignored, layout93 suites, Python18, fmt and
  strict all-target Clippy. Evidence: `/tmp/issue-83-check-final2.log`.
- Final rebuilt debug binary SHA256:
  `bccbe30aab271abdcad9ba37da4cd2f6150e7681a772ae5d5d277b3ffd8993ee`.
  The same actual PTY command, now `--out /tmp/issue-83-pty-final`, independently
  passes8 sessions/56 completed frames after rebase/Enter-only following. Tests
  also verify Space remains folding, unavailable source stays local and child
  Enter follows the saved Thread. `py_compile` and `git diff --check` pass.
- Same140x40 TestBackend,128 nodes/50 tools, debug,20 warmup/500 timed frames,
  interaction mode on both main e933054 and candidate7028c3b, three interleaved
  pairs: p95 microseconds3268→4449,3388→5135,3187→4169. All pass<=8ms and
  <=current-main+2ms (largest delta1747us). Both keep131 layout builds,
  258 unit builds/16510 unit hits and focus-only reuse assertions. Logs/JSON:
  `/tmp/issue-83-p1.log`, `/tmp/issue-83-p1.json`. This is an incremental
  current-main comparison, not a fresh observation of historical main1fec7f0.
- Source/script changes stop at7028c3b; subsequent status/acceptance edits are
  evidence-only. Branch CI and review outcome are recorded in issue #83; local
  success alone is not three-OS admission or physical terminal qualification.

## #89 CI observation ordering qualification

2026-10-31, native macOS 26.3.1 arm64 / Rust 1.99.1. Synthetic owners, loopback
fixtures and native owned child processes only; no user DB, settings, installed
binary, owner or release/tag changes. Build jobs are capped at 2 and test threads
at 4 to limit memory. Every new wait uses the shared 60-second watchdog; there
is no performance threshold or increased production/test timeout.

### Causes and controlled order

- Run 41172774266 attempt 1 failed at the immediate live-shell unwrap.
  `signal_shell` saves stopping and can clone no handle before `register_shell`;
  the reply is nevertheless valid. A cfg(test, unix) registration gate now
  deliberately holds native handle publication until after successful acceptance.
  The test asserts that the live entry is absent at acceptance, then subscribes
  before releasing the gate and waits for the entry. Native stdin remains
  writable, the runtime-operation grace owner remains present, and shutdown
  drains registrations/operations/requests. Ten fresh test invocations passed.
  The gate is absent from production. The Unix lifeline's startup work can
  widen this valid interval; no evidence establishes incorrect native killing.
- Run 41168775635 attempt 1 observed metadata (0,0,1) versus (0,0,0).
  The runnable driver's `record_context_observations_tx` independently inserts
  `index.observed.THREAD`. The real CLI test now observes that exact saved key
  with engine notifications before taking the baseline, verifies (0,0,1), then
  preserves the complete preview/retransmission non-mutation assertion. Five
  fresh invocations passed; index metadata is not excluded from the comparison.
- Run 41170243551 attempt 1 required a signal diagnostic after parent PTY loss.
  Diagnostics admission uses `try_lock`/`try_send`, so shutdown drain cannot
  restore records dropped before admission. Grep audited signal/owner diagnostic
  presence assertions in termination tests. Shared graceful checks now require
  saved tool completion, stopped Run/app_shutdown and stopped shell, successful
  native exit where observable, and ESRCH for the known native process group.
  Present signal records retain stage/boundary validation; missing records are
  explicitly tested and malformed records are rejected. EOF retains its stricter
  terminal_lost check. Repeated-signal tests wait for listener endpoint removal
  rather than log admission, including with a blocked database writer. Three
  complete native termination-suite invocations passed before the additional
  malformed-record check; final standard/native CI covers that check too.

Local full-check and exact-source three-OS CI outcomes are retained in issue #89.
Repeated passing runs are supplementary observations, not the causal evidence:
that evidence is the forced acknowledgment-before-publication order, saved
index-context boundary, and the documented lossy diagnostic admission contract.
No runtime interruption capture or native lifeline/stderr source is changed,
leaving the concurrent #82 work separate. No schema, table, worker or queue is
added. Prior failed-job reruns from #80 remain historical, not proof of a fix.

Local standard check (`CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=4 sh scripts/check.sh`)
passes: Rust 1,889 passed / 0 failed / 33 ignored, integration layout 93,
Python 18, fmt and strict all-target Clippy. The document-role test passes
separately. Status adds 10 lines and deletes 0; this qualification appends its
record without deleting prior evidence. Rebase/pre-push validation is repeated
on the exact candidate; native branch CI must pass before main integration.

## 2026-10-31 shell metadata palette follow-up (#83)

- User-requested separate change before #81. `rows.rs` owns right-hand metadata
  color: the existing palette muted role for routine facts, existing attention
  role for failure/nonzero exits/cancellation, previous Reset under NO_COLOR.
  Both compact headers and live-seconds painting use it. No text, identity,
  source/layout, persistence, protocol or theme configuration changes.
- macOS 26.3.1 (a), arm64 debug: standard check passes 1,889 Rust tests,
  33 ignored, zero failed; layout 93 suites, Python 18, fmt/strict Clippy pass.
  New tests cover all row kinds, dark/light/plain metadata spans, stable text
  and live elapsed-color painting.
- Actual Unix PTY plus pyte and synthetic loopback SSE: 80 columns, dark/light/
  NO_COLOR/vt100, four candidate sessions with 36 completed scenes. Normal
  metadata uses #8a8a8a/#4e4e4e in dark/light, attention remains #ff0000.
  Live background elapsed seconds are muted. Candidate NO_COLOR shell-row
  visible cells/foreground/background equal an independent baseline owner.
  Baseline is the pre-follow-up binary built for #81 at `1610b3b`; its Rust
  sources equal reviewed main `0ca4ec5` (guide-only differences).
- PTY driver failures are preserved: initial attention expectation used a
  symbolic ANSI name instead of pyte's indexed RGB value; next comparisons
  exposed empty orphan-stub versus space representation. Corrected the driver
  to compare displayed blank cells without relaxing color/nonblank checks.
  Final independent rerun succeeds; failures are not counted as passing scenes.
- Evidence: `/tmp/issue-83-metadata-check.log`, `-build.log`, `-pty-v4.log`,
  `/tmp/issue-83-metadata-pty-v4/` (screens, raw, cell-color JSON and results).
  Physical-terminal/font readability, IME, OS clipboard and Windows ConPTY
  remain unverified; this small color-only change does not rerun P1.

## 2026-10-31 storage/resource guide audit (#81)

- Scope: documentation only. Initial audit used reviewed #83 main `0ca4ec5`
  (main CI `41175347395` passes on all three OSes). Revalidated on `0e53156`
  including accepted #89 and the separately committed shell-metadata follow-up;
  retention/cache/docs-system implementation sources remain unchanged.
- Requirements: #81 body and its two additional comments. Both User Guides
  explain storage/GC/cache/memory and bundled delegation IDs; Esc, last-successful
  usage and the usage-section placement are aligned. Only the stale live-work
  qualifier is removed; watch/forms and deferred F8 names remain unchanged.
- Sources: `src/config.rs` Retention/Cache (7 days, 1GiB/256MiB;
  32/16/32/64MiB), `src/control.rs` maintenance (initial pass, then 60-second
  sleep), `src/gc.rs`/`src/gc/history.rs` (raw/history batches and reachability),
  `src/attachments.rs` collect_managed (protection and owned clipboard paths),
  `src/memory.rs`/`src/control/memory.rs` (on-demand metrics), `src/docs.rs`
  and `src/control/driver/request.rs` (bundled guides and short system entry).
- Important clarification: 128 is a raw/history DB batch size, not a global
  collection-per-pass/minute limit. Raw/history collection can repeat batches;
  managed-image candidate selection has no equivalent LIMIT 128. The managed
  report truncates details to 128, not collection work. No runtime change here.
- Compared against recovery/storage, configuration and attachment guides.
  No remaining checkout/absolute-guide-path premise was found in feature guides.
- Python document/script tests: 18 pass. All 33 Japanese-guide local links and
  fragment targets pass an ad-hoc check (the initial checker mishandled same-file
  fragments; correcting its path resolution fixes the checker, not guide text).
- Rebased candidate `32c0673`: standard check passes 1,891 Rust tests / zero
  failures / 33 ignored, layout 93 suites, Python 18, fmt and strict Clippy.
  `scripts/check-docs.py` against its rebuilt debug binary passes 33 offline CLI
  checks: 13 guides, three categories, 141 resolved bundled links, valid hashes
  and UTF-8 section boundaries. Empty cwd/environment, absent config and no
  owner/API are independently checked. Japanese local links are 33/33 passing.
  Evidence: `/tmp/issue-81-check-rebased.log`, `/tmp/issue-81-bundled-docs.json`.
  Binary SHA256: `b9c153e6f0bc083caa3430ec7234f77e8b4d799faf629878131024342391b852`.
- The separate metadata commit `0e53156` also passes the post-#89 standard check
  (1,891 Rust tests) and fresh 80-column PTY at
  `/tmp/issue-83-metadata-pty-rebased/`. The earlier 1,889-test observation above
  predates #89; both are retained rather than relabeling old results.
- No release build, performance/PTY rerun for the guide-only delta, physical
  terminal/IME/clipboard/ConPTY verification or real-data GC is performed.
  Exact-SHA three-OS CI/integration outcomes are tracked in #81/#83.

## #82 Low follow-up: bound snapshot values and guardian EOF

2026-10-31, native macOS ARM64/Rust1.99.1, a separate commit before #74.
SHELL_SNAPSHOT uses SQL parameters for startup state and JSON extras, not source
text replacement. The existing app_ipc restart fixture now reads the actual
interruption_context JSON and asserts failed/unverified on Unix, stopped with
no os_state field elsewhere, and retained result uncertainty.

close_from's lower bound is inclusive, so fd2 was already closed in production;
no /dev/null reassignment or lifetime policy change is needed. The new native
natural_shell_exit_drains_stderr_to_eof_before_guardian_release test confirms
stderr bytes and EOF while try_wait still says the guardian is alive. A separate
normal supervisor test natural_exit_releases_guardian_and_finishes_output_drain_
without_stderr_eof_cycle confirms stdout/stderr tail bytes, Exited0, complete
output and confirmed group cleanup through the production release/drain path.
All three focused checks pass. The first native harness incorrectly retained
Command's parent pre_exec FD copies while waiting for the launch acknowledgment;
that harness blocked and was terminated. A first whitespace-based edit missed
its target; the unchanged harness was terminated again. The fixture was fixed
to match production drop-before-ack in existing blocking work. This is not an
observed production stderr-retention defect. No test deadline was shortened.

## Bounded Thread wait/readback qualification (#74)

Base main0ca4ec5, native macOS ARM64/Rust1.99.1. Build concurrency is1, test
concurrency2; no release build or concurrent duplicate full check is used.
New waits use the existing Notify-before-read loop, receipt/cleanup and graph.
Terminal state/time comes from Run rows; genuine recovery/resume uses retained
existing recovery events, also recorded for root Runs without parent delivery.
No lifecycle classification from arbitrary attempt failures is introduced.

The following focused observations pass during implementation; final combined
full-standard/native results are independent gates recorded by exact SHA in
issue74 rather than inferred from this list:

- Actual EXPLAIN QUERY PLAN on next/head Run and recovery, current Run/Turn,
  last Run and latest answer/input statements proves indexed Thread/range or
  primary-key seeks, without history-wide table scans or temporary order trees.
  A manual SQLite3.51 check also shows thread/state/rowid and expression/rowid
  range seeks. Root human waits skip the unrelated model ownership graph.
- Synthetic131 changes cross the64-change cap without loss, using per-stream
  positions and round robin. Current active work is not an old completion;
  unknown/deleted targets and invalid scopes/cursors/limits are distinguished.
- A real pending model observation is registered before the existing Changed
  publication; a completed Run and newer live Run are committed between reads.
  The wait still returns the saved completion and leaves new work live.
- A real temporary owner stores a root recovery fact, resumes the Turn, and
  reads both the prior completion and recovery via cursor. After shutdown and
  restart the same cursor reads the new stopped fact. Self/ancestor/composed
  ownership protection remains; no polling or target stop is used to wait.
- A schema15 temporary store upgrades/reopens with all four indexes and unchanged
  Run/event row identities. The touched schema14 corruption fixture retains its
  source-preservation assertions and uses the shared watchdog; its one obsolete
  allowance is removed, not broadened.
- A real owner plus two gated loopback JSON model responses verifies multi-Thread
  settlement and old final answer versus new received input during the second
  request, same metadata in list/get, saved sender/label, then the new final
  answer. Another saved-fact fixture keeps held input distinct, strips an unknown
  credential field and preserves identity when body bytes are reclaimed.
- The CLI maps --after to the shared wait contract and rejects65 targets or after
  without a Thread condition. Guide examples explain cursor continuation and
  distinguish received input/labels from execution or recovery authorization.

The initial plan fixture used &str-to-SQLite-Value conversions unsupported by the
pinned API; explicit owned Values fixed compilation. The initial loopback fixture
assumed ThreadCreateArgs::default; explicit current fields fixed compilation.
The first latest-data fixture set a Turn answer FK before inserting the node;
correct insertion order fixed that fixture. These are harness failures, not
suppressed assertions. Further final-gate failures, if any, must be retained in
the issue result. All async waits use the common60s watchdog, not speed criteria.

Source excerpts select one node, at most64 natural fragments and512 returned
characters, excluding tools/reasoning/opaque bodies. Missing bodies remain known
identities with an empty excerpt. The selected path's fork-only ancestor answer
is not expanded; no idle-without-Run state, subscription or UI watch screen is
added. No new table/worker/queue/dependency, real provider, user data, installed
binary or owner swap is part of qualification. #80/#83 main is retained; UI edits
only initialize new optional DTO fields after their owner work completed.


### Final-gate integration findings (#74)

The first combined local gate stopped in lib tests:960 passed/3 failed/13 ignored.
The new receipt Thread-order index gave the submission fence an equivalent
Thread-prefix SEARCH instead of receipt_target; the plan test now allows these
two named prefix indexes while retaining the target binding and no-scan checks.
No retention assertion or production fence policy is changed. The lightweight
header intentionally leaves new optional CLI message bodies unobserved; its test
still enforces three statements and all shared facts, separately asserting the
canonical received message and the header's None fields. It does not add body
queries or drop the old header checks. An old-schema startup fixture left the
four new indexes while rolling back its ledger; it now drops those derived
indexes too. Runtime migration stays strict, with no IF NOT EXISTS workaround.
That touched fixture's10s guard moves to the shared60s watchdog, removing one
more stale allowance (two removed in this task). Revalidation follows normally;
these deterministic integration findings are not called unrelated CI flakes.

The next full gate passed lib tests but exposed the same old-schema fixture
omission in ten integration cases (index reset, message recovery, readonly RAG
schema rejection and storage/hot-query upgrades) plus one Run plan name check:
913 passed/11 failed/17 ignored in integration. All downgrade scripts that drop
the prior readback indexes now also drop the four new derived indexes, keeping
ledger/schema consistent before migration. The hot Run plan accepts only the
old one_live_run or new wait_run_state named index, with an explicit Thread
SEARCH predicate; unrelated plan checks remain unchanged. Production migration
stays strict and source-preservation/rollback/reopen assertions are retained.

## #76 explicit usage-limit and connection-cooldown qualification

2026-10-31, native macOS 26.3.1 arm64 / Rust 1.99.1. Temporary synthetic owners
and loopback HTTP only. No real provider, account limit, paid request, user DB,
installed binary, running owner, settings, tag or Release is changed by testing.
Build jobs are capped at 2 and test threads at 4. New expiry tests advance Tokio
time; guards use the shared 60-second watchdog, not latency thresholds.

- Typed future integer resets_at (preferred) / positive resets_in_seconds with
  exact usage_limit_reached type/code; direct and LiteLLM JSON-message/raw-text
  wrappers are tested. Missing, stale, fractional/string/overflow timestamps,
  unknown codes and prose are not inferred into quota facts. Relative seconds
  round upward; fractional-current-time boundaries and zero relative reset are
  tested. Retry-After seconds and RFC2822/IMF-style HTTP dates retain facts.
- Owner loopback scenarios cover six shapes: direct absolute, LiteLLM message,
  relative, long 429 Retry-After delta, long 429 HTTP-date, raw LiteLLM text.
  Each saves recovery_wait with one initial attempt and no automatic_retry;
  run get contains kind/UTC reset/window when present and /model guidance.
  A second Thread on the same connection recovers locally without POST; its
  fact has locally_blocked=true and no fabricated HTTP observation. Advancing
  Tokio by 3,602 seconds then allows a third Thread's ordinary successful POST.
  The real request JSONL per fixture is 0->1->1->2 lines. A long-header case
  withholds its advertised error body and proves recovery does not require it.
- Unknown 429 with no quota/reset fact retains ordinary retry (explicit zero
  Retry-After avoids real sleep), completes with two attempts, and logs 0->2
  request lines. Existing unknown-429/backoff/overflow/partial-output/stop tests
  also pass: the response_retries module has 24 passing tests. Non-quota 503
  above the existing ten-minute ceiling retains its independent defer policy.
- Shared-client tests cover both Responses and Chat Completions error paths,
  locally blocked second invocation without a live server, per-connection clone
  sharing/isolation, rejected unconfigured keys, later-deadline retention,
  virtual expiry, and empty slots in a fresh owner-scoped client table. Two
  configured connections still have exactly two entries, each with one fixed
  optional fact/time/duration slot; no timer, table, worker or queue is added.

Local standard check succeeds: Rust 1,894 passed / 0 failed / 33 ignored, layout
93, Python 18, fmt/strict Clippy. Rebase and exact-source native CI are separate
gates reported in issue #76. Initial development checks exposed an unboxed
error-size increase and a wrongly populated raw-text fixture; both were fixed
without suppressing Clippy, extending watchdogs or weakening assertions.

The first version does not infer other provider shapes, forecast limits, switch
connections, resume automatically, cancel already-in-flight calls or skip input
preparation. The generation gate includes summaries but excludes embedding and
model-list traffic. Long 429 headers return before body parsing, so those cases
do not extract body-specific plan/window details. Real-provider compatibility
is unverified. src/ui/activity.rs and src/ui/chrome.rs are untouched; #85 tracks
the deferred short status display, with the data contract in its comment
6540361176. Existing reason text and run get already expose the reset.



## F latest-main integration qualification (source742d523)

The F-only candidate merges E d0a92bf/main a0f7022. Native macOS standard
`sh scripts/check.sh` succeeds:2,016 passed/0 failed/35 ignored (lib1069/0/15,
integration928/0/17, termination12/0/1, forced7/0/2), Python18, layout/watchdog,
format and strict all-target Clippy. Gitleaks d0a92bf..742d523 scans15 commits
without a leak. The new parent migration17 passes rollback/reopen/fresh-schema
checks, retaining main schema16 observation indexes and canonical facts.

An explicit ignored P4 native measurement runs one actual lineage test at this
source:30 retained samples after10 warmups for each depth. Depth1: p50 3.415ms,
p95 3.607ms, max3.678ms; depth8: p50 3.822ms, p95 4.078ms, max4.126ms. The
fixture returns256 descendants and checks the cap/depth truncation policy.
Both satisfy the50ms target on this host; they are not Windows/Linux timings
or full selected-view/Watch/count P3 costs. The million-sibling stress remains
ignored/unexecuted, not a successful observation.

The combined standard includes actual separate-CLI scoped Watch lifecycle,
stale-retry guard, selected/List shared-snapshot and paired descendant tests,
current #84 composer contracts and all main #74 observations. The main #83
row vocabulary is byte-identical; F adds its own scoped completion hints to
main's composer without replacing paste provenance.

Open admission evidence: F native terminal80/wide/palette/fallback review,
retained-memory accounting, full consumer cancellation/contention and P3 costs,
formal F P2/P5 CI windows and ordinary3OS CI. This local success is not main
admission and does not qualify G. Publication/CI may follow E admission under
6540583854; F main still requires reviewer approval. No release, installed
binary, paid endpoint or user data was touched.


## E admission and F publication checkpoint

E is admitted at exact SHA d0a92bf after ordinary CI41181650657 succeeds on
Linux1949/0/32, macOS1948/0/33, Windows1817/0/24. Downloaded source_sha and
actual paired-snapshot harness lines match all3 OSes. The main fast-forward
from a0f7022 starts automatic main CI41182386543 at that same SHA. Scope and
remaining F/G were reported in #41 comment6540842872. No extra E migration
or #83/#84 source change was introduced.

F source742d523 also passes actual native macOS #84 PTY regression:8 conditions
(80/140 columns, dark/light, NO_COLOR and vt100),10 captures each, exactly one
local mock request per case and no paid API. Test-only pyte uses the existing
terminal-review virtual environment; default Python lacks that optional module.
This proves the retained paste/composer interaction, not full Watch visual
acceptance or physical Windows UX. Combined F standard2016/0/35 and P4 values
are recorded above; F native/P2/P3/P5/memory gaps remain open. F publication/CI
is authorized after E admission; F main admission is separately gated on review.


## F follow-up qualification and parallel handoff

Detailed source-specific F standard/CI, native Watch, P4, retained allocation
and combined-consumer P3 observations are maintained in
[the F qualification record](conversation-redesign-f.md). Discarded shared-target
evidence is kept separate from the isolated950c9ed success. Main admission
still requires the latest combined source gates and reviewer approval.
## Developer record-delta helper (#94.1)

2026-10-31, native macOS ARM64/Python3.12. The standard-library-only helper uses
Git's line-change semantics, including staged/unstaged changes, explicit refs
and worktrees, and produces one compact JSON result. Disposable Git fixtures
cover unchanged/additive records, middle insertions, same-count replacements,
deletions, missing/binary records, missing baseline paths, option-shaped invalid
refs and inherited Git routing. Seven new tests pass within the 25-test Python
suite. CLI help, py_compile, integration-layout/watchdog checks and diff checks
pass. The helper is immediately used on this task and the #91 worktree.
No Rust/product changes or build are needed per the user's scripts-only scope;
sh scripts/check.sh and native CI are not claimed. CI/integration is delegated
to worker4; no CI dispatch/main merge/issue close occurs here. #94.2 PTY capture
and #94.3 batch replacement remain separate subsequent changes. Native Windows
and Linux execution and the multi-day token/time reduction are not yet measured.

Rebase repair: #94.1 now sits on main9b4ae34 (#92) on a separate branch, without
force-pushing the original submission. The setup conflict retains both the
upstream batched/exact-SHA CI reuse section and the record-helper usage section.
The helper/test source is unchanged from 5bf85f3. The rebased scripts-only checks
pass again: Python25, layout94/watchdog, help, py_compile, diff and self-use record
retention. Evidence: /tmp/issue-94-1-repair-tests.log and
/tmp/issue-94-1-repair-record-delta.json. Rust/native CI are not newly claimed.

## Standard-library disposable PTY helper (#94.2)

2026-10-31, macOS26.3.1(a)/ARM64, Python3.12.7. The extracted #84 transport now
uses a bounded standard-library cell decoder rather than pyte. On base5fc5cc0,
32 Python tests pass: the 18 existing tests plus 14 VT/frame/config/CLI/Unix-child
checks. Coverage includes split UTF-8/escape sequences, CJK/combining/ZWJ cells,
indexed/RGB colors, alternate screen/wrap, cluster/output bounds, OSC52/hyperlink
removal, unsupported operations, explicit OSC52/no-auth loopback preflight,
minimal child environment, scratch argument paths, normal cleanup, replay,
output overwrite refusal, and rejection of image/raw-key actions before spawn.
Live Unix cases are skipped on Windows; portable replay/cell tests remain active.

No Rust build is run: validation reuses the existing #91 candidate binary
1e3b3be21c2787a8025c347ecba6ea5054a2441815155e9a3861d035222e9e42
and its matching visual_fixture. The extracted #84 driver passes eight sessions
and 80 completed screens across 80/140 columns x dark/light/NO_COLOR/vt100;
each session makes its one explicit synthetic-loopback model request. An in-memory
adapter binds #91's existing six-session band/canonical-copy fixture to the new
transport; all six pass, including decoded OSC52 body/selection/original copy.
It does not edit the #91 worktree or make pyte a new helper/test dependency.
The CLI's live help capture and offline replay are immediately used in this task.
A historical #83 80x36 static frame replays byte-for-text equal to its saved pyte
cell text; that is replay evidence, not a new product/terminal run.

Failed observations are retained: an initial test syntax error was fixed; a live
help wait for /copy timed out because that text was below the visible help page,
so the example now waits for its actual modal footer. The first adapter import
lacked the old fixture_safety module path and was corrected. A live legacy #83
row assertion expected the old human-looking mark while the reused #91 binary
renders external input as its provisional distinct mark; that run is not called
a passing main-baseline row-vocabulary test. The old static-frame replay and
#91-specific fixture validate the shared decoder separately without weakening
the legacy row assertion or rebuilding Rust.

Evidence: /tmp/issue-94-2-tests-final-pre.log, /tmp/issue-94-2-paste-final/,
/tmp/issue-94-2-bands-2/, /tmp/issue-94-2-live-final.json,
/tmp/issue-94-2-replay-final.json, /tmp/issue-94-2-old-row-replay.json.
py_compile, integration-layout/watchdog checks and git diff --check pass. The
#94.1 record-delta tool checks these records. Product changes and sh scripts/check.sh
are deliberately not run for this scripts-only stage per the user; no local CI
job dispatch, main integration or issue closure occurs. Three-OS CI belongs to
worker4. Full VT/emoji/font/IME/ConPTY/physical terminal fidelity and the multi-day
aggregate token/time reduction are not established by these checks.

Combined #94.1/#94.2 qualification: retain main9b4ae34, then #94.1@661de822 and
rebase the submitted #94.2 onto it on a new branch. All setup/status/acceptance
conflicts retain both submissions, including the #94.1 rebase note. The two
helpers/tests are byte-for-byte unchanged from their separate qualified SHAs;
product, manifests and workflow source are unchanged from main9b4ae34. Combined
Python39 tests, layout94/watchdog, py_compile, both helps, record-retention and
diff checks pass. Immediate live help and completed-frame replay pass again
with the existing #91 debug binary; no Rust build or paid provider is used.
Evidence: /tmp/issue-94-combined-tests.log, /tmp/issue-94-combined-record-delta.json,
/tmp/issue-94-combined-live.json and /tmp/issue-94-combined-replay.json. Native
three-OS CI and integration remain delegated to worker4; old branches are not
force-pushed. This qualifies combined scripts, not physical terminal/ConPTY or
universal VT/font behavior; the earlier failed observations remain recorded.
## #5 stage 1: shared owner bootstrap structural evidence

2026-10-31, native macOS arm64. The former frontend startup module moves intact
into `src/owner_session.rs`; only its module-title comment and visibility of
`initialize_reporting` change. A byte comparison after those two substitutions
confirms all function bodies and test fixtures are unchanged. TUI call sites
change module paths; no RPC, signal, environment, runtime or shutdown policy is
added in this stage. No new table, worker, queue or request channel is added.

The complete moved module is 285 lines before and after. Across changed Rust
files there are 7 inserted and 7 deleted lines (net zero), including moving the
module declaration. Argument counts are unchanged: initialize_reporting 4,
initialize 4, close 3, deliver 2, Prepared::shutdown 1 including self; all other
functions are identical. Three existing tests move with the module: cancellation
at database preparation, abandoned ready delivery, and recovery failure cleanup.

Validation commands are `CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=4 sh scripts/check.sh`
and the separate `test_validation_documents.py` unittest discovery. Exact-source
results, final SHA, record-line audit and integration status are tracked in #5;
this structural evidence alone is not an unrun native-OS test result. serve,
shutdown RPC, shared signal observation and real headless child-process tests
remain later stages. User data, installed owner/binary and releases are untouched.
## Folded-history scroll diagnosis (#93)

Native macOS ARM64, Rust1.99.1; starting main5fc5cc0. Temporary source fixtures
and real PTY/data-dir/loopback only; no user DB/log/clipboard/provider is used.
The user allowed the overlap with F67cc688 after the initial file-overlap stop.
The earlier summary request failed with EOF before response.completed; the one
authorized retry selected about half its source characters and completed
(1,744,861 ->1,531,188 replay characters). No additional summary retry was made.

On unchanged production source, the initial short-tool UI probes fail for
zero-distance Down resuming follow and history-focus Up returning no navigation.
A partial large-output source additionally sends LoadNodeWindow for hidden output
rather than LoadHistoryWindow for older nodes. The native initial harness first
failed its synthetic output_item.status NOT NULL constraint; that fixture error
was corrected, not hidden in the migration. Later native probes expose the
short local-projection problem while a synthetic model response is gated.

The saved ui_history filter intentionally suppresses window_applied when node
count does not shrink and follow is unchanged. Therefore an absent record alone
does not prove no metadata read happened. Typed request vectors, rendered old
command witnesses and the actual trace are used together. A partial repair still
showed a50-node user_follow pause at view revision51 and no older-command frame:
upward nonnegative targets in a short local neighborhood could be mistaken for
the forward edge, causing projection-focus oscillation until the finite guard.
The existing128-source/192KiB budgets can make the rendered neighborhood shorter
than the viewport, so that direction test matters even for a loaded50-node tail.
Those projection budgets remain unchanged; hidden closed output is not hydrated
just to navigate summary rows.

Native harness refinements retain the60-second shared watchdog: fixture FK/status
and a temporary catch-unwind delimiter error were corrected; repeated full repaint
requests hit the existing4MiB flood guard during failed investigations. Fixed
command expectations across a later bounded snapshot were not a valid source
anchor. The final observer synchronizes wheel gestures with actual repaint/footer
output and selects an older command witness from that frame, rather than relying
on a settling delay or a presumed final scroll position. Temporary debug output
is removed from product code before the final gate.

The local standard, final native fixture and latest-main rebase are independent
gates still to be recorded; branch/main CI is intentionally not dispatched by
this worker under the integration-worker workflow. Existing records are retained.

The final temporary-data native fixture now passes: real wheel input crosses the
initial50-node tool-result tail, observes an older canonical command (CMD069 in
the recorded run), completes the gated synthetic response, receives a separate
CLI title update and still displays older history. Saved ui_history records one
true->false user_follow pause and no false->true transition. Same-sized window
reads are intentionally filtered; visible older commands prove the older read.
Shutdown drains the PTY concurrently with child.wait using async readiness, so
PTY backpressure is not mistaken for an owner shutdown defect. The preliminary
full gate passed all other integration cases (934/1/17) but exercised the older,
non-draining harness; a final full gate must use the corrected fixture.

A controlled final comparison reran the same corrected synthetic fixtures with
only the three production UI files restored temporarily to starting main5fc5cc0.
The baseline fails six regressions (including actual PTY older-history progress),
4 other filtered cases pass; no time bound was changed. The script restores the
fixed files from dd55c23 on exit and verifies a clean tree. With the fixed UI,
the real PTY case passes in the recorded run: it shows an older CMD069 witness,
completes the held request, observes REFRESHED_PAUSED through the separate CLI,
keeps older history visible and follow=false, drains shutdown and exits0. That
5.14s observation is not a latency assertion. Only the shared60s watchdog guards
permanent stalls. No diagnostic-policy relaxation or new timer was introduced.

Final rebased source e80b723 on main9b4ae34 passes
CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=2 sh scripts/check.sh:
1,960 passed /0 failed /33 ignored; layout94, Python18, fmt, strict Clippy and
watchdog policy pass. The native short-history regression also passes in this
full run, without timing suppression. src/ui.rs changes exactly line1728 in
UiState::prepare_visible_render_width (function1697-1795); it only excludes
known closed result hydration. ui/history_interaction.rs and ui/view.rs own the
navigation changes. Existing projection/body budgets, rendering, Core and schema
are unchanged. The record is delivered to worker4 with review required; no local
workflow_dispatch or main push is authorized under this handoff workflow.

## Exact batch replacement helper (#94.3)

2026-10-31 native macOS/ARM64, Python3.12. The stdin-JSON editor accepts explicit
existing text paths within a chosen source root. All rules/files are checked in
memory before any target staging; zero/multiple/overlapping matches fail the
entire batch. Same-file rules follow JSON order. Empty/no-op arrays do not replace
inodes. UTF-8, CRLF and executable modes are preserved; missing/binary/linked,
read-only, non-owned and out-of-root paths are refused. Bounds are 4MiB JSON,
256 rules and 32MiB aggregate text. Targets on filesystems without hardlinks fail
before committing. CLI exits and recovery limits are documented in setup/help.

Python54 tests pass (39 prior plus 15 replacement tests): multi-file/sequential
rules, NFD/emoji/CRLF, nonunique/overlapping failures with unchanged content and
mtime/mode/inode, empty/no-op rules, invalid/missing/binary/directory/outside paths,
read-only/hardlinks, Unix symlink/executable mode, stage failure before any commit,
second-rename failure restoring the original first inode, interruption just after
rename, concurrent staging/committed edits, incomplete rollback retaining original
copies, text bounds and real CLI JSON/exit behavior. Faults are synthetic and
only temporary files are used. A code audit hardened the rename/interrupt recovery
window and excluded read-atime from preservation assertions before submission.
No external writer is overwritten in the explicit concurrency regression cases.

Ordinary I/O failures attempt full rollback. This is not an OS/filesystem
multi-file transaction: power loss, process kill, I/O failure during rollback and
uncoordinated concurrent editors cannot guarantee global atomicity. Recovery
failure exits3 with retained original directories rather than claiming success
or silently discarding originals/new user edits. Successful edits with cleanup
failure report leftover directories. These limits are not hidden by passing tests.

Immediate self-use: two JSON batches with 7 and 5 exact substitutions update the
new helper and tests, replacing repeated inline read/replace/write boilerplate.
No default user data/config path, clipboard or provider is accessed; no Rust build,
release/install change or paid model test is run. py_compile/help, integration
layout94/watchdog, diff and record-retention checks pass. Evidence:
/tmp/issue-94-3-tests-final.log and /tmp/issue-94-3-help.txt. Windows/Linux native
execution and crash/network-filesystem behavior remain unverified locally; CI is
delegated to worker4. The issue's original measurement is file rewrite327/1070000
characters, record checker186/530000, PTY29/80000. Those are the supplied baseline,
not a newly measured reduction. Multi-day follow-up with the same command-metadata
counting method remains outstanding; no savings percentage is invented here.

#94.3 input-shape revision supersedes b2c5b186: --blocks accepts literal whole
lines and --marker changes the exact delimiter prefix. The parser preserves LF/
CRLF payload bytes after UTF-8 decoding, does not split on Unicode line separators,
rejects missing/duplicated with/end and nested blocks, and passes the complete
rule array to the unchanged JSON preflight/commit engine. A malformed later block
cannot apply an earlier valid block. JSON remains available for partial-line and
no-final-newline changes; setup leads with the quoted-heredoc block form.
Python63 tests pass: nine new block tests cover multi-file/same-file ordering,
exact delimiters, custom-marker collisions, missing delimiters/nesting, empty and
bad markers, all-or-nothing nonunique matches, real CLI input and a real quoted
Unix heredoc retaining $variables, quotes and backslashes. The new --blocks form
is immediately used to update its own setup example (custom %% permits literal
@@ delimiters inside the example). Evidence: /tmp/issue-94-blocks-tests1.log and
/tmp/issue-94-blocks-help.txt. No user clipboard/config/data, provider, Rust build
or installed binary is used in these scripts-only checks. Existing rollback and
crash/concurrency limitations remain; they are not replaced by parser success.
## Input band structure and clipboard fixture safety (#91, in progress)

The implementation separates mark/body/optional right metadata, preserves source
anchors and canonical copy/original text, and applies band backgrounds to all
spans without changing foreground, emphasis or link identity. Unit coverage uses
all source classes, palette/plain modes, narrow metadata omission and wrapping;
frontend copy coverage uses injected FixtureClipboard and temporary stores.

An initial PTY run omitted ui.copy_backend and the Auto backend wrote synthetic
text to the host OS clipboard. Work stopped and the user authorized continuation
with explicit temporary OSC52 config. The prior clipboard value was not saved
or restored. This was a test-side side effect, not a passing copy validation.

A grep audit of scripts/, tests/ and examples/ found omitted copy settings in
synthetic config generators and omitted --config on several fixture CLI calls.
Those paths now explicitly use OSC52 and the fixture config. Shared PTY startup
rejects missing/Auto/OS copy settings before opening a terminal, strips agent
SHELLBOY context and overrides HOME/XDG directories inside the scratch fixture.
All fixtures retain explicit disposable --data-dir. Copy/image integration tests
inject FixtureClipboard rather than calling the native adapter. No direct
arboard/pbcopy/pbpaste/get/set-clipboard call was found in the audited tests,
examples or scripts. Default-configuration parsing tests intentionally still
verify the product defaults without performing clipboard I/O.

OSC52 itself is not an OS-clipboard sandbox when rendered by a GUI terminal.
The GUI capture helper therefore rejects explicit copy/image commands and
potential copy shortcut input; clipboard verification uses pipe-drained Unix
PTY output only. This conservative helper can reject ordinary text containing
c/C; use the PTY driver for such input. No GUI capture is run for this task.
These test guards are not a security boundary for arbitrary supplied programs.

Python fixture guard tests pass (20 total tests). The prior local check passed
1,894 Rust tests with 33 ignored before the safety changes/rebase; it is not final
source evidence. Early safe PTY attempts exposed ambiguous adjacent Esc handling
and a predicate that matched the word 名乗り in the synthetic body; cleanup now
preserves the primary assertion, waits for the full main scene, and clears item
focus before /quit. Failed watchdog attempts are not accepted sessions.

Final-source standard checks and the three candidate PTY observations follow
below when complete. Physical glyph rendering/fonts, actual IME/OS clipboard and
Windows ConPTY remain unverified. The user delegates CI and main integration to
worker4; no workflow_dispatch is run by this worker. Do not merge before mark
selection and approval.

### Final #91 local handoff qualification (2026-10-31)

The final Rust/scripts/tests source is a2c7b40ce2c34b059dc5d16e83dba1a0af484d55,
rebased on main 5fc5cc0c6d67e8986d220e09f1cd7cbe60b207bc (#76/E/#74).
Environment: macOS26.3.1(a)/25D771280a ARM64, debug, Python3.12.7/pyte0.8.2.
CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=2 sh scripts/check.sh exits0: 1,956 Rust
passed (1,008+929+12+7), 0 failed, 33 ignored; 94 integration-layout suites,
watchdog policy, 20 Python tests, fmt and strict all-target Clippy pass.
Binary/visual_fixture build, Python compile and git diff --check also pass.
The final regression preserves inherited line foreground/italic styling as well
as explicit span colors, emphasis and link identity while overriding background.

Three real product builds select only External's candidate index, 0/1/2.
Each runs 80/140 columns x dark/light/NO_COLOR: 18 accepted Unix PTY sessions.
Every session validates band cell backgrounds, name/no-name and wrapped/code/link
input, /copy, item copy and original-panel copy against stored canonical text.
Those 54 decoded OSC52 copy checks exclude marks, names and padding. The source
is restored exactly to A, rebuilt, and its six sessions independently repeated.
The comparison manifest fingerprints the tracked Rust/scripts/examples/tests
and Cargo inputs, records per-candidate binary hashes, and verifies exact source
restoration. The final A binary hash is 1e3b3be21c2787a8025c347ecba6ea5054a2441815155e9a3861d035222e9e42.

Earlier a0f7022/d0a92bf-based checks and captures are separate observations, not
additional accepted final sessions. Safe-driver failures exposed both ambiguous
Esc handling and a false-positive original-page predicate (main body/footer had
the same words). The driver now awaits the actual panel title and sender fact;
cleanup preserves primary failures and clears item focus before normal exit.
An intermediate assertion expecting identical binary hashes across separate
links also failed; no binary reproducibility guarantee or product regression was
inferred. Final qualification binds each actual binary hash and checked source
content, not equality between independently rebuilt binaries.

Evidence: /tmp/issue-91-check-handoff.log, /tmp/issue-91-build-handoff.log,
/tmp/issue-91-pty-A-handoff-restored/, /tmp/issue-91-mark-comparison-handoff/,
/tmp/issue-91-script-grep-full.txt and /tmp/issue-91-clipboard-audit-after.txt.
Screens and raw bytes remain synthetic temporary artifacts, not repository data.
The new #94.1 record-delta helper is used on these records to verify no baseline
lines were deleted. Functional/cell observations do not establish P1 performance,
physical fonts/terminals, actual IME/OS clipboard or Windows ConPTY success.
No resumed test writes the host clipboard or real user data/config; no installed
binary, release/tag or paid product API is used. Knowledge search is separate
from product verification. This worker does not dispatch CI, integrate main or
close the issue; worker4 owns CI/integration after user mark selection/review.
Keep the branch and worktree until confirmed main integration.

## User-selected B external-input mark (#91 final)

2026-10-31: the user selected and approved B (⇥); External remains 外から.
On main2b3cd91 with integrated #94.2, B Rust/source qualification c9651bb passes
sh scripts/check.sh: Rust1956 passed/0 failed/33 ignored, Python41, layout94,
watchdog/fmt/strict Clippy. The explicit selected-mark/label/width_cjk()==1 test
passes. Build and six fresh 80/140 x dark/light/NO_COLOR PTYs pass with B,
including18 canonical body/selection/original OSC52 copy checks. Shared PTY/VT
APIs are unchanged from main and retain fixed OSC52/no-auth/scratch preflight.
Evidence: /tmp/issue-91-B-check.log, /tmp/issue-91-B-build.log,
/tmp/issue-91-B-final/results.json and /tmp/issue-91-B-record-delta.json.
The external-CLI row fixture expectation is updated from the obsolete human-like
mark to B; an edit indentation error was caught by py_compile and fixed before
submission. This is a test-only change after Rust qualification; src is unchanged.
No physical-font/ConPTY/IME or performance qualification is inferred. Three-OS CI,
main integration and any user-binary replacement remain delegated; provisional A
must not land. No installed binary, release or real OS clipboard is modified.

## #96: integration race source-order qualification

2026-10-31, native macOS arm64 / Rust 1.99.1, synthetic databases, a real owned
Unix shell and loopback fixtures. Builds use CARGO_BUILD_JOBS=2; local tests use
RUST_TEST_THREADS=4. No provider billing, user data, installed owner or binary
was used. Native Windows/Ubuntu qualification is integration-owned.

Ubuntu run 41192807282 at afe4c087 reported no second model request and the
status "No foreground shell is being awaited". The fixture's running-row test
precedes its single Background action. No rendering happens before that failure;
#91's diff does not change the fixture, Control, or Background dispatch. A manually
polled real run_shell future commits running while its caller has not resumed.
Against the old order, the first Background deterministically returns the same
InvalidState (the fixture reaps its process before asserting red). Registering
the current call's receiver before reapply_shell_intent publishes running makes
that test green. Its two-line temporary witness records running then admission;
0->2 lines, not a model request log. Explicit background mode remains receiver-
free, failed publication clears the slot, and completion/later-call isolation
rules are retained. The original integration multiple_calls test passes without
changing its commands, assertions or waits.

Windows run 41192626561/job124512348162 reported draft revision 99, 30 user nodes,
30 accepted receipts and an unfinished frontend. Send workers held their 32
permits across optional HistoryReader::saved display work. Quit drops the
background result receiver but did not cancel those reads; stalled presentation
admission could prevent remaining closing submissions from obtaining a permit.
A stronger version of the existing real closing fixture holds both presentation
slots until its end. The old boundary deterministically fails with exactly 32
saved inputs and frontend unfinished, while retaining the original 10-second
watchdog and every 40-input/draft/FIFO/force assertion. The corrected boundary
passes with both display slots still held: only the optional post-commit
projection observes the existing frontend cancellation token. Durable sends
are never raced against cancellation. Ordinary send/read admission capacities,
UI critical retention, and shutdown ordering are unchanged. A poll-level test
also verifies cancellation releases a pending display future and never starts
new display work after cancellation.

These source counterexamples reproduce each failure class. The original CI did
not save foreground-registration or presentation-admission traces, so exact
past interleavings, or whether other contributors existed, remain unproved.
They are not declarations that a rerun alone fixed an intermittent failure.
The targeted foreground test, 16 outbox tests and unchanged real TUI recovery
test pass locally. New asynchronous guards use the shared 60-second watchdog;
legacy targeted guards were not extended, assertions relaxed or tests ignored.
Full latest-main standard checks, role/delta audit and exact submitted SHA are
recorded in #96 rather than substituting an older tree's successful result.

Latest-main local qualification on afe4c087 plus #96 succeeds: standard checks
1,965 passed / 0 failed / 33 ignored, layout 94, Python 41, watchdog policy, format
and strict all-target Clippy. The separate document-role test passes; bundled
docs checks pass 33 CLI checks / 13 guides / 3 categories / 147 links, in empty
cwd with cleared environment, absent config and no owner API. No earlier source
success replaces the submitted tree's prepush recheck. Build contention was
observed while other task builds ran; it is not a runtime performance result.

Windows CI repair: batch71960f9 run41192167865 failed
ReplaceTests.test_custom_marker_allows_default_delimiters_as_literal_payload;
Linux/macOS passed. The fixture used write_text on LF payload text, which uses
platform newline conversion on Windows, while the parsed before string stayed LF.
The fixture now writes explicit UTF-8 bytes. Exact matching/parser/product behavior
is not weakened or changed. A portable regression proves LF does not match a
CRLF target, leaves it intact, and an exact CRLF block then succeeds. Python64
passes locally; py_compile/layout/watchdog/diff/record-retention pass. Windows
success requires a new CI run and is not claimed from this macOS result.
Evidence: /tmp/issue-94-crlf-tests.log. This supersedes 7115399 as well as the
older JSON-only b2c5b186; it is independent of #91 B's passing-parent repair.

## #98 tool rows, right-side facts and heading-only focus

Base main f1e2a16 includes #96. On macOS arm64/Rust1.99.1,
CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=2 sh scripts/check.sh passes: Rust1969 passed,
0 failed,33 ignored; Python66, layout/watchdog policy, format and Clippy pass.
The bin/visual-fixture build also passes. The target cache is owned by this
worker at ../issue-87/target; the #87 work-in-progress source is not compiled or
changed by #98. No installed binary, real provider, clipboard or user data is
changed. Evidence: /tmp/issue-98-standard-v2.log and /tmp/issue-98-build-v2.log.

Synthetic tests cover normal ID suppression without losing identity fields,
running versus true background/release states, separate body/metadata spans,
long-body metadata omission and retained attention, palette colors, shared
notice indentation including wrapped rows, and heading-only focus with no column
shift in dark/light/NO_COLOR/non-256 modes. Clock painting retains richer cached
background/release facts while changing seconds only, even if its source node
has less correlation detail. No elapsed tick performs a query or Markdown parse.

Actual loopback SSE and native PTYs pass eight conditions (80/140 columns,
dark/light/NO_COLOR/vt100). Each runs one completed, one stdin-held foreground
and one stdin-held background command. Compact rows have no execution-ID prefix;
foreground has no background label, true background does, and exit/duration are
right-aligned. The driver completes the synthetic shells through their exact IDs,
clicks the foreground heading, opens real output, verifies one colored focus row
(or emitted bold SGR fallback), and closes it. Note rows share item indentation.
Evidence: /tmp/issue-98-native-v2/{manifest.json,CASE/*.txt,CASE/raw.vt}. The plain
VT decoder does not model font attributes; exact heading-only fallback styles
are additionally verified by TestBackend, while raw VT retains emitted SGR.
Physical font rendering and Windows native PTYs remain untested.

Failed observations remain recorded: initial focused compile exposed a lifetime
erasure in body_spans cloning and was corrected with owned display spans.
The first full check failed an old left-side literal expectation in
multiple_calls_keep_source_order_numbers_commands_background_and_cancel_reasons;
the test now checks command at left and the human-background reason on the same
row at right, without weakening state/order/cancellation assertions. This also
exposed lost correlation detail during time painting, now fixed with cached
metadata preservation. The first native driver selected a notification instead
of a tool and mistook composer borders for output guides; the corrected driver
clicks the exact rendered header and requires the specific output text. These
harness failures do not establish a foreground publication-order regression.

The user permits #98 review to be unnecessary as restoration of B. #97 owns the
future selected description/command/ID inspection surface; existing exact source,
expanded/original and F6 diagnostic references remain intact. #99's timeout and
closed-preview ownership corrections are NOT claimed by this #98 result and
must be handed off separately as review-required.
### #95 third failure added during investigation

The user recorded macOS645cd36 run41189322766's pinned-tool wait at line1031.
A request-boundary assertion deterministically rejects the old fixture's follow
state before dispatch (red), rather than relying on a longer timeout to observe
its later hang. A pure UI regression then exercises non-tail-window arrival both
before and after the initial tail is cached: following correctly falls back when
there is no tail; already-paused navigation retains the old node in both orders.
The original fixture now pauses and anchors before requesting ordinal0. Its
running->exited metadata, retained text/base offset and anchor assertions remain;
all three10-second legacy guards are unchanged, with no skip/retry allowance.
The focused real-owner case passes three native macOS runs. UI source changes
in this increment are confined to cfg(test); this is not a retention-policy fix.
The original CI had only Elapsed, so its exact cached-node state was not saved;
the deterministic counterexample establishes the invalid prerequisite and repair.

### #95 rebase onto admitted #91 B / #93

The adopted-design EOF conflict keeps the complete afe4c087 conversation design
(including #91/#93) and appends the approved paired-snapshot rule once. No source
conflict required manual edits. The approved cb75209 src/tests patch and rebased
patch have stable ID 7ecc4bde107275ae8ccf7737fdc8fa9ce5488b2c. Independently,
applying the approved source patch onto afe4c087 in a temporary Git index yields
an exact byte-for-byte src/tests tree match with the rebased candidate. This is
source-patch equality, not a claim that the older whole repository equals main B.
Latest-tree standard checks and exact resubmission SHA are recorded in #95;
Windows split x3 qualification remains integration-owned, with no local dispatch.

### #95 ordinary Windows return: no periodic echo behind an in-flight view

2026-10-31, native macOS arm64 / Rust 1.99.1, CARGO_BUILD_JOBS=2 and
RUST_TEST_THREADS=4. Failed ordinary Windows run 41198580797, product 8c4fed7,
reported two selected observations instead of one at frontend_reactivity.rs:902.
Seven latency lines put this at step 6; the last two report about 4.81s/3.39s.
Old-product split runs 41198667760, 41198669358 and 41198671273 all passed their
two shards and audit, but do not override the ordinary failure or qualify this
new source. No rerun/exception was used to erase that failed ordinary result.

Source counterexample: Changed sets progress_running until activity returns,
even for an idle Thread. The old tick reinserts the selected Thread into dirty
while refreshing already contains it. A delayed snapshot/projection can thus
return idle activity and leave a redundant second read queued. The tick helper
was first extracted with exactly the old rule. A fixed-order eight-tick test
failed (0 passed/1 failed, exit 101) because it left a dirty Thread behind. The
correction filters only the existing in-flight set, without new lifecycle state,
epoch, timer, worker, queue, pool or capacity. Real Changed during a read still
advances the epoch and remains independently dirty; stale activity is rejected.

Six selection tests pass after the correction, including that real-Changed
case. The synthetic periodic-poll-witness.jsonl grows 0->9 rows (eight ticks and
idle completion); no provider log is involved. The original 16-update E test,
including exact one-observation, paired sequence and no-reload assertions, is
unchanged and passes three explicit native macOS runs (48 CLI edits). The actual
CLI was rebuilt explicitly before these subprocess checks rather than inferring
its identity from a previously shared target. Watchdogs/production cadence and
retry behavior are unchanged; no assertion or test exclusion was weakened.

This reproduces and closes a source ordering class consistent with slow reads;
the old CI lacks the full admission/tick trace needed to prove its exact past
interleaving or exclude all other contributors. Standard/doc checks and final
SHA are reported in #95. Native Windows/Linux validation on this new source is
still an integration gate, not inferred from local success or old split runs.


## G forms and standalone-client local checkpoint333fd9d

Commits are separated by responsibility: c4bc793 (owned RAG input and its tests),
3b16ef5 (bounded popup cache/geometry foundation),333fd9d (typed forms/client
routing). The combined production/test source at333fd9d passes native macOS
`cargo test --locked --lib`: **1124 passed/0 failed/15 ignored**. Actual harness
lines verify all35 operation-form tests, all61 identities/45 owner forms/16 client
builders, current readback/memory/settlement arguments, three owned-ingest tests
and the scoped Docs-failure/RAG-init client-dispatch test. Initial focused passes
were33/2fail then34/1fail: stale59/60 count assertions and an old guide caption;
all full mapping/argument/screen assertions are retained with current61/62 and
catalogue captions. The final full library run has no failures. Fullstandard,
strict all-target Clippy, native form captures, current-source P3/P6 and3OS
admission are **not yet completed** for G.

The catalogue now runs in the normal UI build; the earlier G1 test-only boundary
is superseded. F8 uses actual typed owner/standalone clients, effect markers,
nonselectable headings, filtered/action caches, operation-specific explanation,
field origin/discovery help and display/execute/send labels. No new Core command,
protocol action, table, worker, queue or generic form interpreter is added.
Input remains bounded1MiB/form and16MiB at the RAG shared boundary. Only the CLI
wrapper reads stdin; TUI dispatch passes owned text and frozen startup paths.
The scoped test replaces2ms polling/raw timeout with a bounded notification
forwarder and existing test_watchdog helper; watchdog policy passes.

Remaining: command/result titles still have an older independent frontend match
and must use the typed catalogue; full popup wheel ownership/cache work is not
qualified by this foundation. Normal-build unused helpers for that pending work
were visible in the intermediate cargo check; do not report a strict clean check.
General saved-inspection reactivity, shared readonly admission, Core projection/
index rationale, actual mutable-target tests, native forms and final gates remain
open in G1–G12. #100 overlap notice6544035614 is retained; do not overwrite its
independent F-based fix. No G branch push/main/release/install/user-data change.


## G1–G4 local standard atf9c2d73 (not G admission)

Sourcef9c2d7337660447fb1526430479cf9ab3f0f1b75 passes native macOS arm64
`sh scripts/check.sh`: **2089 passed/0 failed/35 ignored**, Python66,
layout/watchdog/fmt/strict all-target Clippy. Totals derive from actual harnesses
(1131 library,939 integration,12 block-replacement,7 frontend-structure passes).
Log/source audit remains local. The source is split into title/catalogue mapping,
shared popup wheel/cache, integration caption expectations, nested wait metadata
and guide commits. No full G or3OS/native-terminal claim is made.

Actual new tests cover painted top-owner bounds/epoch, outside/edge/F2/resize
no-ops, same-kind reader/confirmation replacement, queue/structured selection
without execution, immutable original-copy preservation, and one wrap build over
repeated paints/wheels/confirmation keys until width changes. Confirmation height
also uses the cached row count. All production confirmation constructors share
cache/hit-target retirement. Help/details/management readers use lazy source
preparation; model and operation filters cache their row selection. Original
source bytes/copy IDs are not replaced by wrapped/truncated cache rows.

Earlier runs are retained: result-title library1123/1/15, first G4 library1130/1/15
(old unit captions), then unit1131/0/15 plus integration936/3/17 (old result-title
predicates). Eleven integration caption strings were changed to the canonical
catalogue labels; no identity/body/cancellation/scope assertions or watchdogs
were relaxed. Final actual success lines include stale-report ownership and all
three formerly failing original/overview/wait integration tests. Secret scan
fff2076..f9c2d73:12commits/no leaks. No new timing/variance measurement was run.

Still open: native form/popup captures and actual P6 counts/observations; any
remaining non-reader list/form wrapping-cache inventory; all saved-inspection
read/reactivity/source/notification domains and shared-read P3; necessary index
proofs/migration; final standard/CI/reviewer main admission. The archive's twelve
inspection targets must not substitute for the full mutable-field inventory.
The adopted design records status/history/readback coverage and narrowly justified
on-demand/external snapshots plus why private domain helpers may belong in Core.
#100's separate F-based correction remains to be retained on reconciliation.
No G push/main/release/install/user-data change occurred.
## #95 remaining approved changes on F, without E-state replay

Base is admitted F fff2076. Ancestor checks confirm periodic-echo8677807 and
pending-selected307e531 are already incorporated. The new candidate leaves
src/frontend.rs, browser.rs and selection.rs byte-identical to that F base.
Do not interpret a whole E-tree diff as remaining work: F has new Watch/schema/
UI behavior that must remain intact.

The four remaining files (model/http/deadline.rs, tests/response_retries.rs,
frontend/control_tests.rs and ui/view.rs) are unchanged between9fd7b1c and F.
Apply only their previously approved9fd->b01dfe0 delta. After application, each
of these four files compares byte-identical to approved b01dfe0, while the F
frontend guard files compare byte-identical to F. No manual source adaptation,
new policy, #100 viewport/stale-ID patch, reproduction or #103 documentation is
mixed into this branch. Remaining navigation changes are test-only, preserving
all existing product retention semantics and assertions.

The source includes ready-header priority with expired-entry no-poll safety,
paused-clock permanent-error/no-extra-request tests and the pinned-tool fixture
intent-before-dispatch correction with its tail-hydration-order regression.
Fresh local standard/doc-role/bin/docs results and SHA are recorded in #95.
Old0ce ordinary/split successes remain valid only for old source; native/split
qualification of this post-F candidate is a new integration gate.
## #103 terminal-owned link guidance (documentation only)

Source: issue #103 decision comment6543709978 and the user's WezTerm/macOS
observation in comment6543687265 on main afe4c08. The user opened a displayed
file URL with Shift+click under the terminal's default rules. This is not an
agent-run physical-terminal test of the new guide or a universal OS guarantee.
The short optional ASCII absolute-POSIX-path example follows WezTerm's current
hyperlink_rules documentation (default_hyperlink_rules and capture format $1);
it is not a Shellboy path detector and was not installed into a real config.

On native macOS, Python document-role1 and README2 tests pass; both updated User
Guide links resolve to the new source TUI section. check-docs.py against the
existing installed executable passes33 CLI checks/13 guides/147 bundled links
with empty cwd, cleared environment, absent config and no owner/API or state
creation. This checks that executable's pre-existing catalogue, NOT embedding
of the edited Markdown. The edited bodies and new bundled link must be qualified
by the next combined exact-source CI; no Rust build/test ran locally. Neither
installed-binary bytes nor user configuration/data/OS clipboard were changed.
The Japanese User Guide matches the English guidance. The optional bare-path
rule, customized terminal bindings and physical iTerm2 behavior remain untested.
## #100 post-F reproduction and fitting viewport correction

2026-10-31, native macOS arm64 / Rust 1.99.1. New task worktree starts at admitted
F fff2076. Cherry-picking saved test3de75eb required only an additive EOF merge:
all existing F test source, including 2b9514e, was retained before appending the
reproduction. No E-specific browser/admission patch was replayed.

After explicitly building this source's CLI, the preserved real-CLI/held-loopback
model reproduction is red at cycle0: header/internal membership and CLI retain
three rows, the promoted worker title is not painted, and closing/reopening
restores it before the assertion fails. This is an assertion failure, not a
watchdog failure. On the same unmodified F source, the original 2b9514e open-list
foreign-running-row test passes. The tests differ deliberately: the original
checks membership, while the new test also checks actual painted text.

List sorts/retains membership correctly but restores a selected-screen anchor
by increasing TableState offset. At unchanged geometry it previously clamped
only when the viewport grew, even if every row fit. Reconcile a fitting List's
offset to zero. A separate Watch unit is also red on F: its c5785c5 formatting
band computes a local clamped offset while the actual TableState still starts
below the first row. Set the actual fitting offset before formatting/rendering/
visible-detail scope. Keep bounded neighborhood formatting; do not format all
2000 rows to mask the problem. No timestamp, database, membership or browser
read-admission changes are made. The withdrawn future-timestamp lead is not a
clock bug or a reason to repair user data.

After correction, all 58 ThreadBrowser tests pass, including the unchanged
2000-row viewport-neighborhood count assertions. The actual CLI reproduction
passes four alternating running/stopped worker cycles (browser-witness.jsonl
0->8 rows, unchanged browser generation/page count), and the original F test
passes all 16 competing/running-row reorders. New waits use the existing shared
watchdog; no speed criterion, allowance, retry, ignore or assertion relaxation
was added. These are synthetic UI buffers/owned CLI processes and loopback only,
not a trace of the user's original terminal geometry or real-provider behavior.
Final standard/doc-role/bin/docs/record checks and SHA are recorded in #100;
fresh native CI and user review remain the integration coordinator's gates.

## G first saved metadata readonly checkpoint

Source: G worktree based on merge dd1ae66 (main d29e0fc plus G1–G4), with the
first saved-metadata extraction in this commit. Native macOS, pinned Rust1.99.1;
synthetic temporary databases/configuration, no model API or user database.
`cargo test --locked --lib frontend::inspection_saved::`:8 passed/0 failed.
`cargo test --locked --lib`:1143 passed/0 failed/15 ignored.
`cargo clippy --locked --lib -- -D warnings`, watchdog policy and fmt pass.
The initial library compile failed because the fixture awaited DbReply directly;
it now uses its canonical receive method. This was a test compile error, not a
product failure or suppressed retry. The first targeted run after correction and
the full library run both pass.

The blocked-writer fixture exercises actual management_request for all three
commands while the writer is gated; release and shutdown precede assertions.
It verifies exact target identity, observed time and no operation-receipt growth.
Other tests cover canonical message/shell parity, tombstones, terminal Revisit
rejection, malformed/oversized metadata without invented zero values, combined
64KiB JSON budget, closed allowlist before SQL and bounded latest-output work.
No elapsed-time acceptance threshold is introduced; the shared60s watchdog is
only a deadlock guard. Optional scalar copies are bounded before decoding;
required identity overflow is explicit. This does not qualify the full standard,
integration suite, live refresh/shared snapshots, P3/P6, native popup screens or
3OS CI for this source. Those remain open in the G ledger.

## G bounded Run/Turn extraction — local standard checkpoint

The next G unit adds readonly Run/Turn metadata and retains initial saved usage
totals in that same transaction via the canonical saved-usage helper. Context
reason/lifetime preparation is shared with the existing domain implementation.
Required lifecycle scalar overflow is explicit; reason plus Revisit copy shares
64KiB, and owned Run/execution lists report their128-row coverage limit. Actual
prefix VM work remains constant between150 and1150 matching children; small
results retain canonical order. Complete latest-Turn lookup work remains a G9
qualification item for equal-time rows; a finite result alone is not that proof.

Native macOS, Rust1.99.1, temporary synthetic databases and loopback-discard model
configuration only. The13-test targeted saved/lifecycle run passed after fixing
an invalid oversized-JSON fixture. A following full library run passed1149/0/15,
including exact canonical totals and deleted-scope markers; strict library Clippy
passed. The first compile required wiring existing pure context helpers and the
correct collected tuple type. The first targeted test run was4/1 because the
fixture used non-JSON text where SQLite requires JSON. Later standard v5 passed
Clippy/Python/layout/watchdog but stopped1149/1/15 in the library: a newly added
ordering fixture used NULL where started_at is NOT NULL. That fixture now uses a
legal empty timestamp; no schema constraint, assertion or watchdog was relaxed.
Final `sh scripts/check.sh` v6 completed successfully on this commit's runtime
source:2109 passed/0 failed/35 ignored (library1150, integration940, termination_signals12, tool_group_crash_recovery7), Python66, layout94, watchdog/fmt and strict
all-target Clippy. The exact initial totals/deleted marker, legal empty ordering,
blocked writer across all five targets, and existing overview/history/edit detail
integration tests all ran and passed. These results do not qualify live subscriptions,
shared500ms/3s admission, P3/P6, native popup layouts or three-OS CI for G.

## G exact operation receipt extraction

This commit on9adcb77 adds the operation primary-key readonly reader and shares
canonical receipt decoding/envelope preparation with CLI. Native macOS, Rust1.99.1,
synthetic temporary databases. Targeted saved-inspection suite21/0 (including six
operation cases), canonical control::receipt suite10/0/1 ignored, integration
operation_contract suite11/0/1 ignored, strict library Clippy and fmt pass.
The actual blocked-writer management request covers six target kinds and unchanged
receipt count. Operation tests prove canonical null/expiry/old-instance parity,
message and queued/running/completed compact downstream facts,64KiB aggregate
witness limits, no SQLite JSON parsing/copy of8MiB outcomes, explicit corruption/
missing errors, and constant exact-key query work with1000/10000 unrelated receipts.
This checkpoint has not rerun the full standard and is not live subscription,
shared admission, native-layout/P6 or three-OS qualification. No schema, worker,
queue, operation or authorization change; replay/expiry semantics stay canonical.

## G bounded saved attachment references and common field admission

Runtime source: this attachment/reference unit plus its common-field parent,
on288ec0d. Native macOS, pinned Rust1.99.1; temporary synthetic databases and
loopback-discard configuration; no model API, user database or installed binary.
Final `sh scripts/check.sh` v6:2124 passed/0 failed/35 ignored (library1164,
integration941, termination_signals12, tool_group_crash_recovery7), Python66, layout95,
watchdog/fmt and strict all-target Clippy pass. Actual named tests for bounded
metadata roots, exact selection outside the list cohort, explicit foreign record
scope/source, blocked writer across eight read routes and existing structured
history/detail navigation all ran and passed.

The actual frontend integration reads a150-item bounded list, opens a saved
operation's attachment outside that cohort using its explicit foreign Thread,
then follows the source to the exact saved text/node. Its fixture forwards finite
128-slot channels by notification and joins the receiver off the async thread;
it uses the common60s safety watchdog, no elapsed-time acceptance assertion.
Exact-selection VM/fullscan work stays unchanged at1000/10000 other selections.
Canonical small rows retain managed-file unavailability precedence, delivery and
selection/source/tombstone facts; oversize paths/reasons remain explicitly limited.
Neither the saved list nor exact selection performs a filesystem availability
probe. The canonical exhaustive CLI still provides that explicit native check.

Retain observed failures: first attachment suite24/1 found missing canonical
managed-file reason precedence; it was fixed by sharing the existing pure rule,
without broadening CLI reason-copy work. A new reference guard initially failed
to compile because its test moved a boxed value in a match guard; it now borrows.
Next targeted runs27/0 and reference1/0 passed. The initial actual frontend v3 hit the safety watchdog because expansive rows
starved scope metadata; the shared collector now reserves common metadata under
the unchanged128-field/64KiB cap. V4 passed that stage but hit the watchdog later
while the fixture awaited an incorrect original-source pointer.
A v4 targeted filter matched zero tests and is not evidence; the corrected named
collector test and final full standard both executed it successfully. The fixture
now asserts each completed report immediately; v5 exposed a wrong fixture source
pointer (`/text/content`), since canonical history returns plain text at `/text`.
That expectation was corrected to exact source text; v6 dispatch passed before
the standard and again within it. No watchdog was increased or failure retried
without a code/fixture correction. These are not completion of G's live update/
shared-read/domain coverage, P3/P6, native-layout or three-OS gates.

## G7 bounded storage facts and separate physical observation

This commit on c3b3a17 adopts the private GC observation reader and an initial
explicit frontend storage read. Native macOS, Rust1.99.1, synthetic temporary
SQLite/files only. GC observation suite8/0, actual blocked-writer management
route1/0, strict library Clippy, fmt and watchdog policy pass. Tests compare
small complete values/retention eligibility with canonical CLI accounting,
prove bounded actual SQL work with100000 scalar rows, retain incomplete lower
bounds/overflow/unknown values, separate physical timestamps, bound directory
entries, reject symlink traversal and exercise the existing finite file pool.
The management fixture releases its writer and shuts down before asserting;
storage has no invented Thread scope and its read adds no operation receipts.

The first policy check rejected five ad-hoc positive file deadlines from the
archive. They now use the shared60s Deadline helper through its new at method;
explicit expired-input coverage remains intentional. Two fixture compile errors
used nonexistent/default StorageArgs constructors, corrected to the actual
StorageArgs{category:None}; no public type/default was added. Initial explicit
storage still reports saved totals separately from its physical sample. Current
CLI's exhaustive scalar protocol and collection/eligibility are unchanged; the
existing frontend assertion now verifies the new byte-observation value and
complete flag instead of the old exact-scalar presentation. Integration/full
standard on this unit, live refresh cadence/visibility/cancellation, P3/P6/native
popup screens and three-OS qualification remain unperformed.
## #5 stage 2: terminal-free startup

2026-10-31, native macOS arm64 / Rust 1.99.1. An actual child executable starts
with stdin null and stdout/stderr piped, an explicit relative data directory,
empty synthetic config, and bogus parent SHELLBOY data/socket/instance/origin
values. Its ready row identifies its own absolute directory and fresh instance;
two real read-only CLI requests succeed after EOF. A second owner on that data
is rejected, the parent data directory is not created, and receipt/model-attempt
counts remain 0. Captured stdout JSONL grows 0->1 row, remains 1 after repeated
CLI reads and forced fixture cleanup; stderr remains 0. This is not a normal-
shutdown result (stage 3 is separate).

Missing/empty data, invalid serve syntax and inappropriate operation IDs produce
0 stdout / 1 stderr JSON row without opening the inherited data directory. A
non-UTF-8 directory cannot panic readiness serialization: startup rejects it
and closes any acquired owner/endpoint. The latter fixture is Unix-only. All new
process waits use the existing shared 60-second watchdog, not a speed assertion.
The first native path assertion exposed macOS /var versus /private/var aliases;
its expected path now uses filesystem canonical identity, not relaxed readiness.

Existing driver assembly overrides each shell's data/instance/Thread/Run/Turn/
execution/origin; Windows additionally clears an inherited Unix-only socket.
API keys/proxy variables are not globally removed. There is no runtime global
namespace mutation, new table, worker or queue, nor an installed-owner change.
Exact-source standard/doc-role/bundled-guide checks and CI integration are
reported in #5. These local startup fixtures do not qualify Windows nested Jobs,
parent-owned shell termination, normal shutdown, all crash boundaries or a real
provider. Later stages must establish those separately before declaring #5 done.

### Stage 2 restart and readiness-failure fixture correction

After the 20:34 owner restart, worktree/branch state was inspected before any
rerun. Only the expected startup-row specification and process fixture were
uncommitted. The completed #95 serialized check/push had saved exit 0 before
shutdown; interrupted work is not counted as a passing run. Stage 2 is rebased
onto main containing #91/#93 and approved #96, without the private stage 3 draft.

The initial failed-ready fixture timed out because Tokio 1.54.1 Command::output
unconditionally replaces stdout/stderr with pipes (process/mod.rs:1065 onward).
That bypassed its configured read-only stdout handle, so serve correctly stayed
alive after successful readiness. The fixture now uses spawn/wait_with_output
and explicitly pipes stderr, preserving the read-only handle. It retains the
shared 60-second watchdog and exact output/cleanup assertions; production
startup, shutdown and timeout behavior are not changed to accommodate the test.

The first rerun after restart also identified an old non-serve normal executable
in the previously shared target: its actual --help lacked serve, and all four
new subprocess tests rejected its stdout. That run is not serve qualification.
An explicit local bin rebuild and serve-help check precede subsequent process
checks; prior shared-target artifact identity is not inferred from Git HEAD.
The actual corrected process results, standard check, record delta and exact
submitted SHA are recorded in issue #5. Windows/Linux native execution, nested
Jobs, parent termination and shutdown/crash boundaries remain later gates.

### Correction to the inherited stdout fault fixture

The fresh serve binary passes missing/invalid startup, non-UTF-8 cleanup and
EOF/duplicate-owner tests. The read-only stdout fixture still waited because
Rust stdout can treat EBADF as a silent sink on Unix: a standalone native probe
with the exact writeln/flush pattern returned Ok for read-only stdout, but
BrokenPipe for a writer whose reader was dropped. Thus read-only stdout was not
proof of a failed ready write; do not classify that timeout as a product cleanup
deadlock. Preserve the failed attempt and replace the fixture with portable
std::io::pipe, dropping its reader before spawn. Keep explicit spawn plus piped
stderr (Tokio output would replace the configured writer), the shared watchdog,
and exact error/endpoint/lock/database integrity assertions. Production behavior
is unchanged. The final fresh-source process and standard results are recorded
in issue #5; earlier read-only fixture descriptions are superseded by this note.
## #99 development checkpoint (not final qualification)

The first serialized focused run passed 44 preview-related tests. The initial
standard run stopped at Clippy (test-only imports, a now-test-only source accessor,
and a collapsible eviction cancellation branch); these were corrected without
lint allowances. The next standard check completed exit0: Rust1975 passed,
0 failed,33 ignored and Python66 passed, with layout/watchdog/fmt/Clippy.
The bin/preview_gate example build also passed. Evidence:
/tmp/issue-99-focused.log, /tmp/issue-99-standard-v2.log and
/tmp/issue-99-build-v2.log. Newer terminal-no-raw proof, shared observation ranking
and fixture-request-counter changes still require their next serialized run;
this record does not qualify those untested deltas.

Eight real PTYs passed actual DB-worker-gated open, close while pending, reopen,
release, raw-output delivery and final close (80/140,dark/light/NO_COLOR/vt100).
Each also received three real Thread-title Changed updates while keeping its
observed preview notice unchanged. Raw/frames are /tmp/issue-99-native/.
The tested example predates its optional request-counter output; results correctly
record requests=null, not a measured SQLite/raw-read count. Controlled-clock and
DB-gate unit tests, rather than elapsed wall time, establish pending-read behavior.
No paid provider or OS clipboard was involved. A next rebuilt fixture run and
review-required handoff remain pending. No all-OS/native or latency guarantee is
claimed from these local observations.

The later serialized run on the rebased main passed focused3/0 and standard
Rust2059 passed/0 failed/35 ignored, Python66, layout/watchdog,fmt,Clippy and
bin/preview_gate build. Rebuilt native eight-condition results and the versioned
driver pass with exactly two admitted UI preview requests (one retired, one
reopened), fixed observation notices and three unrelated title changes per case.
This is a UI-request count, not a SQLite statement count; the valid-source test
with output_chunk absent separately proves metadata enrichment does not read raw.
Latest logs: /tmp/issue-99-focused-latest.log,/tmp/issue-99-standard-latest.log,
/tmp/issue-99-build-latest.log,/tmp/issue-99-native-latest/ and
/tmp/issue-99-native-versioned/. Earlier null-counter and lint-failure records
remain historical. Final submission/review and all-OS CI are not established by
these local results. The issue comment contains the detailed reproducible report.

Final pure-rebase qualification on main d29e0fc: the serialized standard check
completed exit0 with Rust2061 passed/0 failed/35 ignored and Python66. Layout/
watchdog policy,fmt,Clippy and all targets pass. Evidence:
/tmp/issue-99-standard-final-rebase.log, execution
7748ea6f-0efe-4ba0-b18e-19567feb8e2c. The rebase changes no #99 source; only main's
already-approved ui/threads.rs viewport correction differs from the previously
native-tested baseline. This supersedes the queued-final-recheck gap in the
issue comment, not review-required status or all-OS/native testing limits.
## Pairwise conflict forecast (#104)

Simplified --attr-source helper:4 synthetic cases pass on native Git2.50.1.
Latest-main standard passes2,056/0/35 and Python70; details in#104. Other OS Git/CI remain unqualified.

## G reconciliation on main66d2746

Merge b9c29f2 retains main's #5 termination/admission/Unix cfg and #99 preview
lifetime work. Merge conflicts were module declarations and the old caption
match: keep both operations/os_signals modules and typed captions. Core now
rejects OwnerShutdown through TUI; catalogue returns explicit None for that
no-form command, with an exhaustive match and no panic. Fresh clap inventory
checks64 executable paths:62 applicable bindings plus explicitly tracked serve/
shutdown lifecycle controls and their global options. The integration decision
is documented in the design/decision log, not a silently missing successful form.

Native macOS on b9c29f2: library1186/0/15, selected structured-history/details UI
integration1/0 and strict all-target Clippy pass. The catalogue/lifecycle tests,
main's TUI shutdown refusal/replay test, and blocked-writer saved-storage route
all executed successfully. Initial merge compilation found an old test comparing
a nonoptional catalogue ID; wrapping the same expected identity in Some corrected
the fixture, without changing its assertion. The storage UI assertion intentionally
checks /logical/bytes >0 and /logical/complete=true instead of the old scalar
presentation; canonical CLI StorageStatus fields are unchanged. Diff checks retain
main-identical os_signals, owner_shutdown, preview_reads, streams and tools files.
Full standard and three-OS/runtime qualification for this merged source remain
unperformed; earlier full standards qualify their own sources only.

## G7 bounded deletion observations and dense review metadata

Runtime source: this deletion-read unit and its common-field parent on885ee14.
Native macOS, pinned Rust1.99.1, synthetic temporary SQLite only. Targeted domain
suite7/0, blocked-writer management route1/0 and strict library Clippy passed.
Final `sh scripts/check.sh` v2:2160 passed/0 failed/35 ignored (library1194,
integration947, termination_signals12, tool_group_crash_recovery7, four zero-test
harnesses), Python70, layout95, watchdog/fmt and strict all-target Clippy pass.
Earlier G checkpoint harness labels were corrected against their saved Running
lines; totals/source results are unchanged, not new test evidence.

The readonly saved preview examines256 descendant candidates with a sentinel,
depth8 and1024 combined relationship witnesses. Existing physical indexes admit
candidates before deleted/noncompleted exclusions; larger hidden populations do
not turn negative evidence into a full scan or an invented zero. Small complete
results match canonical preview, including deleted bridge semantics, shared paths,
replacement sources and attachments. Scope/reference truncation keeps unproven
negative counts/permission unknown; a known live/stopping Run still denies deletion.
A1MiB scalar is rejected with InputLimit before UTF-8 decoding/owned copy, and
restoring the cell restores canonical parity. No read modifies selected paths,
receipts or target state. The dedicated exact confirmation/execution and Core
deletion implementation are unchanged; generic preview facts are not authorization.

Dense results reserve permission/snapshot/reference metadata and exact blocker
count pointers under the existing128-field/64KiB field cap; full retained detail
is unchanged. The regression uses50 Run rows and asserts both exact counts and
snapshot/permission/coverage/time facts. Storage category help now states the
canonical rule: collection filters, status shows all categories. GC saved views
reuse the existing terminal-state predicate. No new schema/index/worker/queue or
Core protocol operation. No new observed test failure on this unit. Reactive
notifications/shared admission, remaining index/context/history/status/readback
coverage and G9–G12 evidence remain incomplete.

## G7 saved index initial-read extraction (local, partial qualification)

Source: main66d2746 plus the G branch through7c04dbc and the current saved-index
extraction. Native macOS, Rust1.99.1, synthetic temporary SQLite fixtures;
no paid API or user data. `cargo test --locked --lib index::saved_view::`
passes13/0; `cargo test --locked --lib explicit_saved_gets_finish` passes1/0.
The latter includes IndexStatus alongside existing saved readers while the
writer is deliberately blocked and checks unchanged receipts. Strict library
Clippy passes. The existing edit-adapter frontend integration passes1/0 with the retained
management spaces/coverage/definitions assertions. Canonical index tests pass42/0/1; the complete standard check was interrupted by SIGTERM during all-target Clippy;
these targeted results do not inherit the preceding2160-test standard result.

The initial canonical parity regression caught a refactoring error: a shadowed
witness compared itself instead of the current source, incorrectly accepting
changed revisions. Corrected to compare against `current_witness`; the retained
regression now verifies both unchanged coverage and two stale segments after
revision change. Initial failed logs are retained privately, not counted as
passes. Budget/cursor/selected-space/scalar/disabled-auto-history tests also pass.
No live-observer/P3/P6/three-OS or native layout completion is established here.

Source2ae8c40: the standard attempt completed layout95, watchdog policy,
Python70/0 and formatting, then all-target Clippy was terminated by signal15
(exit143). The cause is not established; no Rust diagnostic or passing
all-target result was produced. This attempt is interrupted, not a successful
standard check. Targeted13/0, blocked-writer1/0, frontend1/0, canonical index42/0/1
and strict library Clippy remain their separately qualified results. Commit
secret scan7c04dbc..2ae8c40 passes. Resume with the standard check before claiming
whole-source qualification; no G publication/admission follows from this record.

## G index resumed standard attempt and cleanup

On source2ae8c40/1bb0441, the resumed native standard attempt passed layout95,
watchdog policy, Python70, formatting and strict all-target Clippy, then library
passed. Integration ended946 passed/1 failed/17 ignored: the real PTY test
`real_tui_accepts_text_renders_proxy_response_and_exits_cleanly` could not see
`一時停止: false` after refreshing index status. The assertion is retained and
the display is under investigation; no full-standard success is claimed.
The earlier SIGTERM was explained by the user's emergency disk cleanup.

User-authorized cleanup removed retired measurement targets and completed
measurement executables (40.78GiB logical). Source/instrumentation, private
logs/results and Git worktrees remain. No open file in the retired G target
was observed before deleting it. The one retained build location is the clone
root target; all future local measurements use that CARGO_TARGET_DIR.

The G index repaint failure is corrected in common field ordering. Native
`cargo test --locked --lib saved_review_field_tests` passes2/0 and the unchanged
`cargo test --locked --test integration real_tui_accepts_text_renders_proxy_response_and_exits_cleanly`
passes1/0 (5.06s recorded, not an acceptance threshold). Exact paused JSON
pointer appears immediately after the original-detail action; coverage, spaces,
definitions and full retained detail are still asserted. All builds use the
root shared target. No full-standard/three-OS success is inferred.
## Footer phase/time/recovery qualification (#85)

Starting main9b4ae34, then rebased onto2b3cd91 for the shared #94 capture helper.
No Core state, schema, public protocol, new worker/queue/scheduler or provider
retry policy changes. The user authorized starting before F/G; current published
F overlap was checked, and later integration rebases. Worker3 received the exact
chrome activity-facts/draw_status recovery wiring ranges before more shared edits;
key-hint selection/layout/truncation remains #87-owned in a separate worktree.

Pure monotonic-time tests pass for saved attempt age/new attempt vs continuing
Turn, same-anchor stability and no equality-driven repaint, tool/request overlap,
both receive/snapshot orders including an older phase in between, stale attempts,
invalid saved times and lost observation. The existing header-liveness assertion
was intentionally updated to Receiving as a transport fact, not generated text.
Snapshot timestamp strings are bounded; wall observation time is excluded from
semantic snapshot equality. Age anchors are parsed on acceptance, not every draw.
The existing Core saved-fact classifier and terminal precedence remain intact.

A temporary real-SQL projection test preserves original recovery_reason_json but
suppresses its active footer after running/completed/stopped saved resolution;
unresolved failed recovery and typed #76 reset facts remain. The old explicit-stop
test previously expected a historical reason as active recovery: it now expects
no active recovery and separately checks the original saved stop_reason. All
1,011 nonignored library tests passed at that checkpoint. Wrong UsageLimit paths,
a fully explicit snapshot constructor and an on_receipt fixture signature were
compile-time test/setup errors, corrected without changing production policy.

The first real PTY run reached successful external CLI retry but still displayed
provider:fixture failure in the footer. This exposed generic EngineEvent recovery
errors being promoted to independent Info notices. The corrected frontend defers
only typed saved-execution recovery/config failures to the existing reactive read;
storage/unclassified errors keep their existing notification path. No string
matching or TTL guesses failure resolution. The same native fixture then passed.

Native macOS ARM64/Rust1.99.1, actual PTY with temporary HOME/config/data-dir,
copy_backend=osc52 and unauthenticated loopback only. Shared scripts/dev/Terminal
and VT frame decoding are reused; no user clipboard/config/provider/data read.
The handler gates first headers/body completion, then observes a synthetic saved
running-group marker while that request is inflight (no early tool execution or
real OS shell is claimed). It removes the marker before completing, then exercises
HTTP400 failure, real separate-CLI run retry and success, then typed quota recovery.
All8 width/palette/fallback conditions pass with4 requests and8 captured stages
per condition:64 frames total. Captures include separate tool/request ages, no
active recovery or stale failure footer after successful retry, quota kind/reset
UTC and explicit wait/retry or /model guidance. A passed reset is not automatic
resume; the unchanged connection-scoped remembered-limit rule still applies.
No new short timer, latency pass/fail bound or settling wait is used: only the
shared capture/CLI60-second watchdog and explicit state/transport gates.

Project knowledge was searched first; it returned the helper procedure, not
relevant new activity facts. Guide semantic scope was degraded/unready; no index
rebuild or knowledge mutation was attempted. P1 and final standard/branch handoff
are separate gates, not inferred from these screenshots or library results.

The existing manual P1 test was reused unchanged:140x40 TestBackend,128 nodes,
50 tool rows,20 warmups/500 timed interactions, same debug profile. Copied native
lib-test executables from baseline2b3cd91 and candidate66ac885 were run in three
interleaved pairs with no compilation during the timed portion. p95 microseconds:
8350->7327,5713->6935,8919->6300. Candidate observations stay below8ms and relative
+2ms; no speedup is inferred from the noisy baseline (two baseline values exceed
8ms under shared-machine load). Both preserve131 layout builds,258 unit builds,
16510 unit hits and all focus-only reuse assertions. This is a current-main
incremental comparison, not a fresh historical1fec7f0 measurement. Artifact/source
witnesses and logs are retained in the temporary P1 directory. Source changes
made during baseline build are restored before any candidate check.

A later source review replaces aggregate MIN on active groups with one existing
first-match LIMIT1 read (bounded timestamp) so capturing a tool age does not force
an aggregate full traversal beyond the old EXISTS behavior. A lost-observation
regression also prevents a new snapshot from fabricating first-byte age: age stays
unknown until a new receive observation, without counting the unknown gap.
Older mismatching receive observations cannot displace a newer early candidate.

The first full integration run exposed real compatibility regressions, not flakes:
a sampled133-second UI observation was not committed to the new phase anchor;
and removing the entire RecoverySnapshot also removed the intentional historical
recovery panel after stop. Phase anchors now commit the existing sampled UI time
and never go backwards. RecoverySnapshot.resolved distinguishes historical panel
facts from current footer ownership, preserving that panel, original reasons and
stop details while clearing only active footer/action facts. The unchanged
frontend_activity suite8, external-CLI reactivity case1 and tui_recovery suite2
all pass after these corrections. No wait length or assertion of saved history
was weakened. The native fixture's old 'waiting' expectation after HTTP headers
was intentionally changed to transport Receiving under the adopted #85 contract.
The fixture likewise checks absence of active footer recovery, not absence of the
historical recovery panel. The initial four integration failures are retained in
logs; they are not covered by the separate #96 unrelated-CI classification.

The later exact-main #91B comparison is NOT a closed P1 gate: baselineafe4c08 and
candidatef432751, same debug fixture, three interleaved pairs yielded p95us
8579->9006,8690->8370,8335->10961. Both sides exceed8ms under shared-host load,
and the third candidate delta exceeds2ms. Cache counts/reuse assertions remain
identical. All observations are retained, not replaced by the earlier passing
checkpoint or called a speedup/causal regression diagnosis. Host load around the
build was10.65/10.27/9.85; one lib-test compilation took11m23s with concurrent
workers and1 build job. A controlled low-contention paired measurement is needed
before claiming this P1 admission goal, with no timer/assertion relaxation.

Resource coordination followed the user notice: the already-started final
receive-order lib build continues at1 job; no subsequent Rust build/check starts
until worker1 releases the #96 standard-check slot (its current build uses2 jobs).
The saved native P1 executables need no rebuild to inspect host-contention effects;
a short quiet observation interval was requested directly. Existing/user processes
are not interrupted. The previous over-budget P1 numbers remain visible and no
closed gate is claimed merely from a future rerun.

After explicit slot release from workers1/3 (both holding all new Rust work),
the already-built baselineafe4c08 and final receiver-fix candidate executables
were observed without rebuilding,11:27:45-11:27:59UTC. Three interleaved P1 pairs
p95us:4490->4447,4398->4465,4418->4435. All candidate values satisfy8ms and
+2ms relative goals in this controlled interval; cache counts131/258/16510 and
focus-reuse assertions remain identical. The earlier over-budget loaded-host
values remain above, not discarded or called a causal speedup diagnosis.
Binary hashes, source/diff witnesses and complete logs are in the quiet P1
capture directory. The two peers were notified once of results and slot release;
no new local Rust work starts until the next #96/#95 resource slot is released.

A final race test additionally passes Preparing(a)->receive(a)->Preparing(a)->
Awaiting(a), and the analogous running transition: bytes alone do not invent
inflight, but the matching later saved inflight projection retains reception.
All15 activity/time cases pass on that source. The final latest-main standard
check and rebuilt native matrix are still separate gates after the resource hold.

Shared helpers were reused for record deltas and PTY/VT lifecycle. Another
extraction candidate was measured from saved shell commands:3 actual lib-test
artifact-selection/paired-build scripts,4275 characters (the fourth matching
entry is the measurement query itself and is excluded). A source-witnessed
paired-artifact helper could remove repeated boilerplate; this task does not
expand #85 into that new helper implementation.

After the user-reported normal owner restart, the clean worktree/branch and
completed observations were verified rather than replaying completed operations.
The final serialized sequence on source6303d91/mainafe4c08 completed exit0:
CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=2 sh scripts/check.sh gives1,972 passed /
0 failed /33 ignored, layout94, Python41, fmt, strict Clippy and watchdog policy.
The debug product was then rebuilt and the actual temporary-owner PTY matrix
repeated successfully:8 conditions/64 screens,4 loopback requests each, including
external retry, historical resolved panel and quota guidance. No stopped build is
claimed as success. The heavy slot was released directly to worker1 afterwards.
Subsequent changes in this qualification commit are record-only; Rust and fixture
source blobs remain identical to the fully checked source. CI is not dispatched
by this worker, and the branch is handed to worker4 as review required.

The user subsequently requested one shared elapsed presentation for footer,
tool, owned-work and notice durations, retaining subminute tool tenths, and the
saved ended_at time/date beside terminal phases. These follow-up sources are
not covered by the earlier green gate until their new validation completes.
The broader performance-policy update makes p95/CPU observations only; prior
loaded and quiet values remain recorded, without repeating a quiet measurement.
Structural job/read/draw counters and virtual-time redraw bounds remain gates.
Formatter follow-up gates initially found two obsolete expectations (60秒待機
versus1分0秒 and133s versus2分13秒) and a guide scanner interpreting a dynamic
completed-time example as a literal navigation label. Expected strings/example
notation were corrected without weakening duration/source/clock assertions.
Before integration of main #98, the formatter/end-time follow-up passed targeted
elapsed/calendar/activity/footer tests, sh scripts/check.sh (1,977/0/33,
Python66/layout94/fmt/strict Clippy/policy), and the rebuilt native8/64 matrix,
now requiring a saved terminal HH:MM alongside completed answer frames. Main
then advanced to9fd7b1c. Its three presentation conflict hunks were resolved by
retaining #98 tool geometry, body priority and cached release facts, applying only
the shared formatter and recognizing an existing hour/minute suffix during paint.
This latest integrated source requires a fresh final gate; earlier green is not
claimed as that gate. No force push or own native CI is used for the replacement.
Final source on maind29e0fc passes sh scripts/check.sh:2,064/0/35, Python66,
layout94/fmt/strict Clippy/policy. Rebuilt actual PTY matrix8/64 also passes,
including saved completed-time labels. Detailed source/result evidence is in#85.
Subsequent record-only commit leaves Rust/fixture blobs identical to checked code.
#85 final owned-chain source8796e5b on2954dda passes standard2,078/0/35,
Python70/layout94/fmt/strict Clippy/policy plus rebuilt native8/64. JOBS2 and
TEST_THREADS2 used under reviewer16:24 override; no elapsed-time regression or
external-process intervention inferred. Current main66d2746 also contains the
later stage3 stack, which is not covered by this checked-base gate. New source
review/Windows CI remain distinct; no rerun of failed41214341584 was requested.

### #91 Conversation Band And Header Extent Follow-Up

2026-10-31, macOS arm64 / Rust 1.99.1, single-job Cargo and two test threads.
Candidate code 3670eacec9365d3cb977835ed39583bb5da7ffed is rebased on main
19b9f87ef76e6786cbad3128e15e27d8cc6a1224. `sh scripts/check.sh` passes:
Rust 2058 passed / 0 failed / 35 ignored, Python 66; layout/watchdog, fmt,
Clippy and all targets pass. The separate chrome preflight passes all nine
cases. `cargo build --locked --bin shellboy --example visual_fixture` passes.
Evidence: `/tmp/issue-91-width-qualified-v5.log`, execution
7e570b9a-a56f-421f-a98c-abb44f2f3181. No installed binary was changed.

Fresh real-process PTY/VT captures pass 27 cases: 80/81/140 columns plus
79/83/139/141, each dark/light/NO_COLOR; 80/140 additionally use TERM=vt100.
Every capture checks exact full-width header color with one-column text margins,
input colors through x=1..width-2, normal outer cells and canonical OSC52 payloads
for slash/focused/original-page copy. The first 21 cases include synthetic local
SSE input followed by PageUp and return-to-tail captures; no provider is called.
The OS clipboard is never touched. Captures are in `/tmp/issue-91-width-after-v5`,
`/tmp/issue-91-width-odd-after-v5` and `/tmp/issue-91-width-vt100-after-v5`;
executions ed1d7078-2aee-45d8-86ca-401018681413 and
8efcf7be-d6eb-4979-8e45-58c51ce24678 both exit0. Binary SHA-256:
a16294d8b1555fcbe15b00e0e34430fe1bcbe71e9a94a220618984dce0eda949.

The earlier baseline did not reproduce the user's one-cell-short continuation;
this change enforces the requested geometry, not a proven cause for that physical
terminal symptom. Before/after synthetic cell evidence is retained separately.
Validation exposed TestBackend's unpainted wide-glyph continuation slots; the
full-color invariant is asserted on the actual Frame buffer and real VT cells,
not those backend placeholders. Earlier failed checks are retained in
`/tmp/issue-91-width-qualified.log`, `-v2.log`, `-targeted-v3.log` and `-v4.log`:
the last full run found two old header-position/minimum-banner assumptions.
The updated contract asserts both blank margins, phase at column one and a
compact freeze label at eight columns; it does not drop the visibility check.

Untested: physical GUI terminal rendering, real OS clipboard completion, native
Windows/Linux PTYs and a fresh opt-in release P1 latency/memory profile. The
standard functional suite is not claimed as that performance measurement. No
new worker, queue, timer, DB read or source-format field is introduced.

#91 additional explicit trailing-cell qualification: test-only script changes
assert every live/PageUp/return-to-tail conversation row, not just save colors.
All 21 live width/palette cases pass again, producing 63 per-scene edge audits:
colored continuations fill the entire inset, and non-band rows keep both last
content cells at the normal background. Fresh Python66 also passes. Evidence:
`/tmp/issue-91-width-edge-audited/`, `/tmp/issue-91-width-edge-audited.log`, execution
669162aa-3da8-4187-a790-6b0346d02b37 exit0. Product src and binary are unchanged
from the standard-tested 3670eac; this adds no new Rust qualification claim.

## G saved index plus main5321310: exact-source standard qualification

Source0a5569b2ac4d0a7d9e582fbedfb0ed5f0fbc061a, native macOS/Rust1.99.1,
`sh scripts/check.sh` on the shared root CARGO_TARGET_DIR succeeds:2194 passed,
0 failed,35 ignored. Harnesses: library1223/0/15; integration952/0/17;
termination_signals12/0/1; tool_group_crash_recovery7/0/2; main and three examples
have zero tests. Python70, layout95, watchdog policy, formatting and strict
all-target Clippy pass. Commit secret scan7c04dbc..0a5569b passes. Private
named-harness audit/log retained; these are this source's results, not the old
2160-test run or the intervening interrupted/failed attempts. The original PTY
index refresh assertion succeeds. No paid API or user data.

After qualification, the authorized cleanup ran Cargo clean only after no open
file/process in root target was observed: Cargo reports24978 files/39.7GiB
removed. Its missing target makes a subsequent du return ENOENT; this does not
mean cargo clean failed. Earlier retired .local cleanup inventory is preserved.
Workspace2.7GiB/.local2.6GiB and disk free162GiB were then observed separately.
No new semantic code followed qualification. Next build must use the one root
location and be announced in #41 before long Rust/clippy work. No three-OS,
G5/G6/P3/P6/final native-layout or complete-G admission is established here.

## G private context-usage reader extraction (partial qualification)

Native macOS/Rust1.99.1, base2322b57 plus the shared-reader extraction:
`cargo test --locked --lib compaction::context_usage::` passes5/0; the existing
`compaction::edit::tests::legacy_large_usage_feeds_context_and_baseline_without_rewriting_rows`
passes1/0; `cargo clippy --locked --lib -- -D warnings` passes. An initial
`context_metrics` test-name filter matched0 tests and is explicitly not a
passing regression claim. Build start was announced in #41. A single root
CARGO_TARGET_DIR, one build job and incremental disabled were used after clean.

Assertions cover manual expected canonical latest-by-rowid/current-profile/model
selection, unknown newest usage without fallback, raw replacement timestamp
comparison, exactly two reads in a caller-owned transaction and preparation
after DB shutdown. Huge/malformed/negative/out-of-range usage JSON normalizes
as before without rewriting saved bytes. A1.2MiB timestamp and1MiB model/profile
cell reject bounded display reads while canonical reads retain their existing
contract. Invalid limits/oversized display identity fail before a table exists.
Captured scope stays distinct from the public result. No real API/user data.

This qualification does not prove bounded total latest-attempt/replacement SQL
work or connect the live inspector: G9 query plans/cost and G5/G6 shared/adopted
reads remain mandatory. No full-standard/P3/P6/three-OS result is established for
this extraction. The earlier2194/0/35 run remains scoped to0a5569b.
## Context-edit wait observation (#102)

Reconstruction/prototype only; exact operation timing/source references and log
coverage limitation are in#102. Existing safe-boundary policy is retained, with
read-only waiting_reason projection and queue-start invalidation. Synthetic SSE
with controlled foreground/background stdin, plus virtual-clock readback cases,
are not yet run. No Rust build/standard/CI or integration claim. #99 preview/read
ownership is preserved. No actual provider or user DB mutation was performed.
#102 reader2/footer1/gated-SSE1 pass. Standard2,076/0/35, Python70/layout94/
fmt/strict Clippy/policy and bin build pass on checked source3817003/base66d2746.
Fixture compile corrections add the optional snapshot field and use current
Control::new; no production scheduling/order/timeout change. Full evidence in#102.

### #102 post-review qualification scope

The owned macOS chain `41eadd51-1a25-441b-881d-210b2ceabb24` qualified source
`6b8c0c818628bd8724eee5c0b8c06d6b2ef03bec` on base
`1ee0d8405323c4ce50c542e4dc8090422faaf2b7`: targeted footer test,
`sh scripts/check.sh`, and `cargo build --locked --bin shellboy` all passed
with `CARGO_BUILD_JOBS=2` and `RUST_TEST_THREADS=2`. It exited 0 and released
at 2026-10-31T17:06:27Z, with process-group cleanup confirmed. This supersedes
neither preserved historical source records nor native cross-OS qualification.

The subsequent review correction omits damaged auxiliary wait witnesses and adds
`damaged_wait_witness_does_not_block_run_get_or_projection` (eight cases).
Only formatting and `git diff --check` have run for that correction; the new
regression and standard check await the next allocated gate. The preceding
full/bin result does not qualify this changed source. No additional Rust/bin,
PTY, quiet chain, installed binary change, provider call or user DB mutation ran.

Ancestry clarification: `bc9582d` was the observed fetched `origin/main`, not the
checked source's ancestor. The checked source descends from `1ee0d84`. A final
rebase attempt encountered conflicts in `docs/design/control.md` and
`tests/runtime_features.rs` while replaying the initial feature commit; it was
aborted without retaining partial integration. Latest-main integration and its
native CI remain separate, with the integrator. No whole-stack equivalence claim.
## #87 stage 1: contextual hints and readonly navigation

macOS arm64, Rust 1.99.1, synthetic temporary databases only. UI-focused tests
passed 352/0 (3 ignored) before the shared readonly-key helper gained an explicit
Ctrl/Alt preservation test. Subsequent full library run passed 1013/0 (13 ignored).
Width 0..179, configured send/newline invalidation and unchanged-cache reuse,
notice precedence in history focus, real original-page continuation availability,
and F1 popup return are covered. Initial standard runs exposed old label/help
expectations and an F1 Thread-information regression; those are corrected, and
ui::dense_thread_rows_and_selected_preview_stay_compact passes independently.
The last full run did NOT pass: frontend::persisted_output_identity_is_delivered_
before_each_history_window hit its existing 60-second watchdog after 927 other
integration passes (17 ignored). No watchdog/test-state weakening is applied;
a final latest-main check remains required. Evidence: /tmp/issue-87-check-final.log,
/tmp/issue-87-check-verified.log, /tmp/issue-87-thread-f1-test.log.

Native PTYs using the existing standard-library helper passed 16 combinations:
80/140 columns; dark/light/NO_COLOR/vt100; default Enter and custom Ctrl+Enter
send bindings. All cover empty composer/image guidance, typing/newline keys,
completion, F2 long-answer escape guidance and return. Eight default-binding
cases also cover canonical saved Markdown, history focus, details, uppercase O/C,
real OSC52 bytes (not a host clipboard claim), F1 over original and Esc return.
Separate loopback SSE holds an actual Turn open and confirms Esc interruption /
Ctrl+C stopping hints at both widths. Evidence: /tmp/issue-87-hints-pty-v5/
(manifest includes the binary hash) and /tmp/issue-87-running-pty/. Earlier PTY
attempts used incorrect predicates (newline alias rendering, an offscreen heading,
an old popup title, and clipped notice suffix); their failed observations remain
in /tmp/issue-87-hints-pty*.log. No product fix is inferred from those harness
predicate corrections. The final source adds pending-original suppression and
preserves Thread-browser F1 information; a rebuilt final-source capture and
latest-main standard check are still required before final qualification.

No paid provider, real clipboard/image acquisition, user data/config, installed
binary, release, Core event/schema or protocol change is part of this stage.
Windows/Linux native PTYs and measured P1 interaction comparison remain untested.
The canonical code-copy chooser, numbered block targets, /copy preference/all,
finite complete-block retrieval, copy character counts and image-failure /attach
alternatives remain pending; these observations do not establish #87 completion.

Latest-main continuation: the task branch is rebased onto the exact #91 B main
parent afe4c087. The only rebase conflict was additive design text; the incoming
#93 contract and main's E/#91 qualification text are retained. The rebuilt native
PTY matrix passed all 16 cases again at /tmp/issue-87-hints-pty-rebased-v2/;
manifest and current binary SHA-256 agree at
8ec5c970fbcd8856f117e809d58137370c1ed7cf4d73f77a681995ac86839df0.
The captured selection row uses the corrected "history c" guidance rather than
forcing Tab (which would leave an already-focused history). The actual running
Turn scenes also pass at /tmp/issue-87-running-pty-rebased/. The first rebased
harness run appended a second [ui] table to main's now-OSC52-safe fixture and was
rejected by TOML before spawn; the corrected driver appends only bell/palette/
bindings to the fixture's existing table. Product safety was not relaxed.

A latest-main RUST_TEST_THREADS=2 sh scripts/check.sh is still running at this
recording point (/tmp/issue-87-check-rebased.log); Python41, watchdog/layout,
format and Clippy have passed, and the Rust tests are in progress. No full
standard-check success or all-OS result is claimed yet. Resource coordination
now defers any additional Rust build/check until worker1's #96 check releases
the host; already-running processes are not interrupted. This supersedes the
earlier final-source-capture gap only, not the pending standard check/P1 or
canonical code-copy implementation gaps.

Final stage-1 standard-check result: the existing latest-main check completed
exit0. RUST_TEST_THREADS=2 sh scripts/check.sh passed Python41, integration-layout/
watchdog policy, format and Clippy. Rust results: library1016 passed/13 ignored,
integration936 passed/17 ignored, live-thread-patch12 passed/1 ignored,
usage-aggregates7 passed/2 ignored; total1971 passed/0 failed/33 ignored.
Evidence: /tmp/issue-87-check-rebased.log, execution
513db94c-5cc0-4132-8538-df43a3d4652f. The selection-guidance regression is included.
This supersedes the pending-check status above; it does not establish the cause
of the earlier frontend watchdog failure, OS-wide success, P1 performance or
completion of the remaining canonical code-copy/image-failure work. No Rust
process remains from this task. Both worker1 and worker2 received one concrete
slot-released notice so #85's quiet observation can proceed; no new Rust run is
started during that observation.

### #87 Remaining Copy/Image Paths: Qualification Pending

Additional WIP code extends the already-qualified contextual-key stage with a
canonical chooser, numbered assistant captions, source-digest validation, Unicode
copy counts and image-failure `/attach` advice. Static formatting/layout and Python
syntax checks pass; compilation, new source/ownership tests, full standard check,
P1 measurements and the new `scripts/check-code-copy.py` real PTY matrix remain
pending. Earlier stage-1 PTY captures do not prove these additional behaviors.
Only synthetic fixtures and intercepted OSC52 are planned; no installed binary,
real clipboard, paid model request or user data is part of this qualification.

### #88 Saved Notification References: Partial Native Qualification

macOS/aarch64, exact8097b1d on main805efc9, shared target, two build jobs and
two test threads: `sh scripts/check.sh` passed formatting, clippy, layout94,
70 Python tests and2116 Rust tests (0 failed,35 ignored). All four new source,
rename/delete, generation/scope and off-window original unit cases passed.
`cargo build --locked --bin shellboy --example visual_fixture` passed; both
binaries were immediately copied to the task's `.local-bin/` before testing.
Execution d77c5af6-fbd2-4169-8fa6-8c789aa6fdc2 ended1 after383.004s with confirmed
process-group cleanup. Binary SHA256:
`a798074f53d4dc1e1b4a3f9f213d8111bddc62636f37a6464c5228e9bacff211`;
fixture SHA256:
`114ea0672883a09ee94d3e15514414c94a0301ca37658b52ea422f41a474052c`.

The synthetic PTY matrix passed80/dark/shell including exact original outside
the current history window.80/dark/child showed the current name, then timed
out at its60s watchdog awaiting a display-freeze label after Tab. Saved scenes
show selected-history original/copy hints and the paused-follow down arrow;
`history_interaction::select_history_item` sets follow=false, `chrome::footer_lines`
renders that arrow, and the freeze label belongs to the separate F2 mode.
The harness predicate is corrected to those actual selected-history/follow
signals, but has not been rerun. Native rename/delete and14 unstarted matrix
cases remain unqualified. Evidence: `/tmp/issue-88-second-qualification-pty/`
and saved shell output parts `/tmp/issue-88-second-qualification-shell.json`,
`/tmp/issue-88-second-qualification-tail.json`,
`/tmp/issue-88-second-qualification-tail2.json`,
`/tmp/issue-88-second-qualification-tail3.json`.

The first chain e97a1c3d-e65a-4fac-9f97-8c565fb5ce57 stopped101 at clippy's E0425
(new test in the wrong module); no Rust tests/build/PTY ran in that chain.
That failure remains in `/tmp/issue-88-first-qualification-shell.json`.
Python SQLite3.51 synthetic SQL/query-plan observations are non-Rust evidence,
not native acceptance. No paid API, user data, installed binary or host clipboard
was used. P1 is deferred, record-only and non-gating. No matrix rerun, cross-OS
qualification or overall acceptance is claimed by this record.

### #88 Corrected Full Matrix And Latest-HEAD Standard

The third chain supersedes the partial-matrix limitation above, not its failure
history. On macOS/aarch64 at8f9e457af8dbafa6be92f1f5278393a877b7d2e8,
`git fetch origin` confirmed main805efc9 and rebase was a no-op. Rust/Cargo inputs
were identical to8097b1d; copied binary/fixture SHA256 values matched the recorded
`a798074f...` / `114ea067...` hashes before use. No rebuild was needed.

`python3 scripts/check-notice-references.py --binary "$PWD/.local-bin/shellboy"
--fixture "$PWD/.local-bin/visual_fixture" --out /tmp/issue-88-third-qualification-pty`
passed all16 cases:80/140 columns x dark/light/NO_COLOR/vt100 x shell/child.
Each shell scene displayed bounded command/background facts without raw output,
then opened the exact saved original outside the current history window.
Each child scene distinguished two identical short names by saved identity,
showed the new current name without input while history-follow was paused,
opened only the correct child, and reacted to deletion without input. No OSC52
clipboard command was emitted. Raw captures and screen snapshots are retained
with the manifest in `/tmp/issue-88-third-qualification-pty/`.

The same bounded chain then ran `sh scripts/check.sh` at latestHEAD: layout94,
70 Python tests, fmt, clippy and2116 Rust tests passed (0 failures,35 ignored).
Execution7716219a-0ed5-4133-8cfc-e357a38b7aae exited0 after360.771s with confirmed
process-group cleanup. Saved output is in
`/tmp/issue-88-third-qualification-0.json` through
`/tmp/issue-88-third-qualification-3.json`. Two build jobs/test threads and the
shared target were used. No additional Rust/PTY/P1 chain started on release.

These are synthetic native macOS results, not Windows/Linux or physical-terminal
qualification. P1 remains deferred, record-only and non-gating. Python SQLite
observations remain separately classified. Both prior failed attempts remain
recorded. Push approval does not imply acceptance; reviewer review is required.

### #105 Frozen Cold Pair: Development Debug Information

macOS/aarch64, rustc/cargo1.99.1, two build jobs/test threads, no profile/Rust flag
overrides: frozen main5ecbebc03b43d9b1b760d64d93eca14141a2cfa6 versus profile-only
7a4bfc8880203b2c5ff08cd8db8cfa332cd7eb4b. Each ran `sh scripts/check.sh` exactly
once in its own initially empty temporary `CARGO_TARGET_DIR`: fmt/clippy/layout94,
71 Python tests and2125 Rust tests passed (0 failures,35 ignored) on each side.
Verbose shellboy dev/test commands show debuginfo=2 versus line-tables-only.

Allocated target snapshots:5773640 KiB/5.5G versus4680264 KiB/4.5G, an18.94%
reduction. Executable bytes before/after: main111329272/107014568;
integration171932160/165743952; termination_signals9987192/9779800;
tool_group_crash_recovery55526808/53677032. Elapsed440.9526/427.8692 seconds are
single-run observations, not acceptance thresholds or a controlled speed claim.

After candidate size capture, a temporary Cargo example deliberately panicked
(exit101); its actual stack retained `examples/issue105_backtrace_probe.rs:2:23`
and `examples/issue105_backtrace_probe.rs:3:13`. Probe and both temporary targets
were removed in finally blocks. Pre-leg free bytes139730665472/139564847104
exceeded the40 GiB guard. No shared/worktree-local target was modified by the pair.
Evidence: `/tmp/sb105-verify-evidence.pz0fk0dv/` (results.json, baseline-check.log,
candidate-check.log, candidate-backtrace.log); execution
f424a5d9-4033-486a-bdf2-b97dd598c8d2 exited0 after874.413s with group cleanup
confirmed. The older805efc9/3a933824 pair is historical context, not this result.

Main advanced to #88 during measurement; the pair stayed frozen. Subsequent
rebase/latest-main standard checks and Windows/Linux CI are separate and not yet
qualified by this record. No extra comparison, P1 run or cold retry was performed.
### #72 External Shell Error Catalogue: Synthetic Python Qualification

macOS/aarch64, Python3.12.7/SQLite3.51.0:12 new catalogue tests passed; complete
`scripts/tests` discovery passed83 tests in7.192s. Fixtures initialize the current
conversation schema17 inside temporary synthetic data directories only. Assertions
cover deterministic path/number/time/hex grouping, exact group/call/result-node
references, same Thread/Turn/step/overlap restrictions, counts/paired fractions,
rare recovery examples, secret canaries (derived bytes and CLI JSON), bounded cut
input/encoding loss, source SHA256 unchanged/read-only writes rejected, explicit
rebuild equivalence, in-place completion/GC/deletion refresh, cap/schema/options
failure, atomic replacement cleanup and symlink/hardlink rejection. Output prefix
EXPLAIN uses indexed output_stream seeks. No raw stdout was copied.

`python3 scripts/measure-error-catalog.py` created10,000 synthetic executions,
5,000 failures, then changed one saved exit code before an incremental refresh.
Observed full1.323557s/refresh1.398745s; refresh identified2 changed facts and9998
unchanged facts, with5001 failures afterward. These are bounded-rescan/atomic
replacement observations, not append-only delta reads or performance gates.
Evidence: `/tmp/issue-72-scale-final.json`, `/tmp/issue-72-python-first.log`;
execution2472dac9-c44c-4314-a783-fe8bb2f71361 exit0/cleanup confirmed. The scale
subprocess has only the60s watchdog; its temporary fixture was removed.

Earlier development observations were1.222/1.290s at an earlier source. One
expanded unit case initially failed because its fixture used literal backslash-n
instead of a newline; the fixture was corrected and all12 cases reran successfully.
Redaction remains heuristic (arbitrary-secret detection is not established),
lexical recovery association is not causation, and data remains untrusted.
No user DB, model/API, provider credential or real command replay was used.
Full standard checks/Windows/Linux and reviewer acceptance remain pending.

### #72 Native Windows Failure And Explicit SQLite Lifetime Fix

Initial batch30 native CI41244820590 passed Linux/macOS but failed Windows job
124665975622 with20 Python errors: WinError32 during staging-file replacement,
unlink and temporary-directory cleanup. Evidence:
`/tmp/shellboy-batch30-ci-failed.log`. This is a task-related resource-lifetime
regression, not an unrelated timeout eligible for a blind rerun. Python's SQLite
connection context commits/rolls back but does not close its connection; the
writer still held the staging database open when publishing it.

The author fix closes statement cursors, finishes commit/rollback and explicitly
closes the writer connection before replace/unlink. Source/catalog query cursors,
configuration-failure connections, fixture setup/inserts/test queries and the
scale fixture are explicitly scoped too. Cleanup failures are chained to the
original publication exception rather than masking it; the previous snapshot
remains intact on write/replace faults. No retry, sleep, GC, ignored error or
weakened assertion is introduced.

Three added regression cases retain strong references to every tracked SQLite
connection/cursor, assert explicit closure before replacement/unlink, exercise
read/refresh/rebuild, inject write/replace faults and check exact original error
identity/atomic snapshot retention, and expose cleanup-error chaining. The15
catalogue cases pass on macOS after the fix. New native Windows qualification
and the exact-source standard check remain pending; neither prior macOS success
nor retained-handle simulation establishes a Windows pass.

After the lifetime changes, complete Python discovery passed86 cases in7.549s
(macOS/Python3.12.7/SQLite3.51.0). The same synthetic10k fixture passed full and
changed-fact refresh:1.365133s/1.462936s,2 changed/9998 unchanged facts, no timing
gate. Evidence: `/tmp/issue-72-windows-lifetime-python.log` and
`/tmp/issue-72-windows-lifetime-scale.json`; execution
fa8d6e3e-90f7-4dbc-8537-4f6179101955 exited0 with process-group cleanup confirmed.
These local assertions do not replace a new native Windows run.

## #97 required description follow-up: pending qualification

Source-only schema change and regression fixtures cover enabled/opt-out required
sets, absent/empty execution and command label fallback. The required-entry-only
serialized request delta is14 UTF-8 bytes; token/model compliance is not inferred.
Two synthetic real shell calls must persist exited/exit0 facts; no provider or
user database is used. Rust/standard results remain pending shared allocation.

## Grouped saved usage (#101): pending qualification

Source/static work on basee83047f only: 20 known inputs10..200 require sum2100,
mean105, max200, p50100, p95190 and cache ratio0.5; fixed fixtures separately
exercise nullable/invalid scalars, states/scopes, offsets/subseconds, insertion
watermarks,168 groups and10,000 candidate limits. Query-only/no-write and held
writer dispatch, schema17->18 row preservation/reopen and indexed EXPLAIN are
separate assertions. Runtime/standard results are not yet available. Durations
are observations only. No API/provider/user-data/installed-binary changes or
TUI changes are included; no new table/worker/queue is introduced.

#101 review update supersedes the pending index/migration plan: no schema change
or new index remains. The upgrade fixture is removed; query-plan/count gates now
require existing primary-rowid range access, bounded candidates and explicit
excluded/malformed/non-current time counts. No prior Rust test result exists.

#101 first allocated chain cb8aabc0 checked formatted source1be86f76/basee83047f:
breakdown10, CLI1 and writer-held read1 passed; recorded synthetic exact/capped
read durations2.937/67.836ms, not thresholds. Layout94/Python71/fmt passed;
strict Clippy stopped on Command-size-driven large_enum_variant in existing UI
enums, so the full Rust suite did not run. Static correction uses boxed slices
only for three new optional text arguments, preserving JSON strings/null/empty
and cursor/timezone behavior without UI/G edits or lint suppression. Regression
assertions cover <=200byte Command and serde roundtrips; corrected source remains
unqualified until a new explicit shared-slot check. No index/schema/migration.

#101 latest-source qualification: chain797554cd-26da-4509-82e9-ba43ef90fb16
exited0 at2026-10-31T22:28:46Z, group cleanup confirmed. Exact checked code
fda663e4953be7a0b2ff63e68c468909af535128 / base1ac2277eaa1ffe87da9b175bc93ff62cbeecc118.
Approved own src/lib.rs and tests/integration/main.rs mtime updates (no byte
changes/deletion) plus explicit manifest and verbose compile proved the actual
own source path. Breakdown12, CLI1 and held-writer1 passed positive count gates.
Full standard2144 passed/0 failed/35 ignored (lib1168,integration957,examples19),
Python71/layout94/fmt/strictClippy/policy passed. Fixed stats/caps/EXPLAIN and
excluded-time/nullable-wire/Command-size assertions all ran. Synthetic exact and
capped read observations2.798/62.872ms are not thresholds or production timings.
No index/schema/migration, separate binary/PTY, provider or user-data work ran.
The earlier Clippy failure and invalid zero-match chain remain in issue101 and
/tmp logs; neither qualifies corrected code. Native3OS/latest-main integration
is still separate. This record-only follow-up does not change the checked code.

## v0.5.0 post-#117 preparation finalization

The three preparation commits rebase without conflicts onto exact main
`1f519332d9e8a09bf5d50f4eb2416e94625a4826`, including #101 and #117.
Main's ordinary native3OS CI41265533220 is confirmed success at that exact SHA;
it is not evidence for the new version/guide/helper preparation commits.
Notes now include near-tail scroll reversal, retained navigation anchors and
loading feedback from #117. Package/lock remain0.5.0; schema17/RAG3/protocol2
are unchanged. Unmerged #107-#109/#115 work is not included or claimed complete.

Finalization does not run a new binary build, release workflow, tag, installer
or user-data operation. Prior private macOS14/16 -> 17 qualification remains
source-qualified above, not relabeled as final-candidate-byte evidence. The
existing candidate_ref release mode and both upgrade commands are ready for
reviewer-authorized final-candidate checks. Preparation-branch native3OS CI is
required before review handoff; build/release/tag/install permission is separate.

### Extended release cut: #120 stage1 / #119 / #121

The reviewer adds those three deliveries before final preparation qualification.
Notes are drafted against author branches' configuration guides: extras are
bounded/non-overriding and not semantically interpreted; credential/transport
header exclusions remain; context_role defaults developer and user is recommended
for Claude via LiteLLM. Mapping occurs at send time without rewriting saved
history. Only internal dialect refactoring is included from #120, not native
Messages or later opaque-block replay. Actual source/API verification and final
branch CI must follow integration/rebase; author-branch docs are not a main pass.
The #117-only preliminary tree passes Python95, layout95/watchdog, package/lock
0.5.0 parsing and diff checks (executiondbe00421 exit0/cleanup); these results do
not qualify the not-yet-integrated extended cut. No new build/tag/install ran.

## v0.5.0 complete-cut source finalization

All requested delivery is now integrated at exact main
`a3e00bed359172d331951831ebf7ea3b8d769d65`: #101/#117 plus #120stage1/#119/#121.
The four preparation commits rebase without conflicts onto it. Ordinary native3OS
CI41266782750 attempt1 is verified success at that exact main; preparation's
version/embedded-guide/helper changes still require their own exact-SHA branch CI.

The final notes are checked against integrated src/config.rs and configuration
and CLI/Agent guides. Extras remain opaque/non-overriding/credential-restricted;
context_role defaults developer and maps at send time, with user recommended
for Claude via LiteLLM. No native Messages/opaque provider-block support or real
provider/cache guarantee is claimed. Schema17/RAG3/protocol2 and package/lock0.5.0
remain unchanged by finalization. No local Rust compiler/build, installer, tag,
release workflow, paid provider or user-data operation is run. Prior synthetic
upgrade evidence retains its original source/binary hashes; final-candidate-byte
checks remain reviewer-controlled after preparation/main admission.

Complete-cut build-free checks pass at the final preparation tree: Python95
(including all9 release-upgrade SQL/WAL cases), layout95/watchdog, package/lock
TOML agreement at0.5.0 and `git diff --check`. Executione78698fd exits0 with cleanup
confirmed. No Rust slot is used while the shared cache is being cleared; standard
Rust/native3OS checks run only on the pushed preparation SHA in GitHub CI.

## v0.5.0 publication validation

On macOS arm64, the reviewer ran `check-release-upgrade.py` twice against the
candidate binary, whose bytes match the published macOS executable. Both PTY
runs exited 0, using synthetic data and loopback only: verified public v0.4.0
(old binary SHA-256 prefix `286f9d17`, checked against its SHA256SUMS), schema
14 -> 17; and a staged synthetic schema 16 -> 17 with `--start-schema 16`.
Both passed ledger entries 15/16/17, RAG 3, search, mock exec and clean stop.

Candidate installer checks passed on macOS sh, Ubuntu sh, and Windows
PowerShell 5.1 and 7 in run 41269475088. Installer checks against the published
files and upgrades on Linux and Windows were not performed. See
[publication](../development/publication.md#v050-publication-2026-11-01) for
the exact source, public checksums, release runs and executable comparisons.

## #86 owned-work section

Local, pre-rebase macOS arm64 observations on the in-progress tree: focused
live-work frontend tests (13) and UI tests (11) pass; `cargo check --tests`,
strict Clippy, and the existing hidden-output and virtual-clock tests pass. The
virtual clock test counts 600 redraws across 600 synthetic seconds and no
clock-driven read actions. A separate ten-row synthetic case counts 60 redraws
across 60 synthetic seconds with no clock-driven read actions. The existing
hidden-output observer test issues 40
notifications without scheduling output reads for the hidden scope. These do not
by themselves establish every #41 P2/P5 multi-consumer condition.

Synthetic loopback PTYs at 80 and 140 columns, dark/light, NO_COLOR, and vt100
show the owned background shell section; all eight existing row-vocabulary
scenarios pass. A separate synthetic loopback PTY and replay show a shell-ended
wait, its visible target row and estimated 30-second deadline. A separate
process-limited PTY shows a live synthetic child Run. Row-limit variants and
motion-off remain unverified. The
128-node/50-tool redraw-only fixture reports p50 2796 us, p95 2845 us and max
2981 us (500 samples); it does not populate ten owned-work rows or supply a
matched baseline, so it is not a maximum-row performance comparison. Timing is
observational, not an acceptance threshold.

Full branch CI on all three OSes, the maximum-row hidden-output count gate, an
exact-tree matched-baseline performance observation and review
remain outstanding. No real provider, installed executable, or user data is used.
## #113 compact --dry-run qualification

New own worktree issue-113-compact-preview starts at main02fda155; the old
preparation-only worktree is not reused. compact.preview is additive/read-only
and all ordinary compact mutation modes remain unchanged. Collection runs on one
query-only WAL snapshot via the existing finite presentation pool. Original-node
counts and stage source_nodes are separate when a virtual spill is summarized;
synthetic replacement IDs have the same36-character width as actual UUIDs.

Initial rust-slot check --tests succeeds. The first focused build was interrupted
by the owner restart and is not a test pass. Restarted lib run6c12f610 passes12/0;
extended lib runs9ff8a5e/e6fa5110 pass14/0, including actual deterministic adoption
sizes/targets, #112 short-marker versus real visible indivisible refusal, default
spill order, source protections, ancestry equivalence, bounded unknown coverage,
SQL trace/query plans and CLI variants. The first IPC compilation fails on an
untyped fixture channel; its explicit unit type/unused import are corrected.
The next IPC run reaches the watchdog because Control.start was first attempted
after the writer gate: owner startup legitimately uses its writer. The fixture
now finishes startup before capturing baseline counts/holding its WAL writer;
no watchdog duration or behavior assertion is relaxed.

Warm-owner real local IPC in executiona944a9cb passes1/0, across all6 modes with
the writer still held and an uncommitted title change. Node/replacement/source/
path/app_meta/operation_receipt/attempt counts remain unchanged, writer rollback
is observed and counted provider requests stay0. Unit summary comparison itself
uses one synthetic loopback response, not a paid provider. All stores are temporary
and the OS clipboard/config/user data are untouched.

Strict lib Clippy first rejects tuple type complexity and a single-range array;
use a domain type alias and from_ref without lint suppression. Tests Clippy then
rejects a pure helper placed after a test module; the helper is moved before it.
Those failures remain unqualified. Latest tests add actual saved-part reuse and
malformed-cache miss after an explicitly synthetic path rewind, plus rendered
virtual/actual spill size equality. Their final result and exact-source native3OS
CI must be recorded separately. Python95, layout96/watchdog and diff checks pass.

Final corrected local qualification passes focused14/0 and strict tests Clippy
through rust-slot (log issue113-qualified), with positive-count guard. Cached
parts/invalid cache, virtual UUID rendered size, forged hidden bounds, and zero
removed messages for preflight budget refusal are covered. Registered real IPC1/0
is qualified on the unchanged routing/fixture above. Full standard/native3OS CI
is deliberately left to the exact pushed branch; no full local suite is claimed.

## #113 initial native CI catalogue correction

Exact7747290f CI41272580406 fails the same executable-catalogue assertion on all
three OSes: clap exposes --dry-run but compact's shared CLI binding omits it.
All other library tests, integration suites and termination/crash suites pass;
this is a task-related registration omission, not a Windows intermittent failure.
No same-SHA rerun is used. The existing DryRun logical field and false-default
flag binding are added to compact's catalogue. Its help is generalized so it is
truthful for both RAG and compaction. Manual owner forms remain a strict subset:
no new unimplemented checkbox or mutation builder is exposed. The exact clap
coverage test remains unchanged and must pass rather than weakening assertions.

The unchanged full executable-catalogue filter passes9/0 and the dry-run CLI
contract case passes1/0 locally through rust-slot after the binding correction
(execution39e4198e exit0/cleanup). Rebase onto latest main1120c8bb is required;
that main delta is publication/status/acceptance documentation only. Original
CI7747290f remains failed; latest exact-SHA3OS CI must pass before handoff.

## #113 after #107/#109 main qualification

Author rebases the approved four-commit forecast change without conflicts onto
exactbfb501e8275a871b8765812040b752782b497cc9 after #107 and #109 land. Own
entry-file mtimes are refreshed inside rust-slot to avoid cross-worktree metadata
reuse. Sourceaddcc6af passes check --tests plus preview14/0, real IPC1/0 and full
catalogue9/0, total24 positive executions (execution71ebae35 exit0/cleanup).
No forecast policy/queue/runtime behavior changes during rebase. The earlier
native greenbe644276 remains old-base evidence; latest exact-SHA native3OS CI
must qualify this rebase before the coordinator integrates it. Old published
refs are retained rather than force-rewritten, with owner cleanup after landing.

## #90 one motion per region

Local macOS arm64 (Darwin 25.3), branch `issue-90/motion` on `63952ab2`:
`cargo check --tests`, strict Clippy (`--all-targets -D warnings`) and the focused
lib tests (`ui::motion ui::live_work ui::activity ui::redraw presentation::rows
layout_cache_tests`, 57 passed) pass. Virtual-clock tests count: foreground shell
with spinner and the same scene without a spinner give the same frame count
(101 per 10 s at the <=10fps cap; no history revision, layout rebuild or
deferred action); spinner alone with a still footer uses the same step; only
the owned-work heading blinking gives exactly 60 frames and 60 brightness
changes in 60 s, on the section's seconds boundaries, and the existing
600-second owned-work test still counts 600 redraws (blink coalesced with
elapsed seconds); motion off and `NO_COLOR` give 0 blink frames; completed idle
schedules nothing (`completed_idle_has_no_animation_and_no_unsolicited_frame`).
Spinner frames are width 1 (ordinary and CJK); painting changes only the mark
cell of one row; the heading blink changes only the `◉` cell's color.

Synthetic loopback PTY (temporary root, one bounded tool call, 2 model
requests, `ulimit -u 1000`): 80/140 columns, dark/light, `NO_COLOR`, `TERM=vt100`
and motion off. With motion on the foreground `sleep` row cycled the braille
frames and, after it finished, the streaming answer row did; about 20–22 frames
per 2 s; 0 frames in 3 s after completion. Motion off showed only `⏵`/`▸` and
4–5 frames per 2 s (seconds updates). Heading blink color was not inspected in
PTY (the decoder records text). The quiet background-only scene, Windows/Linux
terminals and real fonts are unverified in PTY; spinner shape awaits the user's
choice. No real provider, installed executable or user data is used.
## #108 first lightweight qualification failure

At source88edcf2c/basecdcb70da, allocated execution1bf25314 passes formatting
then `cargo check --locked --tests` fails on five unsupported rusqlite FromSql
uses: usize ordinals in retained/lifecycle readers and a test, plus optionalu64
raw length/oldest sequence. One unused test import is reported. Own-path compile
is visible in verbose output; execution exits1/cleanup confirmed, slot released.
No targeted tests ran and this chain is not a pass. Correction reads SQLitei64
and checked-converts ordinals/nonnegative metadata, preserves NULL and rejects
negative values. Existing raw test adds0/i64::MAX/NULL/negative assertions; test
count remains14+3+2. Corrected source is formatted but Rust-unverified until a
new explicit slot; no automatic retry or full local standard check is performed.

## #108 checked-integer qualification and IPC fixture correction

At sourced225f8ec/basecdcb70da, explicit executionefae60c2 passes fmt and
`cargo check --tests`, then retained14/0 and raw3/0 with positive count guards.
Own-path compile is confirmed; no Fresh substitute is used. Registered IPC2
both fail at their first real ThreadSet: fixture supplies bare UUID operation
IDs and receives OperationExpired (no active instance prefix). This is not an
observation pass. Execution exits1/cleanup confirmed and releases the slot;
there is no automatic retry. Test-only correction uses the actual owner instance
prefix for ThreadSet and the later explicit ShellRead. Production/leaf source and
the17 passing cases are unchanged; corrected IPC2 and latest exact-source CI
remain unrun. Full standard is still CI-only; final acceptance awaits107main.

## #108 latest lightweight positive results before #107 main

Explicit slotf37d7ca1 on sourcef78663e445068d39cf954788f282f351fbd7bc00 /
basecdcb70da passes fmt, `cargo check --tests` and registered IPC2/0. SHA/base/clean
and positive exact-count guards pass, own entry-file mtimes force own-path
recompilation, exit0/cleanup confirmed and shared Rust slot released. Production
source and the17 cases are unchanged from d225f8ec's retained14/0 plus raw3/0;
this is split-source evidence, not a claim that all19 ran in the last chain.
No full local standard, Clippy, PTY or native3OS result is inferred. Real IPC now
exercises the title-change notifications using the actual instance ID; synthetic
GC/output growth/terminal/tombstone are labeled, and original revision/bytes/cursor
stay retained through current metadata refresh. Explicit test-side cursor replay
succeeds without a raw-read/registration call in the observation reader.

Textual conflict forecast before #107 landing reports frontend.rs,
frontend/inspection_read.rs and ui/inspection.rs between the task branches.
The #108 author owns resolving the rebase while retaining both list and retained
source dispatch/targets. Final acceptance requires that #107-main rebase plus
refreshed checks; preliminary branch CI cannot replace it. #117-main release
preparation remains the reviewer's priority interrupt.

## #108 preliminary native CI lint correction

Ordinary CI41266443031 at source6fdb980e fails on all three OSes in strict Clippy:
collapsible_if in retained capture and original-request decoration. This is
related task code, not an intermittent Windows failure; no same-SHA retry or
successful standard/native result is claimed. The corrected let-chains preserve
short-circuit ordering and both target limits/serialization behavior, without
lint suppression. The reviewer authorizes build-free correction/push/CI; no local
compiler/build/test rerun is made. Preparation rebases onto main1f519332 with
#117 before pushing; latest native CI must qualify that exact resulting source.
The previous published branch is retained to preserve the first CI's history;
a new own task branch carries the rebase rather than force-rewriting it.
Final #107-main rebase/qualification remains required and release preparation
preempts this task when #120stage1/#119/#121 integration completes.

## #108 after #107-main author rebase

Rebased the task onto exact03bf8570b0dac5335af7eefc8b8cd16046e7d180 after #107
admission. Author resolves frontend.rs/inspection_read.rs/ui/inspection.rs while
retaining both Lists and RetainedSources module/target/capture/read/prepare paths,
list_command ownership, original request decoration and retention classification.
A later formatting-only conflict retains both enum branches. No other worktree
is edited and no counterpart change is discarded. Own source before final record
is7b1c40ad plus rustfmt-only ui/inspection.rs; runtime semantics are unchanged by
that final formatting diff.

rust-slot execution428190ae passes check --tests and retained14/raw3/retainedIPC2,
then existing #107 list adapter7 and registered listIPC1, total27 positive test
executions. The latter covers all4 list routes and8 real IPC Changed commits.
Exit0/cleanup is confirmed. The prior source5e20cf1e three-OS CI remains qualified
only on its older base; latest native3OS checks must qualify the new pushed SHA.
## #125 proactive threshold compaction

Environment: macOS (darwin) sandbox worktree issue-125-proactive from main
0d922b9e, temporary data dirs and a loopback mock Responses endpoint; no real
provider or user data. Commands via rust-slot: `cargo check --tests`,
`cargo test --test integration -- proactive` (4/0: config defaults/override/
rejection; adopted once with exactly one summary request and one
`compact list` event `reason=proactive_threshold`, `state=adopted`, trigger
100/150; no summary request or event below threshold, with global 0, or with a
connection 0 override; a rejected summary is recorded once and not retried at
the later boundary of the same Run, Turn completes, zero summary nodes).
Related `cargo test --lib -- readback compact context_usage driver` passes
180/0. Not run locally: the full standard check and Linux/Windows; those run in
GitHub CI on the pushed branch. Providers that omit usage are only covered by
the unknown-usage rule, not by a live provider.

## #133 compaction summary request usage

Environment: macOS (darwin) sandbox worktree issue-133-sumusage from main
48a85799, temporary data dirs and loopback mock Responses endpoints; no real
provider or user data. Commands via rust-slot: `cargo check --all-targets`,
`cargo clippy --all-targets -- -D warnings` (clean), `cargo test --lib --
compaction:: usage readback:: gc:: storage:: driver` and `cargo test --test
integration -- storage proactive compact usage` (61/0). Evidence: N=3 summary
requests (2 map, 1 reduce) with mock-reported usage give `usage get`
`compaction.requests=3`, `compactions=1`, input 350, output 37, cached 80 with
1 unknown, cache_creation 5 with 2 unknown, `coverage.summary=recorded`, and an
empty `model_attempts`; the same range repeated from cached parts sends 0
requests, adds no record and lists `summary_usage.requests=0` on its event,
while the first event lists 3/350/37 (`compact list` via the retained summary
node). The record range read uses the app_meta primary-key index. A typed
refusal keeps `summary_usage` (1 request, unknown values) in the error details.
The #125 proactive path over a real owner shows `metrics.summary_usage`
(1 request, input 120, output 6, cached 30, cache_creation 4) and `usage get`
`compaction` 1/120/6 with conversation input 210 unchanged. Schema 18 upgrade
matches a fresh schema and counts `summary.usage.*` bytes.
Not verified: real provider usage reports for summary requests; Linux/Windows
and the full standard check run in GitHub CI on the pushed branch. In one local
batch, unrelated `control::driver::shell_output::drain_tests::successful_incomplete_output_is_saved_and_cancels_unexecuted_calls`
failed once under concurrent sandbox load and passed on an isolated rerun.
`scripts/error_catalog.py` now requires schema 18; `python3 -m unittest discover
-s scripts/tests` passes (95).

## #127 prompt caching hint

Environment: macOS (darwin) sandbox worktree issue-127-cache from main eca1b81b,
synthetic requests and a loopback mock Messages endpoint; no real provider or
user data. Commands via rust-slot: `cargo check --tests`, `cargo clippy --tests
-- -D warnings` (clean), `cargo test --test integration anthropic_messages`
(20/0). New fixtures check: breakpoint paths and count (exactly tools, system,
conversation tail; at most 4; all `{"type":"ephemeral"}`; none on thinking
blocks or the trailing `Shellboy context v1` line; no top-level marker); the
cached block sequence of each request is a prefix of the next request's across
a tool round trip and a new Turn; with `context_role = user` the tail stays
before the context line; default-on and `prompt_caching = false` over loopback
(opt-out body contains no `cache_control`, `system` is a string); Responses and
Chat encode identically with and without the hint; config defaults, opt-out,
`cache_control` rejected in Messages `extra_body`, and `prompt_cache_key` without
`prompt_caching` rejected. `cache_creation_tokens` is saved from the Messages
fixture (100) and round-trips through the usage whitelist (`cargo test --lib
usage`, 59/0; `--lib -- dialect config compaction`, 116/0).
Not verified: any real Anthropic cache read or write (requires approval after
credit returns: second request `cache_read_input_tokens > 0`, record
`cache_creation_input_tokens`); full standard check and Linux/Windows run in CI.
Local `runtime_features::headless_stop_boundaries_*` failed in this sandbox with
the restarted owner reporting the data-directory lock unavailable; it does not
use the Messages dialect and is left to CI.
## #124 state-driven frontend fixture preparation

Historical Windows cd86d259/run41272742726 attempt1 fails at shutdown joins5/8s
and the proxy's independent10s phase gate. The issue records additional107/109
same-SHA intermittent failures. Keep those failures distinct from later reruns.
The changes and per-case conditions/budgets are documented in frontend-stability.
All original5/8/10s shutdown bounds, draft phases5s, saturated-outbox5/10/15/2s
and the shared60s diagnostic watchdog remain at their original values. Proxy
phase progress now follows consumer oneshots instead of an independent10s speed
requirement; missing consumer progress still has its unchanged state watchdog.
No runtime deadline, queue capacity or production observer is changed.

Local rust-slot check --tests passes. New receive/drain helper regressions and
the retained-batch cancel/quit/force case pass. The initial same-Thread rewrite
incorrectly demanded a completed model answer from a no-provider fixture and
failed its existing5s guard; corrected matching uses the actual submitted draft
and Thread durable-acceptance event plus the original saved-instruction SQL check,
not model completion. That correction and edit_adapter/persisted_output_identity/
structured_history each pass1/0 locally. No timing cap is enlarged or assertion
removed. A later Clippy attempt selected library metadata without Lists after
rebase, even though own source includes that variant; it is unqualified. Force
own entry-path recompilation rather than changing the source API to fit stale
shared artifacts. Final source will be checked again before Windows qualification.
Five consecutive actual Windows4-thread CI passes at one frozen SHA remain the
acceptance gate; build/OS/CI results are not inferred from macOS focused passes.

## #122 bounded deletion marker

Base48a85799; synthetic temporary SQLite/data directories only, no installed
binary changes, user-data deletion or provider/embedding requests. Marker wire
compatibility is tested with the exact old protocol2 Request shape, omitted and
null optional args, and malformed count/hash rejection; an old released executable
was not used. Project hybrid RAG had no relevant task history and reported its
docs semantic scope not ready; it was not rebuilt. Current issue122 and adopted
repository sources are authoritative for this change.

macOS own-entry rust-slot check --tests passes. Focused executioncc809752 gives
registered marker6/0, UI18/0, bounded/real frontend deletion unit22/0. The latter
includes existing indexed/physical-count SQL gates and new exact/bounded digest,
deleted-bridge, depth overflow and known SHA-256 encoding checks. Later execution
31c652a4 passes strict tests Clippy, catalogue9/0, canonical deletion plus UI23/0
(5 canonical and18 UI), and marker6/0 at the shared-derived admission ceiling.
Marker tests assert added descendants and same-count substitution refuse without
changing physical/deleted Thread counts, root revision or GC admission; a fresh
marker succeeds; root-only leaves new unselected children live; current live Run
blockers remain;256 descendants attest257 IDs, overflow yields explicit no-marker,
guarded execution fails closed, and legacy unmarked deletion still handles258.
UI tests verify exact marker transfer, no Y without a marker, stale-triggered fresh
preview/request ID, rejection of old responses and a newly required explicit Y.
Operation-form entry also uses the existing guarded confirmation, not an unmarked
mutation. No new real-time guard, timeout increase or weakened SQL/count assertion.
Integration registration100 and watchdog policy pass. Local full standard suite
was not run; standard checks and native3OS qualification run on the pushed SHA.
Initial compile failed on SHA0.11 hex formatting and the new error hint arm; both
were corrected without dependency changes. An exact-block patch refusal wrote
nothing and that repeated compile is not claimed as a successful qualification.
## #132 automatic compact-list reason

The real #125 mock response -> reported usage -> next boundary -> adopted summary
fixture reproduced metrics.reason=null before the fix (execution7296dff1,
1 executed/failed, exact expected proactive_threshold assertion). Copy the already
bounded reason used by top-level reason into metrics; no SQL/row/text/admission
change. Existing top-level output and failure fallback remain. Own-path rust-slot
check --tests and readback10/0 plus proactive integration3/0 pass (2e638f9a).
The latter exercises adoption, below/disabled thresholds and summary failure.
Readback regression includes both legacy automatic_trim and automatic_trim_attempt
without a replacement node and asserts failure in both reason fields. Existing
indexed/physical count gates pass. No user-data edits or provider requests; mock
loopback responses only. Standard/native3OS CI must qualify the pushed final SHA;
no local full standard suite is claimed. Project hybrid search found no task
history; unavailable docs semantics was not rebuilt.
## #123 external observation retention

The focused owner/frontend integration cases use a bounded loopback
model-catalog responder and an isolated SQLite owner; the unrelated Thread
change is submitted through the real CLI and owner. Separate index and deletion
confirmation cases check unrelated changes, retained evidence, disabled Y, and
no second preview response. No real provider or user data is used. Full standard
checks and native Linux/Windows coverage run in branch CI.

### Recovery transition time (#129)

A committed transition from preparing, running, or waiting to `recovery_wait`
stores one RFC3339 `details.last_failure_at` in the existing reason JSON. The
same enriched error is in the durable recovery event. Existing code, message,
hint, and object details are preserved; non-object details are retained under
`original_details`. A stale generation, settled Turn, repeated transition, or
rolled-back transaction cannot replace the prior reason or add an event. The
original caller-visible error remains unchanged. Older, malformed, and oversized
reasons still follow Watch's existing bounded unknown-time behavior.

The covered entry paths are: preparation with a new or existing Turn;
permanent attempt admission failure; capacity failure in the driver; request
client initialization failure; and a permanent provider failure through the
retry/overflow path. These converge on the same transaction writer. The tests
count the saved field and compare the durable event to the saved reason; the
provider failure is synthetic and does not contact a server.

## Post-v0.5.0 schema 18 synthetic upgrade

On macOS arm64, `scripts/check-release-upgrade.py` used the public v0.5.0
archive (SHA-256 `641a01415bc73e56dd3b4287207b2966bf054faeafdb1f071974360727f01e61`;
extracted executable `06a25d8ca88678e0f3cdf0f1a0b0cd51b795c9e6c44982425955046794bf215c`)
and a debug candidate built from `5cb06aaa` (binary SHA-256
`dcc4dad4690022da0df918642c8973306d830c23a8443a276c64241d20fe4bd5`).
Both report version 0.5.0. The temporary synthetic public-schema 17 fixture
upgraded directly to 18 without staged migrations. The unchanged ledger prefix
plus migration 18, both indexes, integrity/foreign keys, saved originals and
lineage, held delivery, usage, conversation and RAG search, mock exec, and clean
shutdown passed; RAG remained schema 3. Five model and twelve embedding calls
used loopback mocks. The `scripts/tests` cases also cover historical schema
14/16 paths, an existing synthetic summary-usage row, incorrect indexes and
invalid schema arguments. This is not a real-data, paid-provider, native
Linux/Windows binary-upgrade, installer, or future-release qualification.

## #118 Navigation structural boundaries (partial)

Own-worktree macOS rust-slot validation, synthetic TestBackend fixtures only:
baseline characterization19/0 (9f3496b3); ownership check --tests +19/0 +view41/0
(517d24d2); enum check +19/0 +navigation2/0 +view41/0 (9c822221); extracted-step
check +19/0 +view41/0 (42d3c823); atomic-read check +19/0 +navigation2/0 +view41/0
(03a5b0fd). Repeated characterizations are repeated qualification, not 95 distinct
cases. The six marked behaviours remain unchanged. Metrics include new setters
and enum writes: 147 ->141 mutation/transition sites, 395 ->307 scroll lines,
20 ->18 stored fields. No timing threshold, cache budget, protocol, Core, worker,
queue or dependency change. No real terminal/clipboard, user data or paid provider
validation. Native branch CI still required before handing this checkpoint off.

The final full-field metrics audit also counts view_revision and all projection
geometry writes: baseline151 direct +10 transitions=161, checkpoint99 +56=155.
The earlier147/141 comparison is the narrower legacy inventory, not a different
outcome. Fields20 ->18 and scroll395 ->307 use the same archived source and
helper. check/fmt + scrolling6/0 + ui_scroll_stream/refresh11/0 (57f169e8) pass;
metrics-helper4/0 and test watchdog policy pass. Native branch CI is pending.

First branch CI 41300987588 (3d5dc0fb) failed strict Clippy on the new test module
preceding impl ThreadView; no behavioural test failure is claimed. Move that
module to EOF, without suppressing the lint. Local strict clippy --tests and
characterization19/navigation2 pass (2aa2b2d6). Replacement exact-SHA CI required.

## Resource Benchmark Stage 1 (#116)

Scope: Python scripts, documentation and an opt-in Linux CI job only; no Rust,
Core, schema, provider account or other-agent changes. Portable synthetic tests
exercise owner/descendant PSS (not RSS), missing metrics/owner, PID-start-time
churn, deterministic SSE progression, per-Thread bounded tools and environment
isolation. These are correctness tests, not performance thresholds.

The local macOS debug binary is source-built through rust-slot and copied in
the same slot before transport checks. A real disposable PTY verifies visible
first frame and pasted composer echo; disposable `serve`/loopback SSE checks
exercise all five rounds, exact saved answers and successful native shell
cleanup. Local transport checks replace only the PSS reader with an explicit
`transport_smoke_no_linux_pss` stub: they are **not Linux PSS observations**.

Linux release-binary PSS and startup are qualified by manually dispatching
`ci.yml` with `footprint=true`. Inspect its exact-SHA artifact's `status`, all
four N cases, five rounds each, PSS process sums, nine requests/four tool calls
per conversation and startup fields, in addition to workflow/job conclusion.
The artifact records machine/OS/binary hash and CI source SHA. Broken sampling
must be error, not zero or incomplete success. Normal native Linux/macOS checks
remain independent; Windows PSS is not supported. Real-provider performance,
physical display latency, peak memory and cross-agent comparisons are untested
and outside stage 1. See `scripts/bench/README.md` for the workload discrepancy
with the upstream reference and the full measurement boundary.

### macOS frontend initial-observation readiness (#138, CI half)

The real-fixture delayed-`CurrentView` regression permits initial `Thread`,
`History` and browser updates while holding selected refreshes, rejects the
browser-only baseline, then explicitly releases the actual updates. The pure
readiness matrix covers both delivery orders. The original paired-observation
case retains 16 measured CLI mutations and its exact-one/shared-sequence,
unchanged-generation and unchanged-page-count assertions. Existing watchdogs
and the informational latency target are unchanged. See
[frontend stability](frontend-stability.md#macos-paired-observation-baseline-138-ci-half)
for the source panic and comparison with #124. Local focused repetition and
three ordinary exact-SHA Linux/macOS workflow runs qualify this change; their
actual outcomes and URLs belong to the handoff evidence, not a presumed pass
in this acceptance definition. Windows remains an integration-batch check.
## Resource Benchmark Stage 2 (#116)

Scope: Python bench scripts, docs and an opt-in Linux CI job; no Rust/Core
change. `scripts/tests/test_fake_model.py` checks, on any OS, that each of the
three wires yields four tool calls then five fixed answers, schema-shaped shell
arguments, auxiliary replies, the per-round tool budget and the request cap.
These are correctness tests, not thresholds.

Local macOS `--smoke` runs (no PSS; not observations) drove Claude Code, Codex
and opencode through all rounds for N=1/3 and jcode for N=1/2 before jcode was
restricted to Linux: its first macOS TUI start registered a launchd hotkey
agent outside the temporary HOME (removed; no other home state remained).

Linux observations come only from `ci.yml` dispatched with `footprint=true`:
inspect the `compare` artifact's `status`, every agent's `status`, four N
cases with five rounds, the fake model's per-round tool/answer counts, PSS
process lists and startup samples. CI 41306805531 (fac69f4f) completed all five
agents; the final branch SHA's run is recorded in the #116 handoff. Real-model
behaviour, tokens, peak memory, physical display latency, Windows/macOS PSS and
default-install variants (jcode memory, sandboxes) are not measured.
Continuation-only lifecycle movement passes check --tests / characterization19 /
navigation2 / view41 (00054e88), still preserving all six marked expectations.
It is not part of the first frozen checkpoint 3d5dc0fb.

Frozen checkpoint 79fb04a7 passes native Linux/macOS and standard branch CI
41301714398, replacing the failed 3d5dc0fb run. Handed to integration as partial
scope, not stage-2 completion. Continuation child-module movement passes check,
characterization19 and view43 (a5f25a6b); no six-case expectation changes. It is
not contained in the handed-off SHA.

Navigation event boundary before #131 rebase: strict Clippy --tests, unchanged
characterization19, view45 (including phase16 round-trip and legacy settlement
checks), scrolling6 and stream/refresh11 pass (237ec764, log
/tmp/issue118-event-gate2.log). Earlier strict attempt stopped solely on an unused
fixture seed method; it was removed, not suppressed. Watchdog policy passes with
no new guard/fingerprint change. Read reducer and projection publish every state
change via events, measured161 ->90 combined sites and fields20 ->15. Full local
suite not run; native branch CI and latest-main/#131 qualification still required.
All six suspect behaviours remain intentionally unchanged.

#118 event boundary, latest-main requalification: base275eb122 (#131 included),
check --tests, unchanged characterization19, view45, scrolling6 and stream/refresh
11 pass (88de4364). Only the two characterization conflicts were resolved,
retaining #131 heights33/13 and resize60x33 plus the new getter API. Bounded
boundary-planner extraction then passes strict Clippy --tests and the same19/
45/6/11 gates (224f4573; /tmp/issue118-boundary-plan.log). Conservative metrics
are161 ->91 overall (approved checkpoint155 ->91), fields20 ->15 (approved18
->15), scroll395 ->237 (approved307 ->237). Native branch CI still pending;
no local full-suite claim. No marked behaviour fix is included.

## Resource-usage guide publication (#116 stage 3)

Source: successful public workflow run
[41307629104](https://github.com/tya5/shellboy/actions/runs/41307629104), measured
SHA `5a2e8474a52928eaafd35d64c3eb29dfd6c34626`; documentation prerequisite
`issue-116/compare` at `4d6f128a604170ff2bc4ece19b02d8adba04ce86`.
`footprint` job 124851174890 produced artifact 13084027318 and `compare` job
124851174914 produced artifact 13084610570. Both contain `status=complete`;
the comparison is not a smoke run. The two jobs are separate runners even
though their CPU model matches. The published guide uses the fresh stage-1
observation in this workflow, not a mix of historical measurements.

Downloaded public JSON was cross-checked offline against every published row:
four stage-1 baseline/five-round rows, five round-5 comparison rows, and five
startup median/min/max rows (five observations each). Source SHA, executable
hash, KiB units, agent completion, N matrix and recorded Node 22.23.4/npm 10.9.9
were checked. The generic startup definition is not stage 1's completed-frame/
pasted-echo method. The guide preserves fake-model, process-shape, disabled
approvals/sandboxes, feature-scope and same-comparison-run limitations. Source
inspection corrected the README's blanket 3-second baseline claim: only the
four external-agent adapters have that delay; Shellboy reuses stage 1 unchanged.
No measurement code, installed agent, provider account or user data is changed.

Qualification: existing bundled-doc tests cover links, sections, UTF-8,
pagination, cursors and keyword search. A new real CLI regression retrieves and
searches `shellboy/resource-usage` with missing config and an uncreated data-dir,
checks provenance/caveats, and verifies that no files/owner are created. The
resource page is a fourteenth English logical guide under the existing reference
category, with its relative link registered. Adding it changes the existing docs
version/hash; a previously indexed docs corpus can require explicit reindexing,
never an automatic embedding call. Native focused checks and ordinary branch
CI qualify the commit; actual commands/results and exact-SHA CI URL are handed
off after completion, not inferred from the measurement run above. Windows is
reserved for integration-batch checks. This task does not repeat measurements
or establish real-provider, physical-terminal, IME or cross-run performance.

Native macOS qualification: `cargo check --locked --all-targets` passed;
`cargo test --locked --lib docs::` passed 8/8; `cargo test --locked --test
integration docs_cli::` passed 7/7 including the new page regression.
`docs_proxy::` was intentionally ignored (0 passed, 1 ignored): that opt-in test
would call an actual model API and is not evidence here. Python script tests
passed 118/118; `scripts/check-test-layout.py` passed 101 suites and watchdog
policy. Formatting and final post-fetch focused verification are repeated
before push. No new test suite or target is introduced.

Downloaded `result.json` byte digests (SHA-256, not archive hashes):
- Comparison: `4e9a9aab8460c834e10ad2a0a48819ec7bcb3f891c02c76f101c01a984fd99e7`.
- Footprint: `f537b7963ac3a686c1ee126de5e077f20ed0f8aa5c2d0a886e11591147f44f05`.

Before first push, main advanced to `3faff8988d352ebf5e1071154be28b26cbdf182f`
with stage 2 integrated. Only the two owned guide commits were rebased onto it
(`--onto origin/main 4d6f128a`), without conflicts; no stage-2 commits were replayed.
The three comparison runner/installer/fake-model files and all script tests are
byte-identical to `4d6f128a`. Post-rebase Python 118/118 and layout 101 suites /
watchdog pass. This supersedes the 100-suite count on the original staging base;
only the focused native checks and CI on the final rebased source qualify the
handoff. The reproduction example explicitly requires Linux x86_64 and Bash.

The first guide-branch CI, run 41312172387 at `41483fda`, failed on both Linux
and macOS in `all_guide_navigation_captions_match_actual_display_labels`: its
full-catalogue count still expected 13, while the adopted guide count is now 14.
Integration harnesses (including the new page test) passed. This was a missing
fixture update in this change, not a timing flake. The exact count is updated
to 14 and explicitly requires `shellboy/resource-usage` in the scanned set;
the full caption scan, wrong-caption negative control and coverage gate remain
unchanged. The focused caption test is added to local qualification before the
next push; a new exact-SHA CI is required. No UI product behavior changes.
### #118 behaviour boundary: tail no-op

macOS synthetic TestBackend: `cargo test --locked scroll_characterization_tests
-- --test-threads=2` passed (19 characterizations); fmt passed. Same metrics
helper: combined91, scroll237, fields15. Native branch CI follows the six-item
boundary. No real provider/terminal/clipboard/user-data validation.

### #118 reverse input

The two-gap regression retains +150 rows behind the existing read owner, then
compares the settled screen with a fully cached reference. Combined91 /
scroll237 / fields15 (same helper); read helper is four lines shorter. Focused
Rust tests are queued behind the shared build slot; not yet claimed passed.

### #118 failure/rejection replay

Read failure and admission rejection now discard their deferred displacement;
the explicit retry assertion requires exactly the original single-input delta,
not twice that amount. Reducer test expectation changes to zero. Same helper:
combined91 / scroll237 / fields15. Focused Rust validation pending shared slot.

### #118 Tab review: retain intentional pause

Doubtful marker removed: focus is explicit history navigation, unlike a no-op
wheel. Regression now checks selected-item identity, source anchor and rows stay
fixed across streamed growth, with unseen set. No production code added.
Same helper: combined91 / scroll237 / fields15. Rust validation pending slot.

### #118 projection-cache review: retain derived publication

`viewport_row` is renamed `project_viewport` throughout callers and fixtures to
make mutation explicit. The regression now checks width/height publication on
resize while authoritative navigation stays equal; the existing repeated-frame
check remains. Same helper: combined91 / scroll237 / fields15. No production
lines added and no watchdog allowance/deadline changes. Final Rust checks pending.

### #118 true-end clamp review: retain projection-only full pages

Doubtful marker removed. Regression now requires the source anchor and request
revision to survive clamping, appended growth to remain paused at that source,
and partial body windows not to clamp as EOF. No production code added.
Same helper: combined91 / scroll237 / fields15. Final focused checks/CI pending.

Focused-check corrections: the stronger Tab growth test initially rejected new
text filling a previously blank last row; it now requires every existing content
row to stay fixed, while allowing blank rows to fill. All19 characterizations
then passed, including EOF append/partial-window coverage. The next view check
found two older failure/rejection tests expecting retained movement; those are
updated to zero pending and no automatic retry, matching the adopted contract.
No deadline or watchdog assertion changed. Final rerun follows.

### #118 six-case boundary: final local evidence

This supersedes the per-case pending local checks above. macOS, synthetic
TestBackend/local fixtures, shared rust-slot: fmt; `cargo check --locked --tests`;
`cargo test --locked --lib scroll_characterization_tests` (19);
`cargo test --locked --lib ui::view::` (45); integration `scrolling::` (6) and
`ui_scroll_` (11); `cargo clippy --locked --tests -- -D warnings` all pass.
Focused tests use `--test-threads=2`. Metrics helper tests7 and watchdog policy
checker pass. No watchdog allowance, guard, deadline or timing threshold changed.

| Disposition | Combined sites | scroll() lines | Stored fields |
| --- | ---: | ---: | ---: |
| Approved structural boundary cd958b4d | 91 | 237 | 15 |
| Tail no-op fix | 91 | 237 | 15 |
| Reverse-input fix | 91 | 237 | 15 |
| Failure/rejection replay fix | 91 | 237 | 15 |
| Tab pause specified | 91 | 237 | 15 |
| Derived frame publication specified | 91 | 237 | 15 |
| True-EOF clamp specified | 91 | 237 | 15 |

Direct writes32 + transition/entry calls59 remain91. UiState scroll wrapper stays
7 lines. Navigation/read production source is shorter (no relocated writes or new
stored state); regression test additions do not inflate production orchestration.
The branch handoff carries the exact candidate SHA and successful native CI URL;
Linux/macOS full standard checks are delegated to CI. Windows belongs to the
integrator's all-OS batch. No local full-suite, real terminal/clipboard, paid
provider or user-data validation is claimed.

Native CI run41315488946 (197de69c) failed on both Linux and macOS in the same
older metadata-eviction regression: it supplied another PageDown after the reply
and indexed an empty action list, although the retained reverse input had already
issued the expected next-ordinal read on reply. The test now requires exactly one
LoadHistoryWindow(ordinal7, from_end=false) in deferred reply actions, without a
second key. Focused macOS execution of that exact integration test passes. This is
a test-contract correction for the reverse-input fix, not a production/budget or
guard change. Combined91 / scroll237 / fields15 stay unchanged; new-SHA CI follows.

## #138 saved-context status diagnostic boundary

Ubuntu run41316050841 attempt1 timed out in the #141 long-thread status test.
Its previous failure showed a completed saved attempt but neither the terminal
frame nor CurrentView context-usage snapshot. Before changing readiness, the
fixture now retains the last drawn synthetic frame and the last eight paired
snapshot identities/usage scalars and includes both in its watchdog failure.
No user data, raw conversation logs, provider request or watchdog budget change.
This was committed before the state-driven correction described below.

The diagnostics-only original test passed locally once (macOS); that is not a
reproduction or proof of the Ubuntu cause. Source tracing identifies a concrete
startup race: SelectThread publishes separate Thread/History/progress; the saved
context is only attached to CurrentView. Selection starts with progress_running,
but completed activity turns it off. A periodic tick admitted before that result
can issue CurrentView; a later tick correctly issues no idle read. Fixture rows
were committed before subscription, so no Changed event guaranteed that snapshot.

The correction holds initial paired views until completed activity is observed,
asserts the footer is still unknown, releases them, commits a real title change
through the separate CLI/owner, and waits for that title's actual CurrentView
input159534/cache103424 plus rendered input/cache. A pure readiness regression
rejects absent, old-title and unknown snapshots and a missing footer. It also
finishes/drains the real bridge. Local focused readiness and owner tests plus
strict Clippy passed before the additional unknown-before-release assertion;
final local rerun and same-SHA CI evidence are reported separately at handoff.
The later #118 batch failure41317992047 was reproduced in a separate task-owned
worktree, not dismissed as unrelated; the fix branch includes that exact batch.

Batch comparison: the original predicate with diagnostics only on a6fb0eba
reproduced locally (exit101, /tmp/issue138-batch-diagnostic.log). The last frame
showed completed / input unknown although the delivered CurrentView contained
input159534/cache103424, read_revision1, selection_revision1, generation1.
Thus "no snapshot delivered" alone does not explain this case. An older admitted
snapshot can be correctly rejected behind a newer initial History watermark;
a completed selection then has no timer-driven convergence obligation. Extra
fence diagnostics are collected to distinguish rejection from absent delivery.
The new state-driven fixture on that exact failed batch passed all17 frontend
reactivity tests and strict Clippy locally. A deterministic unit regression now
orders initial History watermark2 -> usage snapshot1 -> repaint -> snapshot3,
checking unknown until the new observation and preserving the stale-read guard.

Counterfactual confirmed on pre-#118 base acb476bc with diagnostics only:
/tmp/issue138-baseline-fence.log attempt1 passed (snapshot1 arrived at UI read0),
attempt2 timed out (snapshot1 arrived at UI read2; selected generation1 and
selection1 matched). Its screen remained completed / input unknown. This pins
the read-fence ordering, demonstrates the same failure without the #118 behavior
commits, and does not equate one earlier pass with no regression. The #118 batch
was independently reproduced and the correction is tested on that batch. No
navigation/read guard, production query, poll rate or watchdog was relaxed.

Local final owner/frontend regression:17 passed; deterministic stale-initial
context regression:1 passed. The first strict lint run found clone_on_copy in
the new synthetic fixture; the fixture now assigns Copy usage directly and
strict lint/fmt are rerun before push. CI receipts and the exact candidate SHA
are supplied in the handoff rather than changing the validated SHA afterward.

## 2026-11-01 - #142 Part 1: Cache Diagnosis, Not Sticky Routing

Adopted contract: [cache diagnostics](../design/cache-diagnostics.md). Scope is
fake replies over loopback HTTP only, in disposable temporary data directories.
No installed binary or user data is changed; no provider/cache-effectiveness,
LiteLLM or sticky-routing result is claimed.

| Surface | Local evidence | Result / Limit |
| --- | --- | --- |
| Default-off / strict config | `config::cache_diagnostics_are_strict_opt_in_responses_only_and_reserve_dynamic_options`; original #137 fixture asserts options absent | false default, typed bool, Responses/master-opt-out and owned-body-field validation pass |
| Comparison and adoption | `cache_diagnostics::{json,sse}_cache_diagnostics_follow_adoption_and_are_inspectable` | Both JSON/SSE pass first request, tool loop, cross-Turn, missing adopted ID, nonadopted incomplete response, Thread/fork isolation and owner restart |
| Reported observations | `model::cache_diagnostics::tests` | Unknown bounded type/reason strings preserved, zero distinguished from missing, negative/fractional/oversized values discarded, no inferred cache hit; encoded object <=2048 bytes |
| Terminal CAS | `control::state::cache_diagnostic_tests` | Completion/failure/incomplete use guarded metadata updates; disabled or stale attempts cannot fill/overwrite the saved observation |
| Usage inspection | `usage_totals::tests::attempt_diagnostics_read_is_bounded_exact_read_only_and_not_an_aggregate` | PK/scalar reader does not copy unrelated large manifest bodies; unavailable diagnostics are null; read-only and missing-target cases pass |
| Existing standalone F8 path | `frontend::inspection_state::cache_attempt_reader_tests` | Same query-only reader; source-Thread notifications, not periodic timers or viewer-Thread changes, refresh the result |
| Request reconstruction | Existing #137 fixture plus JSON/SSE diagnostic fixtures and `request_inspect::cache_witness_tests` | Admitted wire digest preserved even after newer responses; changed/missing/malformed witness is unknown rather than guessed |
| Public selector | CLI `usage_attempt_selector_is_typed_and_exclusive`; wire `attempt_usage_wire_validation_rejects_aggregate_options_and_blank_ids` | CLI and typed operation/wire metadata reject aggregate-option combinations; current selector remains part of usage.get, not a new operation |
| Native migration | `storage::schema_twenty_backfills_only_adopted_attempts_and_clears_deleted_identity` plus existing storage suites | Completed-only backfill, missing-ID clearing, FK deletion-index EXPLAIN, reopen without fallback and FK integrity pass; fresh/upgrade equivalence retained |
| Supporting scripts | `python3 -m unittest discover -s scripts/tests` | 122 pass, including synthetic public schema17-to20 preservation; current-only error catalogue now requires schema20 |

All-target check/Clippy, 40 focused cache-related library cases (one pre-existing
ignored), 50 operation/form cases, JSON/SSE suites and the named config/wire/#137
cases pass locally. Prior focused storage (25 integration, 4 library), state
(11), scalar-reader and CLI cases pass. Test layout and `git diff --check` pass.
Initial implementation check failures (fixture/CLI compile mismatch, enum size
lint, schema-pinned Python guard) were corrected; they are not CI or provider
results. Full standard check is reserved for branch CI after push approval.

Not yet confirmed here: post-rebase rerun, exact-SHA Linux/macOS branch CI,
independent review, Windows/3-OS integration batch, main inclusion, native public
binary/PTY schema17-to20 upgrade or any physical terminal/provider behavior.
Sticky headers are deferred and receive no implementation or verification claim.

### #142 Rebase And Verification Delta

Rebase base: `acb476bc8d51745f9d17f2b01877137da88c6e8b`. Main's schema19 belongs
to #141; this feature uses schema20. Startup bootstrap, ordered migrations,
release-upgrade helper/default, current-only catalogue guard and historical
fixtures are updated together, preserving both sides. On the rebased tree:
Python helpers 122 pass; all-target check; cache library 40 pass/1 existing
ignored; bounded attempt reader 1; JSON/SSE integration 2; storage integration
25 + library 4; CLI selector 1 all pass. Shared record delta against origin/main
is additions-only. Prior all-target warning-clean Clippy and 50 operation/form
cases remain pre-rebase evidence, not an unrun new CI result.

Explicit task-branch push approval has been requested and is not recorded yet.
No push or branch CI has occurred; integrator handoff is gated on that CI.

### #142 Approved Branch Dispatch

Task-branch push/CI approval is received. Pre-push execution
`e95dc948-4c6d-4400-b9a9-4453dbb7e80b` exits 0 after fresh fetch/rebase,
all-target check, cache library 40 pass/1 existing ignored and JSON/SSE suites
2 pass on source-equivalent `abb91c5b`. The next action is default task-branch
Linux/macOS full standard CI; Windows, review and main inclusion remain separate.
The resulting exact-SHA CI URL/conclusion will accompany the formal handoff;
this pending-dispatch record is not CI success evidence.

The CI-wait self-check also adds the exclusive attempt selector to the public
operation dictionary (the detailed guide section already describes it). The
final handoff must reference the subsequent exact-head CI, not a run started
before this documentation correction.

### #142 Branch-CI Failure And Fixture Repair

- Run 41321728888 (`5379d828`) and run 41321966358 (`6f1b27ba`) fail on both
  OSes at Tests. Do not treat either as successful CI. Layout, format and
  warning-clean Clippy pass in both OS jobs of the latter run.
- Rooted failures: seven old-version fixture preparations forgot schema20's
  column/index; the F8 foreign-run fixture traversed one field too few after
  adding the attempt selector. Repair fixture setup/navigation, not production
  migration idempotence, usage contracts, timeout limits or skip/ignore policy.
- Focused repair log: owner migration cancellation1, F8 foreign-run1,
  storage-hot-query5, index reset1, pre-marker recovery1, readonly-history1 pass.
  The combined command then exits101 on the unchanged #141 status-bar watchdog.
- The same status-bar test passes a separate temporary screen-observed run
  (`2fd5adf1-a3be-423e-bb84-5180c140f27f`, exit0); that instrumentation is removed.
  Its timing sensitivity is unresolved, not proven pre-existing and not hidden
  as a fixture-repair success. Final uninstrumented tests/CI must be separate
  evidence; the failure history belongs in the handoff even if those pass.

### #142 Review Revision Admission (Schema19)

- Supersedes prior schema20/pointer claims: reviewer rejects `6b687296`; shared
  #141 context selection over 33 recent Turns is the replacement. Missing
  in-window adoption omits comparison/options. Uses the context display's
  current connection/model scope; no older-history scan. Record D-142 revision.
- Rebased onto main `6a3291a`. Bootstrap, migrations, adoption implementation,
  old-schema fixtures and release-upgrade/error-catalogue support are restored
  byte-for-byte to main; no schema change, backfill or redundant adoption write.
- Initial source check/cache40+ignored1/context10/JSON-SSE2 pass before adding
  the two dedicated Native window/cost tests. The final expanded source checks
  and new exact-SHA CI remain pending here; do not cite the rejected head's
  successful run as evidence for this revision.
- New test contract: 33rd Turn available / 34th excluded; no request options
  for absent/invalid IDs; no older-ID fallback; excluded noncompleted states;
  exact shared scope; query-only/no mutations; existing three-key completed
  index seek and VM work insensitive to 5000 older Turns and attempt depth.
- No Windows, integration batch, provider efficacy/API, installed binary/user
  data, physical terminal or native published-release upgrade claim. Earlier
  unchanged status-bar watchdog failure history is retained, not erased or
  claimed fixed by this lookup revision; latest main includes #138 follow-up.

Review-revision local evidence now observed (macOS, disposable fixtures, every
Cargo invocation serialized): execution438be45f exits0 with check/cache42+one
existing ignored/context10/JSON-SSE2/storage24/hot-query5; execution0a28b58e
exits0 after final disabled-inspection gate with fmt/all-target check/cache43+
one existing ignored/JSON-SSE2/original #137 inspection1. Both new window/cost
cases pass, including the three-key plan and growth-independent VM bound.
Python121 pass. The next-inspection gate's missing-index negative/positive
control passes; default-off inspection avoids the added selection just like
the driver. No new skip, ignore, timeout or dependency changes. New exact-head
full CI and independent review/integration are still pending at this record.
## UI language and terminal width foundation (#139, 2026-11-01)

On macOS arm64, Darwin 25.3.0, Rust 1.99.1, using a shared `rust-slot`, two
Cargo jobs/two test threads and an isolated synthetic data directory, the UI run
passed 540 tests (3 ignored), presentation library 65, integration presentation
48, integration UI 86 (1 ignored), model picker 11, and real-TUI PTY 2. The
foreign-CLI recovery/stop case passed separately after correcting its changed
visible marker. This observation predates the last emoji-modifier/keycap cases;
it is not final-HEAD or CI evidence. The layout watchdog and 115 Python tests
passed separately. Final commit and CI evidence belong in the handoff.

The synthetic PTY exercises two measured widths, disagreement, malformed and
missing replies, keyboard/modified-F3/bracketed-paste preservation and worker
width propagation. Synthetic TestBackend fixtures exercise the migrated
confirmation in English/Japanese at 80/120 columns under width one, two and
unknown, including border integrity. Markdown tables check generated ASCII borders
without replacing the same character in model text. Configuration checks cover
both languages, unsupported values and separate locale environments.

These are synthetic fixtures, not real user data or a guarantee for all fonts,
terminals or multiplexers. Windows uses the documented unknown-width fallback;
its native DSR is not claimed. Stage A translates only the confirmation and width
diagnostic; the remaining screens are stage B. CLI/API/saved content and raw
user/model text remain outside the translation boundary.

### #139 review revision

The original vendor-based implementation described above was superseded by the
review revision. It removes both crate patches and measures through upstream
`cursor::position()` in a bounded helper process before the UI loop. PTY tests
cover width one, width two, disagreement and timeout. Shellboy layout follows
the observation; Ratatui remains upstream and a measured mismatch is exposed in
TUI status and diagnostics. Windows behavior depends on the public backend; an
unsuccessful probe is unknown. Exact candidate CI evidence is in the handoff.

## #138 Windows initial selected/browser committed baseline

Windows run41342038037 attempt1, job124953965722 failed the delayed-selected-view
fixture at frontend_reactivity.rs:342 (observation watchdog, bridge unfinished,
no delivered CurrentView); the retry passed. The initial tick was not guaranteed
before idle activity disabled selected polling, whereas browser excerpt delivery
was independent. Both baseline waits now use a real CLI title commit/Changed,
matching owner snapshot identity and adopted consumer titles, not elapsed time
or a speculative startup read. The delayed case still asserts no selected view
adoption before release and adoption afterward. The measured 16 changes keep
all original count/shared-sequence/no-reload assertions. Production, watchdog
budgets and stale-read rejection are unchanged. See the failure and qualification
record in [frontend stability](frontend-stability.md#windows-committed-baseline-follow-up-138).
Local results and the required three consecutive Windows successes at one exact
SHA are reported separately at handoff, not inferred from earlier CI passes.
## #152 task 1: non-UI English wording

Scope: Core title fallbacks, startup data-directory/IPC errors and hints, and
model-facing summary headings; no UI source changes or data migration.
`create_thread` omits the title column (empty schema default). The Core callers
of `thread_display_title` derive read-time details/list values; the frontend
program-status caller also only reads the value. `set_thread` writes only
explicit title input. A nonblank stored title returns unchanged,
including the old Japanese fallback text if explicitly persisted elsewhere.

Source inspection with recursive grep found no summary-heading parser. The
remaining Japanese summary headings in runtime/attachment fixtures are model
output data, intentionally retained. New summary instructions request Goal,
Constraints, Current state, Open issues and next steps, and Key references.
The loopback summary proxy checks the outgoing prompt; an English-heading
fixture checks summary adoption, and the existing fixture checks caching.
Separate regressions cover saved Japanese summary replay without a payload
rewrite and Japanese stored titles. Unicode stress fixtures (including
`"あ".repeat(...)`) are unchanged.

Local native macOS focused checks and branch Linux/macOS CI are pending at this
entry's creation; the task handoff records exact commands, results and run ID.
No paid model, user data, installed binary, Windows-specific behavior, or full
bilingual UI acceptance is claimed by these synthetic tests.
## #152 Stage C Initial Language Inventory

On current mainb12a8291, the read-only scanner enumerates Japanese-containing Rust
string literals and conservatively classifies proven Japanese local!/language
arms and test-only scopes, leaving other candidates for explicit review. Thirteen
synthetic Python tests pass (134 total Python tests; raw/multiline/escaped strings,
comments, lifetimes,
bounded branches, external test modules, stable/stale IDs and no-write failures).
The [initial per-file table](language-audit-stage-c.md) is an inventory, not a
zero-mixing result. W6 Core/startup/model-instruction changes landed with their
own three-OS CI41359768326; combined final zero-(c), En/Ja 0/1/2/many rendering
and Stage C three-OS CI remain unverified. B-5/B-6 owner source files
have not been edited by this preparation. The approved independent protected
canonical-token category requires individual saved/comparison/classifier and
UI-projection references, not blanket exemptions. Its current count is zero;
remaining conservative (c)=1888 is not a confirmed-defect count or completion.
W6's four files and W8's live_work separator area remain unedited here. No
user-selected mark, branch or separator was changed by this audit.

### Temporary IPC Diagnostic Copy, Batch 2 (#152, Next Release Only)

The pure namespace runtime-error test checks English message/hint, unchanged
`IoFailed`, and exact `component`/Japanese-Unicode `reason` details. It performs
no filesystem or OS clipboard operations. Namespace selection, ownership, path
bounds and lease logic are untouched; the existing namespace tests remain the
platform-specific guard checks. Local macOS and branch Linux/macOS CI results
are reported separately; no Windows-local or physical-terminal claim is made.
This product batch is not part of 0.6.0.
### #152 Post-0.6.0 Child-State Projection Candidate

The follow-up [file-by-file review](language-audit-stage-c.md#post-060-follow-up-review)
records eight diagnostic literals in two independent CI-qualified branches and
one further child-state projection gap. With W8 coordination, the latter changes
only `src/ui/live_work.rs`: identify child rows by their typed target, project
known `ActivityPhase::column_label()` values through existing En/Ja phase text,
and leave `row.state`, classifiers, saved data and all glyph selection unchanged.
Unknown source state text bypasses decoration; shell/wait states retain their
previous projection, even if source bytes look like a canonical child label.

New synthetic tests cover all 17 phase variants in default En / explicit Ja,
byte-preserving unknown and non-child states, legacy child machine codes, and
1/2/10-child registered rendering at 80/120 columns under 1/2/unknown glyph widths.
The existing registered empty/folded tests cover the zero-child case. Local and
exact-tip branch CI results are reported separately; no physical-terminal,
OS-clipboard, live-provider or combined Stage C completion is implied.
### Production Copy Diagnostic Review, Batch 1 (#152, Next Release Only)

The confirmed-leak table is in
[Stage C audit](language-audit-stage-c.md#product-copy-review-batch-1-next-release-not-060).
The six literals in `src/frontend/copying.rs` are generated worker errors, not
saved comparison tokens or source/provider text. No B5/B6/W8 source is changed.
Review returned the English-only tip6335def7: TUI action guidance must honor Ja.
The corrected private error boundary uses the loaded UI language, tests all six
exact pre-batch Japanese messages plus English (ASCII parentheses), unchanged
ErrorCodes/hints/details and a verbatim Japanese/Unicode source reason. An async
temp-DB fixture tests real worker entries with default En and explicit Ja,
bounded source/code retrieval and hash-change errors, plus Unicode copy bytes.
No CLI/wire dispatcher calls this worker; its public Rust copy_source helper is
used by injected tests. The language/caller boundary is documented in the UI
language design before the correction; underlying API errors are not translated.
Local macOS fmt/check and4 copy tests plus1 injected original-body integration
passed. Exact corrected-tip CI is reported separately; old-tip CI does not
qualify this correction. No physical terminal, OS clipboard or live-provider
acceptance is inferred. This product change is not for0.6.0.

## #158 attachment MIME investigation (in progress)

On macOS, isolated source-built binaries at v0.5.0 `02fda155` and v0.6.0
`358ed63b` used separate temporary data directories and a loopback Responses
mock. An external UTF-8 `.log` attachment was sent as octet-stream in the
first and a later Turn on both versions; the mock deliberately accepted the
first and rejected the second. Moving the synthetic file before retry sent an
unavailable notice and completed the same Turn in both versions. A separate
v0.6.0 run explicitly removed the attachment before retry and completed while
keeping the synthetic file. No real provider or user file was accessed.

On macOS, `cargo fmt`, `cargo check --tests`, the focused MIME unit test,
focused `issue_158_` loopback integration tests, all 11 attachment provenance
tests and all 25 attachment service tests passed after the approved change.
They cover UTF-8 files with five unknown/no extensions, unknown binary replaced
with an unavailable notice, conversion and later replay, and both removal and
moving of a rejected protected attachment before retry. Native Linux/Windows and
actual provider compatibility remain untested locally; branch CI is pending.
On macOS, the latest revision also passed focused tests for colored UTF-8
logs, unknown image-kind binary, localized warning rendering and bounded lookup,
and an octet-rejecting mock. Four `issue_158_` loopback tests passed. These are
synthetic tests, not verification against a real provider.
## Content-filter recovery facts (#159, 2026-11-02)

Local native macOS, temporary databases and loopback HTTP mocks only. Before the
implementation, `content_filter_terminal_preserves_facts_without_automatic_retry`
passes with retry budget 8: terminal incomplete JSON and typed SSE preserve the
original response and enter recovery after one attempt, without another POST or
an `automatic_retry` record. The implementation does not change retry policy.

Fresh source-mtime validation through the shared Rust slot, locked dependencies,
CARGO_BUILD_JOBS=2 and RUST_TEST_THREADS=2: cargo check --locked --tests and strict
all-target Clippy (-D warnings) pass; ten focused unit tests and two integration
tests pass. Coverage includes bounded nested verdicts, unknown enum strings,
missing versus returned null after JSON roundtrip, 1,000 overlong synthetic
categories, explicitly filtered category display priority, raw hint/pointer/copy
preservation, both languages, and actual TestBackend screens at 80/120 columns.
The integrated loopback fixture exercises JSON/SSE, present/missing verdicts,
Turn/Run readback and the saved activity projection into the real UI.
Layout/watchdog policy check passes: 102 suites registered once, no new guard.

The fresh-run progression caught a test API-name compile error, an incorrect
assumption about existing safe-detail JSON-string expansion, and Japanese
wide-cell reconstruction in a new screen assertion. Corrections retain raw
wire/hint data, keep generated advice separate from real JSON pointers, and do
not weaken existing watchdogs, guards or assertions. No provider API, OS clipboard,
user data, installation or release operation was used. Fresh native checks also
pass after rebasing onto main 3fe829a9 (#158), including its existing attachment
warning regression. A self-review regression keeps JSON-looking provider string
values opaque before generic safe-detail expansion. Python checks pass (134).
Exact candidate branch CI remains pending; evidence is reported separately.
Windows, physical terminal/IME and real Azure/provider behavior remain unverified.

### #159 restack after #160 main landing (2026-11-02)

Base b0265deb615ce157e0c9acfa57224be307aa0786 is the confirmed #160 main,
with exact three-OS CI41388835583 success. The four #159 feature patches reapply
without conflict and range-diff marks every patch equal. Existing main acceptance,
status and decision records remain intact; record-delta against b0265deb reports
zero removed lines. This restack adds evidence instead of replacing either side's
prior records. #160's attachment selection, both test files and recovery guide
are byte-identical to the landed base.

Fresh native macOS validation, shared rust-slot, locked dependencies, build/test
threads2 and refreshed source mtimes: fmt --check, check --tests and all-target
Clippy -D warnings succeed; all ten filter unit tests and both filter integration
tests pass. The landed #160 regression
`issue_160_auto_compaction_detached_attachment_and_retry` passes (one test), as
 does `detached_excluded_tombstones_do_not_hide_active_or_live_source_errors`
(one test). These are actual mock compaction/send/retry and active/live-source
protection checks in the combined tree. An initial `excluded_attachment` filter
matched zero tests and is not counted as coverage; the two explicit names were
then run and each reported one pass. Layout102 and watchdog policy pass.

No feature code, retry policy, attachment rule, config, glyph or existing assertion
changed during restack. New exact task CI is pending; its SHA and outcome are
reported in the formal handoff. Windows for this new candidate, physical terminal,
IME, real provider and OS clipboard remain unverified. No real user data, provider
API, installation, release or main push is involved.

## Resource Benchmark Stage 4 (#116)

Scope: synthetic Linux comparison instrumentation and documentation. Measured
source `8d3201e398c0fcb21d53eb6fc8e54424da6b7c04` in workflow run 41381063988:
attempt 1 compare failed because Codex N=20 process trees changed during every
PSS snapshot; attempt 2 compare succeeded and produced artifact 13113493939.
All five agents completed N=1, 5, 10 and 20 with zero unattributed requests. The
artifact's per-request byte lengths sum exactly to the per-conversation totals;
first/last match each conversation's recorded receipt order. Temporary storage
is sampled after round five, including HOME and agent data roots, without following
symlinks; missing/unreadable roots fail rather than yielding zero. Shellboy DB and
WAL are included; workspace and externally installed executable are excluded.

Portable tests cover UTF-8 HTTP lengths, session ID and workspace-path association,
ambiguous/missing attribution, missing roots, symlink and hard-link behavior,
thread-level Shellboy traffic, and table rendering of unknowns. The existing
startup probe now recognizes the same ready indicator in either language; its
sampling method is unchanged. Local macOS `cargo check --locked --tests` and the
focused Python tests passed before the measurement. The measurement is one
fake-model run, not a repeatability study, peak usage or real-provider token-cost
claim. jcode was run only in Linux CI; no real model or paid provider was used.
### #152 Batch4 Watch Discovery Notice Language

The2generated scope-unavailable / bounded-search continuation notices follow
the loaded UI language. Explicit Ja retains both original sentences; default
En is English and preserves the PageDown key. No Core/wire error, stored title,
source byte, seen-hash/cursor/watermark/sort key, limit or read sequence changes.
Production changes are confined to frontend/watch_page.rs, outside the current
#149/#150 change sets. Draft-save work overlapping #150 is explicitly deferred.

New focused tests cover actual temp-config/DB read entry for default En and
explicit Ja,0/1/2/10source rows, Unicode saved-title preservation, a129row
witness-cap notice and an En-created cursor consumed under Ja. Exact source
pages/cursors match across locales. Local checks and exact-tip CI results are
reported separately, not inferred as physical-terminal, Windows-local,
OS-clipboard, live-provider or complete Stage C acceptance.
## v0.6.1 Release Preparation

At preparation base `3fe829a965d7511069f93ab28c0cda09e5265689`, the current
schema and migration sources match tag `v0.6.0`: conversation schema 19 and
standalone RAG schema 3. The upgrade-check parser and synthetic fixture accept
0.6.0/schema 19; focused Python tests cover argument defaults. This is a source
comparison, not an executed upgrade test. Recheck the final cutoff and run the
disposable synthetic upgrade test on
the candidate; no candidate, tag, publication or installed-binary change is
claimed by this preparation.

The subsequently selected cutoff `49d68c63896cbd660740793c07eec88927388cf4`
has no differences from `v0.6.0` in the current schema, migration and RAG
schema sources. Candidate upgrade verification remains outstanding.

The revised v0.6.1 cutoff `2b366e32ce70ceb76984a0e53cab2f26451107d5`
includes #116 Stage 4 and #152 batch 4. Schema, migration and RAG schema sources
remain unchanged from `v0.6.0`; candidate upgrade verification is still pending.

## #150 review acceptance / レビュー受け入れ (2026-11-02)

### English

Review baseline: ed93ee18, following landed #149/#158. Review fixes keep the
previously accepted glyph handling, untranslated data, behavior and settings.
Copy notices now select complete Japanese/English sentences for all seven scope
forms, OS-completed versus terminal-request outcomes, and counts 0/1/2/1,000,000;
code-block numbering still saturates. The Japanese visible text is unchanged.
English uses ASCII parentheses and `. Request sent.`, not Japanese punctuation.
Preview notices join complete English sentences as `. ` (without inserting a
Japanese stop), while the Japanese join is unchanged. Failed admission keeps the
raw error but uses ASCII parentheses in English.

The four diagnostics printed to stderr after TUI restoration are English-only:
`Terminal error: ...`, `Shutdown error: ...`, `Runtime adapter failed`, and the
existing full `Forced exit: ...` notice. No Japanese alternatives or locale lookup
remain for those diagnostics. Live startup/Closing screens keep the configured
locale and existing control, cancellation and cleanup ordering.

Both guides quote `Refresh failed; previous captured result` /
`再取得に失敗・以前の取得結果`. For a 12-character full-body OSC52 request, they quote
`OSC52: completion unconfirmed for copying the complete body (12 characters). Request sent.`
and `OSC52: 本文全体のコピー要求を端末へ送りました（12文字・完了未確認）` exactly.
Guide-quote regression checks compare those strings with the actual formatter.
Remaining Japanese state words in fields::state_label / management reports are
not fixed here; they are recorded for #152 in issuecomment-6569021575.

Native macOS verification uses shared rust-slot, locked dependencies, two build
jobs and two test threads, temporary data and mock clipboard only. Fresh check
and strict all-target Clippy pass. Focused results: diagnostics4, mock clipboard1,
startup5, management15, owner-session5 and integration UI86 (one preexisting
ignored). Layout/watchdog checks and Python134 pass. The initial Clippy warning
was a test-only import left in production scope, fixed by moving the import;
no lint/guard/assertion was suppressed. Exact-SHA CI is reported separately.
Windows, physical terminal/IME, real OS clipboard and
provider behavior remain unverified; no release/install/main-push operation.

### 日本語

ed93ee18 のレビュー差し戻しに対応しました。受け入れ済みの記号処理、翻訳しない
データ、挙動、設定は変更しません。コピー通知は7種類の範囲、OSコピー完了と端末への
要求送信、文字数0・1・2・1,000,000ごとに完成した文を選びます。コード番号の飽和処理と
日本語の表示文はそのままです。英語の括弧と句読点だけをASCIIにし、previewの英語は
完成した文を `. ` の形で連結します。日本語の連結と原文エラーは変更しません。

TUI復帰後のstderr診断4種類は英語固定です。日本語の候補や設定言語の参照は追加せず、
稼働中の起動・失敗・停止・Closing画面の言語と終了順序は保持します。両ガイドは実際の
「再取得に失敗・以前の取得結果」と `Refresh failed; previous captured result`、および
本文全体12文字を対象とするOSC52の完成文をそのまま引用し、formatterとの一致を試験します。
状態語とmanagement reportの日本語残存は今回変更せず、指定どおり#152へコメント済みです。

検証環境はnative macOS、共有rust-slot、locked依存、build/test各2並列、一時DBとmock
clipboardです。新しいcheckと全targetのstrict Clippyは成功しました。重点試験はdiagnostics4、
mock clipboard1、startup5、management15、owner-session5、integration UI86が成功し、
既存のignoredは1件です。layout/watchdogとPython134も成功しました。初回Clippyで検出された
試験専用importはtest moduleへ移動し、lint・guard・assertionは抑制していません。
確定SHAのCI結果は別に報告します。Windows・実端末/IME・OS clipboard・実providerは未検証で、
ユーザーデータ、インストール済みバイナリ、リリース、mainへのpushは操作していません。

### #150 restack after v0.6.1 prep main landing (2026-11-02)

The user approved the five review corrections at 027e455e. After release prep
45c34c75e266c5bfde3d28e07a303bdea93e137a landed with CI41396397531 success,
all fourteen #150 commits were rebased onto that main. The two management.rs
conflicts were resolved by retaining both language plumbing and #159's raw filter
projection before safe_detail_value, plus both sides' complete regression modules.
The other twelve patches are range-diff equal; the two changed patches reflect
existing main plumbing/context and the union of both test modules, not a new
feature or retry-policy choice. Existing #159 prepare_in/report_in routing stays
in place. The five reviewed corrections, exact Japanese fixtures, post-TUI
English-only stderr and raw/provider/copy boundaries remain unchanged.

Record-delta against both the landed main and original 027e455e reports zero
removed status, acceptance or decision lines. Existing release cutoff/history,
#159/#160 and #150 records are retained, with fresh evidence appended here.
Fresh native macOS shared-slot checks used locked dependencies, build/test
threads2 and refreshed Rust source mtimes: fmt --check, check --tests and
all-target Clippy -D warnings succeeded. Diagnostics4, raw/mock clipboard1,
startup5, management18, content-filter unit10 and mock integration2 passed with
nonzero observed counts. Layout102/watchdog passed. Python146 passed; intentional
failure-gate fixture diagnostics are expected within that successful suite.

No settings, dependencies, storage/schema/protocol, queue/cancellation ordering,
retry admission, normalization, byte limits, raw/provider values, glyphs or
existing assertion/guard were changed by restack. New exact task CI is pending;
its SHA and outcome will be reported in the formal handoff. New-candidate Windows,
physical terminal/IME, real providers/Azure and OS clipboard remain unverified.
No main push, release, installed-binary or user-data operation is performed here.

### #157 restack after #150 (2026-11-02)

Native macOS shared-slot `cargo check --tests` passed. Focused library tests
passed: text-width 4, startup 5, frontend diagnostics 2, command diagnostics 1,
observation notice 3 and management 17. Width-aware diagnostics preserve raw
arguments and localization; task CI on the restack and Windows integration CI
remain to be verified. No physical terminal or OS clipboard was tested here.
A focused #158 UTF-8 unknown-extension attachment integration test also passed
(1 test). A first run of the new diagnostics regression selected zero tests
because the shared target had another worktree's artifact; it is being rerun
after refreshing the crate source mtime under the slot.
The fresh-mtime rerun passed `cargo check --tests`, `cargo fmt --all -- --check`
and the new localized-diagnostics regression (1 test), all in one slot.

### #152 Audit Scope And Consistency Check

The scoped lexical audit is expected to reject same-function predicate changes,
stale IDs and malformed decisions while allowing sibling item edits and remaining
(c) records. The selected item includes recognized attached attributes, with a
whole-file fallback when no item is recognized. Scope is not semantic analysis;
external callers/macros/configuration, enclosing module attributes and duplicate
ordinal shifts require manual review. All 25 existing Message::render decisions
were re-reviewed on main7f5f8897 after #157 and remain (a), before migration.
CI runs consistency without `--check`; this does not establish language
completeness or final Stage C, physical terminal, live provider or clipboard
acceptance. Local tests and exact-tip CI results are reported separately.
### #152 Batch5 Deletion Cleanup Storage Warning

The generated storage-failure wrapper returned after confirmed deletion follows
the loaded UI language; explicit Ja is byte-identical to the old sentence.
Underlying ApiError display remains opaque and code/hint/details are unchanged.
The same SaveFailed predicate controls wrapping; non-storage errors pass through.
The existing DeletedWithWarning UI status already localizes its outer sentence.
Only frontend/thread_deletion.rs production changes; #150/#157 remain untouched.

Local fresh-library macOS fmt/check/focused Clippy and10deletion tests passed;
145Python tooling tests (13audit fixtures) passed. Regressions cover exact both-
language messages, verbatim Unicode source diagnostics, absent/present hints,
classification-independent source words, real cleanup failure after Core commit,
both configured UI locales/final status, admission closure and SaveFailed
shutdown, plus existing descendant/stale-autosave/bounded-cleanup cases.
Exact branch CI and later3OS integration are separate qualifications. No physical
terminal, provider/clipboard, full-screen localization or final Stage C completion
is inferred. After0.6.1 cutoff; no release tag/binary change is part of this batch.
### #152 Batch6 Draft Worker Stopped Notice

The generated stopped-worker notice delivered through DraftSaveFailed follows
startup UI language; explicit Ja is byte-identical. Only frontend/draft_saver.rs
changes in production. #150's rejection messages and their recovery guidance are
not changed. Snapshot/user text, cursor/anchor, coalescing, queue capacity,
reserved update timing/order, once-per-Thread warnings, persistence and shutdown
outcome remain unchanged. No root/composer/#157 or shared-presentation change.

Fresh first-pass macOS fmt/check/focused Clippy and5 draft_saver tests passed.
The extended regression adds default/explicit En and Ja config,0/1/2/10 drafts,
critical-outbox saturation and ordered release, unchanged source bytes, IDs,
revision/cursor/anchor and active UI text, no phantom DB save, and failed join.
Final extended-test result and exact-tip CI are separate evidence. Python tooling
ran146 tests including13 audit fixtures; no final Stage C, provider/clipboard or
physical-terminal acceptance is implied.
### #163 Unix IPC Namespace Failure And Fallback

Synthetic Unix fixtures exercise private base validation, counted and bounded
reasons, redaction, path length, permission denial and data-directory fallback.
The fallback test injects rejected mkdir calls for all three temporary kinds,
then uses the real listener and endpoint.json. Unix CI additionally reruns the
same synthetic denial case with a freshly built CLI process, with socket override
variables removed; this is not a real sandbox policy or physical terminal test.
Stale cleanup, owner/mode/symlink protections and existing cleanup behavior
remain required. Windows named-pipe behavior is outside the changed path.
Exact commands, platform and CI results are reported in the task handoff; no
live-provider or actual sandbox-exec observation is inferred.

### #169 Persisted Index Pause Footer (Pending)

On macOS, the pre-rebase tree on main 89dd221d passed `cargo check --tests`,
four persisted-pause tests (including real retry and an oversized marker), the
bilingual footer test, a draft update test, and an actual DB-to-selected-snapshot
test. The later width-policy and PTY changes, plus the rebase onto 72f163d5,
require a fresh check. The first post-rebase `cargo check --tests` failed on an
out-of-scope PTY binary variable introduced by the test change; the helper was
corrected. The subsequent fresh run was interrupted by signal 15 while waiting
for the shared Rust slot; it did not execute and must be restarted. The work is
preserved on the task branch, rebased onto 99d10a78, while #165 takes priority. Before handoff, verify saved
family/profile selection, clear/retry, bounded fixed classifications, 80/120
columns in both languages, snapshot updates, and the no-selected-thread case.
Fresh exact-tip task CI is required. No physical terminal, live-provider,
Windows-local, or post-integration behavior has been verified.
