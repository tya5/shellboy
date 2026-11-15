# Development status

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
