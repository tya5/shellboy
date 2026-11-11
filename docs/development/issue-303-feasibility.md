# Issue 303: Pre-Implementation Feasibility And Contract Proposal

Source checkpoint: 4e6241a9a3235bb6ceffb03574ae983c5919765b.
Source inspection only: no Rust/Node build, paid call, benchmark, bound change,
installed change, user DB access, migration reservation or B guard relaxation.

## Conclusion

A configurable 20% target is implementable as a planning policy, NOT a promise
that every 600000-token input can be reduced to 120000 tokens. It needs about
480000 fewer provider input tokens at the threshold (more if input overshoots).
Actual attainability is conditional on safe selectable content, retained input,
model output and bounded map/reduce completion. Current source does not prove
universal sufficiency, and several shapes are provably unable to reach it.
No budget/performance improvement is authorized by this report.

| Constraint | Actual source / default | Sufficiency / limit |
| --- | --- | --- |
| Paired estimator | proactive/target.rs plan uses same request token/character pair; ceil(excess*chars/input) | At exactly600k and 4chars/token example, desired removal1.92M chars. This ratio is an example, NOT observed workload/tokenizer proof. Fixed prompt, attachments and changed content can invalidate it; missing pair means no calibrated plan. |
| Map preflight | compaction.rs summarize_recorded counts uncached whole-group segments; rejects >128 before requests | At64k chars/segment, 1.94M selected chars would be roughly31 ideally packed segments, below128. 128*64k=8.192M is a nominal packing envelope, NOT guaranteed capacity: many indivisible small/large groups, extra prompt and cache misses matter. Actual needed segments UNKNOWN without workload. |
| Indivisible spans | compaction.rs segments; complete groups cannot be split | A group above segment_chars is rejected, not subdivided. Tool dependency boundaries/protected or incomplete groups can block a wide interval even when total chars look sufficient. |
| Reduce work | config.rs defaults reduce requests16, rounds4; bounded.rs reduce_groups and reduce_summary | Outputs must converge within BOTH limits. Source ID overhead and many batches consume requests. No guaranteed shrink ratio per response; even one oversized final output may exhaust rounds. No increase proposed. |
| Summary cap | summary_limit=min(20000 chars,5%source), floored at2000 | Caps requested/final summary text, not the entire post-compaction request; map parts/ID annotations/reduction and retained context remain. A20k-char summary is not a120k-token request bound. Nonshrinking/empty/refused results can fail. |
| Closed interval | policy normal_boundary/safe_prefix/target-prefix; range::validate | Current and latest completed Turn boundaries, latest tool group, incomplete/protected sources and actual eligible new work restrict range. Cannot select arbitrary480k tokens or repeatedly summarize unchanged summaries. |
| Active interval | policy target_active_range/active_turn_plan | Keeps request and prior context outside range; latest complete tool group excluded by target-active path or carried as plain text in active plan. User-message/protected fences stop widening. Cannot bypass these to meet20%. |
| Retained tail lower bound | whole latest group/current request/active plain latest tool payload plus fixed instructions and unsummarizable input | Structural minimum, NOT a configured token count. Can exceed120k itself; then goal is unattainable under existing policy. No measured numeric tail bound available. |
| Adoption and verification | #300/#301 source/current-tail/generation/Run/Turn/cancel guards; fresh reported usage verifies | Completion or prediction is not adoption or target success. Rejection, no safe wider source, exhausted budgets or unknown usage stay failure/defer/unknown, not fabricated success. |

Necessary condition: unavoidable retained input + replacement summaries + fixed
request overhead must fit120k provider tokens, and selectable replacement must
remove the remaining excess. This condition is not established for the user's
workload. The600000 threshold remains unchanged. Existing #208 observations
support the historical80% choice only: observed120000-159999 band upper edge,
median39 (W7) /39.5 (W3) subsequent requests. New20% is USER POLICY, not an
empirical efficiency claim or a context-capacity estimate.

## Proposed Functional Contract For Independent Review

- Name: auto_compact_target_percent; Thread nullable override -> connection
  nullable setting -> default20 (no new global policy layer).
- Integer1..=100 inclusive; reject fractions, negatives, >100 and zero. CLI empty
  string clears only Thread override. Effective get exposes resolved value even
  if threshold0 disables automatic compaction.
- Shared target_tokens(threshold,percent)=floor(u128(threshold)*percent/100),
  safely narrowed to u64. A small positive threshold can round to0; preserve
  this explicitly rather than silently clamp. Disabled threshold0 never triggers.
- Use identical resolved percent+function in target.rs plan and proactive.rs
  pending comparison/reset. Store percent in pending identity as well as target
  count: a changed percent invalidates pending even when rounding gives the same
  target. Legacy pending percent unknown resets rather than inferring80.
- Each new automatic attempt observation records the percent actually resolved
  at attempt start alongside existing target tokens, including uncalibrated
  plans; subsequent config edits do not relabel it. Old attempt records remain
  unknown/absent, never backfilled20/80 from token ratios.
- Thread persistence needs a new nullable field and canonical schema/CLI/docs
  updates; fresh main currently ends at0042. 0043 is another task's in-flight
  number: NO number reserved here. Coordinate before choosing next migration.
- Test inheritance/clear/effective/invalid/default20/explicit80; floor/overflow/
  disabled; pending percent change including equal rounded target; new attempt
  persistence/reopen/old absent; existing map/reduce/adoption guards unchanged.
- Public optional fields / strict consumers require coordinated schema generation.
  Update docs #209 to separate historical observation80 from USER policy20.

Implementation awaits independent feasibility/design review. Local Rust/Node
heavy hold stays in effect; later approved validation uses cloud CI, not a
silent local build. Performance optimization and paid measurement are deferred.

## S6 Conditional Review Clarifications

- 100% and floor-to-zero targets do not grant permission for busy retry, forced
  range widening, new budget, or adoption relaxation. Existing bounded failure,
  source witness and defer behavior remain authoritative.
- Pending identity uses the EFFECTIVE percentage. Clearing an override while
  inheriting the same value must not force invalidation merely because origin
  changed. Conversely, different effective percentages invalidate even when
  rounded target tokens coincide. Bind/reset under the existing transaction/CAS;
  attempt-start immutable observation is separate from mutable pending state.
- The default change from implicit80 to20 changes existing unset-user behavior.
  Leader must explicitly adopt it; document this compatibility change and how
  explicit80 retains the previous policy. Historical attempts remain UNKNOWN.
- Connection nullable values and strict numeric Thread strings reuse existing
  validation conventions. Optional public fields require schema/strict-client
  compatibility review; no claim that all consumers silently accept additions.
- 0043 remains coverage-exclusive. No schema implementation until fresh maximum
  and a distinct migration number/order have independent leader adoption.

## Persistence Proposal (Not Implemented / No Number Reserved)

One nullable Thread column is needed:
`auto_compact_target_percent INTEGER CHECK(auto_compact_target_percent IS NULL OR auto_compact_target_percent BETWEEN 1 AND 100)`.
Proposed DDL is a single ALTER TABLE thread ADD COLUMN with that definition.
Existing rows remain NULL/inherit, not backfilled20/80. This changes neither
applied0040-0042 nor coverage0043; filename/registry/version await leader order.
Attempt percentage uses the existing qualified automatic observation JSON, not
another model_attempt column; old observation absent stays UNKNOWN.

First source checkpoint intentionally wires connection/default only; Thread
storage/get/set and public schema remain incomplete pending migration allocation.
No local compile, formatting build or tests have run under the heavy hold.

## Authorized 0043 And Cloud-Only Preparation

Leader reassigned the unimplemented coverage0043 reservation to Core303 after
O confirmed its tree clean/no SQL/no persistence wiring. Fresh main4e6241a9 has
maximum0042. New0043 adds only the nullable checked Thread percentage;40-42 are
unchanged. Coverage next number is unassigned. Thread read/write, inheritance,
strict CLI empty clear and effective get are now connected (no None placeholder).
Pending CAS qualification occurs before the same-attempt branch, so changing the
effective policy cannot reuse a same-attempt old target; attempt history stays.

Cloud workflow `ci.yml` input `source_prepare=true` runs format/check, produces
format.patch + protocol.schema.json + source-sha artifact, then focused fixtures.
Download artifacts, verify source SHA, apply only own source formatting patch,
and replace generated schema from that artifact. No local Rust/Node execution.
The generated-content commit must then receive its own normal/all-OS exact-SHA
CI, independent review and fresh landing guards. Preparation is not final CI.
