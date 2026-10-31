# Development status

## CI observation ordering (#89)

Three intermittent observations are traced to test boundaries: index-context
metadata can commit independently of preview; starting-stop acknowledgment is
not live-handle publication; shutdown diagnostics can be dropped at admission.
Tests now fence registration and observe saved facts/notifications, and use
native/process cleanup instead of requiring log presence. No production shell
or lifeline ordering, timeout, retry or assert-on-durable-result is relaxed.
Exact-source native CI and integration results are tracked in issue #89.

## On-demand owner memory (#80)

CLI status --memory returns existing body/layout/attachment/replay cache accounting
and native process metrics through existing UI delivery and control-file slots.
Unknown values remain null with reasons. No periodic sampler, cap changes or new
work queue/worker is introduced. Synthetic cache-growth/held-writer/on-demand tests
pass; native exact-source checks are tracked in #80. F8 display stays with #41.

## Attachment list file admission review (#79)

Read-only attachment rows now leave the projection closure and its connection
before path metadata/open work starts. File checks share existing control-file
admission with AGENTS/cwd preparation; no additional pool or queue is created.
Targeted attachment tests pass; standard/native validation is recorded in #79.

## Managed clipboard paths (#79)

Managed images use the existing path field in CLI results and current request
metadata. Attachment list observes lifecycle/filesystem availability outside the
writer lane. Permissions, successful-send hashes and reclamation rules are shared
with existing attachment behavior. Targeted synthetic tests pass; exact-source
standard/native checks are tracked in #79. F6 path display is delegated to #41
because its published display branch overlaps; no rendering changes are included.

## README overview (#78)

The user selected candidate C: 89 colors, mean original-channel error about
1.0/255, with no visually discernible difference confirmed by the user. The
621,395-byte PNG is after the README title with alt text; intro/Features remain.
Original and comparison files stay outside Git. Optimization hashes, layout
checks and their limits are recorded in acceptance; exact-source CI is in #78.

## Noncontrolling EOF test observation

Only the noncontrolling-PTY EOF shutdown test now treats best-effort diagnostic
rows as optional. Durable tool/Run/shell state and native process cleanup remain
mandatory. Other termination tests and runtime logging/shutdown are unchanged;
validation and original/same-SHA rerun history are recorded in issue #75.

## Readback follow-up (#75)

Shell snippets also suppress secret-access-key flags, sshpass password arguments,
attached -p values and generic credential-suffix assignments. Usage documentation
states explicitly that boundary-crossing lifetimes belong to their start side.
Targeted synthetic redaction checks pass; final standard/native CI evidence is
recorded in issue #75. No user database or installed runtime is changed.

## Saved readback (#75, unreleased main)

CLI now reads retained compaction observations, latest compaction in thread get,
saved-start usage periods, and cross-conversation shell metadata with bounded
redacted snippets/cursors. Heavy observations share the two-slot presentation
admission on read-only WAL snapshots. Schema 15 adds four derived indexes only;
no new tables/workers/queues. Local standard check passes; exact-source native CI
is tracked in issue #75. Public v0.4.0 and installed binaries remain unchanged.

## #73 review fixes / D-017

Map cache identity is independent of output limits and retains the pre-#73 key.
Independent configured map/reduce budgets default to 128/16, with uncached-map
preflight before HTTP. All summary paths, including automatic stage 2, share
reduce/size rejection and existing recovery behavior. Omission commands honor
user settings. Local checks pass; exact-source native CI is tracked in issue #73.

## #73 bounded explicit summaries

Range summaries now consolidate chronological parts into one bounded text,
with D-015 defaults, per-call overrides and finite shared map/reduce requests.
Oversize/nonshrinking results retain the original path; omission notices favor
bounded source lookup. Config check and guides expose the new settings.
Local loopback and standard checks pass; see issue #73 for exact-source main CI. See the
[validation evidence](../validation/acceptance.md#73-bounded-explicit-summary-qualification).

## v0.4.0 published (#70)

v0.4.0 is public from the approved annotated tag at `dae2ba1` (2026-10-31 JST).
The exact-source ordinary three-OS CI and tag-triggered release workflow pass.
All public assets/checksums, both latest installer scripts, extracted native
startup and temporary public one-line installations pass on three OSes
(Windows PowerShell 5.1/7). The retained notes match the published body.
See [publication evidence](publication.md#v040-publication-2026-10-31-jst)
and the [validation record](../validation/acceptance.md#v040-publication-verification-70).
User installation is separately authorized; owner restart remains a user action.

## #65 owner-version guard and validation-record repair

Exec checks the required final-answer read argument before creating/sending work.
A refused projection is an owner/version error6, not user input error2; saved IDs
and the ordinary history get recovery hint prevent accidental resubmission if
support is lost after execution. Core formats/execution are unchanged.
The complete acceptance history has been restored; detailed qualification and
repair provenance live in the validation record, not copied into this overview.

## #65 exec client: local qualification complete

A thin noninteractive client composes existing Thread/message/wait/answer reads,
with local bounded Schema-subset validation, at most two default corrective
messages, stdout-result/stderr-metadata separation and explicit failure codes.
Default creates a saved Thread; existing active Runs accept queued input. No new
Core execution mechanism/table/worker/queue/dependency or owner auto-start.
A final-only history read projection excludes reasoning without changing ordinary
history text and binds its own cursor scope. Unhandled Schema features reject
before contact. Nine loopback native CLI cases pass locally (paged70KiB UTF-8/stdin,
reasoning exclusion, bounded repairs/history, recovery, non-stopping timeout,
active-Turn tool boundary, missing Thread/owner/input and exact numeric spelling).
Native macOS arm64/Rust1.99.1 RUST_TEST_THREADS=2 full standard check passes
**1713 passed / 0 failed / 32 ignored** before Chat integration, layout91, Python6,
fmt and strict Clippy.
Four Schema unit regressions and final-only/default cursor preservation pass.
After rebase onto Chat sourcee65baaa, exact source6b2f5c9 passes **1719/0/32**,
layout92, Python6, fmt and strict Clippy in the same environment. Only appended
document histories conflicted; both are retained and runtime patches unchanged.
No dependency/feature changes remain. Native main CI is pending.
## #41 D2 live work (local functional qualification)

Source807ac97 adds owned shell/child/wait rows, saved timing and bounded live
output in independently reviewable local commits552e325/39b81cc/9bf7369/51a1eda/
761c232/45b8b15/78511d4. New work, recovery/completion and no-Driver child stops
notify the owning view without adding a model wake for metadata. The accepted
selected projection seals at most3 child dependencies; existing500ms reads and
postcommit output markers drive display updates. No new table/worker/queue,
public operation or migration is introduced.

Final native macOS arm64/Rust1.99.1 standard passes **1,777/0/32**
(library891/integration876/termination7/crash3), layout90/Python6/fmt/strict
Clippy. Actual shell commit/rollback markers, snapshot-clock races, decoder
continuity, stale/hidden output, owned/wait proof and delivery pressure are
covered. Six actual PTYs (80/140 columns ×dark/light/NO_COLOR) verify the shell
row after Turn completion, CLI-driven output updates and removal after stop.
English/Japanese User Guides, TUI guide and built-in help describe the controls.
See [live-work qualification](../validation/conversation-redesign-live-work.md)
for historical failures, the wide-cell harness correction and exact limits.

D is not closed: P2's native ten-minute CPU/draw/DB counters, hidden-output P5,
combined multi-consumer P3 and applicable OS/visual admission gates remain open.
The three-second PTY transport samples and simulated600-second UI-clock test
are explicitly not those native performance results. E ID-summary patches,
F Watch/descendants and G catalogue/forms/popups remain. Changes are local;
no push, installed executable update, release, paid API or default-store change.

## #41 D1 actual SQL batching and raw header (locally qualified)

Commits2f2e39b/5000b05/ee369f6/92e9450 separate tool/sender/recovery SQL batches
from raw-header preparation. Merge46963c9 integrates main1b559b9's submitted-copy
retention, Thread-scoped fences and setup fixes. Final558b870 fixes physical join
order and skips non-user title windows. Native macOS/Rust1.99.1 standard passes
**1,745/0/32** (library859, integration876, termination7, crash3), layout90,
Python6, formatting and strict all-target Clippy.

Real SQLite traces verify tool direct/projected50-node reads in2 statements,
whole-group scopes in3, sender tool scopes in2/user scopes in4, recovery50-event
scopes in1 and raw headers in1/3. Collision/ambiguity, original provenance,
GC source lookup, sparse ordinals, byte caps and old-query parity are covered.
Header/title and initial draft JSON prepare after DB release. Bounded earlier
source loss is explicit rather than replaced with a later title. CLI behavior
and conversation schema14 remain; the existing node-source index suffices.

The same-main128-node/50-tool P3 comparison retains1 worker job while SQL
executions fall176→26. Worker p50 is4.813→3.334ms and p95 is9.826→4.359ms in this
instrumented local sample. VM work still rises16,602→22,632; no global SQL/CPU
speedup or every-surface P3 completion is claimed. An intermediate61,255-VM plan
was investigated and corrected; a minimal-schema missing-index assumption was
rejected before any schema change. Full evidence and limits are in
[qualification](../validation/conversation-redesign-shared-read.md#d1-actual-sql-batches-and-bounded-raw-headers).

Next: D live-work/clock/output, E ID-restricted browser patches, F Watch/descendants
and G catalogue/forms/popups, plus full planner and applicable performance/native
gates. These changes remain local: no push, installed update, release, paid API
or default-data change.

## #41 D0 shared current-view read (locally qualified)

Local foundation commits6d1e04a/54fd089/635cd9b/e6b1cfc extract connection reads
and off-DB preparation. Commitf43f7dd consumes those helpers in one current-view
transaction: receipts, Thread/page and paired pending, source/tool/recovery/usage,
received audits and activity. The selected consumer shares one500ms start deadline
and one in-flight refresh; paused tool facts are scoped to that transaction.
The UI applies coherent generation/read revisions without dropping paid text,
original sender evidence or #58 shrink/stale diagnostics. No new worker/table/queue.

Separate commit55cdc67 caches Thread tree structure/ID maps and retains selected
identity and screen row through refresh/removal; first preferred selection and
viewport growth have explicit regressions. This is E's cache foundation only,
not E's batched external-change projection or Watch implementation.

The first shared-read library run found2 failures (818 passed/13 ignored): an
old Thread-only notification expectation and a real initial-viewport regression.
Both are corrected; additional audits restore event-kind filtering and paired
history diagnostics. Latest main throughbb7e29b, including #59's explanation and
#69's standalone setup, is integrated. Source67f6f09 now passes the standard check:
**1,711 passed /0 failed /32 ignored**, layout90/Python6/fmt/strict Clippy.
The first integrated check's2 stale observer failures and their correction remain
in the [qualification record](../validation/conversation-redesign-shared-read.md).
All4 real separate-CLI/GC reactivity tests pass in that final suite.

Actual128-node/50-tool observation now verifies7→1 DB jobs per selected refresh;
worker p95 is3.675→3.940ms, so no SQL-time speedup is claimed. Source identity is
checked after rejecting one shared-cache artifact mix-up. At that D0 checkpoint, per-node helper SQL and the reused
ThreadDetails title scan remained open. D1 above implements their actual batches
and frontend raw-title boundary; global multi-consumer gates still remain open.
See the qualification.

Next: D live-work/clock/output behavior,
E/F list/watch/descendants,
and G catalogue/forms/general popups. Visible source-detail reads are still their
existing separate finite consumer; activity polling also remains until D's next
unit. Do not claim every-surface1job/500ms or P2/P4/P5/P6 completion. These commits
are local only; no installed update, release, paid API or default-data change.

## #41 E prerequisite: external deletion notifications (local)

Commit161b4c9 returns exact committed singleton/cascade deletion targets through
existing Changed effects, with no effects on replay/rejection/rollback. Two real
Store/Control regression tests pass. Follow-up standard check at161b4c9 succeeds:
**1,713 passed /0 failed /32 ignored**, layout90/Python6/fmt/strict Clippy.
E browser patch consumers and native Windows/Linux qualification remain unfinished. This adds no schema, event type,
worker, queue or lifecycle policy.

## #60 Medium follow-up: indexed Thread-scoped receipt fences

Autosave, selection and archive fences now constrain receipt.target_thread_id to
the receiving/current Thread. The existing receipt_target index limits JSON reads;
there is no schema/migration/new index. A regression verifies Thread isolation
and EXPLAIN QUERY PLAN SEARCH (not SCAN) for the actual generated fence SQL.
Native macOS arm64/Rust1.99.1 RUST_TEST_THREADS=2 standard check passes1699/0/32,
layout90, Python6, fmt and strict Clippy. Native main CI remains pending.

## #60: bounded submission facts and terminal copy disposal

Existing receipt.result_json holds only a bounded four-field submission binding.
Known admission/cancellation removes the correlated submitted copy transactionally;
submitting/unknown, editable drafts and cancellation archives are retained.
Autosave/editable/submitting restoration and queue correlation consult the receipt
facts after the copy is gone. Ordinary history/all GC backfills at most128 old
eligible copies per pass. No schema/table/column/worker/queue/setting is added.

Native macOS arm64/Rust1.99.1 full standard checks pass **1688 passed / 0 failed /
32 ignored** before integration, layout89, Python6, fmt and strict all-target Clippy, with
RUST_TEST_THREADS=2. Eight new regressions cover bounded body-free markers,
admission/deletion rollback, known/unknown retention,16 repeated sends with
logical storage drops, exact receipt replay after copy disposal, durable DB reopen,
130-copy GC batches and cancelled text/attachment restoration without sending
after GC. Initial full validation found two old frontend tests that required
permanent copies; they now check retained receipt facts, positive admission,
canonical text, independent edits and known-versus-uncertain copy lifetime.
After integration with #69 base bd75c9a, exact source7d34cbe passes **1698/0/32**,
layout90, Python6, fmt and strict all-target Clippy. Its first full run failed the
unchanged PTY unpaused-index repaint; the same-SHA/same-environment full rerun
passes. That failure is retained, not claimed fixed. Native main CI is pending.
Source-draft watermark checks may scan receipt JSON; no O(1) lookup or physical
SQLite shrink is promised. Real user data and physical terminal are untested.

## #59 review follow-up: legacy sender explanation (D-006)

The CLI reference and protocol design distinguish legacy null's historical human
conversation band from evidence-based unknown in sender facts/F6. New external
CLI records already use the generic input category; the previous blanket claim
of unchanged conversation rendering is removed. F6 keeps sender.kind=unknown
and adds an explanation for absent evidence, including old records. Source
classification, band styling and #41 renderer/refresh code are untouched.
The full local standard check passes1680/0/32 with RUST_TEST_THREADS=2,
layout89, Python6, fmt and strict Clippy. Two preceding default-concurrency
checks failed the unchanged PTY index-status/model-wait fixture; both are retained,
not claimed fixed. Physical-terminal F6 remains untested; final main CI is pending.

Windows watchdog follow-up f07ac85 main CI41072990263 passes all three OSes on
its first attempt, following repeated native Windows branch qualification.

## Windows retry fixture watchdog follow-up

The two response retry fixture files share60s progress/cleanup watchdogs and
notification-driven persisted-state waits. Product timers, retry/phase/count
assertions and intentional timed absence/outage checks stay unchanged. Main
b16f414 run41063015359 attempts1/2 remain failed evidence; the long-Turn arrival
guard was already60s, so its cause is not established. Its third attempt passed;
the two preceding failures remain evidence. Native macOS standard checks pass
1666/0/32. Exact source3b45c3c passed two independent manual Windows four-thread
runs,41067174706 and41067200942, both first attempts. After #41 main integration,
combined d72155f also passes two Windows runs41069488710/41069483605; the latter
needed a same-SHA failed-job rerun for an unrelated clients test. Local combined
check passed1679/0/32 after an unchanged PTY footer failure. Both failure histories
are retained in acceptance; final main CI is pending.

## #59: saved sender inspection and self-asserted names

`message get` and user-node `history get/list` provenance expose the original
receipt sender, including additions to an active Run. Verified model Origin IDs
are credential-free and survive sender-Thread deletion; new external CLI input
is `external_cli`, trusted in-process TUI submissions are recorded `human`, and
legacy missing/incomplete evidence is `unknown`. Optional `--sender-label` is
bounded to 64 Unicode scalar values without controls/line separators, stored in
existing receipt JSON and displayed as a distinct self-asserted name only in F6
details. No table/schema migration or label-based authority is added.

The existing source-metadata classifier recognizes recorded `human` objects to
preserve its established conversation category; it ignores label. Historical
null's existing conversation convention remains separate from unknown CLI/F6
sender facts. No renderer/layout/index-refresh redesign or new conversation
badge is included. CLI, Agent Guide and delegation instructions explain sender
inspection and cross-data-dir Origin stripping/naming.

Native macOS arm64/Rust1.99.1 standard check passes **1,666 passed / 0 failed /
32 ignored**, layout88, Python6, format and strict all-target Clippy. Synthetic
checks cover owner-issued Origin/forgery rejection, appended receipt provenance,
deleted sender IDs, external labeled/unlabeled input, trusted human submissions,
legacy unknown, Unicode/control label limits, unchanged cancellation/recovery
authority, credential exclusion, cross-data-dir real CLI and F6 TestBackend.
Native Windows/Linux CI and physical-terminal sender-details checks are pending;
see [acceptance](../validation/acceptance.md#59-sender-inspection-and-labels).

## #41 B+C freshness shipped; D shared-read refactor next

The structural/interaction subset shipped on main b0e3c29 with three-OS
CI41057195099 green. Review6523314574 correctly identified unfinished B+C
freshness; it was explicitly recorded and reported in issue comment6523388336.

Follow-up sourcefd968e5 (including main ed40f33/#59) implements scoped reactive F6/F7 facts, new-result
source membership, GC availability notifications, snapshot read/freeze times,
and bounded rereads that preserve known paid text on failure. External CLI
header/queue/footer mutations and real raw/history GC are tested without later
UI input. Local standard: **1,679 passed / zero failed / 32 ignored**;
layout89, Python6, formatting and strict all-target Clippy pass.

The final binary passes six actual macOS PTY width/palette conditions, including
fixed F2 time and preview read time; UI p95 is3.974ms (baseline2.110ms).
The merged #59 sender regression preserves kind/name after a source snapshot;
external_cli and historical-null display conventions match comment6524373262.
See [qualification](../validation/conversation-redesign-bc.md) for exact source,
measurements and limits. Main17c4271 now passes three-OS CI41068514157. Downloaded exact-SHA artifacts
confirm Linux1,680/0/31, macOS1,679/0/32 and Windows1,563/0/24
(passed/failed/ignored; Windows threads4). This qualifies the follow-up itself,
not merely the earlier b0e3c29 structural source.

The [BC-R1–R6 ledger](issue-41-plan.md#bc-freshness-follow-up-review6523314574)
is complete through its native main gate. Next is D's shared current-view
read refactor before adding live-work rendering. D–G, global refresh
consolidation and their performance/freshness gates remain open; #41 stays open.
An external-delete notification producer gap is explicitly tracked under E.
This task did not replace the installed binary, create a release/tag, call a
paid API or modify default user data. The separate v0.3.0 publication below is
source8fb885d and does not include the later #41 main increments.

## v0.3.0 published (#62)

[v0.3.0](https://github.com/tya5/shellboy/releases/tag/v0.3.0) was published
2026-10-30 at 12:38:45Z with tag source
`8fb885d843a39069aac80fa63708b5c70df0531c`. The retained
[release-body file](release-v0.3.0-draft.md) now matches the published body;
its historical filename does not mean the release is still a draft.
Cargo/lock version is 0.3.0; README remains version-independent.

Anonymous HTTPS downloads of public `SHA256SUMS` and all three archives pass
SHA-256 verification; all three archives extract. The public macOS arm64 binary
reports `shellboy 0.3.0` on macOS 26.3.1. The isolated
[native public-byte smoke](https://github.com/tya5/shellboy/actions/runs/41058668366)
passes on Windows Server 2025 x64, Ubuntu 22.04 x64 and macOS 15.8.2 arm64:
each anonymously downloads the public manifest/archive, verifies pinned hashes,
extracts and runs `--version` with the exact result `shellboy 0.3.0`. Candidate
build/launch evidence is separate because the published archive hashes differ. See
[publication evidence](publication.md#v030-publication-2026-10-30) for exact hashes.
No installed binary or real user data was changed by this cleanup.

Earlier synthetic qualification used the public checksum-verified v0.2.0 macOS
binary to create a schema11 store via owned PTYs/local mock API, then tested a
candidate0.3.0 backup upgrade to schema14, searches/usage/new work and RAG3.
That candidate qualification does not establish migration success for every
public OS binary or real providers/terminals. See acceptance for method/limits.
Cleanup standard check on native macOS arm64/Rust1.99.1 passes **1,658 passed /
0 failed / 32 ignored**, layout88, Python6, format and strict all-target Clippy.
The prior PTY repaint failure passed unchanged in fresh validation and cleanup
checks; its cause is unresolved, not claimed fixed. The isolated validation
workflow is not a main CI change. Physical-terminal and real-provider checks
on every OS remain untested.

## #61: current user and agent guidance

Documentation-only, single-conversation update: CLI reference includes send
--now and fixed-ID message cancellation, EN/JA User Guide usage is three user
points, and both languages explain the same safety assumptions, rationale,
application guarantees/limits, user responsibilities and prompt-injection risk.
Agent Guide adds safety reminders and usage get; TUI guide records user-confirmed
macOS WezTerm Shift+click. Existing refresh details stay in the TUI guide.
No runtime/CLI/schema/test changes or root README edits. Defer three-OS sandbox
recipes until actually validated; tools and owner-mediated-access limits are
named without untested profiles or network rules. Local checks and guide-source
hash verification pass; post-rebase standard/main CI are separate final gates.

## #58 diagnostic review follow-up

Ordinary history/window refreshes are filtered before diagnostic queue admission.
Only node-count reduction, follow changes, stale replies and body eviction emit
history records. The regression drives 200 unchanged UI refreshes plus growth
and verifies exactly the four meaningful records; silent refreshes do not
increment dropped-record counters even under contention.

Native macOS arm64/Rust1.99.1 `sh scripts/check.sh` passes: **1,610 passed /
0 failed / 31 ignored**, layout88, Python6, format and strict all-target Clippy.
This is the current combined source on main c6397b1 (#50 included), distinct
from #58's earlier 1,614-test checkpoint. Main CI41041115046 at source75152eb passes all three native OS jobs;
issue58 is closed for this logging-only correction. #41 changes remain isolated on their work branch;
the installed binary and user data are unchanged.

## #50 review follow-up

Explicit overflow now resends only after an adopted reduction. A no-target
maximum edit immediately tries the existing final spill/summary stage; rejected,
no-target or exhausted reduction enters recovery without an unchanged POST.
The reason includes automatic_reduction_unavailable. Transient retry policy is
unchanged. Review items 2-4 fix the sample configuration, remove obsolete Guide
estimation advice and bound the manual-edit test's wait with a 60-second watchdog.
Local standard check: 7 groups / 1,610 passed / 0 failed / 31 ignored, layout88,
Python6, fmt/strict Clippy. Native manual Windows and final main CI are later gates.

## History retention and conversation redesign (#58 then #41)

#58 runtime/tests at `7478074` (including post-result notification80de5f0)
pass the macOS arm64/Rust1.99.1 standard check: **1,614 passed / 0 failed /
31 ignored**, layout88, Python6, formatting and strict all-target Clippy.
Gitleaks passes. Final real macOS PTYs80x40/140x40 retain already-painted paid
messages and exited results through three held next-response gaps, with tiny
shared cache; explicit pause/return-to-tail behavior passes. No paid API or
user-store operation is included. See [acceptance](../validation/acceptance.md).

Standalone main e8c1ff2 passes native CI41034892332 on Linux1,615/macOS1,614/
Windows1,499 with zero failures; artifact SHAs match. Issue58 is closed with final
results. Its source is the prerequisite for the #41 branch.
The installed binary remains main1fec7f0, including #47/#52/#53/#56, and is not
changed by this source work. #41 design is now adopted on issue-41/conversation;
complete B+C conversation/inline interaction is next. G's added operation catalogue,
form clarity, headings and popup-wheel requirements are in the full ledger.

[The #41 delivery plan](issue-41-plan.md) tracks the full body/comments including
staged-main instruction6518940167. Finish #58 independently first, then adopt
and implement complete usable increments. Each receives separate commits,
standard checks and successful main three-OS CI; partial delivery does not close
#41. English migration #35 follows it.

## #50: failure facts, not token estimates

First small implementation removes calibration, 75k, compact_hint_tokens and
pre-POST reduction. Maximum reduction and explicit context overflow share the
normal consecutive retry budget; successful responses reset it. /compact-max
and compact --maximal reuse safe API-free omission. Context edits report replay
characters; thread get exposes measured usage and times only. Recovery remains
a minimal factual line/F8 details, not a redesigned screen ahead of #41/#58.
Single-conversation implementation/self-review; no delegated work.
Local standard check passes 7 Rust groups / 1,588 passed / 0 failed / 31 ignored,
layout 88 / Python 6 / fmt / strict all-target Clippy. Native Windows pre-push CI
and final main CI are separate gates; no success is claimed before observation.
See [solo record](issue-50-solo-trial.md) and acceptance for limits.

## Pending Message Publication (#52)

The review corrections at `6e62349` pass the complete local standard check
(macOS arm64/Rust1.99.1:1,572/0/29, layout88, Python6, fmt, strict Clippy) and
manual native Windows CI[41019801385](https://github.com/tya5/shellboy/actions/runs/41019801385):
**1,457 passed / 0 failed / 21 ignored**, artifact source SHA verified, threads4.
The new interrupted unfinished-group regression ran on Windows. Commit582ec70
fixes the busy loop without modifying saved pairs or introducing polling;beace3d
uses reactive test observations and fixes the guide typo. Submitted draft cleanup
is deferred with replay/watermark constraints in decisions.md. Gitleaks passes.

The correction is qualified for the user-authorized main push. Main CI remains
an independent exact-source gate; final run links/results are recorded on issue52.
Previous CI failures and the unchanged-source reviewer rerun are retained in
acceptance, rather than being overwritten by this successful qualification.
No #52 installed-binary change or release is included.

#53/#56 are published on main `b686ec1`, with successful exact-SHA native CI
41005281669 (Linux1,527 / macOS1,526 / Windows1,411 passed, zero failures).
Both issues are closed after review. The installed optimized local binary is
that main53/56 source, SHA-256
`7bbc7a0264ffe993fa8a66741c311f6adf2f2d6c4ee5351de6bc66cca55fc3b9`;
help/version/offline-guide checks pass. It does not contain #52.

#52 implementation `74592a9` and native-scenario tests `47e6751` pass the local
macOS arm64 / Rust1.99.1 standard check: layout88, Python6, format, strict
all-target Clippy and **1,571 passed / 0 failed / 29 ignored**. Schema14 uses
one nullable Turn-intent scalar and two indexes on existing drafts; no new table,
worker, poller or queue. Pending UI, per-ID cancellation/durable restore, frozen
send correlation, explicit immediate delivery and human-only Turn interruption
are implemented. An additional compaction fix preserves absent owner fences
without invalidating ordinary queued input or adding a counter.

Earlier, the separate issue52 branch at `33936cf` passed the required manual native
Windows qualification [41015832578](https://github.com/tya5/shellboy/actions/runs/41015832578):
1,456 passed / 0 failed / 21 ignored, four test threads, artifact source SHA
verified. All six new real HTTP/shell Turn-interruption/quiet-delivery scenarios
and three native-gate/binding tests ran successfully on Windows. Final independent
backend, UI and Core/native reviews found no concrete blocker. Main admission is
authorized and qualified at that checkpoint; the subsequent main three-OS CI
and later review corrections are recorded separately. No #52 release or installed-binary update is claimed.

#42/#43/#55 remain published on main343c2d6, with successful exact-SHA CI
41001556952 and review-result comments; all three are closed. Those results do
not validate #52's new native behavior.

## Reviewed index integration into main5350fc8

Reviews #42/#43/#55 have no Medium findings. Merge51a819f preserves current
main usage changes and index changes; native macOS standard checks pass with
1,504 passed, zero failed, 28 ignored, six Python tests and strict Clippy/fmt.
The #43 batch deadline guidance is added without changing defaults/algorithms.
See [review dispositions](index-review-follow-up.md). Main publication is
authorized; exact new main CI is the closure gate. Prior branch-native results
remain separately identified historical checkpoints. #53/#56/#52 work is on
a separate branch and is not included here.

## Current integration: index operations and main usage accounting

The reviewed index branch at `e12ab8f` is being combined with origin/main
`5350fc8`, which includes the usage-accounting work recorded below. Latest
reviews for #42/#43/#55 report no Medium findings. The combined tree's standard
checks are pending; the earlier three-OS index result validates `ae9bc19`, not
this new combination. Both sets of earlier checkpoint records are retained below.

The user has authorized main push after integration and verification. Issue
closure still waits for successful main CI on the integrated source. No tag,
release, paid provider test or default-user-store operation is implied.

## Previous index-branch checkpoint: implemented, pushed and native-verified

The reviewed index branch `issue-42-43-55/index` is pushed. Exact source
`ae9bc19` passes [manual CI40996181881](https://github.com/tya5/shellboy/actions/runs/40996181881)
on all three OSes: Linux1,499 / macOS1,498 / Windows1,386 passed, zero failed;
ignored27/28/20. Windows uses four test threads. All baseline16fe4aa test identities
and ignored sets remain, with 30 additional active cases per OS. Native macOS
standard checks also pass (1,498/0/28, six Python gates, 85 registered suites,
formatting and strict all-target Clippy). See [acceptance](../validation/acceptance.md).

Main contains #57 at `16fe4aa`, with
[push CI40995235000](https://github.com/tya5/shellboy/actions/runs/40995235000)
successful on all three OSes. Branch push of ae9bc19 started no automatic run;
explicit workflow_dispatch selected that branch and completed all three jobs.
CI runs automatically only on main push; retained manual inputs/jobs are unchanged.
Tags/releases require an exact green main SHA and separate user authorization.

The index code includes canonical Windows repairs from main5234f94. The earlier
failed b22a30a Windows run is retained; the duplicate local alternative34ca8d3
was abandoned and not published. Index implementation/tests and UI are split
across meaningful commits, followed by current-main integration. Remaining work
is review and separately authorized main integration of the index changes.
During final handoff, origin/main advanced to `5350fc8` with another session's
usage-accounting work (CI40997452704 still in progress). That later feature is
outside the native-verified index branch. Integrating index into current main
remains a separate review/combined-check step; do not attribute its success to
the already verified ae9bc19 baseline.

No paid provider test, default-user reindex/GC, installed-binary update, tag or
release is included. Subsequent evidence-only updates do not change the verified
Rust, bundled guides or workflow source and do not trigger automatic branch CI.

## Issues #42/#43/#55: pre-main-repair combined local checkpoint

Origin/main `c0f1ad9`, containing #48/#51, is now merged into the local index
branch at merge `80caf38`. The combined tree passes macOS standard checks:
1,498 passed, zero failed, 28 ignored; formatting and strict all-target Clippy
also pass. The earlier 1,482-pass result belongs only to pre-merge `f9d3ee7`.
No native CI or installed-binary verification is claimed for this combination.

Work is isolated on `issue-42-43-55/index`; user data and the installed binary
are unchanged. The adopted [index operation design](../design/index-operations.md)
covers saved pause observations/recovery preview, deterministic bounded batches,
explicit dimension candidates/probes and space payload facts. Existing workers,
spaces and app_meta own the work; no table, queue, dependency or poller is added.

The pre-merge implementation tree at `f9d3ee7` passed macOS standard
checks: 1,482 passed, zero failed, 28 ignored; all-target strict Clippy and
formatting succeeded on that tree. Real PTY recovery verified saved stop time, target counts,
Y confirmation and S status refresh. Mock HTTP covers deterministic batches,
first-dimension validation, refusal/transient/late replies, persisted query/ingest
mode, probe receipts and explicit inactive-space GC. Tests are split by issue;
Core request-mode/claim plumbing is shared because batching must carry the
space's frozen mode. See [acceptance](../validation/acceptance.md).

The default batch count remains one; users opt in with documented caps. No paid
API calls, user-store rebuild/GC, installed-binary update or release was performed.
At this checkpoint the index branch had not been pushed and native CI
authorization was pending. Later b22a30a results and current manual-CI permission
are recorded above; main's successful #46 CI does not validate index changes.

Separately, the reviewed #46 measurement branch was fast-forwarded into main at
`a949688` and pushed on user instruction. Main CI
[40987103680](https://github.com/tya5/shellboy/actions/runs/40987103680) succeeded
on all three OSes; #46 is closed.
The three index issues were not included in that push.

The #48/#51 sections below retain their original local integration and review
checkpoints. Their historical publication restrictions and base descriptions
record those stages; current combined integration is described above.

## #49 Then #47: Standard Usage And Accounting

#49 implements the shared five-number whitelist, guarded SQL scalar reads for
large saved usage and normalization of known provider-envelope usage in completed,
failed/incomplete, stream-audit and summary copies. Exact source `ad9343ce` passes
local macOS standard checks: layout82, Python6, format, strict Clippy and Rust
7 groups / 1,474 passed / 0 failed / 28 ignored. No existing usage rewrite migration
or Core cleanup automation is added. Task-boundary cleanup is Agent Guide guidance;
real-model dogfood and new exact-source native CI are unverified. Independent
review and latest-main integration checks are separate gates.

The existing non-2xx HTTP path remains diagnostic-only in #49. #47's latest
measurement-first scope reads saved states/numbers only and does not extend it.
The former write-time/period design is superseded. Read-only SQL plus CLI/get and
scoped UI detail-cache wiring is implemented; no schema, producer/state/driver
hooks or period API. Initial and varied optimized query measurements pass the
approved extreme-size~500ms direction; metadata-inclusive46eabea release projections pass the approved direction
(varied Thread412-433ms median). Latest UI P2/full-standard/independent/native
gates remain separate. Exact c4b2337 passes the final local standard check
(Rust7groups1495/0/30ignored, layout82/Python6/format/strict Clippy), independently
confirmed alongside usage27/0/3, selection1/0 and all-UI78/0/1. The reported UI
P2s and index confirmation regression are resolved. Parent-approved docs clarify
that explicit F8 usage/get forms refresh on Changed, while followed references
and F6 pages are one-shot/cached snapshots with explicit re-query and no freshness
deadline. Final wording/surface review, new exact documentation checks and
latest-main integration/native CI remain separate gates. Latest mandatory
UI cooldown is3s using the existing500ms tick, one pending/inflight; slow-only
>=100ms diagnostics use existing shellboy.log with typed non-content fields.
The focused usage suite passes27/0/3 ignored with synthetic clocks/data/logs.
Unrecorded summary/embedding coverage is explicit. Stop after #47 main exact CI;
#50 and other new work are not authorized until a new post-restart instruction.

## #51/#48: Windows CI fixture and diagnostic repair

Repair-branch CI run `40991269818` at `0cf9c405` clears the earlier two lib failures
and completes all harnesses with failed-name diagnostics, but Windows exposes
one further mixed-separator expectation in the persistence-only history fork
test. Its expected inherited path now joins `project` and `AGENTS.md` as separate
components; full path equality, explicit-override rejection and atomicity checks
remain intact. A read-only search of all #51/#48 added Rust test path comparisons
finds no other unresolved same-kind expectation. Raw CLI/form relative strings,
fixed mock response paths and filesystem-write inputs intentionally stay unchanged.
The focused history test and pre-commit standard check pass locally on macOS:
layout82, Python6, format, strict Clippy and Rust 7 groups / 1,468 passed /
0 failed / 28 ignored. No runner or production change is added. The current #57 publication rule supersedes the earlier branch-CI gate:
after independent review, fetch/rebase onto latest origin/main and pass the local
standard check before a normal main push; then verify exact-SHA main three-OS CI.
Branch dispatch is optional for platform-specific concern, not a required gate.
Do not admit unrelated work while main CI is red. Workflow changes are out of scope.

Main source `c0f1ad984be7a5d64d9dfbab4189cd3ba0438151` failed Windows in both
attempts of run `40988162209`. The two native failures are portable-expectation
bugs in `explicit_agents_resolution_inheritance_validation_and_receipt_replay`
and `empty_conversation_explains_actual_bindings_cwd_and_unobserved_agents`:
Windows-native separators differed from slash-containing expected strings.
Expected values now use Path components; production normalization/rendering and
the full equality/rendered-label assertions are unchanged.

The timing runner's raw artifacts were already UTF-8 and recoverable. A cp1252
console error while printing the failed log tail interrupted harness-result
recording. The separately scoped repair uses UTF-8 stdout/stderr and libtest
listing decoding, saves the native harness result before diagnostics, and prints
all failed names as ASCII-safe lines before the unchanged tail. A strict-ASCII
child-runner regression fails against the old script and passes after repair,
retaining full Unicode logs, exit 101/names and runner exit 1.

Focused macOS fixture tests and six Python self-checks pass. The clean exact
repair `ff84d81682e477db91aa34be751609081cdb535b` also passes the local standard
check: layout82, Python6, format, strict all-target Clippy, Rust 7 groups /
1,468 passed / 0 failed / 28 ignored. Independent review approves `ff84d81`;
repair-branch native CI was not yet verified at that checkpoint. Neither failed
c0f attempt is relabeled successful. The earlier publication order required
reviewed local checks, repair-branch CI,
then latest-main reconciliation and main CI. That mandatory branch-CI gate is
superseded by the #57 rule above; the failed runs remain failed evidence.
The integrator alone handles authorized pushes. No force push, issue close,
release, installed-binary update or GitHub post is authorized.


## Issue #48: working-instruction provenance

Combined local integration on the independently approved #51 documentation
base `f46fa2e` passes at exact `702cef9`: 82-suite layout, five Python checks,
format, strict Clippy and Rust 7 groups / 1,468 passed / 0 failed / 28 ignored.
The #48 two-commit range-diff is unchanged, and independent new-SHA review
repeats the header and full runtime_features suites without findings. This is
still the #46 branch-based local integration, not origin/main adoption or CI.

The actual instruction developer message now starts with one metadata line:
JSON-quoted fixed Turn source path, user-editability, new-Turn rereading and
same-Turn snapshot reuse. The following LF separates the original unmodified
body. JSON/control/Unicode line-separator escaping prevents a path from adding
another header line. Snapshot body/hash/size/storage, roles, precedence and
permissions remain unchanged, including empty/missing/read-failure behavior.
The Agent Guide explains configured references and authorized conflict handling
without implying every file lives in cwd or promising automatic exceptions.

Coder validation at exact `120218c` on the approved #51/#46 local base `872c4b8`
passes layout82, Python5, format, strict all-target Clippy and the full Rust
suite: 7 groups / 1,468 passed / 0 failed / 28 ignored. Header unit tests pass
2/2 and the unified runtime_features suite passes 22/22, including #51 tests
strengthened to parse the heading/path and assert byte-exact original bodies.
Independent review approves `120218c` with the same focused results; integration
on the newer #51 documentation base is a separate exact-SHA gate. This is not a claim
of origin/main tracking, new three-OS CI, physical-terminal verification or
improved real-model/user behavior. No push, GitHub post or installed-owner
replacement is performed. See [acceptance](../validation/acceptance.md).


## Issue #51: local integration on the completed #46 branch

The original nine-commit series through `7f425e8` is preserved. A separate
`issue-51/on-46` branch rebases it onto exact #46 source `a949688`, whose native
CI [40982193189](https://github.com/tya5/shellboy/actions/runs/40982193189)
passes all three OS check jobs. This is local advance integration, not completed
origin/main adoption: remote main was still `0469ca0` when the base was chosen.

The nine commits retain their meanings. Four documentation insertion conflicts
retain both designs and validation histories. Existing test files are already
registered as modules in #46's single integration harness; no new target/file is
needed. The first rebased source `144721f` fails Clippy because six new runtime
calls lack the Control argument added by #46's diagnostic helper. Test-only
`872c4b8` supplies that argument without changing production behavior or assertions.

At exact `872c4b8`, macOS arm64 / Rust 1.99.1 standard checks with four test
threads pass layout registration (82 suites), five Python checks, format, strict
Clippy and Rust tests: 7 harness groups / 1,464 passed / 0 failed / 28 ignored.
Independent post-rebase approval is tracked separately. These grouped counts
are not missing suites; the added idle-effects unit test explains the extra pass
relative to the historical 1,463 checkpoint. See [acceptance](../validation/acceptance.md).

Push, new CI dispatch and installed-binary updates remain unauthorized. Once #46
lands on main, compare the actual origin/main base and determine whether another
rebase/review/check is required. The overlapping #48 change has separate
worktrees and commits and is not included in this #51 result.

## Issue #51: explicit instruction references

Control/CLI thread create, set and fork support `--agents`; TUI create/fork/
settings forms and `/agents [PATH]` select or display the reference. Explicit
files receive bounded strict validation, revision-checked atomic mutation and
actor/path/time audit. Cwd-only updates never rediscover instructions. A running
Turn keeps its durable body and reference-path snapshot; reference-change events
wait for the next new Turn without delaying unrelated context notifications.

Independent review approves production implementation through `bfd96f5` after
three P2 fixes: unsaved TUI adoption, DB-only History helper boundaries and
rejected UI admission releasing its pending selection. Unsaved selection now
retains the origin draft and correlates results/rejections with draft,
generation and one request ID; first send reuses the created Thread.

The coder's clean `bfd96f5` macOS standard check passes: format, strict all-target
Clippy, 86 test groups / 1,462 passed / 0 failed / 28 ignored. A subsequent
src-only regression additionally proves child/Control fork overrides select a
*distinct* file rather than merely inheriting the same path; its focused test
passes and its test/documentation commit `9ddb49d` is independently approved.
The exact integrated `9ddb49d` also passes the macOS standard check: format,
strict all-target Clippy, 86 groups / 1,463 passed / 0 failed / 28 ignored.
This includes the additional distinct-reference test; it is separate from the
older 1,462-test production-code evidence. See
[acceptance](../validation/acceptance.md). Main and remotes remain unchanged;
push, GitHub posting, installed-binary updates, physical-terminal evaluation
and three-OS CI remain unperformed in this task's local-only scope.


### External Review Follow-Up

The external [review](https://github.com/tya5/shellboy/issues/51#issuecomment-6511989087)
and [ordering/test follow-up](https://github.com/tya5/shellboy/issues/51#issuecomment-6512018897)
are read as findings, not independent test execution. Replace the #51 idle
assertion based on a 100 ms receive timeout with a saved Run-ended boundary and
Thread-scoped Run/Turn/attempt ID/state comparisons. A src unit regression also
checks that settings return no driver/retry wake effects and create no delivery
or history node before the next Turn. Production behavior is unchanged.
The focused src effect test and loopback runtime test each pass once on macOS
arm64 / Rust 1.99.1, as does strict lib/runtime Clippy. The earlier exact-SHA
whole-suite results remain historical checkpoints, not post-rebase evidence.

Three Low observations are recorded only, outside closing conditions: agents-only
selection increments profile_revision (including same-path selection); initial
create/fork audits describe default/inherited-to-selected references even when
the prior reference was never used; adding an optional field spreads constructor
updates because argument structs do not all implement Default. No production
changes for these observations are included.

Land #46 first, then rebase #51 onto origin/main and adapt its existing tests to
#46's final integration-binary layout. Do not transplant or resolve predicted
conflicts before #46 lands. Re-run standard checks after rebase and retain the
physical-terminal/native-platform limits. This publication sequence is not push
authorization; push, main changes and GitHub posting remain on hold.


## Issue #46: measured native test performance completed

The reviewed measurement branch `issue-46/test-performance` was pushed and
subsequently fast-forwarded into main at `a949688` on user instruction. Main
three-OS CI 40987103680 succeeds; #46 is closed. Ordinary integration links fall from 82 to 3; registration, normalized
identity and ignored-set checks retain every original test. Historical schema
fixture setup is batched, while unproven index seed batching was discarded.
Shared runtime/retry fixture guards now use bounded phase diagnostics and
existing change notifications. Product behavior, durability and dependencies
are unchanged.

Exact source `03ceac1` succeeds in
[three-OS CI 40980978976](https://github.com/tya5/shellboy/actions/runs/40980978976):
Linux 1,453 / macOS 1,452 / Windows 1,341 passed, zero failed. Windows uses four test
threads. Its Tests/job timing is 285 / 379s, compared with 640 / 888s at the pulled
baseline `0469ca0`. All same-OS inventories and ignored sets match. Earlier failed
four-thread candidates and their fixture repairs remain in the
[investigation](issue-46-ci-performance.md); successful retries do not overwrite
failed evidence.

The [acceptance record](../validation/acceptance.md) records exact sources,
runner/measurement limits and the final benchmark. The evidence update is docs
only; subsequent workflow results must be identified by their own SHA. No release or installed-binary update is part of this task.

## Issue #44 review follow-up: readable activity column

The external review's Medium finding is addressed by 99982d7: a separate
column label (at most eight display cells) and an eight-cell reservation even
in narrow layouts. Background is explicitly background work, idle is not-yet-run;
full detail/footer labels and JSON classification remain unchanged. The actual
state cells, not selected details, are checked for all 17 phases at 20 terminal
widths and two heights. macOS standard checks pass: 86 groups / 1,452 passed /
0 failed / 28 ignored. Exact-SHA native CI for this follow-up is confirmed
separately after push. The Low wall-clock ordering limitation is recorded and
unchanged. Issue closure and installed-binary replacement are not part of this
follow-up.

## Issue #45: read-time shell observations

Shell get and bounded status pages now share the saved start/output timing
reader. Live duration is a read-time wall-clock observation; terminal duration
remains the saved monotonic completion measurement. Unknown/collected output is
null. Status fingerprint, cursor and sort keys exclude the new observations.
Focused work-status tests pass (13 tests before the bulk-shell fixture was
added), including output retention, read-only status, stable fingerprint and
get/status agreement. Primary-key query-plan and clock-boundary tests pass.
Combined macOS standard check passes 86 groups / 1,451 passed / 0 failed /
28 ignored. CI 40951871499 at 68f4f5d succeeds on Linux/macOS/Windows;
Tests-step passed totals are 1,452/1,451/1,340 respectively. The 1,001-shell
bounded status fixture also passes. See the [single-conversation trial
record](issue-44-45-solo-trial.md) for request/token accounting and limitations.
Later documentation changes require their own exact-SHA CI confirmation.

## Issue #44: unified saved activity

Thread list and conversation activity now share one pure classifier. Native
ThreadSummary.state is unchanged; activity is separately available in JSON
and used for list labels, colors and details. The query keeps one latest-Run
seek and uses the existing shell/child ownership indexes. Focused list/footer
fixtures, query-plan assertions and browser tests pass; 2,000 synthetic
background Threads across 40 pages took 279 ms in the macOS debug fixture.
Combined full-suite and three-OS CI validation is successful at 68f4f5d;
see the #45 entry and acceptance record. Later documentation commits are not
covered by that earlier exact-SHA CI.

## Issue #40: remaining low-severity fixes

No-safe-reduction failures now retain ordinary retry eligibility and finite
budgets without spending the reduction allowance. Failed attempts preserve the
structured reduction diagnosis even when an ordinary retry recovers. The
loopback Responses regression suite passes (22 passed, 1 intentionally ignored);
Index storage-stop reasons now use the same snake_case codes as search details;
the library suite passes (627 passed, 10 ignored). Draft backlog rejection now
distinguishes a stopped worker from transient pressure, without promising an
automatic retry for a rejected snapshot. Admission/coalescing and shutdown
ownership are unchanged. The coder's macOS standard check passes: format,
strict all-target Clippy, 86 test groups / 1,443 passed / 0 failed / 28 ignored.
Independent review approves all three implementation commits with focused
re-execution. The integrator's separate macOS standard check at `e01d397` also
passes 86 groups / 1,443 passed / 0 failed / 28 ignored. The reviewed series
through `8ff6c46` is pushed to `issue-40/integration`;
[native CI 40916772603](https://github.com/tya5/shellboy/actions/runs/40916772603)
passes all three OS jobs (Linux 1,444 / macOS 1,443 / Windows 1,332 passed,
zero failed). Later documentation commits require their own review/CI boundary;
see
[acceptance](../validation/acceptance.md) and the
[Shellboy trial record](issue-40-shellboy-trial.md).

## Issues #36–#39 review follow-up (2026-10-30)

Review fixes are committed separately by issue: #36 `ce2f101`, #37 `b1979b6`,
#39 `f5769f0` plus formatting-only `0288bc1`, and #38 `d245d5c` plus IPC-readiness fixture `9e4f8ea`. The
[follow-up record](issue-36-39-follow-up.md) tracks every review finding,
partial adoption and deferred wording/UI work. Strict current-format state and
SQLite integrity checks remain in force; no new table or recovery engine was
added.

The reviewer-provided interactive zsh PTY reproduction confirms duplicate HUP
as the premature-force cause. Baseline exits in about 0.1 seconds with unfinished
Run/shell state; current bash and zsh finish in about 2.1 seconds with stopped
Run/shell state and no orphan matches. The reviewer withdrew M2 after correcting
its process-wait script; it is not claimed as a newly fixed normal-close defect.

The macOS standard check passes format, strict all-target Clippy and all tests:
86 groups / 1,442 passed / 0 failed / 28 ignored. Platform limits are recorded in
[acceptance](../validation/acceptance.md). Work remains local; no installed
binary replacement, push or release is part of this review round.

## Revisit, resilient exit and interrupted-conversation visibility (2026-10-30)

Issues #36/#38/#39 and the #37 review are implemented locally. Revisit reserves
one bounded instruction per live Run, delivers it through ordinary developer
events at normal completion, and exposes count/pending facts in Run/status,
Thread list and the activity footer. Stop/shutdown/restart do not resume it;
interrupted reservations remain inspectable. The optional per-Run limit defaults
to unlimited. See [Revisit](../design/revisit.md).

Delivered TERM/HUP, terminal loss and Windows console-close notifications use
bounded existing shutdown; second signals/deadlines force exit. The latest Run's
saved app-exit reason marks interrupted Threads without a new state or automatic
resumption. See [OS termination](../design/os-signal-shutdown.md) and
[Thread browser](../design/thread-browser.md).

The #37 review now isolates structural group/cursor failures while other Threads
remain usable, keeps SQL/global metadata errors fatal with context, and repairs
against post-startup stopped shell facts. No live collector is guessed complete.
Conversation schema 12 extends derived metadata byte accounting; protocol 2 is
unchanged. The macOS standard check passes format, strict all-target Clippy and all tests:
86 groups / 1,430 passed / 0 failed / 28 ignored. Actual Unix PTY termination
checks also pass separately with two test threads. [Acceptance](../validation/acceptance.md) separates final results
from remaining native-platform and real-model evaluation limits. Changes remain
local; no push, release or installed-binary replacement has been performed.


## Issue #37: interrupted tool-group recovery (2026-10-29)

Startup now completes missing current-format tool results, including groups
whose Turn was already terminalized. Original results and DAG edges are retained;
new results distinguish no admission from uncertain side effects and never
repeat OS work. Replay normalizes output adjacency without rewriting saved
history, including current spills. Unapplied authenticated shell-origin
attachments are cancelled before group completion, while applied refs/files
are preserved. See [design](../design/tool-group-recovery.md) and
[verification](../validation/acceptance.md).

Tests use synthetic temporary data and loopback HTTP. Actual Unix SIGKILL,
restored storage-write failure, pending/partial/old terminal/child groups,
restart idempotence, GC tombstones and cross-Thread attachment policy are covered.
The macOS standard check passes: format, strict all-target Clippy and all tests
(82 groups / 1395 passed / 0 failed / 27 ignored). Windows/Linux native
execution and physical TUI force-quit are not claimed. SIGTERM/SIGHUP shutdown handling remains #38.
Current work is local; no release or installed binary update is part of #37.

## v0.2.0 published; #32–#34 review follow-up completed (2026-10-29)

[Shellboy v0.2.0](https://github.com/tya5/shellboy/releases/tag/v0.2.0) is published
from verified tag/source `4a5eb3e`. Windows x64, macOS ARM64 and Linux x64 archives
plus SHA256SUMS are publicly downloadable. All three optimized native builds,
version/help smoke checks and the publisher succeeded in
[release run 40813637451](https://github.com/tya5/shellboy/actions/runs/40813637451).

The [18-item follow-up](issue-32-34-follow-up.md) covers #32–#34's new reviews.
Retry/resumption/connection classification, optional bounded reduction, indexed
scope membership, overlapping rank votes, deletion errors, receipt/save failure
visibility and bounded stop/shutdown evidence are implemented. Local standard
checks pass 80 groups / 1,380 / 0 / 26 ignored; final
[native CI](https://github.com/tya5/shellboy/actions/runs/40812283328) passes Linux
1,381 / macOS1,380 / Windows1,277, zero failed. Each OS also passes the separate
closed-proxy diagnostic and two fresh attachment processes. Earlier fixture
failures, measurement conditions and limits remain in [acceptance](../validation/acceptance.md).

Public HTTPS downloads of all archives match their published SHA-256 hashes.
Windows ZIP integrity and AMD64 PE, Linux ELF64 and macOS ARM64 are checked.
The installed `~/.local/bin/shellboy` is the official macOS release binary,
verified as 0.2.0 and byte-identical to the downloaded artifact. Its ten-chat
HTTP+PTY, docs33 and RAG CLI13 checks also pass, using temporary data and local
mocks. The previous binary is backed up locally; no default datastore migration
or automatic restart of the previously running owner is performed. Exit the
old owner normally and start the new version to adopt protocol2/schema11.

The original intermittent attachment-watchdog cause remains unproven: no
recurrence is observed in the tagged-source samples, not a claim that repetition
fixed a product defect. Representative provider latency/retry tuning remains
unmeasured; defaults eight/120 seconds are provisional with revisit conditions.
Exact peer delivery/socket reuse and company-proxy/physical GUI behavior are
not inferred from loopback or CLI checks.


## Issue #34 and #32/#33 follow-up: pushed, three-OS CI passed (2026-10-29)

Implementation and test follow-up `b8f1f84` are pushed to `main`.
[Native CI 40804700576](https://github.com/tya5/shellboy/actions/runs/40804700576)
passes format, strict all-target Clippy and all-target tests on all three OSes:
Linux **1,367 passed / 0 failed / 24 ignored**, macOS **1,366 / 0 / 25**,
Windows **1,263 / 0 / 19**, each across 80 groups. The dedicated Windows
overflow-recovery diagnostic also passes once separately; do not double-count
it as an additional distinct regression. See [acceptance](../validation/acceptance.md).

Early maximum safe reduction, explicit-hint pre-POST checks, shared Thread
keyword/semantic scope, a separate header deadline and truthful transport
facts are implemented. Repeated known-incomplete uploads stop in recovery.
The #32 autosave/receipt and #33 index-stop/store-upgrade review findings are
fixed. See [issue #34](issue-34-plan.md) and [review round 5](review-round-5.md).

Native findings are tracked rather than hidden: the explicit RAG writer lease
release in `f659197` passes ownership tests and 280 local parallel cases;
FIFO fixtures no longer depend on per-item timers. `b8f1f84` separates IPC
notification and whole-owner cleanup bounds and strengthens synthetic attachment
preconditions/diagnostics. All those cases pass the final native run. The exact
cause of the earlier attachment watchdogs remains unproven; a green rerun is
not evidence of a transport fix.

The current production executable also passes ten-chat HTTP+PTY, docs33 and
RAG CLI13. Local format/strict Clippy/full tests pass 1,366 cases; the later
fixture-only changes additionally pass their focused 19 cases. No external
model API or default user data is used. No release or installed-binary update
is included.

Representative operational retry/default tuning remains unmeasured; count
eight and the two-minute header deadline are provisional with recorded revisit
conditions. Source consumption does not prove peer delivery; exact socket reuse
is unknown. Historical loopback failure and post-reboot baseline evidence remain
separately attributed in acceptance and decisions.


## Review round 4: fixes committed, whole-suite gate unresolved (2026-10-29)

The reviews of `e977dfe` confirm #19/#20/#27/#30/#31 are resolved. #32's four
new findings are addressed in `7fb41d9`: shutdown persistence precedes UI waits,
autosave is independent of UI capacity, receipt scans repeat fairly and Clone
cannot consume the main reserve. #33's Medium and eight Low findings are
addressed in `c6f2b39`, including schema-11 counter migration and failure/pause
preservation. `8368874` changes only local HTTP mock transport behavior.

Focused storage, index-worker, IPC, frontend and RAG checks pass; see the
[tracking record](review-round-4.md) and [acceptance](../validation/acceptance.md)
for exact evidence and unsuccessful attempts. The final standard check passed format, strict Clippy and compilation, but
ended with two failed library cases and one stuck summary case after the other
591 finished (579 passed / 2 failed / 10 ignored). See acceptance for names and
the limited baseline comparison. The manual HTTP+PTY initial-delta observation remains unsuccessful;
prior whole-suite/manual success below is historical. No push, release or
installed-binary update is part of this request. All seven issues remain open
pending their push and native three-OS CI gate.


## Latest comments and #32 / #33: verified locally (2026-10-29)

The latest comments supersede parts of the previously verified batch below.
Implementation is committed locally as `02b994a` (#32/shutdown delivery) and
`0a72f9c` (#33 and review corrections). The final standard check passed format,
all-target strict Clippy and **76 groups / 1,334 passed / 0 failed / 24 ignored**.
The [tracking plan](issue-32-33-plan.md) covers every item.
The documentation catalogue remains English; only the User Guide retains a
Japanese translation. Search language is not prescribed. Runtime UI language is
unchanged.

#32 reserves bounded main-loop delivery capacity and preserves priority actions
through receipt batches and shutdown. #33 isolates startup migrations, removes
runtime compatibility branches and deprecated CLI/config entries, and advances
the wire protocol to 2. Existing conversation schema 10 and RAG schema 3 remain;
no automatic paid index rebuild or changes to user data are performed.

Additional actual-executable checks passed: offline docs 33, RAG CLI 13,
macOS deletion PTY 5, and local HTTP/PTY 10 conversations with exact canonical
outputs. The isolated ten-thread delivery test passed (action p50 6.50ms, p95
6.84ms, lagged 0, seven 250ms UI pauses). These are synthetic observations, not
production latency guarantees. Links, schema equivalence and secret scans passed.

Push, release, three-OS CI and installed-binary updates are outside this request.
The intended next release is 0.2.0; the package remains at the last released
version until release preparation. Earlier sections below are historical results.

## Latest reviews #19/#20/#27/#30/#31 (2026-10-29; verified locally)

All four Medium and eight Low findings from #31 are resolved or adopted as
recorded choices in the [tracking table](issue-31-documentation-review.md),
including the added pre-1.0.0 no-shim policy in AGENTS. Runtime UI language and the
system prompt remain unchanged. Recovery discovery and guide/real-operation
navigation are covered by cross-component regressions.

The reopened-issue re-review confirms prior Medium fixes. Its additional Low
findings now have bounded fixes or recorded limitations in the
[tracking plan](reopened-19-20-27-30.md): stopped-admission warning, deletion unwind
recovery, complete warning-path test, symlink-root protection, precise key/sync
wording, readonly repair/charge hint and worker wakeup notification. Explicit-force
presentation remains best effort. Exceptional full-reserve priority delay is
tracked separately in Issue #32 rather than claimed solved.

The final standard check passed **76 groups / 1,322 passed / 0 failed / 24
ignored**, including strict Clippy and format. Offline docs (33 checks), actual
RAG CLI (13 scenarios) and the separate ten-thread delivery pressure fixture
passed, along with five deletion PTY scenarios and ten HTTP-mock/PTY
conversations. Current acceptance records distinguish native-platform and terminal
coverage. Changes remain local: no push, release, version bump, installed binary
replacement, paid API or user-data modification. Issue closing gates still require
push and three-OS CI.

## Issue #20 clarification for 0.2.0 (2026-10-29; verified locally)

The updated ticket withdraws Windows legacy-key compatibility. Removed the
lookup and its two retention tests; corrected the adopted specification, RAG
design, English/Japanese guides and the Low 2 disposition. Windows uses current
slash keys and earlier indexes must be recreated. No user index is modified.
Format and all-target strict Clippy passed, as did four focused RAG/docs test
groups (40 passed, 0 failed) and 33 actual offline docs checks. The full-suite
result below belongs to earlier `deee3e1`, before this follow-up. No push, release,
version bump, installed-binary update or user-data change occurred.


## Reopened #19/#20/#27/#30 (2026-10-29; local verification complete)

All four Medium findings are fixed. Deletion cleanup write failures stop owner
admission while preserving confirmed deletion plus warning; file extraction
failures no longer block unrelated RAG removals; readonly search errors do not
stop the writer; root/worker UI updates share one ordered bounded outbox.
Every Low finding has a fix or explicit disposition in the
[reopened-issue plan](reopened-19-20-27-30.md).

The final standard check passed: **76 groups / 1,315 passed / 0 failed / 24
ignored**, with format and strict all-target Clippy. Additional delivery pressure,
actual-executable RAG (13 scenarios), offline docs (33 checks), deletion PTY
(5 scenarios) and local HTTP+PTY (10 completed conversations) passed. See
[acceptance](../validation/acceptance.md) for measurements and untested cases.

Changes are local commits only. Current-task Windows/Linux CI and push are not
performed; issues remain open pending their requested push/three-OS gates. No
release, installed-binary replacement, paid API call or user-data change occurred.
The pre-existing English documentation commit remains on the same local branch.
Below are historical records of earlier tasks/releases.

## English documentation defaults (2026-10-29; complete locally)

All 13 unsuffixed product guides are English. Twelve Japanese user-facing counterparts use `.ja.md`; the Agent Guide is English only. Root README/AGENTS and documentation entry points are English. Existing guide IDs remain stable; bundled docs metadata/categories and the RAG docs scope use English. Japanese guide files remain in source/release archives rather than adding a second runtime catalogue.

The [language policy and UI evaluation](../design/documentation-language.md) recommends an English-default UI with an explicit Japanese startup choice in a later task. No UI implementation, locale configuration, Core state, database, or system prompt changes are included. The current installed/published 0.1.6 is unchanged; these are local source changes, with no push/release/install requested in this task.

Translation review, 713 relative links/50 heading fragments, and command-flag/TOML checks passed. Format and all-target strict Clippy passed; focused docs/RAG tests passed in 7 groups (52 passed, 0 failed). Actual offline docs passed 33 checks / 13 guides / 3 categories / 127 bundled links, plus explicit English catalogue/Agent Guide and Unicode-literal checks. See [acceptance](../validation/acceptance.md) for scope and limits. Work is committed locally without push, release, or installed-binary changes. Earlier sections below describe previous work.

## v0.1.6公開・常用更新完了

#19〜#30の設計・実装・検証を完了し、検証済みsource `025388c`を[v0.1.6](https://github.com/tya5/shellboy/releases/tag/v0.1.6)として公開しました。最終ローカル標準checkは76群1,296 passed / 0 failed / 24 ignored、[3 OS CI](https://github.com/tya5/shellboy/actions/runs/40712696984)はWindows 1,195 / macOS 1,296 / Linux 1,297 passed・全OS failed 0。性能用6種類を別途実行し、実HTTP+PTY 10会話・削除PTY 5項目・RAG CLI 10項目・docs 33項目も成功しました。

廃止予定の配布runnerを`42c9519`でmacOS 15へ変更し、既存tagを固定して再ビルドするdispatchを追加。[配布workflow](https://github.com/tya5/shellboy/actions/runs/40717582688)は全4 jobが成功し、全buildが同じ025388cを使ったことを確認済みです。全3 archiveの認証なし取得・checksum・同梱内容・CPU形式、macOS版のminos 14.0とversion/help/offline Guideを確認しました。

常用`~/.local/bin/shellboy`も公開0.1.6へ原子的に更新し、旧0.1.5はGit管理外へ保持。配置後SHA-256は`ed1bd720e676e1ba3d4c0f74a268327bf82d04f4a1cf74703253cf26876d0d87`で配布版と一致。既定データ・設定・稼働processは変更しておらず、再起動から適用されます。次回起動前にbackupが必要なDB移行はREADMEに明記しています。

全12 Issueへ完了コメントを残しcloseしました。[追跡表](issue-19-30-plan.md)、[判断記録](decisions.md)、[検証範囲・未実施](../validation/acceptance.md)を参照してください。以下は作業当時の記録です。

## Issue #19〜#23の追補（ローカル検証済み、未公開）

- #19: 削除成功と後片付け警告の分離、閉じた一覧の失敗通知、重複削除抑止、過去のgeneration履歴に依存しない有限候補、画像失敗の二重表示と確認キーを修正。
- #20: sourceの永続除外glob、既定dot/self-DB除外、unsupportedのskipped分類、完全scanだけのremoved判定、独立RAG schema 1→2の除外列migration。
- #21: scope別の検索実施状態・不足量・回復手順と集約警告。確認済み未準備だけのhybrid成功で、実エラーを隠さない。
- #22: source別の走査/内容更新時刻・現profileの不足chunk数・上限付き失敗一覧、failed/staleの区別、semantic dry-runの量と課金可能性。最終結果は既存shell wait/readで回収するガイド。
- #23: 0〜4096の抜粋制限とpayload_chars。既定512と既存JSONを維持し、forkの生成元/採用先を潰す重複削除は見送り。0指定の固定fixtureで出力量を測定する。

採用設計・ガイド・[判断記録](decisions.md)を更新しました。新しいCore操作・表・worker・queue・eventはありません。globsetの固定依存と独立RAGの除外列だけを追加しています。標準checkは73群1,274 passed / 0 failed / 19 ignoredで成功し、CLI 10項目・macOS PTY 5項目・同梱docs 33項目も確認しました。詳細は[acceptance](../validation/acceptance.md)。既定データ・設定・稼働process・常用binaryは変更していません。利用者の追加指示により対象を#19〜#30へ拡大し、全件完了後にpush・0.1.6公開・常用binary更新へ進みます。#24〜#30は設計・変更前測定の準備中です。


## 常用ローカルバイナリ更新（2026-10-28）

利用者の指示により、`~/.local/bin/shellboy`を公開macOS ARM64版0.1.5へ更新しました。公開archiveのSHA-256を再照合し、archive内の実行ファイルを同じdirectory内の一時fileから原子的に配置。旧binaryはGit管理外へ退避しました。配置後のSHA-256一致、version=0.1.5、help、オフラインAgent Guideが正常終了することを確認済みです。既定データ・設定・稼働processは変更せず、再起動から適用されます。以下の常用0.1.4の記述は公開直後の記録です。


## v0.1.5公開完了

検証済みsource `25d53bd` をタグ `v0.1.5` としてpushしました。#16〜#18のレビュー追補と#19の会話一覧からの削除を含みます。[native CI](https://github.com/tya5/shellboy/actions/runs/40604076661)は各71群、Windows 1,140 passed / 0 failed / 14 ignored、macOS 1,245 / 0 / 19、Linux 1,246 / 0 / 18で成功。fmt・全target strict Clippyも3 OSで成功しました。

[配布workflow](https://github.com/tya5/shellboy/actions/runs/40605371702)は全4 jobが成功し、[v0.1.5](https://github.com/tya5/shellboy/releases/tag/v0.1.5)を公開しました。Windows ZIPを含む3 archiveの認証なし取得・SHA-256一致・同梱内容・CPU形式、取得したmacOS版のversion/help/オフラインguideを確認済みです。#16〜#19へCI・公開結果をコメントしています。既定データ・設定・稼働process・常用0.1.4 binaryは変更していません。検証の詳細は[acceptance](../validation/acceptance.md)を参照。以下の未公開・未実施の記述は作業当時の記録です。


## Issue #16〜#19: レビュー追補と会話一覧の削除（未公開）

#16〜#18のレビューは`641da47`で対応しました。現在依頼の後ろの作業省略通知、対象markerを特定した復元、mode別の境界候補、実際の添付除外だけの警告、縮小を採用しなかった理由の保存を追加しています。コミット再分割をしない理由、旧履歴の入力見積もりの限界、明示summaryの扱いは[判断記録](decisions.md)へ残しました。

#19は`92a875f`と`68e2f98`へ実装しました。既存Coreの削除影響照会・削除を会話一覧のDelete／Ctrl+Dから呼びます。対象タイトルと影響を確認し、子孫削除は既定false、D／Spaceで再照会、Yで実行、N／Escで取消し。現在会話は新しい下書きへ、背景会話は一覧の隣へ移ります。遅い一覧応答・保存・画像取得で削除済み状態を復活させない整理も含みます。新しいCore操作・永続表・worker・queue・event・依存は追加していません。

最終source `31822c0` の標準 `sh scripts/check.sh` は終了0。fmt・全target strict Clippy・71群1,245 passed / 0 failed / 19 ignoredで成功しました。staged差分と公開基点以降の追加履歴のgitleaksは検出0。 macOS PTYの5項目と同梱docs 33 CLI確認も成功。[acceptance](../validation/acceptance.md)に根拠を記録しました。利用者の追加指示により、検証後にpush・3 OS CI・v0.1.5の公開へ進みます。既定データ・設定・稼働process・インストール済み0.1.4は変更しません。


## Issue #16〜#18（未公開）

2026-10-28、#16〜#18の更新コメント（途中範囲omit）を取り込み、`e8153f6`へ実装・設計・ガイド・回帰試験をコミットしました。構造移動は`5ee28ba`へ分離。共通境界検証、切断後の決定論的縮小、範囲指定spill/summary/omit・prefix trim・復元、入力見積もり、TUIの縮小概要と`/restore`に対応しています。新規table/worker/queue/event/依存はありません。

最終の標準checkはfmt・全target strict Clippy・70群1,219 passed / 0 failed / 19 ignoredで成功。CLI helpと同梱guideの実行、差分と追加2commitのgitleaks検出0を確認しました。[acceptance](../validation/acceptance.md)に根拠と未実施を記録。会社proxyの大context切断率改善・Windows実機は未確認です。省略元の要約はmarker全体単位、復元後も添付は自動再送しません。既定データ・設定・稼働process・インストール済み0.1.4を変更せず、push/releaseは行っていません。


更新: 2026-10-27。検証済みsource `60847e3` を [v0.1.4](https://github.com/tya5/shellboy/releases/tag/v0.1.4) として公開しました。#15と#4/#9/#10/#11/#13/#14のレビュア指摘、#12の復旧通知を含みます。[最終3 OS CI](https://github.com/tya5/shellboy/actions/runs/40516537195) と [配布ビルド・起動確認・公開](https://github.com/tya5/shellboy/actions/runs/40518207186) は成功しました。全archiveの認証なし取得・SHA-256・同梱内容・CPU種別を確認済みです。

ローカルbinaryも公開macOS版0.1.4へ更新し、配布版とのSHA-256一致とhelp/version/同梱ガイドを確認しました。binary SHA-256は `c014c22c4c0b9f09abd57dbbb90f576e5772dd71da48c5e2d662440df76723d4`。設定・既定DB・利用中processは変更せず、再起動から適用されます。v0.1.3と同じschema v9を使い、新設定を記入した後に旧版へ戻す際は新項目を除いてください。過去の未公開・未実施の記述は当時の記録で、最終公開結果は [acceptance](../validation/acceptance.md) を参照してください。

## v0.1.4: Issue #15とレビュア指摘への対応

#15のThread session header、任意prompt_cache_key、正確なattemptに結び付くcached_tokensのF6表示を実装しました。#4/#9/#10の文書と再描画試験、#11のHTTP error本文timeout再試行、#13の要求文脈整理、#14の正常SSE後の有限drainと会話経路のpool検証を含みます。追加レビューで見つかった使用量SQLの巨大値の一時複製、drain取消し時の受信推論欠落も修正しました。新schema・依存・worker・queueは追加していません。

最終標準checkは70群1,179 passed / 0 failed / 19 ignored、fmtと全target strict Clippy、履歴/差分の秘密情報検査が成功しました。LiteLLM/lunaの合成4要求も成功し、header有効/無効とも2回目に23,040/23,267 cached_tokensを確認しました。header単体の改善効果とは断定しません。初回pushのCIはLinux/macOS成功、Windowsの既存checkpoint観測試験1件が失敗したため、登録と更新の競合・idle機会の待機を試験側で修正しました。製品の保存処理は変えず、fmt・全target strict Clippyと対象3件が成功しました。次のWindows CIではcheckpointが成功し、別の既存管理画面試験で終了待ちだけがtimeoutしたため、fixture cleanupの共通化・直接await・進捗診断を追加し、Windowsの独立DB fixture同時数を2へ抑えました。製品の終了処理と明示的な並行性試験は変更しません。最終3 OS CIは全70群ずつ成功し、v0.1.4の公開と全archiveの実取得・checksum検証、ローカル更新まで完了しました。公開結果は [acceptance](../validation/acceptance.md) に記録しています。既定データ・設定・利用中processは変更しません。

## v0.1.3公開時の記録

更新: 2026-10-27。検証済みcommit `bbb9758` を [v0.1.3](https://github.com/tya5/shellboy/releases/tag/v0.1.3) として公開しました。#6/#7/#8、Thread一覧の状態色、起動取消し試験の安定化を含みます。ローカル標準checkは70群1,127 passed / 0 failed / 15 ignored、[3 OSのnative CI](https://github.com/tya5/shellboy/actions/runs/40482476917) と [配布ビルド・起動確認・公開](https://github.com/tya5/shellboy/actions/runs/40484109212) はすべて成功しました。全archiveの認証なしダウンロード・SHA-256・同梱内容を確認し、ローカルbinaryも公開macOS版0.1.3へ更新しました。設定・既定DB・利用中ownerは変更していません。

v0.1.2と同じschema v9・保存先・既存CLIを使い、既存設定は編集せず引き継げます。新しい設定項目を記入した後にv0.1.2へ戻す際は、新項目を除いてください。旧tag・配布物は保持しています。以下の個別変更節にある未実施・未公開は当該変更時点の記録で、公開時点の最終結果は [検証状況](../validation/acceptance.md) のv0.1.3節を参照してください。

## ローカル試用バイナリ更新（2026-10-27）

利用者の指示で、`90cf354`までの修正を含む検証済みdebug binaryを`~/.local/bin/shellboy`へ原子的に配置しました。ビルド元と配置後のSHA-256一致、help/version、同梱設定ガイドの取得を確認済みです。旧binaryはGit管理外へ退避しました。表示versionは未リリースのため0.1.3のままですが、EOF修正と#9/#4/#10〜#14を含みます。起動中のprocessは切り替えず、次回起動から適用されます。設定・既定DB・push/releaseは変更していません。

## Issue #14: ownerのHTTPクライアント再利用（未リリース）

#13を`1bcc19a`へ分けた後、Store所有の接続別client表を会話・要約・索引・意味検索・モデル一覧で共有しました。idle poolは45秒、APIキー/headerは要求ごとに解決し、SSE terminalでの終了時点は維持します。HTTP(S) proxy/no-proxyの環境設定変更は再起動後の反映です。独立RAGとテストのconstructorは維持し、新worker/queue/永続表は増やしていません。

mockの複数要求1 TCP、Control経由の再利用、設定分離、資格情報差替え、接続の再確立が成功。実binary/PTYでも、keep-aliveと通知した接続を予告なく閉じるmockに対し、同じTurnが2本目のTCPで正常完了しました。LiteLLM/lunaの合成4要求は約1.36〜1.49秒で完了しましたが、速度優位の証明とはしません。最終標準checkはfmt・全target strict Clippy・70群1,164 passed / 0 failed / 18 ignoredが成功しました。最終sourceの63.18秒outage試験とベルPTY再確認も成功しています。詳細と未実施は [acceptance](../validation/acceptance.md) へ記録します。インストール済みbinary・既定データ・push/releaseは変更していません。

## Issue #13: HTTP受信の事実と再試行判定を整理（未リリース）

#11/#12を`1a1ab57`へ分けた後、HTTP層の再送許可をResponseFactsへ置換し、Driverの1関数で対象を判定する構成へ整理しました。event分類を共有し、期限を時間管理だけにし、非stream JSON読取とGET/POST送信を共通化しました。保存形式・再試行挙動・接続clientの寿命は変えていません。

対象8群564 passed / 0 failed / 4 ignored。http.rsの製品部分746→308行、response_attempt166→95行、HTTP層の再送許可生成4→0か所です。型付き事実と通常moduleへの分割であり、全module合計行数の削減とは主張しません。最終静的検査は [acceptance](../validation/acceptance.md) へ記録します。次は#14の接続別client再利用です。

## Issue #11/#12: 長時間の再試行と復旧ベル（未リリース）

会話の自動再試行を連続失敗予算へ変更し、正常応答の採用成功でリセットします。既定8回・指数待機上限60秒、metadataのみの切断/timeoutと部分応答の切断も対象にしました。部分応答は原文を保全して会話入力から除外し、response_retry_partial=falseで後者を無効化できます。Retry-Afterを600秒まで短縮せず待ち、超過は指定時間を示して復旧します。

既定有効のui.bellは復旧CASのcommit後だけBELを出し、背景/子会話も対象に2秒で集約します。起動済み状態・停止・自動再試行待機では鳴らしません。新しい表・永続状態・worker・queue・依存は追加していません。既存broadcastはbest effortです。

対象8群562件、別枠の61秒proxy障害（実測63.15秒）からの自動復帰、実binary/macOS PTYの有効/無効BEL数1/0を確認しました。最終静的検査と検証範囲は [acceptance](../validation/acceptance.md) に記録します。続いて#13は動作を維持するHTTP整理、#14は接続client再利用として独立commitで進めます。push・release・インストール済みbinaryの更新は行いません。

## Issue #10: 接続先固有の履歴item ID（未リリース）

送信用copyからmessage/function_callの直下idだけを外し、call_id・phase・status・未知フィールド・同epoch reasoningを維持します。通常履歴・wrapper・直接role・保存済みspillに共通適用し、adapter版を2へ上げました。元DBとspill原文は書き換えません。

既存DBの再オープン、生存owner内のHTTP400復旧待ち→明示/retry、実spillの再送、旧adapter cacheの再構築を確認しました。対象6群553 passed / 0 failed / 3 ignored、最終all-target strict Clippy・fmtが成功しました。owner再起動で停止する旧Runをretryする新仕様は加えていません。同じThreadの履歴を使った新しい依頼は現行変換を使います。ローカルLiteLLM/lunaの合成2要求は正常完了しましたが、reasoningは返らず、その実provider再送とWindows社内proxyは未検証です。最終テスト結果は [検証状況](../validation/acceptance.md) に記録します。push・release・インストール済みbinaryの更新は行いません。

## Issue #4: 環境別の起動ガイドとPATH（未リリース）

設定ガイドに端末・起動元shell・モデル用shellの区別と、Windows PowerShell/Git Bash、mintty、WSL、macOS/Linuxの構成を追加しました。PowerShell 7とGit BashのTOML例、避けるランチャー、Windows版からWSL shellを使う構成が対象外であることを示します。minttyはConPTY対応があるため一律非対応とはせず、未検証の構成として案内します。

モデル用shellのPATHは自動設定される実装へ説明を修正しました。macOSの実binaryを配置先なしの親PATHから絶対パスで起動し、モデルのBashから短いCLI名でversion/docsを実行できました。ガイド単体8件と最終CLI3件、TOML例3件、相対リンク検査が成功しました。Windows/WSLの対話実機は未実施です。製品Rustコード・設定・保存データ・公開版・ローカルbinaryの変更はありません。

## Issue #9: Ctrl+Lの全画面再描画（未リリース）

通常UI・一覧・詳細・popup・選択固定中のCtrl+Lで、状態を保持して画面と差分buffer・リンク領域を無効化します。送信/改行へ同じキーを割り当てた場合は設定を優先し、helpへ無効理由を表示します。再描画予約はbool一つへ集約し、既存max_fpsで処理します。Core操作・表・worker・queue・依存は増やしていません。

実PTYで物理cursor照会待ちを検出したため、同一viewportのresize経路でclear相当を行います。最終5群549 passed / 0 failed / 3 ignoredとall-target strict Clippy・fmtが成功しました。GUI端末の視覚確認とWindows実機検証は未実施です。保存データ・公開版・インストール済みbinaryは変更していません。

## EOF実装監査とSSE framing修正（未リリース）

Windows/v0.1.2のEOF頻発報告を受け、自前parserで先頭BOMとCR単独改行によるcompleted取りこぼしを再現し、修正しました。BOMの単一completed streamは報告と同じEOF文言、CR単独はpartial EOFです。通常のcreated先行streamではBOMだけで頻発を説明できず、会社Windowsの実原因は未確定です。HTTP受信loopにEOF通知の追い越しやWindows専用parserは見つかっていません。

既存の完成条件・tool採用・retry条件は維持し、EOFに本文なしの受信/解析カウンタと固定分類を追加します。対象8群529件（重複排除）と最終all-target strict Clippy・fmtが成功しました。修正はローカルsourceのみで、公開済み0.1.3とインストール済みbinaryには未反映です。検証範囲と環境の区別は [検証状況](../validation/acceptance.md) に記録しています。


TUI、同一ownerへ接続するCLI、Responses互換通信、shell、親子の会話と実行、待機、分岐履歴、要約・スピル、添付、埋め込み索引、GC、同梱ガイド、独立RAGを実装しています。責務と対象外は [適用仕様](../design/effective-spec.md) に記載します。

## Thread一覧の状態色（v0.1.3）

取得済みの状態に応じてタイトルと状態欄を色分けしました。実行/準備/稼働はシアン、待機/停止処理中は黄色、復旧待ち/失敗は赤、完了は緑、それ以外は通常の前景色です。文字・親子/現在Threadのmarker・選択背景は維持し、狭い画面でもタイトル色を使います。一覧page更新で同じIDの状態が変わると色も変わります。Core・一覧取得/更新の契約は変更していません。対象テスト13件が成功しました。検証範囲は [検証状況](../validation/acceptance.md) に記録します。

## Issue #6: 受信記録の注意表示（v0.1.3）

途中で切れた応答のpreviewは、元Turnが現在復旧待ちの間だけ注意色にし、回復・停止・処理再開後は通常通知へ変えます。空本文の非表示、監査原文、元Turn・source ID、モデル再送対象外は維持します。スクロール位置を固定している本文窓にも最新の表示分類を反映し、通常通知の省略groupで本文を隠しません。

受信一覧は通常のRust子moduleへ投影を分け、ローカル時刻・同Turnの元依頼の有限抜粋・保存状態で見分けられるようにしました。ページ32件と原文への導線は維持しています。新しいCore状態・表・worker・event・依存は追加していません。対象6群571件と最終all-target strict Clippy・fmtが成功しました。検証結果と未実施は [検証状況](../validation/acceptance.md) に記録します。

## Issue #7: 未受信時の有限再試行（v0.1.3）

#8のHTTP期限・受信観測を利用して、会話の未受信一時失敗だけを既定2回再試行します。接続別の回数・待機上限、Retry-After、取消し可能な待機、既存attemptへの失敗/回数/重複課金可能性の記録とTUIの回数表示を追加しました。予算はTurn/generation内で共有し、tool成功やcontext圧縮では復活させません。受信済み断片、timeout、認証・形式エラーは再送しません。新しい表・worker・queue・EngineEvent・依存は追加していません。

`3569524` の標準checkは70群1,120 passed / 0 failed / 15 ignoredで成功しました。対象9群535件も成功していますが二重計上しません。その後の上限到達時のTUI段階名の補正は対象4群474件と最終all-target strict Clippy・fmtが成功しました。全体結果と最終表示差分の検証範囲は [検証状況](../validation/acceptance.md) に記録します。先行 `13b96a1` の [CI](https://github.com/tya5/shellboy/actions/runs/40449915241) は3 OSとも成功しましたが、今回#7のnative CI成功には読み替えません。利用者の設定・保存データ・利用中owner・インストール済みbinaryは変更していません。

## 公開後のWindows CI失敗の切分け（v0.1.3）

文書のみのcommit `211750e` の [CI 40420705214](https://github.com/tya5/shellboy/actions/runs/40420705214) はmacOS/Linux成功、Windowsで起動取消し試験1件が失敗しました。v0.1.2の3 OS成功は当時の記録として維持します。旧試験の外部SQLite writer解除を2.3秒遅らせるとmacOSでも同じエラーを再現でき、製品の2秒のbusy timeoutと試験の通知待ちが競合する条件を確認しました。Windowsで実際に発生した待機時間は未確定です。

起動進捗callbackの同じ呼出しで取消す試験へ変更し、実移行完了・close・owner解放・endpoint未公開を検証します。受付済みwriterの所有保持は既存storage試験で別に保証します。製品のtimeoutや取消し順序は変更していません。検証結果は [検証状況](../validation/acceptance.md) に記録します。`305e088` のpush前標準checkは全69群1,103 passed / 0 failed / 15 ignoredで成功しました。修正後のWindows native CI結果、release、Issue #3のクローズは本記録時点では未確認・未実施です。

## Issue #8: HTTP停滞対策（v0.1.3）

接続別のHTTP期限を共通化しました。会話・要約は無受信600秒（0で無効）、embeddingは要求全体60秒、モデル一覧は同設定と20秒の短い方です。ヘッダー待ち・本文・エラー本文を対象とし、timeoutには段階・経過・設定名を残します。索引は既存retry/pauseへ渡し、検索には呼出し元切断・owner停止を伝播します。TUIは30秒以上の無受信を既存footerへ補足し、途中本文は復旧待ちでも保全します。新しい表・worker・queue・依存は追加していません。

`6aeac45` の標準checkは69群1,102 passed / 0 failed / 15 ignored。その後の `32e3359` はHTTPエラー本文の非永続受信通知だけを補強し、単体・HTTP関連473件と最終all-target strict Clippy・fmtが成功しました。対象を分けた証拠は [検証状況](../validation/acceptance.md) に記載します。高推論の実proxy測定はHTTP400で成立せず、600秒は変更可能な暫定既定値です。未受信時の自動retryは別Issue #7として分離しています。既定DB・設定・実行中owner・インストール済みbinaryは変更していません。

## レビュー対応goal

v0.1.1公開後、利用者の指示でレビュー対応を再開しました。製品はこのリポジトリ、非公開台帳・旧証拠監査は独立したローカルGitへ置きます。台帳に追加指摘19件も含め、古い定型完了文を個別根拠へ直し、反証や確認不足の親項目は検証待ちへ戻して再検証しました。旧hashを公開hashへ読み替えません。

索引選択・巨大raw参照・停止receipt・検索identity・末尾cursorに加え、復旧CASとTurn作成の共通化、LiveRegistryの所有境界、Core通知の表示責務、Turn境界SQLの索引化を実装しました。TUIは保存本文の取得まで受信済み表示を保持し、中断・ツール・エラーを構造化して表示します。長い出力の省略・折返しを含め、原文との位置対応を修正しました。

`f488196` の単体試験428件が成功しました。実WezTermセルで復旧→retry、複数tool、正式な終了記録のgroupとF6原文を確認し、現行UI経路のrelease負荷測定3回と実PTY14回も成功しました。画像取得はOS側の失敗で未実施です。条件は [検証状況](../validation/acceptance.md) に記録しています。標準checkは全68群・1,058成功・失敗0、台帳の全終端gateも成功しました。219項目は217完了・Windows実機なし2環境対象外で、判断待ちはありません。レビュー完了時点ではローカルcommitのみでした。利用者の指示により、後続の追加レビュー対応を含めてv0.1.2として公開しました。v0.1.1のtag・配布物は保持し、既定設定/DBを変更する操作は行いません。

## v0.1.2公開完了

受信本文の位置・重複表示、RAG rawの一括検証走査、管理画面の詳細表示、起動診断、macOSユーザー一時領域、Quit受付後のtool起動抑止を修正しました。Issue #3は観測のreceipt除外、現instanceの終端結果の期限切れ保護、`operation get`への照会統合で対応しています。追加レビュー7項目は全件の検証を終え、判断理由は [判断記録](decisions.md) に残しています。

`~/.local/bin/shellboy`も公開済みmacOS版0.1.2へ更新し、配布版とのSHA-256一致とhelp/versionを確認しました。設定・既定DB・利用中のprocessは変更していません。次回起動から新バイナリが使われます。schema v9更新後は旧版で同じDBを開けないため、戻す可能性がある場合は起動前に整合backupを用意してください。詳細はリリースノートを参照してください。

## v0.1.1公開完了

v0.1.0以降の修正を、検証済みcommit `e55da58` のtag `v0.1.1` として公開しました。最終Responsesの採用と受信済み本文の保持、Windowsの隠し環境変数、Unix IPCの自動配置、Enter直後の送信状態・診断が対象です。3 OSのnative CIはWindows 925件、macOS/Linux各1,025件成功。Windows x64・macOS ARM64・Linux x64のrelease buildと起動確認、全archiveの公開ダウンロードとSHA-256照合も成功しました。初版のtag・配布物は置換していません。

ローカルの `~/.local/bin/shellboy` も、公開済みmacOS版を検証して配置しました。当該リリース時点の `shellboy --version` は0.1.1でした。実行中processの停止や設定・履歴の変更はしていません。CI・配布物の確認範囲は [検証状況](../validation/acceptance.md) に記録します。

## Enter直後の送信状態

ローカル試用用に `796437e` の開発ビルドをユーザーの `~/.local/bin/shellboy` へ配置し、ビルド元とのSHA-256一致、配置後のhelp/versionを確認しました。未作成だったShellboy設定へ既存のLiteLLM接続設定を引き継ぎました。旧履歴の移行や実行中processの切替はしていません。

Enterを押した時点で既存ステータス行に「送信受付中」を表示し、同じアニメーション時計で動かします。保存成功後は対応message IDを履歴で確認するまで「保存済み・表示待ち」を表示します。すでにRun/Turnが進んでいる場合はその段階を主表示に維持し、表示待ちを併記します。複数送信・経過時間・失敗時の入力保持と、通知順序の前後・高速完了・会話切替を扱います。

既存の有限診断writerへmessage_sendを追加し、draft ID/revision・実際のoperation ID・保存message IDを関連付けます。保存成功をUI通知前に記録するため、保存前の待ちと通知・履歴・描画側の問題の切り分けに使えます。本文・添付pathのログや新しいCore状態・表・workerは追加しません。設計は [実行状態表示](../design/tui-activity-indicator.md)、調査手順は [診断ログ](../guides/recovery-and-storage.md#diagnostic-logs) を参照してください。

macOSで対象8 suiteの441 passed / 0 failed / 3 ignored、strict Clippy、fmt/diff、同梱docs検証、staged秘密情報スキャンが成功しました。詳細と未実施範囲は [検証状況](../validation/acceptance.md) に記録します。修正はv0.1.1へ含めて公開しました。利用中ownerは変更していません。

## 初版公開後のストリーム修正

途中の受信JSONと最終responseの差だけで復旧待ちにする独自照合を廃止し、検証済み最終responseを採用する方針へ変更しました。途中・最終の差は保存証跡と本文を含まない診断ログへ残し、ツールは最終responseの採用成功後だけ実行します。不正な最終応答・通信失敗等は既存の復旧対象です。

受信済み回答と平文推論を復旧待ちでも参照できる表示、再起動後の保存原文、全受信記録を辿るTUI `/received` を追加しました。成功時のメタデータ差だけでは回答を二重表示しません。既存worker・表・Core状態を増やさず、Thread削除後は従来のGCで本文を回収します。

修正sourceはcommit `d170da6` です。macOSの標準チェックは全68 suite、1,008 passed / 0 failed / 15 ignoredで成功しました。最終表示コードのstrict lib Clippy、同梱docsの33 CLI確認/13 guides/3 categories/116 links、秘密情報スキャンも成功しました。詳細は [検証状況](../validation/acceptance.md) を参照してください。公開済みv0.1.0の配布物は変更していません。会社のWindows・社内API proxyによる実機再検証は未実施で、以下の初版検証結果とは区別します。

## Windows隠し環境変数の修正

[Issue #1](https://github.com/tya5/shellboy/issues/1)へ対応し、`=::`、`=ExitCode`等を含む継承環境をshellへ渡せる検査へ変更しました。空名・NUL・名前の途中の`=`は引き続き拒否し、診断へは値を含めず変数名だけを有界・escape付きで表示します。環境値の`=`やUTF-16 code unit、設定envの名前制約は維持します。

macOSで共通UTF-16組立の4試験、対象strict Clippy、fmt/diffが成功しました。Windows専用のPowerShell起動回帰もv0.1.1のnative CIで成功しました。修正はv0.1.1へ含めて公開しています。詳細は [検証状況](../validation/acceptance.md) に記録しています。

## WSL/LinuxのIPC起動条件の修正

[Issue #2](https://github.com/tya5/shellboy/issues/2)へ対応し、XDG_RUNTIME_DIRが使えない環境でもOS一時領域に専用0700 socket directoryを自動で用意します。固定hashや保存先祖先の権限への依存を除き、接続両側のpeer uid照合、モデルshellへのSHELLBOY_SOCKET伝播、既存endpoint.json経由の人向けCLI接続を揃えました。別data-dir指定やinstance不一致で別ownerへfallbackしません。

mkdir前に予定pathを記録し、locatorのみ・空directory・bind後の各段階で落ちた場合も次回起動で検証して回収します。別owner・改変file・symlink・未知の内容は削除しません。Unix保存先は0700へ限定し、親の権限は変更しません。macOSで対象5 suiteの430件、umask002/XDGなしの複合条件の追加確認、strict Clippy、同梱docs検証が成功しました。詳細は [検証状況](../validation/acceptance.md) に記録しています。Linux native CIもv0.1.1で成功し、配布物を公開しました。WSL実機・別uidの実processでの確認は未実施です。

## 初版公開の完了範囲

- package・実行名・Rust参照・環境変数・既定保存先・製品名付き識別子をShellboyへ統一しました。旧名や旧保存形式の互換は追加していません。
- 利用者指定のMIT Licenseを追加しました。既存のインストール・設定・保存データは変更していません。
- 既存履歴はローカルに保持し、公開用には当初選別した320ファイルから独立したGit履歴を作成しました。非公開資料・過去レビュー・個人の画像やログは同梱していません。
- 未統合だった復旧表示修正を別commitで取り込み、元の保存JSON・完全値のコピーを維持しました。
- 公開用ソースでfmt・全target Clippy・67テスト群を実行し、982 passed / 0 failed / 15 ignoredでした。同梱docsのオフライン確認も成功しました。
- 公開用Gitの全履歴をgitleaksで検査し、検出0件でした。詳細は [検証状況](../validation/acceptance.md)、公開範囲と判断は [公開構成](publication.md) と [判断記録](decisions.md) を参照してください。

接続ごとの `ssl_verify`（既定true）を初版へ追加しました。HTTP(S)プロキシ/NO_PROXYはライブラリの既存環境変数を使い、設定ガイドにBash/PowerShell例を記載しています。TLS変更後のnative CIはWindows 891件、macOS/Linux各986件が成功しました。配布物は3 OSでrelease buildとhelp/versionを確認し、全archiveの公開ダウンロードとSHA-256照合も成功しました。後続のWindows試験待機上限調整は製品コードを変更しておらず、別commitの検証として [記録](../validation/acceptance.md) します。

改名前の作業履歴と公開用の新履歴は分離しています。名称変更・公開準備は旧goal全体の完遂とは扱いません。レビュー対応goalはv0.1.1公開後に再開しました。

Windows/Linuxのnative検証はGitHub Actionsで実施しました。実provider API、実IME・OS clipboard、各端末GUIの再検証は今回未実施です。既知の制限は [一覧](known-limitations.md)、レビュー台帳は [運用](maintenance.md) に記載します。

## Guide follow-up (#67, 2026-10-30)

Documentation only: the delegation guide now gives the parent/user the completed
child retention decision, preview-before-delete procedure and usage/audit tradeoff;
the User Guide fixes `at least 100ms`. The English shell guide contains a macOS
Seatbelt wrapper checked through a real synthetic model shell and isolated owner,
with direct-write/network tests, owner CLI calls and process-group stopping.
The English/Japanese User Guides link to it. Linux bubblewrap capability passes
an Ubuntu branch-only probe, but its complete proxy/IPC wrapper is deferred and
no executable Linux recipe is claimed. Windows gives unverified least-privilege
account/ACL, egress-policy and VM alternatives, not a shell-wrapper recipe.
See [verified and unverified scope](../validation/acceptance.md#guide-follow-up-67).
No Rust code, installed binary, real database, real provider or Release changes.

## First-run configuration guidance (#69, 2026-10-30)

Standalone `config init` exclusively creates the bundled commented template at
the selected path and leaves existing files/symlinks intact. `config check` needs
no owner/database, strictly loads that path, reports missing defaults/credential
variable names without values, and only GETs the bounded model catalog. API-root
suffix errors and 404 explain `/v1` versus `/v1/responses`; exact template
placeholders remain unfinished setup, not a ready configuration.

The existing empty welcome and send/recovery notices show Japanese setup facts,
path, minimal example and next steps; the welcome remains visible after a
rejected unsaved first send. Rendering architecture and source/history semantics
are unchanged. Guides (including the Japanese User Guide) and installer output
use init -> edit/set environment -> check -> restart/start. Keyless loopback
configuration and precedence are documented. AGENTS existence display is deferred
because it needs worker-fed observations and overlaps #41. No schema, toolchain,
provider login, interactive wizard, real-provider tests or Release changes.
See [validation](../validation/acceptance.md#first-run-configuration-69).

## Chat Completions conversation dialect (#64, 2026-10-31)

Connections select `api=responses` (default) or `chat_completions`. One HTTP-edge
adapter maps stateless requests/replay and bounded SSE into the existing Core
contract. Storage, schema, lifecycle, tools and retry/reduction/recovery remain
shared; only common input-character diagnostics delegate dialect projection to
the client. Catalog/check results disclose API and unsupported features.

Synthetic wire and loopback checks cover text, grouped fragmented tools and tool
results/images, reported/absent cache usage, length/incomplete/EOF, controlled-clock
timeout, 503/429 retries and explicit context reduction through real Core. Saved
usage and the shared F6 field projector retain cached-token facts and unknowns.
Usage received before a missing DONE is retained on failure, without adopting
incomplete output. The full local standard check (RUST_TEST_THREADS=2) passes,
including unchanged Responses tests. Ordinary exact-source native CI remains a
separate integration gate. No provider is live-tested; Gemini/caching and vendor
reasoning/tool metadata remain unverified/unsupported as documented. Anthropic's
OpenAI facade, extra_body/cached_content, native vendor APIs and provider login
are outside this version. No installed binary or real owner/data is changed.

## Chat wire tolerance review (#64 / #68 D-010, 2026-10-31)

The edge adapter accepts complete stop-terminated tool calls, repeated full
declared shell names, missing call ids derived stably from response id/index,
and omitted choices on usage chunks. Original finish reason, source identity,
derivation/repetition/omission facts are retained in output-item metadata so
normal Core persistence keeps them. Core execution, groups, storage and retry
policies remain unchanged. Incomplete calls and missing both identities are not
repaired. Four separate synthetic traces and negative/replay/persistence checks
pass locally; standard local validation and final native CI are separate gates.
No real-provider test, installed binary or user configuration is changed.

## OSC7501 terminal status (#54, 2026-10-31)

Default-on ui.program_status publishes generic root and at most32 nonidle Thread
records through the existing terminal writer between frames. Existing
activity::classify remains the only lifecycle classifier; no conversation/status
row rendering, worker/process/poller/queue/table/dependency is added. Aggregate
counts include overflow; done/seen handling is ephemeral and acknowledges actual
selected UI snapshots. Historical completion is not invented as new unseen work.

Local unit/transport and native disposable-PTY checks cover grammar/base64/UTF-8,
controls/bidi/format removal and all byte/id limits, all phase mappings/priority,
counts beyond32, diff suppression, eviction/idle/root clear, strict off mode,
selection acknowledgment and model-shell OSC isolation. tmux3.6a isolated-parser
smoke ignores a valid OSC report without corrupting printable text. This does
not qualify receiver GUI behavior: kitty/cmux/herdr, Terminal.app and WezTerm
visual acceptance remain unverified. Standard/rebased native gates are separate.


## OSC7501 review follow-up (#54, 2026-10-31)

The projection now opens a short-lived read-only SQLite connection inside the
existing two-slot presentation blocking pool, under existing frontend read
admission. It does not enqueue projection SQL on the persistence worker. One
read transaction keeps aggregate rows, retained titles and seen pruning on a
single snapshot; the connection closes after each read. A failed projection
preserves the last published snapshot/seen identities and marks status dirty
for retry at the existing coalescing opportunity, without another Changed event.
No schema, lifecycle classifier, worker, poller or frame/drawing change is added.

Focused native regressions pass for failure/retry and 10,000 synthetic Threads
with 9,999 Runs/Turns/model attempts while the persistence worker holds an
uncommitted update. Ten read-only projections take 100.828-120.397ms each
(mean 110.796ms) on native macOS ARM64/Rust 1.99.1 debug; these are observations,
not latency gates. The full metadata scan remains O(Thread count), now off the
writer queue. The full local standard check passes 1,737/0/32, layout 93, Python 7,
fmt and strict all-target Clippy; the disposable-PTY on/off/shell-OSC regression
passes through this reader. Final rebased/native CI remain separate gates.
Physical receiver behavior is not newly qualified by this fix.


## Semantic history retrieval guidance (#71, 2026-10-31)

Agent Guide now gives five concrete uses for indexed meaning, including recovery,
repeatable work and decision rationale across conversations. It distinguishes
known-ID/current-Turn reads, query-embedding cost, global versus Thread scope,
index coverage and historical evidence versus current user authority/state.
Omission notices retain keyword retrieval and add semantic history search only
with both embedding settings. No search is executed by the notice. The subsequent required4 switch below
controls automatic indexing independently of these retrieval suggestions. The one-week manual observation plan is recorded
in issue71 comment6535408537; rollout, not merely main CI, starts the window.
Native macOS ARM64/Rust1.99.1 focused tests and full local standard check pass
(1,739/0/32, layout93, Python9, fmt/strict Clippy). New tests use60-second watchdogs
only, never latency assertions. Final rebased source and three-OS CI are separate
gates. No paid provider query, real user data, new telemetry or worker is involved.


## Independent history-index switch (#71 required4)

indexing.auto_history is a strict bool, default true. False blocks new automatic
node/raw authorization and holds already pending automatic jobs/bootstrap inputs.
Existing vectors stay queryable; explicit backfill/rebuild/probe, prepared-failure
retry and standalone RAG remain available. No worker/table/queue is added.
HistorySearch returns an items/auto_history/degraded/scope_status object (not a
bare array), exposing potentially missing newer sources. Keyword is unchanged.
Index status and config check show the flag; one-week observation decides whether
to change its default later, not whether to remove embedding configuration.
Native mock tests and full standard check pass; actual usefulness still needs
the post-rollout week. Active-index-only deletion has no existing CLI; the guide
states inactive-space GC limits rather than recommending unsafe DB deletion.

## #41 D2-only latest-main integration (local, 2026-10-31)

37e0929 integrates reviewed D2 through4ce6913 with main7393ccb. E/F/G source
and migrations15/16 are absent; conversation schema remains14. #71 index policy
and #73 bounded summaries remain intact. Exact-source standard recheck passes
1827/0/32, layout93/Python9/fmt/strict all-target Clippy. The first standard
failed an existing parent-shell PTY diagnostic assertion after durable cleanup;
the same-source focused rerun and full standard rerun pass. No causal fix or
intermittent-failure diagnosis is claimed. Main three-OS CI and current-source
long measurements remain unverified; no push or installation was performed.

## Index-status review follow-up (#71)

IndexStatus defaults its additive policy fields on decode only (auto_history
false, held_auto_history zero); actual owner configuration remains default true.
Status separates automatic pending authorizations/bootstrap/jobs held by the off
policy into held_auto_history, leaving explicit pending and running/failed/done
unchanged. Space selection retains its prior scope. No queue/schema/worker,
indexing admission, history-search output shape or installed binary is changed.
Focused mock and wire tests pass; final standard/rebased and native CI are
separate gates. Existing seven-day observation remains a reviewer rollout task.

### D2-only latest mainb581283 qualification

2e6973d retains latest #71/#73 corrections and reviewed slow-runner wait
watchdogs, with no E/F/G or migrations15/16. The new exact-source macOS standard
exits0:1835/0/32 (lib915/integration910/termination7/crash3), layout93/Python9/
fmt/strict all-target Clippy. Both native parent-shell cleanup variants pass.
This does not explain the earlier intermittent missing-signal diagnostic.
Local main contains D2 only; push and main three-OS CI remain unperformed.

## Shared test watchdogs (#77, 2026-10-31)

Non-#41 fixture guards now use tests/support/watchdog.rs: one60-second budget,
label/call-site diagnostics, synchronous barrier/PTY guards and explicit virtual
clock tests. The classifier, product deadlines/graces, schema and dependencies
are unchanged. File groups are committed independently. Short quiet windows use
post-operation channel/connection counts or explicit blocked-future polls;
attachment preparation now waits for existing Changed notifications. Native PTY
capture waits for actual labels/colors, drains output while awaiting saved index
state, and does not infer idle from a short no-output window.

Five native force-kill milestones exercise saved-result retention, uncommitted
rollback, interrupted/not-started repair and idempotent reopen. Unix SIGKILL was
qualified locally; the Windows Job-object path awaits native branch CI.
The standard layout gate now runs the lexical test deadline policy and its
reasoned, fingerprint/count-bounded allowance list.135 existing occurrences in
#41-owned files remain deferred, not fixed;14 deliberate shared-guard, injected
clock or non-gating measurement occurrences are documented separately. #41 was
notified; only minimal cfg(test) lib-module wiring overlaps its current files.
Local targeted tests and Python mutation tests pass; final full/rebased checks,
two complete native branch runs and main CI are independent pending gates.


## Unix owner-death shell lifetime (#82, 2026-10-31)

A per-execution native guardian holds a close-on-exec owner lifeline and a status
socket, reserves the execution PGID with its live PID, and waits outside both
model and owner groups. EOF requests group KILL. A short-lived setup child is
reaped before publication; no runtime worker, queue, table or dependency is added.
The owner and guardian each retain two FDs per execution; launch-only descriptors
and native ownership/cleanup bounds are recorded in the Shell design.

Unix restart records interrupted active shells as failed with
previous_instance_interrupted_os_state_unverified, without inventing exit/signal
or confirmed group cleanup. Existing terminal outputs and Windows startup/Job
handling are unchanged. Normal owner Ctrl+Q/SIGTERM routing and TERM/grace/KILL
policy are preserved; raw shell status/reap acknowledgment is native-adapter work.

Native macOS mock/real-process tests pass for two independent shell groups under
owner-PID and owner-group KILL, real owner restart with no replay, natural exit /
late stop, exec failure/script fallback, and normal signal/PTY shutdown. Direct
Bash signal/exit oracles also pass. Full local standard checks have passed; final
rebased checks and Linux/macOS/Windows CI remain separate gates. No installed
binary, real owner/data, release or paid provider is used. Escaped groups and
stop-time guarantees remain outside scope; no stored-PID cleanup is attempted.

## Typed slash commands with pasted arguments (#84)

The composer tracks a transient manually entered leading prefix. A complete
typed command name still dispatches with pasted arguments; pasted names remain
literal and show a keyed warning/retyping hint. Long paste folding and explicit
send remain unchanged. Core, protocol and persisted draft formats are unchanged.
The minimum version omits a confirmation dialog and per-character provenance;
reloaded/recalled source text stays conservative. Local standard checks and
eight native PTY sessions pass; native CI/integration are tracked in #84 and
acceptance, separately from unmeasured P1 latency and physical terminals.

### #83 conversation rows: local qualification complete, review pending

Single EAW=N mark/label/color and spacing table; live/saved background shell
facts and saved-start clocks; factual notifications and Enter source navigation.
No Core/protocol/storage or #86 region rendering change. Standard check passes
1,887 Rust tests / 0 failures / 33 ignored; actual final native PTY passes
8 sessions / 56 completed screens across 80/140, dark/light/NO_COLOR/vt100.
Incremental P1 against main e933054: worst p95 5.135ms, largest delta1.747ms;
all three interleaved pairs meet 8ms/+2ms. Details and failures are in acceptance.
Branch three-OS CI/review/main admission are not claimed by local validation.
Missing names and off-window source lookup are explicitly deferred to #88.

## Thread observation and #82 review follow-up (#74, 2026-10-31)

The separate #82 follow-up binds startup shell snapshot facts instead of SQL
source replacement. Inclusive close_from(2) clarifies that guardian stderr is
already closed; regression tests cover EOF before guardian release and normal
release/reap/output-drain completion. Unix snapshot failed/unverified JSON and
non-Unix stopped/no-OS-claim behavior are asserted directly.

Thread wait adds64-target/64-change bounds, explicit settled-state filters and an
instance-independent scoped cursor. Indexed Run/recovery streams retain finish/
restart and recovery/resume transitions between calls. Root recovery retains the
same existing fact without delivery. ThreadList/Get keep excerpt and separate
own-final-answer from received-input/sender facts. Schema16 adds only four lookup
indexes. #80/#83 main work is preserved; no rendering or composer logic changes.

Focused SQL-plan, gap/restart, cycle, schema-upgrade, CLI and loopback observations
pass during implementation; final combined standard/native gates are reported
by exact SHA in issue74, separately from these focused results. The first version
omits a subscription/UI watcher, idle-without-Run state and fork-only ancestor
answer expansion. Excerpts bound returned text/fragments, not all JSON parsing.
No paid provider, installed binary, owner restart or user data is used/changed.
