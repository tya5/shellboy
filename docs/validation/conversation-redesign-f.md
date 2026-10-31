# F Watch and descendant-count qualification (#41)

This record qualifies specified F sources/workloads, not G or transferred
#85–#87/#90 work. The [requirement ledger](../development/issue-41-plan.md)
and [adopted design](../design/conversation-redesign.md) define the retained scope.

## Functional and native observations

F67cc688 ordinary3OS CI41182628647 succeeds on Linux2017/0/34,
macOS2016/0/35, Windows1885/0/26. Artifact source_sha and actual scoped-Watch
CLI and paired-snapshot harness lines were checked on all3. Actual native
macOS PTY Watch operation passes8 conditions/56 captures:80/140 columns,
dark/light, NO_COLOR and vt100; actual CLI parent/child creation, active-to-
unread completion, preview, Tab List/Watch, Esc close and normal /quit exit0.
The #84 typed/pasted command regression separately passes8 conditions/80 captures.
Physical Windows terminal UX is not implied by functional CI.

Explicit native P4 at742d523 (same code as67cc688):30 samples after10
warmups at each depth;256 descendants. Depth1 p50/p95/max3.415/3.607/3.678ms;
depth8 3.822/4.078/4.126ms. Both satisfy50ms on this host; they are not
other-OS values. The optional million-sibling stress remains unexecuted.


## Cancellation and allocation follow-up

Source950c9ed has a successful isolated-target native macOS standard check:
2,019 passed/0 failed/35 ignored (lib1072/0/15, integration928/0/17,
termination12/0/1, forced7/0/2), Python18, format, strict all-target Clippy and
layout/watchdog gates. All three new cancellation tests are explicitly present
in both the compiled inventory and successful harness output. A prior shared
target run reused the67cc688 unit artifact (1069 passes, missing those tests);
that run is discarded as evidence for950c9ed. The earlier focused249/0/3 run
proved the two then-existing cancellation checks, not the later partial-consumer
SQL case. That third case now passes and observes one actual root-count SQL
while a cancelled Watch part is omitted. Gitleaks67cc688..950c9ed: no leaks.

The F branch advances to950c9ed and starts exact-source ordinary3OS
CI41187246963, now successful on all3 OSes. Main remains unchanged. CI41186128587 separately runs
formal P2/P5 before d0a92bf/after67cc688 on Linux/macOS80/140 with3 P5
producers (P2 one). Watch has no baseline counterpart and is explicitly a
candidate-only observation. Native Windows performance stays unavailable.
Local2s smoke validates all8 available scenarios, actual producer counts and
source/binary/harness witnesses: P5 draws/UI writer jobs/readonly opens all0;
these short windows do not qualify long-window P2/P5.

### Requested-live heap characterization, source67cc688

A private release example (no cfg(test), dev-dependency feature union) wraps
System GlobalAlloc and measures allocator-requested live bytes at object
boundaries. It uses actual WatchState/ThreadBrowser methods, not estimated
String lengths. Fixture:2,000 rows,160 emoji title/parent,512 emoji recovery,
160 emoji answer;36-byte synthetic identities and short synthetic timestamps.
Full Browser loads40 pages of50 and actually draws/scrolls every row through
140x40 TestBackend, then retains2,000 seen identities. Two cycles are observed.
An earlier unrelated-full-unit-LTO probe was interrupted before producing any
result; it is not a successful sample. The dedicated example binary witness
is341c5407c07605d1138841dde3b69bc80fc7f80c38cb53dda0b7b6c17d7d6b2c.

| Allocated component/stage | Observed requested-live delta, bytes |
| --- | ---: |
| WatchState metadata |5,230,576 in both cycles|
| WatchState after details/seen |21,570,272 in both cycles|
| WatchState closed cache |21,570,272; bounded entries remain cached|
| Empty WatchState reopen, seen retained |365,920 in both cycles|
| WatchState dropped |0 in both cycles|
| Full Browser metadata |10,142,336 in both cycles|
| Full Browser visited details + seen + two draw buffers |27,709,696 /27,691,400|
| Full Browser closed, buffers still held |27,539,064 /27,520,768|
| Full Browser and buffers dropped |18,296 first cycle /0 second cycle|

The first full-draw residual is unattributed process allocation; the next cycle
shows no additional residual. This does not prove absence of every leak. These
values are a specified cache-shape observation, not RSS, allocator usable size,
a hard bound for all work/string shapes, whole owner/UI-state heap, or native
other-OS evidence. The64KiB source-witness budget never claimed total Watch
memory. No allocator hook, runtime API, dependency, cache policy, new worker or
queue is added to the product. Raw source patches/manifests/binary/logs remain
local; no user data, clipboard or paid endpoint was involved.

User coordination6541457457 permits Shellboy #85–#87/#90 to start before F/G.
The later side rebases the named small UI overlaps; G uses admitted #87 as its
base, and F adapts startup fixture lines if issue5's shared owner assembly lands
first. F/G main gates and the narrowed #41 scope remain unchanged.


## Native P3 actual CLI and combined-consumer work

Source before d0a92bf; after67cc688. Native macOS140x40 PTY, normal release
binaries without cfg(test) or dev-feature union, using the same CI-only
connection/job counters as the smoke/long fixture. Each independent case uses
50 saved Threads,128 selected-path nodes,50 successful tool results with
50x64KiB retained raw output, and one actual Thread descendant. Live Run/Turn
facts are synthetic read fixtures without model drivers; this does not qualify
model lifecycle execution. Four real CLI title-update warmups precede16
sequential real CLI mutations, each observed in the selected header and the
visible browser row or parent label. No provider call occurs. Dataset counts
are canonical saved counts, not a claim that every raw byte is rendered.

| Terminal Program Status | Source/mode | Shared snapshots | Separate status reads | UI writer jobs | Aggregate RO lexical work ms / mean per mutation ms |
| --- | --- | ---: | ---: | ---: | ---: |
| Default on | Before List |16|16|0|108.682 /6.793|
| Default on | F List |16|16|0|88.542 /5.534|
| Default on | F global Watch |16|16|0|83.146 /5.197|
| Default on | F scoped Watch |16|16|0|109.963 /6.873|
| Explicit off | Before List |16|0|0|56.761 /3.548|
| Explicit off | F List |16|0|0|79.701 /4.981|
| Explicit off | F global Watch |16|0|0|83.602 /5.225|
| Explicit off | F scoped Watch |16|0|0|108.887 /6.805|

Default-on totals include the existing separate terminal Program Status
consumer; call-site counters identify16 snapshot.rs and16 program_status.rs
opens. Its #54 conditional dirty/tick policy is preserved. Explicit-off cases
isolate the shared selected/List-or-Watch/count snapshot: one actual readonly
connection/read task per mutation. All cases have exactly16 separately
classified Store::execute CLI mutation closures, not UI writer jobs. Readonly
time is lexical connection lifetime, not pure SQL or per-read p95. RPC-start-
to-drawn-facts nearest-rank p95 across16 samples ranges530.245–550.858ms.
This is not precise commit-event latency, physical frame latency or a speedup
claim; different mode/features and host-load variation remain explicit.

The first baseline-readiness attempt required F's renamed list heading from E;
its saved screen showed the valid older heading. It failed before any window
and has no metrics. The corrected controller accepts both actual headings.
Initial default-on32 total reads were traced to16 shared +16 preexisting
Program Status reads, not called a duplicated shared snapshot or silently
removed from total cost. P3 does not qualify later integrated-source changes;
those retain their own standard/CI and any additional workload evidence.

## Latest-main integration gates

The F preparation now includes main9b4ae34 (#76 usage/cooldown and #92 exact-
SHA CI reuse). These main changes retain their authored implementation. F's
parent index remains17 after checking main's16 ledger. New combined-source
standard/native CI and reviewer admission are separate from all evidence above.
Do not rename earlier source hashes to the new integration commit.


## Formal native P2/P5 comparison, run41186128587

Manual CI compares E d0a92bf with F67cc688, producer1 for P2 and producer3 for P5. Each Linux/macOS width runs on the same runner, sequentially. All32 available Unix windows have valid fixtures/source/binary/harness witnesses, full requested duration, exit0 and no window model requests. Windows performance is explicitly unavailable, not a pass. CPU is process RUSAGE_SELF percent of one core; results are observations, not speedup claims.

| OS/columns | P2 CPU before / F | F Watch CPU / draws | P5 hidden CPU before / F | P5 List CPU before / F | F hidden Watch CPU |
| --- | ---: | ---: | ---: | ---: | ---: |
| Linux/80 | 3.77033 / 3.77156 | 3.78386 / 600 | 7.29593 / 7.38648 | 7.29765 / 7.56725 | 7.37251 |
| Linux/140 | 3.81142 / 3.81588 | 3.83549 / 600 | 7.98942 / 7.27279 | 8.14751 / 7.40454 | 7.91009 |
| macOS/80 | 0.33397 / 0.26241 | 0.35435 / 551 | 3.83993 / 3.93900 | 3.88507 / 4.55461 | 4.82305 |
| macOS/140 | 0.37605 / 0.36209 | 0.33549 / 473 | 3.26414 / 3.54596 | 3.56914 / 3.53822 | 3.38308 |

Every P5 case draws0 frames at an actual committed-output rate>=40/s (Linux approximately150/s; macOS53.71–55.64/s). Every window has0 UI writer jobs,0 readonly opens and0 UI read-task admissions. Normal P2 draws600 frames; maximum fixed-one-second draw bin is1 after F and1–2 before. Candidate-only Watch has no E counterpart: Linux600 draws, macOS551 at80 columns/473 at140, maximum bin1. Fewer draws are not reported as a missing minimum-frequency requirement.

These measurements qualify67cc688, preceding later cancellation/main integration and Watch presentation fixes. They are retained as original evidence and will not be relabelled to a later SHA.

Published950c9ed CI41187246963 artifacts verify Linux2,020/0/34,
macOS2,019/0/35 and Windows1,888/0/26. All3 cancellation regressions,
paired-snapshot and scoped-Watch CLI tests have actual successful harness
lines at the exact source SHA on every OS.


## Requirement evidence map before final admission

This maps retained F ledger rows to concrete evidence; it does not authorize
main admission. Latest combined-source standard/native CI and reviewer approval
remain explicit final gates rather than inferred from earlier successful runs.

| Ledger | Delivered path and evidence |
| --- | --- |
| F1 | Existing-browser List/Watch/Tab, filter/selection and descendant shortcut tests in `ui::threads::watch_integration_tests`; actual eight-condition native mode-switch captures. |
| F2 | Prepared flat categories/parent details; stable-order tests in `ui::watch`; new80/140 long-title/command field-priority regression and real WezTerm window review. |
| F3 | Run/answer identity, seen-LRU/uncertainty and close/reopen tests in `ui::watch`; no table or TTL. |
| F4 | Shared readonly `view_read`/snapshot broker, exact same-sequence CLI integration and native P3 cases above; all-closed/pre-open/partial-live cancellation regressions verified on all3 OSes at950c9ed. |
| F5 | Visible-only factual clocks and order tests in `ui::watch`; formal P2/P5 above; viewport-neighborhood formatting regression exercises2,000 rows and actual Home/End/Page drawing. |
| F6 | Bounded command/recovery/answer reader, sanitize/oversize tests and specified requested-live retained-memory observation above; source budget is not total heap. |
| F7 | Exact Run/generation action guards and actual separate-CLI scoped-Watch recovery, retry replacement and stop lifecycle integration. |
| F8 | Readonly one-statement BFS/count fixtures: unowned Runs, depth8,256 discoveries, cycles and category counts; explicit native P4 observation above. |
| F9 | Deleted-bridge/missing-root/retained-grandchild and truncated lower-bound tests in `frontend::lineage`/`watch_page`; actual retained records, never fabricated descendants after GC. |
| F10 | `lineage` affected-root/new-frontier tests and `lineage_observer` notification-during-read, hidden-scope, overflow, lag and failed-read tests. |
| F11 | Scoped cursor/watermark/root/filter tests in `watch_page`, runtime late-scope rejection, actual CLI child creation updates parent counts. |
| F12 | Main observation index16 retained; only F parent lookup index17. Fresh-schema/upgrade/rollback/reopen tests and backup guidance; numbering rechecked at main2b3cd91. |
| F13 | No G models, new table, worker or queue. Raw saved facts remain shared; session/discovery state is frontend-owned. Readonly snapshot ownership and projection bounds are in the adopted design. |
| F14 | Exact-source standards/3OS CI above; #71/#54/#77/#83/#84 retained. Main2b3cd91 owner-session extraction retained; its only F difference is the migration-fixture index drop. Final integration qualification pending. |
| F15 | Source-qualified native P3/P4 and formal long P2/P5 above. Later presentation/integration source is not relabelled; final follow-up and unresolved strict P2 before-bin/CPU interpretation remain visible to review. |
| F16 | Independent English TUI/Agent and Japanese User Guide, help/completion; native8-condition PTYs and actual80/140 WezTerm images. Final3OS candidate/main gate and reviewer approval remain open; physical Windows UX unverified. |

Real macOS WezTerm80x40 and140x40 captures use an isolated synthetic failed-run
fixture and a frozen binary built from the Watch layout source. Long Japanese/
emoji titles are ellipsized while the leading state, factual state age, parent
and footer remain visible. Both actual images were reviewed; these captures do
not substitute for live-shell lifecycle or Windows terminal evidence. No paid
provider, user's database, clipboard or running terminal session is used.
