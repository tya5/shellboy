# Development status

## Core #381 history reads (items 7 and 8)

Window sources and execution candidates are bounded independently of response size. Exact saved JSON audit bodies read byte ranges; decoded and parts projections retain their full-rebuild fallback. Prune budget exhaustion withholds the body without a tombstone or RPC failure. No migration or stored projection.

## Core #379 CLI and guide audit fixes

Shell waits resolve the owning Thread like `shell get`, including service executions without a Run. The CLI help and guides now distinguish direct self-waits from dependency cycles, live config adoption from restart-only changes, owner RPC selection from startup config, and Thread wait cursors from shell read cursors. They also explain rotation recovery and quoting dotted connection names. No schema or protocol number change.

## Core v0.15.0 release preparation

The version-only candidate on Core main source `a19b80c` sets the package to
`0.15.0` without changing features, protocol, or schema. The release is not
published by this commit: tag only the landed exact commit after the release
gates pass. The `0.16.0-dev` bump belongs after that tag.

Core [v0.14.0](https://github.com/tya5/shellboy/releases/tag/v0.14.0) was published on 2026-11-23 from tag commit `b644b3c`; see the [release notes](release-v0.14.0-draft.md).
- #377: skip unreadable common instruction files with typed Run warnings; hash-checked config edits reconcile stale live values without rewriting unchanged files.

## Core #370 asking a person versus asking a Thread

A model ask addressed to its own Thread is refused with `invalid_argument`
and a hint to omit `--thread` when asking a person. Ordinary sends and named
questions keep their routing. CLI help and the Agent Guide explain the distinction.
## Core #376 CI check speed

The standard checks run the same feature-only DB observation test in a small
external harness instead of linking the full integration suite again. The
collected-source query test still adds 10,000 unrelated relations to the real
schema and checks the same VM-step bound and index; its seed uses bounded tool
groups to avoid quadratic recovery-trigger work. No tests were skipped and no timeouts changed.
The four independent live PTY scenarios are separately scheduled by libtest
with the existing thread budget. The stream test waits for an opted-in reader
to observe the reasoning delta before asserting that the opt-out view has no
change, removing a fixed 30-second negative wait. No new scheduler or shared
database, and no watchdog or platform gate changes.
Hosted before/after timing awaits the next green all-OS landing run.


Hosted Unix checks reuse the real CLI built by the normal test suite for
fallback-namespace verification before running the separate DB-observation
feature build. Hosted timing savings are not yet verified.

## Core #309 pending message tests

Cancellation ID validation and the send request's default delivery mode are
now tested at the protocol boundary. The cancellation test still checks
outcomes, replay, and archived references without probing the history GC
predicate directly. The retired-copy test still checks space reclaimed and
exact replay after admission, without asserting the database schema version.

## Core #309 preparation tests

The settled-completion test now runs in `state::tests` against the database
schema directly, without an owner, Control, or HTTP. The stop tests check the
saved receipt reason and that concurrent stops start no model attempt, rather
than asserting write order or generation counts. The shell-background and
connection-reload tests retain their behavior checks without a scratch-file
assertion or a pointer-identity check.

## Core #309 headless shutdown witnesses

The headless signal test checks that repeated TERM during a Ctrl-C-initiated
drain still allows a clean exit and reopening. Dedicated forced-owner-death
tests cover shutdown crash boundaries; headless startup interruption remains
a separate witness.

## Core #356 per-version protocol files

Bundle `bundle/core-wire-a` moves the per-version sections of
`docs/design/protocol.md` and `events-and-external-protocol.md` (protocols
5–15; 9 is a pointer to its #346 note) verbatim into
`docs/design/protocol-versions/N.md`; `protocol.md` keeps one pointer line. A
wire patch now adds `protocol-versions/next-<issue>.md`, and
`scripts/land-protocol.py` merges those into `N.md`, deletes them and rewrites
links. Links to the version title target the file, so a later batch addition
does not break them. Stray in-file headings are numbered in place. Rule:
[setup](setup.md#protocol-number-at-landing-356). Tests:
`scripts/tests/test_land_protocol.py`.

## Core #309 compaction fault tests

Removed duplicate cancellation and deletion witnesses and retired-write assertions. The existing fault matrix covers all four cancellation stages; production code and shared fixtures are unchanged.

## Core #309 compaction failure tests

The former runtime new-Run failure-memory witness is superseded by the direct
compaction test with real persisted Run and Turn boundaries. Existing provider
classification, retry, and same-scope suppression witnesses remain; older
validation records naming the removed runtime test are historical.

## Core #359 migration numbers at landing

Branch `task/core359-land-migrations` (on #356) extends
`scripts/land-protocol.py`: `next_<slug>` migrations are numbered main + 1, + 2, …
in batch commit order in the same commit as the protocol bump. The table-rebuild
list (39, 51, 52, 53) moved from `storage::migrate` and
`check-release-upgrade.py` into a first-line marker in those four SQL files;
the release-upgrade choices, the error catalogue's supported range and two
script tests now derive the current schema instead of naming it. Rule:
[setup](setup.md#migration-numbers-at-landing-359). Tests:
`scripts/tests/test_land_migrations.py`. Next: the integration owner uses it on
the first batch with two migrations and records the result.

## Core #356 protocol number at landing

Branch `task/core356-land-protocol` adds `scripts/land-protocol.py` and its
synthetic-repository tests (`scripts/tests/test_land_protocol.py`: no wire
change, schema definition change, `Protocol next (#12)` heading only, second
run, version two ahead). The rule is in
[setup](setup.md#protocol-number-at-landing-356). Mentions inside a code span
and the docs' own rule text are not rewritten; `#protocol-12-` anchors are.
No Rust source changed. Next: the integration owner runs it on the first batch
with a wire change and records the result.

## Core #348 Thread-owned services

Services share the timed-schedule table, maintenance dispatch, and shell cleanup. The owner preserves enabled services across shutdown and startup, but does not restart them after their own exit. The collector rotates output for Thread-owned executions only. See [Thread-owned shell definitions](../design/thread-owned-shells.md).

## Core #324 deterministic Ubuntu integration witnesses

The candidate branch `task/core324-nondeterministic-tests` is based directly on
`origin/main` at `116e82e5965dbc2c736849659f9f48db21788873`. The corrected
candidate `44a072334dcf3a44cc3d135c3d07bcc8eba693b7` passed normal Core CI run
`44895968872`; the 23bd095a SHA is a historical RED (run `44894914451`), not current.
The refused-endpoint
test leaves its loopback port closed for the complete finite retry budget and
checks the initial attempt plus all eight configured retries, all with zero body
bytes sent. It makes no later-success claim. The proactive fixture documents
that proxy arrival orders only against the committed attempt admission, not later
attempt state; report capture selects the k-th committed `model_attempt` row for
an accepted Turn by the test-owned Ordinary-request ordinal. The Driver awaits
one request attempt through response handling before admitting the next; the
proxy body carries no attempt ID. Summary absence uses the persisted per-Thread
Summary-usage `requests` ledger; `UsageLedger::begin` commits it before the Summary
POST (`src/compaction/usage.rs:151-160`, `src/compaction/single_request.rs:599-602`),
with a comment that these witnesses move to the unified #328 request pipeline. The U2 Stop test records the exact Run's model-attempt
count at RunStop acknowledgement and compares it after that Run settles; `control/driver/request.rs:116-135` awaits durable attempt admission
(the `model_attempt` insert at line 408) before `control/driver.rs:390-399`
starts the HTTP request. The test therefore uses the existing DB admission record,
not mock-proxy parsing order, as the Stop witness. The retry matrix still retains
the non-strict root-`response` metadata 500 as fail-closed; the #315 strict
one-key terminal retry-positive test remains separate. No product source,
protocol, state, table or dependency changed.

The resumed U2 request is selected by its unique input text after `recv`; unrelated
already-admitted inbox entries are dropped. Its report evidence is bound to the
Turn returned by that exact `send_accepted`. On the current candidate, pinned
formatting and `cargo check --tests --locked` passed. All three registered focused
selectors passed individually: refused finite-budget exhaustion, typed/nontransient
terminal retry refusal, U2 stopped-spill adoption, plus the later failing-Run identity
case and the affected proactive ordinal fixtures. Pinned formatting, test compilation,
all-target Clippy and `python3 scripts/check-test-layout.py` pass on current local
source. Exact-SHA CI run `44884264205` is RED on parent `abc5756f7fb5a387c9c4a51082fd4e53666a0f0e`
in `legal_new_run_reopens_provider_failure_memory`; its bad independent-Run equality
was replaced locally with record/request-owned identity. The whole-file audit was pushed as `b448df143a439882465e417d660fa05f070a3c87`.
Exact-SHA CI run `44890427963` was RED (Ubuntu: 1411 passed, 8 failed, 18 ignored):
four scope-summary assertions omitted one later explicit Compact Summary from the
Thread ledger total; the bilingual content-filter count used the wrong loop count;
the idle agents-reference query selected a prior Run-targeted event; and one
companion context helper still passed the Thread ID instead of the accepted Turn.
These were corrected locally, and all eight reported selectors now pass focused
re-runs. Exact-SHA CI run `44892130126` passed on `a8d97b5843746f6cd18708222776b3364e0458ca`.
Reviewer round 3 traced the intermittent 3-attempt versus 1-attempt result but did
not establish a concrete cause. The proposed HTTP chunk-boundary explanation was
retracted: complete SSE frames are buffered and terminal classification should not
vary with Body chunking. The chunking fixture change is reverted; the retry test keeps
its original assertion/verdict. Its assertion message now includes bounded per-attempt
`model_attempt` state, saved classification fields from `error_json`, and HTTP
`rate_observation` facts for #329 investigation. The whole-file audit remains intact.
The attempt diagnostics and restored fixture framing passed focused validation. The
historical 23bd095a RED (run `44894914451`) came from a shared Fixture helper comparing
all HTTP request-log rows against attempts for only one Turn; the corrected 44a07233
source restored the helper and passed CI run `44895968872`. Reviewer follow-up now
removes nonpersisted evidence fields, bounds diagnostics by whole attempt records,
restores the original proxy assertion, and corrects this status. The full integration
suite was not run locally.


## Core #325 CLI unique-prefix IDs

The shared CLI target-ID resolver, owner `id.resolve` observation and reply-not-found pending-duty details are implemented from landed base `7aa240f7386f9186ed0a0aeace9892b2cb9390d7`. The resolver covers Thread/message/Run/shell execution fields (including `parent_id` and recovery `run_id` wire aliases); full IDs stay exact, nonempty prefixes are 8–35 characters, and empty strings pass through as command sentinels rather than IDs. Wire target commands receive resolved full IDs only. Owner resolution reads at most nine raw ordered rows from existing lookup indexes before visibility filtering; a full nine-row sample is conservatively `prefix_too_short`, never a uniqueness result. It adds no database schema/index, dependency or persisted state. Protocol remains v6; `Welcome` advertises `id.resolve`, and the CLI checks it before sending a prefix query. Reply not-found details reuse the bounded pending-reply read scoped to the authenticated caller Thread; only `message reply` prefix not-found includes these candidates. Offline `thread request` inspection remains full-ID-only.

Pinned fmt, `cargo check --tests --locked`, Clippy with `-D warnings`, test-layout and focused resolver/CLI/IPC/reply/protocol-schema selectors passed locally. Exact-SHA run `44905626185` on `c67c646ac0f0e25533a6b6de75ad3b22dca47141` was RED (1423 passed, 4 failed, 18 ignored). The two ratchets and protocol expectation were updated for the new read-only command/capability; the missing Thread CLI expectation now reflects `not_found`; the tool-repair fixture now supplies full IDs at CLI boundaries. Those selectors pass locally. The compact-list selector, which failed once in that full CI run, passed four isolated repetitions locally; no cause was established. Exact-SHA run `44907322580` on `2f4db9ef50f3887861f9a2d2826b53f2a91a5062` was RED only in unchanged `runtime::background_read_keeps_target_raw_separate_from_message_send_and_observer_output` (`tests/runtime.rs:334`; 1426 passed, 1 failed, 18 ignored). This failure is tracked as #330; it is unchanged from the landed base/path and is deferred without a rerun-as-fix or diagnosis. The current small follow-up implements Claude's nine-raw-row truncation rule, reply-prefix duty candidates and offline inspection exclusion; its exact-SHA CI is required. The complete integration suite was not run locally.

## #315 Retry after partial output (owner-policy successor)

The latest owner decision removes `response_retry_partial` and permits strict
untyped Responses SSE terminal retries after output for conversations. The
previous local/CI candidate is historical, not validation of this successor.
Independent source review and separately admitted validation are the next gates.

## Core U2 proactive Driver/Stop causal repair (complete qualification pending)

The clean source delta is six files on parent `49f6c8fdeae90205808f14f7e816fba56a679601`; immutable final patch SHA-256 `f353a7ab28fccde07d0e60fcca43c363c3b6345b1d171c76d724cc1868fc9235` is in `.local/private-reports/studio-current-20261113/closed-stop-uninstrumented-final-clean-20261118/`. The independent bounded source closure is `.local/leader/core-u2-closed-stop-2915ecb-causal-repair-review-20261118.md` (SHA-256 `533d6873e0052714fd9d71f8f0c08accc0559093dcf5ceb48200770cfc7bfad0`); its prior review variants and the instrumented correlated trace remain separate immutable evidence.

The repair serializes live-Run selection with registry publication, retains Thread-owned job join identity across replacement slots, removes slots only when inactive/not joining/job-free, and rechecks exact Run ownership at no-driver Stop's Thread-boundary admission. Exact same-Run active Drivers own their Stop; retained inactive jobs join only after releasing the boundary; different replacement Runs are not cancelled by the old Run. Natural completed Runs retain background work. No new worker, table, public protocol, or production cancellation policy was added. The original integrated contract remains; the invalid direct/manual-FINISH diagnostic fixture is retired from source.

**Clean-source evidence.** On uninstrumented bytes, pinned `fmt --check` and `cargo check --locked --tests` passed in execution `6affffc3-677f-416e-bfb6-2bb276511190` (exit 0, cleanup confirmed). The exact original registered integration selector `runtime_features::proactive_closed_spill_then_cancelled_summary_preserves_adoption` passed in execution `4e022876-0da4-49d0-8a76-4b992d3ddabf` (exit 0, cleanup confirmed; 1 passed, 0 failed, 1432 filtered; 1.59s; max owned RSS 2499.36 MiB/footprint 2706.76 MiB under 8192 MiB). The exact output log SHA-256 is `2e424b13964a2df757a39849d793e168800b6bd7bd4e88d21bc583a5dc8f46b9`. It records report Turn `93b6d31a-b73d-4a21-acd7-05aba8cc1a72`, attempt `3c41e219-0047-4d44-9ea1-b189681d2da2`, GC token `f9181070-0678-4a03-9120-a49868ce2a12`, terminal cancelled/nonprovisional row with matching token, and trigger Run `3e7b2e29-60f3-468e-ad27-b9cdf9763023` stopped. This clean-selector evidence is distinct from the earlier instrumented trace receipt.

Full normal/test suite, DB-observation selector, strict all-target Clippy, complete-unit source review and platform CI are not yet qualified on the final committed SHA. Earlier REDs, the `06c`/`7f4` interruptions, historical misses and unrelated U2 STOP remain separate; no push, release or incident-wide acceptance is recorded.


## CORE301 current-main GC port candidate (not main state)

The isolated worktree `task/core301-single-request-port-63724` is based on
`c0c1d82767f061c60c2f4d46673a3cfa30560145`. C8 spill-custody source is
`a5f9b0afd6c02ef0e5314a65720c110d2df29430`; the C3 paired pre-FINISH rejection /
postcommit storage acceptance cell is `08f2602019ee313203ebba86c27319dd49af0c71`,
with formatter follow-up `86e3631ba290dba1a0b29d1708be984f0a597b6c`. The initial C3
bounded review in `.local/leader/core301-2cc731-fault-pair-bounded-review.md` (SHA-256
`db20f044921f7d5a58be9cdf8581c094eddadefa82cc7094fc322919b2bfb4a4`) was later
corrected by `.local/leader/core301-c3-provenance-correction.md` (SHA-256
`bedbfd7aee62165ce29ce792cd8dd2d4e46e62ffbe4b700dd10692863f8a41d6`), which found
failure-path loss of the captured trigger's run/boundary provenance. The corrective
bounded review is now closed on exact source commit `143953a95001484881703ea291c2bdd572c5e0e5`
and complete SHA `7749ff01173da4de0145a231ecd7fd87a373c9fb`; closure artifact
`.local/leader/core301-7749-captured-trigger-provenance-closure.md` has SHA-256
`8b2bdd5cabea054d3853d1e2e9ab5cad444ab4c5e63ca5869bac20e07844a0e7`, with no new
scoped P1/P2. Its finite scope limits remain explicit in that artifact.

C3 runs the true SQL-triggered `Fault::RejectFact` inside adoption (pre-FINISH
rollback) and matched `Fault::Storage` at its postcommit checkpoint for the same
`ActiveSummary` scenario. Both go through the shared path/epoch/source/restart oracle;
the test compares the exact five-field immutable trigger identity, not unrelated
per-attempt `run_id` / boundary metadata. It requires failed/no active-summary stage
fact versus adopted/committed stage fact. The duplicate ActiveSummary cases were
removed only from the two separate fault loops; other scenarios and unique cancellation,
storage, remeasurement, persistent-write, retired-write, `RejectFinal`, routing and
restart controls remain.

**Qualification receipts.** C8 full `sh scripts/check.sh` passed on
`618a2af8dcfbd846d453bf105839f458421e1328` (execution
`03601035-cd39-40d9-9529-835a4e8a2ffe`, exit 0, cleanup confirmed); it predates C3
and does not qualify the C3 candidate. On `dbae793745428dad57eb37d7ed3942c97427dd57`,
layout and Python checks (228 tests), formatting and strict all-target Clippy passed;
the full all-target Rust test executable exited 101 after 2,647 passed, 1 failed and
25 ignored. The sole failure was the new C3 cell's whole-trigger comparison. Source
review then established the reason was not harmless per-attempt variation: W0 captures
`run_id` and optional `a_boundary`, but the pre-FINISH terminal path reconstructed the
trigger from five numeric locals and `attempt::finish_tx` replaced the provisional
record, dropping those captured provenance fields. The independent correction is
`.local/leader/core301-c3-provenance-correction.md` (SHA-256
`bedbfd7aee62165ce29ce792cd8dd2d4e46e62ffbe4b700dd10692863f8a41d6`). The original
red receipt remains `core301-standard-check-dbae7937-63724.log` / `-result.json`;
execution `93b82444-1ad6-457b-ab23-c3ecc0173924`, exit 101, cleanup confirmed.

The follow-up complete `sh scripts/check.sh` on `abf339fe90b28b8a75d534b51e9c2c7d8dd5b2e5`
passed (execution `d4c44659-b586-4ba7-8310-e25092cbbf54`, exit 0, cleanup confirmed):
layout, Python 228, fmt, strict all-target Clippy, library 2,648/0/25 ignored,
integration 1,415/0/19 ignored, other targets 14/0/1 ignored and 9/0/2 ignored,
and exact `test-db-observation` selector 1/1. However, abf used only cross-run numeric
trigger projection and did not assert or preserve each attempt's own run/boundary
provenance; it is NOT qualification for the now-confirmed P2. Its log/result/resources
are `core301-standard-check-abf339fe-63724.*`.

The repair makes `begin_tx` return its exact captured trigger (including `run_id` and
observed `a_boundary`) and uses that same value for terminal W4 persistence. No fallback
JSON merge or current-state boundary reread is used. The paired oracle keeps the five
shared numeric fields for cross-run equality, checks each record's `run_id` against its
own model-attempt turn and its observed boundary shape, and asserts distinct run IDs;
an attempt-level unit checks exact captured-trigger survival through terminal finish.
The independent review notes that boundary type/shape at integration is not itself an
independent original-token comparison; exact-token equality is checked at the writer
unit and supported by one-value source dataflow. The current complete SHA
`7749ff01173da4de0145a231ecd7fd87a373c9fb` passed guarded `sh scripts/check.sh`:
execution `a643b931-bc15-42b2-ac31-9bbbc051e7df`, exit 0, cleanup confirmed after
1,008.298s. It ran layout, Python 228, fmt, strict all-target Clippy, library
2,649 passed / 0 failed / 25 ignored, integration 1,415 / 0 / 19 ignored, additional
targets 14 / 0 / 1 ignored and 9 / 0 / 2 ignored, and the exact
`test-db-observation` selector 1/1. Owned peak RSS 2,375.25 MiB / footprint
4,540.30 MiB under the 8,192 MiB limit; no stop by swap. See
`core301-standard-check-7749ff01-63724.log`, `-result.json`, and `-resources.tsv`.
Three-OS CI and main integration remain pending. Earlier C7.1, C8, dbae red, and
abf green/nonqual receipts remain scoped to their exact SHAs; all original reds remain
visible.

C6-3 selected-source facts authority uses the captured `GcSourceFacts` and checks
group-completeness agreement; C6-2 sequencing retains active spill custody when later
summary work fails and permits active spill only after closed-summary `NoTarget`.
HTTP source and migrations remain unchanged from c0. #306 saved-attempt provenance is
design-only in `.local/private-reports/studio-current-20261113/
core306-saved-attempt-provenance-design-20261115.md`; no #306 implementation is in
this worktree.

The former 21:45 native/review and 23:00 main goals and revised 22:00 freeze checkpoint
were missed. The revised 23:15 one-OS standard/review checkpoint is met at 22:53 JST on
`7749ff`; the 00:30 same-SHA three-OS CI/main target remains pending and at risk. No
push, CI dispatch or landing is recorded yet in this status snapshot. Time does not
waive the remaining gates or authorize unrelated installed-binary, DB-migration or
owner-restart actions. Do not infer final readiness from prior candidate receipts.

## Core #306 saved-attempt provenance producer (in progress; separate from #301)

The sole #306 worktree is `shellboy-worktrees/core306-saved-attempt-provenance-63724`,
branch `issue-306-saved-attempt-provenance-63724`, based on landed main
`031c034edc970f37c02f031c81989b931fbf506d`. Contract commit
`06bf1ae9ef07ab2eddd599c25dfb004975a2798e`; producer/DTO/test/schema commit
`576c76f18031d71c2f89af41ed62cf18b8a74af4`; status/acceptance commit
`d4ddcdcf9408d270535942c22cf716069d0e62a0`. Producer is optional and history-node
only; no schema/index/writer/migration/cache/UI-local state change, and canonical
`(node_id,item_identity)` remains unchanged. Consumer contract was sent to existing
Studio owner f797 (message `9b504772-1834-4d63-8690-fdb6e0e290f8`); no Studio source
was edited.

**Review.** Bounded source review accepted exact d4dd/source 576c with no scoped
P1/P2. Artifact `.local/leader/core306-d4dd-saved-attempt-source-review.md`, SHA-256
`148cc0b20a0877789950d622839ba92381ae99ea91d47bb7f19bfde111063a4c`. It explicitly
limits the query guarantee: index range for one selected node and <=36-byte Rust
scalar result, but matching SQLite row count and temporary DISTINCT storage are not
fixed. This is not a universal CPU/OOM/performance bound or Studio end-to-end
consumer/retirement proof.

**Qualification.** Five exact producer/DTO/schema controls and guarded
`cargo check --locked --tests` passed; receipts are in the private ledger. However,
first full guarded `sh scripts/check.sh` on d4dd was RED (execution
`8196dc7d-4992-410a-8068-f1c3d3268d7f`, exit 101, cleanup confirmed): library
2,652 passed/25 ignored; integration 1,415 passed/1 failed/19 ignored. Sole failure
was existing `history_metadata_links_real_executions_and_preserves_fork_origin`
fixture's three matching `'attempt'` literals, not valid UUIDs. Strict production
validation correctly returns InvalidState. A narrow test-only repair changes those
three references to fixed canonical UUID `00000000-0000-4000-8000-000000000001`; no
source behavior or assertion is weakened. The feature selector was skipped after
the integration failure. The new fixture-fix SHA is not yet committed or
qualified; a fresh full `sh scripts/check.sh` must run first on its final clean tip.
Three-OS CI and main landing remain pending. No installed Core/DB, restart,
deployment/release operation is in scope or performed.

Hybrid knowledge searches were degraded because semantic indexes are incomplete;
project/global keyword fallback had no matching records. No re-index/embedding call
was made. Read-only design source:
`.local/private-reports/studio-current-20261113/core306-saved-attempt-provenance-design-20261115.md`
(SHA-256 `e4e941a4d459c492706c588e27a9a426d9d57ed84d528b7f76aa5d2509649c5e`).

## v0.11.0 preparation


Release preparation updates the package version and synthetic upgrade checker from public schema 32 to 35. Publication, tagging, exact-candidate upgrade and installation remain separate reviewer-controlled gates. See [draft notes](release-v0.11.0-draft.md).

## Model-grouped usage (#272; W8 approved, latest-main revalidation)

## Main

The current main implements the adopted reply-expected lifecycle (#194), protocol 4 with strict requests, tolerant result records, capability metadata and a generated schema (#244/#275), change-hint waits with separately opt-in answer text and reasoning activity (#243/#275), model-grouped and grouped compaction usage (#272), and branch-relative usage with branch provenance (#273). Thread archive/re-parent/tags and the bounded maintenance/recovery work described by the effective spec are also landed. The exact-tip validation for #275 (standard and all-OS CI) remains pending in the current main status record; other environment-specific limits are in [acceptance](../validation/acceptance.md). The design index links adopted contracts; individual designs may include historical proposal material, which is identified in context rather than representing current behavior.

## Branch work

Open pull request #223 (`issue-217/compaction-usage`, head `3c7491264d993441e5f3d35628079fa7f05a4853`) proposes grouped compaction usage totals. Grouped compaction usage is already present on main from `16784db966`; the PR is not treated as product state beyond its reviewed delta. Other local or remote branches are not treated as product state unless an open PR or an explicit current handoff identifies them.

## Not verified by repository CI

CI is not evidence of real-provider behavior, physical-terminal behavior, production-database performance, or release readiness unless a validation record names that environment and exact SHA. Current acceptance limitations and evidence are tracked in [acceptance](../validation/acceptance.md); do not infer these checks from an adopted design or a passing build.

The prior branch-by-branch ledger, including entries added to main after the original snapshot, is retained in [status history](status-history.md); its handoffs and pending-check notes are not current status.

#276 is in progress: the inventory and seven scoped simplification decisions are posted, independent Run admission/
stop/wait/UI paths and schema 38 are implemented incrementally, and focused validation is ongoing. General explicit
Run/Turn observations remain; delegation is expected-reply delivery rather than a supervised live parent Run.
Handoff requires both normal and all-OS CI on the final rebased tip; landing is after v0.11.0.
## #262 bounded human reply duties (verification in progress)

Admission is 16 per recipient; legacy summaries report 17+ with exact=false.
Deletion resolves at most 16 synchronously and continues durably in existing
history-GC batches of 128. Schema38 adds only the human-pending partial index.
The historical status ledger is not reintroduced. Current focused repairs cover
RAG/conversation schema separation, migration fixtures and eventual GC completion.
Exact-tip independent review and normal/all-OS CI remain required; failed older
runs are not success evidence. See acceptance.md for validation scope.

## #284 request-history diagnostics

Implementation uses an isolated public-main base and synthetic fixtures only.
Existing invalid_state reasons distinguish selected active missing outputs,
interrupted missing outputs and invalid/unresolved history. Strict model replay
and read-only snapshot/prefix boundaries are retained. Fresh check and focused
matrix/strict-replay/error-boundary tests pass, as do final format/check/strict
Clippy. Independent review approves product commit 13c7b0923c with five focused
tests independently passing. Full CI and integration with later main fixes
remain pending; no private incident cause is claimed.
## v0.11.1 preparation checkpoint (2026-11-09)

The version-only task starts from `ff18f32640f0b41f042f034f4b1a3936a9abe92d`.
Only #286 outbound assistant replay normalization is in scope; package/lock move
to 0.11.1 without a protocol/schema change. See the
[release preparation and upgrade plan](release-v0.11.1-draft.md).
Exact version-commit native CI, candidate artifacts and public-v0.11.0 upgrade
remain unverified here. This entry is not publication, installed-binary swap or
real-provider/affected-Thread recovery evidence. Existing checkpoints remain historical.

## #278 typed operation results (in progress)

Rebuilding isolated `issue-278/typed-results-rebased` from public protocol-v5 `ff18f32640f0b41f042f034f4b1a3936a9abe92d`. Old dirty WT remains untouched; its patch is backed up privately with verified SHA-256 `43d01e6738e506a0e663eeaa27b4649d9f4de177a63556c3c2586e88d4d5951a`. Types only the four current operations status/shell.list/usage.get/config.get; excludes `src/model/http.rs`, #283 future stdin fields, and stale #279 body-separation assertions. Config/provider/group-key-dependent leaves remain open. `status` models its two existing JSON modes via untagged union. Protocol v5 remains unchanged. Generated schema and initial focused tests passed. A follow-up raw `StatusReply` JSON fallback after `ad0ad5a7` preserves unusual non-object legacy reply values and explicit null-vs-omitted behavior; Exact nested-Option follow-up check/fmt, typed status-union, work_status, schema golden, and full protocol suite pass. S6 old e73 archive count (36 protocol +1 status DB) belongs only to e73, not this null-preserving delta. Current tip is a separate follow-up commit after validation; S6 review and delegated current-main rebase/push/CI remain.

### #296 Saved Item Projection Candidate

The author candidate adds opt-in ordered saved items to history.window under the
adopted Claude1528 contract. Default legacy reads are unchanged. Source/byte guard,
projection, actual producer/schema oracle, privacy and response-budget rewindow
checks are separate focused gates; see the current effective-spec candidate section.
Earlier v2 new-executor/cursor/deadline proposals were rejected and are historical
only. Local red compile failures are not runtime-negative successes. Candidate
source/tests do not establish release adoption, all-OS CI or actual-owner acceptance.
Core0.11.2 publication follows independent exact review and hosted CI; owner updates
are exclusively outside Claude after publication, never author shell operations.

### Per-Thread output limit (branch `feat/thread-max-tokens`, local only)

`thread set --max-tokens N` stores a per-Thread output limit (schema 44, nullable
`thread.max_tokens`, positive integer). It replaces the connection `max_tokens` for
conversation requests in all three APIs, is inherited by child Threads and forks,
and is reported as `max_tokens` / `effective_max_tokens`. Summary requests keep the
connection value. Focused unit/integration tests and the pinned schema-43 upgrade
ran locally on macOS; the full `scripts/check.sh`, all-OS CI and a real provider
were not run. Not pushed.

### v0.13.0 Candidate (per-Thread output limit)

Adds `thread set --max-tokens` (schema 44). See [release notes](release-v0.13.0-draft.md).
Independent review found no correctness bug; its documentation and early-clash
follow-ups are included. Tag and publication follow exact-SHA all-OS CI.

### v0.13.1 Candidate (release-only preparation)

The published `v0.13.0` tag (`c212ecb8e3f56e2f7e8cee0ee49f9e7bd942c3bd`) is an
ancestor of current remote main `3cbc9a1f295dc246d2dab9d3e0a603a32caa1de0`. This
candidate changes only the Cargo package/lock version and release documentation;
protocol remains 5 and schema is 49. See [release notes](release-v0.13.1-draft.md).
No build, test, hosted CI or candidate workflow was run for this preparation.

The intended tag is `v0.13.1`, on the final exact main SHA. Qualification requires
independent review and same-SHA three-OS CI, followed by `.github/workflows/release.yml`
with `release_tag=v0.13.1` and `candidate_ref=<exact main SHA>` for native
build/package, smoke, installer checks and candidate provenance. This candidate
route does not tag or publish. Tag/publication require separate explicit approval.

### v0.12.0 Candidate (#303 single-request compaction)

#303 landed on main at cf9b662d0c (exact-SHA all-OS CI success, two independent
reviews). See [release notes](release-v0.12.0-draft.md). The version bump, candidate
build verification, tag and publication follow; installation on a real data
directory is a separate approved step (schema 42 to 43, backup first).

### v0.11.2 Qualification Candidate

See [candidate notes](release-v0.11.2-draft.md). The combined #296 producer and
isolated two-file guard tree has focused six-producer/two-guard test success and a
check pass. Actual generated schema/fixture export, final artifact freshness,
independent exact review, hosted same-SHA CI and release/owner acceptance remain
separate gates. This entry does not replace historical failures or earlier records.

### #296 Actual Producer And Schema Qualification

The existing public Store.execute read-lane request reproduced nested-BEGIN on
87d266 and passed on corrected bdc852. One pinned single-slot run passed seven
focused tests and standard Draft 2020-12 validation of six actual synthetic results,
including legacy omission, response-budget suffix and rewindow/public route; six
invalid mutations were rejected. The public protocol schema is copied byte-exactly
from that same producer export. Previous compile failures, bare-connection-only
success, rejected schema export and source-routing corrections remain recorded;
they are not silently reclassified as passes. Final exact review, hosted all-OS CI,
publication and actual installed-owner acceptance are still separate gates.


## #299 Responses Readable Reasoning Initial Increment

Owner branch issue-299/readable-reasoning; baseline 45e91c6c. Formal spec ff9bbe
preceded payload implementation. Exposes only exact qualified Responses reasoning
summary/content leaves; all provider opaque remains marker-only. No schema migration,
installed binary change, private DB, live provider or paid request. Focused source
and response-budget/privacy controls are in history/items and history_window_items.
Independent plan condition S6 a824 accepted; implementation review / exact-SHA
all-OS CI are still required. See acceptance.md for actual failed and passed runs.

## Core #298 Bounded Structural Tools

Owner issue-298/structural-tools, baseline05e358. Approved typed saved call name /
call IDs only, closed descriptor/detail availability, shared response byte ledger,
canonical identity/execution authority unchanged. Synthetic producers and protocol
schema golden pass; first spill omission and zero-filter mistakes are retained in
acceptance history. S6 source gate accepted; exact final CI/main remains pending.
No #274 reader rewrite, #297 sender edit, schema40 or installed change.

## Outbound replay metadata boundary (2026-11-13)

The narrow source repair from v0.13.0 main preserves saved summary metadata while
excluding `reasoning_epoch` from outbound canonical input items at the shared
replay-copy boundary. Original REDs reproduced the invalid field; fifteen focused
synthetic tests passed after repair (see the validation record). This is not an
installed-owner change or successful provider recovery; independent review,
full-suite and exact-SHA three-OS CI remain separate gates.

### #301 S1 Terminal Recording (Source Qualification)

A recording-only unit replaces the current generic discard observation with a
single bounded rejected/failed outcome. It retains all fifteen reachable guards,
original two-slot deterministic policy and existing scheduling/retry/adoption.
The adopted wire/privacy/paired-test contract is
[Terminal Compaction Recording](issue-301-s1-recording-contract.md).
Public RED evidence exists before implementation; qualification, independent
exact-source review and this unit's own normal/three-OS CI are separate gates.
No SSE parser/halving/suppression/adoption relaxation/progress UI or installed
Core change belongs to this unit.

### #301 S2a Refusal Halving (Unqualified Candidate)

S1b is landed at `9baa554e2fc0f8a7dfdaf51e7de705835adce638`. The fresh S2a
worktree is based on that exact tip. S2a currently adds complete actual HTTP
400/413/422 and trusted 2xx SSE refusal classification, a bounded weighted
whole-group retry to singleton, auth/quota veto before typed-context handling,
and the new schema-bounded `rejected_at_minimum` outcome. Minimum failures write
the original E through the existing deterministic slot; prior failure categories
and suppression semantics remain unchanged. Existing context halving and weighted
selection remain unchanged;
provider-keyed memory/CAS (S2b) is not implemented.

Synthetic public REDs were observed at the landed-S1b base. Focused S2a public,
status-provenance and veto controls now pass, together with schema/decoder
parity, weighted zero/skew bound, context regression and `cargo check --tests`.
The first exact-SHA review requested changes. Follow-up source now gates numeric
HTTP refusal halving on complete bounded body + provider classification, preserves
terminal trusted SSE evidence, adds interrupted/truncated body negatives, maps an
actual Retry-After 429 quota veto to provider_error with HTTP status, and regenerates
the checked-in protocol schema. Focused controls and artifact parity pass. The full
standard attempt initially caught stale T10 expectations for the newly contracted
SSE halving; after updating those, a repeat reached a slow unrelated test and was
stopped without a standard exit. Exact-source narrow rereview and a successful
standard check remain pending; no hosted CI/landing is authorized yet. S2b remains
separate and unimplemented. No provider, owner or private database operation.

S1a schema-review follow-up is an UNQUALIFIED source-preparation candidate while
local heavy builds are held by the 8192 MiB swap guard. The real schema mismatch
RED is reproduced; shared schema/decoder constraints are authored but await hosted
generation/parity evidence and a later committed artifact. Source preparation is
not standard/all-OS qualification or permission to land the current candidate.

The schema-preparation run on `3ff3dc5d` now supplies verified generated bytes and
one passing real schema/decoder parity test; those exact bytes are adopted in the
follow-up source. This does not qualify the artifact-adoption or integrated tree:
independent delta review and exact-final-source standard/three-OS CI remain open.
## #304 Active Shell Readback Candidate

Candidate source isolates starting/running/stopping candidates with the existing
active partial index before the saved-history scan limit. The public regression
on c212ecb8 plus test-only86e20bb891 reproduced an empty first page after2,205
newer terminal rows. Unfiltered/terminal listing, cursor watermark, saved owner
scope and privacy projection remain unchanged. Scoped/global active query-plan,
terminal-growth and current-state pagination controls accompany the fix.
This is a source candidate, not independently reviewed/CI-qualified main or an
installed owner update. Exact execution evidence is reported separately.


## S6a settings branch (in progress; not main state)

`task/compaction-s6a-settings-6372` is based on `9baa554e2fc0f8a7dfdaf51e7de705835adce638`.
The current branch adds settings/persistence for the S6a contract; it does not
implement or qualify automatic runtime-off routing. That remains gated on S2b
integration and separate review. Baseline public REDs were observed for real CLI
parsing, `Request::from_value`, and synthetic-database `thread.get`; see the
S6a entries in [acceptance](../validation/acceptance.md). Current guarded
`cargo check --tests` did not run: the source-cargo helper selected
`clean-package`, but `cargo clean` refused because shared `target-shared` lacks a
valid `CACHEDIR.TAG`. No target metadata was repaired or bypassed; source build
qualification is pending the approved cache-boundary correction.
### S2a e527 Standard Failure Follow-Up

The review-approved e527 correction passed the isolated indexed-cost test and its occurrence later passed in full standard. Full `scripts/check.sh` on exact e527 nevertheless exited101: 1394 passed, 2 failed, 19 ignored. Both failures were traced to HTTP400 fixtures that changed category under S2's numeric refusal rule: the per-report rejection test now uses an actual HTTP401 veto, and the transient retry test now uses HTTP500. The 400 deterministic trial control remains in the S2a matrix. Focused fully qualified tests have each passed one selected case. This is test-oracle/fixture correction only; no S2 production behavior changed. A new SHA, narrow delta rereview, full standard pass and then hosted CI remain required.

### Raw Thread Override Inheritance Repair

The shared typed raw-override snapshot now feeds both child creation and
fork/rewind branches. It carries target/output overrides along with threshold,
max-token and spill values without resolving NULL defaults. Registered history
and public Store regressions cover inheritance, clearing, parent independence
and transactional rollback. Guarded native validation of the committed source
passed production library check, integration test build and six selected tests
(6 passed, 0 failed, 0 ignored). Exact final documentation-commit validation is
recorded separately in the handoff; no CI, independent approval, landing or
runtime deployment is claimed. The separate long-model identity/conversion work
remains design/RED-fixture preparation outside this unit.

### Canonical Scope Identity Preparation

The successor identity branch carries typed framed identities, current-only v2
facts and forward migration 46 with a startup-only v1 converter. Valid causal
fields are preserved; invalid raw rows stay unchanged. Registered synthetic
conversion/ledger rollback and public long-model admission/completion regressions
are authored. Initial compile caught and corrected a SQLite usize parameter;
initial test build caught an incorrect fixture DTO method. Original failures are
preserved. Focused final-SHA validation and independent implementation review are
pending; no full, CI, installed migration or deployment qualification is claimed.

## Core v2.1 U1 Final-Path Closeout (source preparation; unqualified)

A follow-up source worktree, `task/core-v21-unit-a-closeout-63724`, is prepared from
native-green-but-not-yet-CI-qualified `0ce8bfcd91c0755ee516ef98b09aa886284992ce`.
The frozen/pushed U1 worktree is not edited. The follow-up routes multi-range spill
replacements through `FinalAdoption` against current R, gives restore a current-marker
splice with unchanged source facts and current prefix/suffix preservation, and narrows
manual failure-memory writes to cancellation/owner lifetime plus slot CAS. The former
full-snapshot validator is named for strict explicit edits (trim), not summary adoption;
three never-emitted nonappend/prefix/tail reason variants are removed. The operative v2.1 source contract is in `docs/design/compaction.md`; superseded
design narratives remain recoverable through Git history.

`cargo fmt --all` and `cargo fmt --all -- --check` passed via the pinned worktree toolchain
and shared rust-slot; `git diff --check` passed. A first formatter attempt from the wrong
working directory used the sandbox stable toolchain and failed because its rustfmt
component is absent; it changed nothing. No component was installed. The first guarded `cargo check --tests` attempt on `0de70de1` exited 1 before Cargo because
the nested shell redundantly changed to a relative worktree path; it consumed no build
resources and is not source evidence. The next guarded run on `0de70de1` reached Rust and
exited 101 at E0451 because `FinalAdoption` request fields were private to `summary`; the
exact compiler log/resource receipt is saved at
`.local/private-reports/studio-current-20261113/core-v21-u1-closeout-check-0de70de1-r2.log`
and its adjacent `-result.json` / `-resources.tsv`. The first follow-up compile failure was fixed by making the request fields `pub(super)`.
A guarded check on `a322ee8d` then reached Rust and exited 101 because the exhaustive
`all_canonical_guard_causes` test still referenced the three removed rejection variants;
that stale test-only list is now updated. Exact report:
`.local/private-reports/studio-current-20261113/core-v21-u1-closeout-check-a322ee8d.log`
with adjacent result/resources files. The next source hunk also unifies active spill's
multi-interval R replacements under one current-path reasoning projection, increments
the live projected epoch once for DROP_ALL, and stamps every spill payload with the same
final epoch; the separate caller-side snapshot epoch write is removed. Restore validates
retained-anchor positions in original order before descending-index removal and rejects
reordering with typed `SourceChanged`. The operative docs now state the actual awaited
U1 driver and isolate nonblocking launch/reuse as planned U2. These fixes are authored,
formatted and committed at the current follow-up head; no corrected-SHA compile/focused
suite/fullnormal has completed yet. The separate `0ce8` three-OS workflow is green and
does not transfer to this candidate.


### U1 Closeout Reviewer Corrections and Focused Evidence

Independent review of immutable `0de70de1ff74c40e0b4da34a4bf7b16bdcf28e0f` returned
REVISE with three P2s. Source report `.local/leader/core-v21-u1-closeout-0de-independent-review-20261116.md` has SHA-256
`b18251718d2528e006fc5c2091ca33b0b3011c49acc84601fc4c39fa939d495a`. The three
findings are: active spill's captured `snapshot.epoch + 1` after shared current projection;
restore removal order failing to validate retained positions before reverse deletion; and
operative docs claiming U2 nonblocking/reuse behavior while U1 driver still awaits
`maybe_compact_with_run_stop`. The exact `0de` native failure remains the earlier E0451
followed by stale deleted-enum test references, not a green candidate.

Current follow-up source adds a single batch final-adoption authority for multi-interval
spill. It computes one live current-path projection, selects the full path once, then
performs active DROP_ALL from that projection: all spill payloads share the projected
pre-drop epoch, then the kernel advances the Thread once and clears keep so old signed
reasoning remains non-replayable. Restore now checks retained-anchor order in
`replacement_ids` before any removal and only then removes verified positions descending;
a reorder is a typed `SourceChanged` rejection before path mutation. Current docs describe
the actually awaited U1 driver and explicitly mark nonblocking launch/reuse as planned U2.

Guarded `cargo check --tests` on commit `c622888c050e8264ba61d90fd44f982d7b7f4417`
passed (exit0; log/resource receipt `.local/private-reports/studio-current-20261113/core-v21-u1-closeout-check-c622888c.*`). The next guarded focused group on that SHA ran the
`final_r_adoption_tests` filter: 11 passed, 1 failed in fixture setup with SQLite
`conversation edge cycle` while trying to create the reordered path through `history::select_path`;
the other three filters in the `&&` group were not reached. This is a test-fixture setup
RED, not evidence against the source rejection path. Its log/resource receipt is
`.local/private-reports/studio-current-20261113/core-v21-u1-closeout-focused-c622888c.*`;
cleanup was confirmed and the remaining slot is free. The fixture is being changed to seed
a valid path then directly install the adversarial `thread_path` order, avoiding edge creation.
No corrected-SHA focused result or full standard result is yet claimed.

The corrected focused group on `ac06f3d4` passed the `final_r_adoption_tests` module
12/12, including reordered retained-anchor rejection and the two-interval current-epoch
projection. The next `same_captured_e...` filter passed 1/1. The following existing
`staged_active_spill_drops_all_prior_reasoning_atomically` regression failed because the
first batch implementation stamped spill output with the final incremented epoch, making
its signed reasoning replayable. That contradicts the accepted DROP_ALL effect. The shared
kernel now stamps all batch payloads with the single projected pre-drop epoch, verifies
that `select_path` recomputes the same keep/epoch, then increments the Thread epoch once,
clears keep and leaves payloads stale for replay. This is in the unvalidated working
correction; rerun of the complete focused group is pending. Original `ac06` focused RED
log/resource receipt remains saved at `core-v21-u1-closeout-focused-ac06f3d4.*`.

The first rerun on `5b6455bf` reached the focused tests and passed the entire
`final_r_adoption_tests` module (12/12) plus `same_captured_e_reopens_for_new_real_run_and_turn_scope`
(1/1). The existing active-spill replay control then failed: stamping replacements with
the *new* Thread epoch re-enabled their signed reasoning. The kernel was corrected so all
active-spill replacements retain the single current projected pre-drop epoch; the kernel
then increments the Thread once and clears keep, intentionally making those payloads stale
for replay. This preserves the existing DROP_ALL effect while removing the captured-epoch
caller write. The new multi-spill test asserts all replacements share that projected epoch
and the Thread is exactly one newer. A follow-up compile caught a missing local test
accumulator; fixed in the next working-tree edit. These are distinct compile/test REDs and
remain preserved. The corrected candidate must rerun compile and all four focused groups.

### Corrected Source/Focused Candidate (pending full standard)

After preserving the earlier 0de and a322 RED receipts, current U1 closeout source SHA
`0f4e9f5c24a4feb1f9da3047a58efc53fb5e919b` (parent `5b6455bff018d3c391ba0fa8fb3d7c073525b58c`)
now passes guarded `cargo check --tests` at `c622888c` and the complete guarded focused
group on exact code SHA `0f4e9f5c`: `final_r_adoption_tests` 12/12, manual failure-memory
positive 1/1, staged active-spill DROP_ALL replay 1/1, and canonical public rejection
projection 1/1 (15 selected; 0 failed). Test build emitted the nonfatal linker warning
that `__eh_frame` exceeded compact unwind offset encoding. Guarded focused peak RSS
2385.44 MiB / footprint 4148.91 MiB under 8192 MiB; shell cleanup confirmed; both Rust
slots free.

The shared batch kernel now keeps each active-spill replacement payload at the one
current *pre-drop* projection epoch, validates the keep-set projection through
`select_path`, then advances the Thread exactly once and clears keep in the same
transaction. This intentional one-step payload/Thread epoch difference makes signed
reasoning in spill outputs stale and non-replayable; no captured snapshot epoch is written
by a caller. The two-interval regression verifies all spill payloads share that projection
and current epoch advances once. The retained-anchor regression now installs a valid base
path and directly fixtures the adversarial current `thread_path` permutation (to avoid
tripping conversation-edge cycle guards); it confirms typed rejection without path writes.

`docs/design/compaction.md` now separates U1's actual awaited driver from planned U2; no
nonblocking or natural-Run reuse behavior is claimed as current. Full standard on the
current exact final source SHA, independent closure review, and same-SHA three-OS CI are
still pending. Earlier failed attempts and their logs remain preserved; no result is
transferred from 0ce or any intermediate closeout SHA.

## Core #326 IPC final frames

A connection task owns one correlated final reply, distinguishes clean peer EOF from read errors or extra input, and holds the connection after writing until peer EOF or explicit shutdown. Listener failure does not itself cancel accepted connections. Windows Studio exact-pair qualification is separate from Core checks.

## Core #327 attachment list pages

`attachment.list` now uses a bounded keyset query in `(selected_order,id)` order, returns `Page<AttachmentDetails>`, and performs filesystem path observations only on the page items. The cursor is bound to Thread, selection revision, and a captured ordering watermark; CLI supports `--limit` and `--cursor`. Focused paging/CLI/index-plan tests (3/3), the path-observation count unit test (1/1), and protocol schema snapshot test (1/1) pass locally; hosted CI remains pending.
