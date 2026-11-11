# Compaction Redesign: Design-Only Review Packet

Baseline4e6241a9a3235bb6ceffb03574ae983c5919765b; authoritative file SHA256
492ce66f174a20018830c0c16743813498277c6f1a7444353a9cddf0164e3ba2.
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
| #50 overflow recovery selection | new10 | Shared summarizer changes; failure-driven stage ordering/maximum omission remain outside scope. Replacement for segment-based range needs review, not a new guessed constant. |
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
| 10 shared summarizer [recommendation] | manual/range/overflow callers -> single_summary | same halving/adoption path, overflow stage ordering unchanged |
| 11 tradeoffs | docs/attempt record | no token-goal guarantee, no latency guarantee, no extra retry/quality gate |
| 12 checks | source report below | UNKNOWN not promoted to proof or approval for paid tests |
| 13 rollout | design->independent review->code->cloud qualification | new migration separately assigned only if required; USER/Claude installation only |

## Five Required Pre-Implementation Checks (Source / Saved Evidence Only)

| Check | Actual source/evidence | Result / unknown / needed evidence |
| --- | --- | --- |
| 12.1 Full request fits window including prompt | compaction.rs summary construction; parallel.rs formatting; KEEP_INSTRUCTIONS and retained-round additions; connection/model request serializers | UNKNOWN for each actual connection/model/range. No tokenizer or trusted window guarantee in inspected source; existing segment bounds are being removed and do not prove full request fit. No token-from-character estimate permitted. Halving requires an actual context rejection, not guessed preflight. No paid request made. |
| 12.2 Overflow rejected before processing per connection | compaction/retry.rs retry_delay; model/retry_class.rs context classification | Classifier exists but does NOT prove provider processing/billing stage. Per-connection guarantee UNKNOWN; no real connection configuration or provider transaction inspected. Need authoritative provider contract or already-authorized evidence, not inference from error code. |
| 12.3 Protected-node positions | compaction.rs snapshot queries receipt accepted/held by target Thread; policy::safe_prefix/protected; range validation | SOURCE CONFIRMED protected messages can occur inside complete tool spans or elsewhere on path; whole containing group fenced. Existing tests (e.g. audit protection safe_prefix) support structure only. Actual positions/frequency/cutoff in user paths UNKNOWN; no private DB read. |
| 12.4 Bounded #A and indexes | context_usage latest_query; bounded.rs33 recent turns/201 replacement cap; attempts_by_turn, attempts_completed_scope, automatic_compaction_turn, turns_by_thread; thread_replacement PK | NOT PROVEN. Existing canonical query finds latest, not first completion after latest trial/manual adoption; bounded display CTE may exclude old boundary and is not exact #A. Automatic trial start/end/no-op and manual adopted scope chronology need exact tie/order semantics. Existing indexes not shown to yield finite seeks for all new predicates. No EXPLAIN proof or query run. No table/column/index added. If persistence/index needed, stop/report for separate approval. |
| 12.5 Single request duration | saved301 baseline summary UTC2026-11-11T22:10:50 source05e:32 Other stage facts total3749028ms; diagnostics observed starts before stage preparation/model/retry/adoption | Single-provider-request time UNKNOWN. Aggregate is stage duration with retry/preparation, not32 requests nor exact single request latency. Cannot divide into a valid request timing estimate. Drops37, possible rotation1, mismatched status/log denominator prevent completeness. No new measurement; no guarantee or performance optimization inferred. |

Read-only source checks do NOT fulfill unknown provider guarantees. No private
user DB/config read, paid compaction, owner startup, Rust/Node build/test or new
cloud dispatch was performed. The old cloud startup_failure remains unrelated
resource evidence, not proof of new design infeasibility.

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
  New persistence may need two nullable Thread settings; NO DDL/column/version
  is created here and no migration is reserved. #A must first be proven from
  current records. A new table is expressly forbidden.
- Manual compact in #A and unified summarizer across manual/range/overflow are
  Claude recommendations in the source, not rewritten as USER decisions.
- Overflow range replacement proposal: use the same structural closed/active
  eligibility rather than an invented segment-size constant; preserve existing
  failure-driven order. This is a review proposal, not implemented/approved code.
- Clarify before code: reliable latest-compression chronology (including no-op,
  manual adopted/reverted and connection/model changes), behavior for incomplete
  bounded evidence vs mandated missing=>0, exact identity of a "report value"
  (recommend attempt/report identity, not equal numeric token count), and config
  names/ranges. Do not resolve by silent fallback, hidden schema or new gate.
- Protected group data retention/reachability remains existing safety structure;
  do not turn it into summary quality assessment. No benchmarks, quality score,
  provider-window-size guess, cache optimization or extra retry cap is proposed.
