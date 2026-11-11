# Compaction Redesign: Design-Only Review Packet

Baseline4e6241a9a3235bb6ceffb03574ae983c5919765b; authoritative file SHA256
b822f2b54f8cb5940bc8e0e65dc7287818b1b55457c629fd655eb12b3c2e1a61.
Its complete text0-13 is verbatim in docs/design/compaction.md. USER decisions,
Claude recommendations and to-confirm facts retain their original labels.
The following branch/test identifiers are implementation PROPOSALS, not existing
functions or executed tests. Independent S6 review must precede product edits.

## Superseded Historical Sections

| Existing section | Superseded by | Scope / retained contracts |
| --- | --- | --- |
| #125 proactive threshold / #153 / #180 | new2,3,6,9 | Remove witness/marks/progress block and open-loop suppression. Retain fresh provider report and safe between-request boundary. |
| #181/#182 active middle/full split | new4.1 | One first-work-to-first-unsafe rule; user messages inside range included; current request preserved. Keep epoch semantics distinguished by reaching latest group. |
| #209 calibrated target | new1,2,7,8,9 | Remove paired-character estimator, target plans/verifications and below-threshold continuation. Target only compares #A. Old303 percentage implementation NOT carried forward. |
| #215 selection evidence/four-stage escalation | new2-4,6 | No target-limited spill, no prefix_adopted order flag; one summary range/operation per trial. Closed spill-only / selected-region spill first. |
| #73/D-017/#231 bounded map/reduce | new4.2,4.3,9,10 | Single request+context-rejection halving replaces segments/parts/reduce rounds. Drop5%/2000 floors; oversized but shrinking summary adopted+recorded. Keep transient request retry and typed refusal/nonshrink safety. |
| #226 recoverable records / #230 post-compaction usage | new1,2,6,7 | Keep recoverable qualified attempt facts; change evidence semantics to #A and report-at-most-once. Old progress witnesses/target verification not authoritative. |
| #271 spill shortfall | new2.2,9 | Remove continuation below threshold and size-estimated widening. |
| #300 append-only adoption | new5 | Retain nonappend generation/prefix/tail proof and atomic fresh CAS. |
| #301A reason recording / #301 body | new5 | Keep typed content-free observation; retire map/reduce labels with code. Review each dependency; do NOT delete guards wholesale. |
| #50 overflow recovery selection | new10 | Omission priorities and final-summary ordering remain unchanged. Final range is path start to first unsafe group or before last group; remove segment_chars remainder condition; common4.2 summarizer. |
| Receipt/source retention, restoration, audit, reasoning rules | no blanket override | Preserve except where an explicit new rule changes them. No quality gate or paid efficacy study added. |

## Rule -> One Branch -> One Test Mapping

All tests below are planned, not run; reuse existing fixture files where possible.
"decide" denotes ONE central pure decision function, not scattered stage flags.

| Rule | Proposed branch / existing seam | Planned case |
| --- | --- | --- |
| 0.1 provider facts only | decide.report; context_usage | paired-character evidence never used to decide or size removal |
| 0.2 no efficacy measurement gate | decide inputs only report/path/config/#A | behavior identical without workload latency/benefit evidence |
| 0.3 no new tables | #A/read witness existing records/app_meta | migration diff contains no new table; unknown evidence reported |
| 0.4 quality loss allowed | single_summary.adopt | no original-reachability/quality/keep-heading evaluation gate |
| 0.5 cache efficiency out of scope | single_summary.request | no part-cache lookup/write; retained old cache bytes unchanged |
| 1 definitions / #A scope | read_a same Thread/connection/model | absent=0; scope changed=0; first completion after most recent trial, including no-op |
| 1 manual #A inclusion [Claude recommendation] | read_a.manual_adoption | adopted manual compact starts boundary; failed manual does not masquerade as adoption |
| 2.1 no report/stale | decide.no_fresh_report | missing tokens and post-adoption pre-report do nothing |
| 2.2 below threshold | decide.below_threshold | no old target-unmet exception |
| 2.3 one trial/report [recommendation] | decide.report_already_tried | adopted/no-op/failed/discarded all consume same attempt-report witness |
| 2.4 deterministic failure stop | decide.same_range_failed | unchanged canonical ordered source IDs block, changed range permits |
| 2.5-6 #A<=target | decide.spill_only | #A=0 first trial and equality select closed spill only |
| 2.5-6 #A>target | decide.summarize | no removal estimate; closed eligible else active else no_target |
| 2.7-8 record/refresh | trial.finalize + driver refresh | trial attached to triggering attempt, immutable scope evidence; adopted snapshot reread |
| 3 eligible spill | policy.spill_ranges | tool/safe/not-latest/not-spilled/strictly-smaller each negative case |
| 3 spill-only scope | trial.spill_closed | active untouched even with open tool loop |
| 3 summary scope | trial.spill_selected_region | spill all eligible in chosen region before one summary; active spill only active branch |
| 3 boundary | driver no-inflight/all-groups-complete | no call-result split; latest/incomplete/protected group not spilled |
| 4.1 closed range | select.normal_prefix | min(normal_boundary,safe_prefix), no unsummarized node=>no target |
| 4.1 active first-work | select.active_prefix | after request, first tool/spill/summary marker; stop at first unsafe group |
| 4.1 user/protected | select.active_prefix | include ordinary user messages inside range; protected node stops before its whole group |
| 4.1 latest/epoch | select.reaches_latest | include latest group as existing plain round; no epoch advance if reached, full reasoning drop if not |
| 4.1 no work marker | select.no_active_target | user/summary-only without required tool/spill marker remains no target per authoritative wording |
| 4.2 one request | single_summary.whole_range | no map/reduce/partcache/numeric round cap |
| 4.2 context rejection | single_summary.halve | only existing context classifier; deterministic maximal half projection at group boundary/min1 |
| 4.2 non-context error | single_summary.request_retry | network429/server keep existing request retry; no halving |
| 4.2 strict termination | single_summary.halve_stop | success ends; refused1group ends; halves losing closed-unsummarized/active-marker eligibility end |
| 4.2 success after half | single_summary.done | do not summarize remainder in same trial |
| 4.3 output request | summary_prompt.output_cap | Thread->connection->default20000, no5%/2000 clamp |
| 4.3 output adoption | single_summary.nonshrinking | projection>=original fails; smaller over cap adopts once and records exceeded without resend |
| 5 atomic/#300 | SummaryAdoption.adopt_tx | append only accepts; nonappend/ABA/source changes reject atomically |
| 5 #301 dependencies | adoption_validator dependency cases below | typed exact rejection or justified acceptance per rule, not blanket guard removal |
| 6 no local target | decide.no_target | no provider request; trial counts/no-op #A boundary retained |
| 6 definitive failures | trial.deterministic_range_failure | nonshrink/refused/empty/one-group-overflow hash source IDs; no unchanged-range retry |
| 6 transient failures | trial.transient | cancel/RunTurn/network429/server do not create permanent range block; next report can try once |
| 7 bounded #A | read_a | query bound/index proof plus no hidden zero on incomplete evidence; unresolved below |
| 8 settings | settings.resolve | threshold unchanged, percentage only #A, summary cap; names/ranges pending explicit choice |
| 9 removals | obsolete API/schema/config removal | no compatibility shim; old paid part-cache data not deleted |
| 10 shared summarizer [recommendation] | manual/range/overflow callers -> single_summary | same4.2 halving/adoption; overflow path-start prefix stops before first unsafe or latest group; omission priority/stage unchanged |
| 11 tradeoffs | docs/attempt record | no token-goal guarantee, no latency guarantee, no extra retry/quality gate |
| 12 sole #A check | source report below | feasibility unproven; other four checks canceled, no measurement gate |
| 13 rollout | design->independent review->code->cloud qualification | new migration separately assigned only if required; USER/Claude installation only |

## Sole Required Pre-Implementation Check: #A

USER update b822 explicitly CANCELS investigation/measurement of window fit,
overflow rejection timing, protected-node positions and request duration. None
is a gate, prerequisite, question or future required task here. Historical
source notes about those four are superseded and removed from this review.

Existing context_usage::latest_query returns latest rather than first completion
after latest trial/manual adoption. bounded.rs caps recent turns33 and
replacements201: that display optimization is NOT a proven exact #A query.
Indexes attempts_by_turn, attempts_completed_scope, automatic_compaction_turn,
turns_by_thread and thread_replacement PK are candidates, not EXPLAIN proof.
Result: exact bounded #A WITHOUT new tables/columns is UNPROVEN, not proven
impossible. No query implementation, table/column/index or migration introduced.
Next permitted source work is a bounded query design and index/order proof using
existing records. If it cannot represent trial/no-op/manual chronology and scope
exactly, STOP and report; do not manufacture missing=0 for incomplete evidence.
No private DB/config read or paid/model measurement is authorized or performed.

## #301 Dependency Review (Decisions Still Require Proof)

| Existing guard | Protected dependency / authoritative new rule | Proposed disposition for review |
| --- | --- | --- |
| Thread unavailable/deleted | cannot publish into deleted owner path,5 atomicity | Retain existence/deletion fence. |
| Selection revision | exact selected prefix / append-only300 | Retain nonappend/prefix/current-tail CAS proof; revision alone must not reject permitted append. |
| Source revision/hash/pruned | summarized input is same and originals retention | Retain source identity and necessary prune safety; no quality/reachability gate added. |
| Profile/connection/model/effort | actual generation input and #A scope reset | Trace captured request input vs adoption dependency separately. No whole-profile-revision deletion approved; unrelated setting change may be irrelevant only after dependency proof. |
| Reasoning epoch | replay projection/input used by summary | Retain where generated source/active reasoning depends on epoch; exact partial/reaches-latest rule must be tested before relaxation. |
| Run live/start/stop | cancellation ownership vs selected input | New6 transient-discard policy does not prove Run identity necessary or unnecessary. Retain until input/owner/delete/cancel dependency traced. No unconditional deletion. |
| Turn generation/start/stop | request_boundary/source/active consumer | Same: map to actual active range/request boundary and owner generation. Candidate irrelevant transitions need explicit proof and branch test. |
| Cancellation in-flight/pre-adoption | stop work vs revoke publication | Preserve all until producer/owner cleanup dependency proof; no short-transaction argument substitutes for it. |
| #301A closed reason | public content-free cause/elapsed | Preserve known reasons and legacy unknown; map/reduce-only labels retire with removed functions, not retrospectively relabeled. |

## Public Surface / Persistence / Open Decisions

- Remove obsolete segment_chars/reduce_rounds overrides from Thread CLI/get,
  protocol DTO/schema, config and config-edit; remove map/reduce budgets and
  summary_max_percent. Agent Guide/user docs must state breaking pre-release
  removal; USER/Claude checks actual config before installation, not this task.
- Shared manual/range/overflow summary results, compact list/status history and
  discard enums need explicit obsolete-field inventory during implementation;
  schema generated only on reviewed source when cloud resources are available.
- Percentage and output-cap names/ranges have NOT been re-adopted from old303.
  Candidate names auto_compact_target_percent/summary_max_chars match existing
  conventions, but accepted integer bounds/rounding require leader decision.
- Current fresh main user_version42; old3030043 exists only in historical WT.
  New settings persistence is unresolved under the no-added-column constraint; NO DDL/column/version
  is created here and no migration is reserved. #A must first be proven from
  current records. A new table is expressly forbidden.
- Manual compact in #A and unified summarizer across manual/range/overflow are
  Claude recommendations in the source, not rewritten as USER decisions.
- Overflow range is USER-fixed: path start through prefix before the first unsafe
  group or before the latest group. Remove segment_chars remaining-size checks;
  keep omission priorities and final-summary stage ordering; use common4.2.
  No alternative-range comparison or new exception is proposed.
- Clarify before code: reliable latest-compression chronology (including no-op,
  manual adopted/reverted and connection/model changes), behavior for incomplete
  bounded evidence vs mandated missing=>0, exact identity of a "report value"
  (recommend attempt/report identity, not equal numeric token count), and config
  names/ranges. Do not resolve by silent fallback, hidden schema or new gate.
- Protected group data retention/reachability remains existing safety structure;
  do not turn it into summary quality assessment. No benchmarks, quality score,
  provider-window-size guess, cache optimization or extra retry cap is proposed.
