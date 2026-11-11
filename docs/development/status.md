# Development status

Snapshot: `origin/main` at `482e7eadb39b0bed4059135842c9b148c162f70d` (2026-11-08). This page describes the checked-out main state, not release certification. The exact base commit and open PR were checked against Git history and GitHub on this date.
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
